//! PE 资源图标（`.exe` / `.dll` / `.ocx` / `.cpl` / `.scr` / `.sys` / `.mui`）。
//!
//! 图标在 PE 里是两层资源：`RT_GROUP_ICON`（14）描述"这个图标由哪几个尺寸组成"，
//! `RT_ICON`（3）才是每一帧的数据。只读 RT_ICON 会得到一堆没有尺寸信息的裸 DIB，
//! 所以必须按 group 组装成 ICO，再交给 `ico::read` 解码——这样 PNG 帧、
//! 32 位 DIB 帧、8 位调色板帧都走同一条已验证的路径。
//!
//! 一个 DLL 可能有几十个图标组（shell32 有几百个）。这里取"像素面积最大"的那组：
//! 对应用图标来说就是主图标；对系统 DLL 来说，取到的是它最大的那个资源，仍然可用。

use crate::error::IconError;
use crate::model::IconSet;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const MAX_RSRC: usize = 64 * 1024 * 1024;
const MAX_GROUPS: usize = 512;

struct Pe<'a> {
    data: &'a [u8],
    sections: Vec<Section>,
    rsrc_rva: u32,
    rsrc_size: u32,
}

#[derive(Clone, Copy)]
struct Section {
    va: u32,
    vsize: u32,
    raw: u32,
    raw_size: u32,
}

impl<'a> Pe<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, IconError> {
        if data.len() < 0x40 || &data[..2] != b"MZ" {
            return Err(IconError::Corrupt("不是 MZ 可执行文件".into()));
        }
        let e_lfanew = rd32(data, 0x3C).ok_or_else(|| IconError::Corrupt("DOS 头截断".into()))? as usize;
        if e_lfanew + 24 > data.len() || &data[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
            return Err(IconError::Corrupt("没有 PE 签名".into()));
        }
        let coff = e_lfanew + 4;
        let nsec = rd16(data, coff + 2).unwrap_or(0) as usize;
        let opt_size = rd16(data, coff + 16).unwrap_or(0) as usize;
        let opt = coff + 20;
        if opt + opt_size > data.len() || nsec == 0 || nsec > 96 {
            return Err(IconError::Corrupt("PE 头长度异常".into()));
        }
        let magic = rd16(data, opt).unwrap_or(0);
        let dd_off = match magic {
            0x10B => opt + 96,
            0x20B => opt + 112,
            _ => return Err(IconError::Unsupported(format!("PE optional magic 0x{magic:04X}"))),
        };
        let rsrc_rva = rd32(data, dd_off + 2 * 8).unwrap_or(0);
        let rsrc_size = rd32(data, dd_off + 2 * 8 + 4).unwrap_or(0);
        if rsrc_rva == 0 || rsrc_size == 0 {
            return Err(IconError::NoIcon("PE 没有资源目录".into()));
        }
        let mut sections = Vec::with_capacity(nsec);
        let stab = opt + opt_size;
        for i in 0..nsec {
            let s = stab + i * 40;
            if s + 40 > data.len() {
                break;
            }
            sections.push(Section {
                vsize: rd32(data, s + 8).unwrap_or(0),
                va: rd32(data, s + 12).unwrap_or(0),
                raw_size: rd32(data, s + 16).unwrap_or(0),
                raw: rd32(data, s + 20).unwrap_or(0),
            });
        }
        Ok(Pe { data, sections, rsrc_rva, rsrc_size })
    }

    fn off(&self, rva: u32) -> Option<usize> {
        for s in &self.sections {
            let span = s.vsize.max(s.raw_size);
            if rva >= s.va && rva < s.va.saturating_add(span) {
                let o = (rva - s.va + s.raw) as usize;
                if o < self.data.len() {
                    return Some(o);
                }
            }
        }
        None
    }

    fn rsrc_len(&self) -> usize {
        self.rsrc_size.min(MAX_RSRC as u32) as usize
    }

    /// 资源目录项：返回 (id, 是否子目录, 偏移)。命名项跳过。
    fn dir_entries(&self, rel: usize) -> Vec<(u32, bool, usize)> {
        let base = match self.off(self.rsrc_rva) {
            Some(b) => b,
            None => return Vec::new(),
        };
        if rel + 16 > self.rsrc_len() {
            return Vec::new();
        }
        let at = base + rel;
        if at + 16 > self.data.len() {
            return Vec::new();
        }
        let named = rd16(self.data, at + 12).unwrap_or(0) as usize;
        let ids = rd16(self.data, at + 14).unwrap_or(0) as usize;
        let total = (named + ids).min(4096);
        let mut out = Vec::new();
        for i in 0..total {
            let e = at + 16 + i * 8;
            if e + 8 > self.data.len() {
                break;
            }
            let name = rd32(self.data, e).unwrap_or(0);
            let off = rd32(self.data, e + 4).unwrap_or(0);
            if name & 0x8000_0000 != 0 {
                continue; // 命名项不是图标
            }
            out.push((name, off & 0x8000_0000 != 0, (off & 0x7FFF_FFFF) as usize));
        }
        out
    }

    /// 取叶子的数据（RVA + size）。
    fn leaf(&self, rel: usize) -> Option<&'a [u8]> {
        let base = self.off(self.rsrc_rva)?;
        let at = base + rel;
        if at + 16 > self.data.len() {
            return None;
        }
        let rva = rd32(self.data, at).unwrap_or(0);
        let size = rd32(self.data, at + 4).unwrap_or(0) as usize;
        if size == 0 || size > MAX_RSRC {
            return None;
        }
        let off = self.off(rva)?;
        if off + size > self.data.len() {
            return None;
        }
        Some(&self.data[off..off + size])
    }

    /// 收集所有图标组：(面积分, group 数据)。
    fn groups(&self) -> Vec<(u64, &'a [u8])> {
        let mut out = Vec::new();
        let Some((_, true, group_rel)) = self
            .dir_entries(0)
            .into_iter()
            .find(|(id, sub, _)| *id == RT_GROUP_ICON && *sub)
        else {
            return out;
        };
        for (id, sub, rel) in self.dir_entries(group_rel).into_iter().take(MAX_GROUPS) {
            let _ = id;
            if !sub {
                continue;
            }
            for (_, leaf_sub, leaf_rel) in self.dir_entries(rel) {
                if leaf_sub {
                    continue;
                }
                if let Some(g) = self.leaf(leaf_rel) {
                    out.push((group_score(g), g));
                }
            }
        }
        out
    }

    fn icons(&self) -> Vec<(u32, &'a [u8])> {
        let mut out = Vec::new();
        let Some((_, true, type_rel)) = self
            .dir_entries(0)
            .into_iter()
            .find(|(id, sub, _)| *id == RT_ICON && *sub)
        else {
            return out;
        };
        for (id, sub, rel) in self.dir_entries(type_rel).into_iter().take(MAX_GROUPS) {
            if !sub {
                continue;
            }
            for (_, leaf_sub, leaf_rel) in self.dir_entries(rel) {
                if leaf_sub {
                    continue;
                }
                if let Some(d) = self.leaf(leaf_rel) {
                    out.push((id, d));
                }
            }
        }
        out
    }
}

/// 组的"面积分"：最大边 × 条目数。用于在 DLL 的几十个组里挑主图标。
fn group_score(g: &[u8]) -> u64 {
    if g.len() < 6 {
        return 0;
    }
    let count = rd16(g, 4).unwrap_or(0) as usize;
    let mut max_side = 0u64;
    for i in 0..count.min(64) {
        let e = 6 + i * 14;
        if e + 14 > g.len() {
            break;
        }
        let w = if g[e] == 0 { 256 } else { g[e] as u64 };
        let h = if g[e + 1] == 0 { 256 } else { g[e + 1] as u64 };
        max_side = max_side.max(w.max(h));
    }
    max_side * max_side * count as u64
}

/// 从 PE 字节提取。`origin` 用于日志，通常是文件名。
pub fn extract(data: &[u8]) -> Result<IconSet, IconError> {
    let pe = Pe::parse(data)?;
    let groups = pe.groups();
    let icons = pe.icons();
    if groups.is_empty() && icons.is_empty() {
        return Err(IconError::NoIcon("PE 里没有 RT_GROUP_ICON / RT_ICON".into()));
    }

    let mut set = IconSet::new("PE 资源".to_string());
    let mut groups = groups;
    groups.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    let mut picked: Option<u64> = None;
    for (score, g) in groups {
        let Some(blob) = build_ico_from_group(g, &icons) else { continue };
        match crate::ico::read(&blob) {
            Ok(mut ico_set) => {
                for f in ico_set.frames.drain(..) {
                    let mut f = f;
                    f.origin = format!("PE 图标组（{}）", f.origin.trim_start_matches("ICO "));
                    set.push(f);
                }
                picked = Some(score);
                break;
            }
            Err(e) => {
                zx_core::log::warn(format!("PE 图标组解码失败（{score}）：{}", e.reason()));
                set = IconSet::new("PE 资源".to_string());
            }
        }
    }
    if let Some(score) = picked {
        set.detail = format!("PE 资源 · 组面积分 {score} · {} 帧（{}）", set.frames.len(), set.size_list());
    } else {
        // 没有 group 的 PE（部分精简过的 DLL）：裸 RT_ICON 也解，取前 64 个。
        for (id, d) in icons.into_iter().take(64) {
            let hint = format!("PE RT_ICON {id}");
            let one = if d.starts_with(b"\x89PNG\r\n\x1a\n") {
                crate::model::decode_png(d, &hint)
            } else {
                crate::model::decode_dib(d, true, &hint)
            };
            if let Ok(f) = one {
                set.push(f);
            }
        }
        if set.is_empty() {
            return Err(IconError::NoIcon("PE 的 RT_ICON 无法解码".into()));
        }
        set.detail = format!("PE 裸 RT_ICON · {} 帧（{}）", set.frames.len(), set.size_list());
    }
    Ok(set.finish())
}

/// GRPICONDIR + 引用的 RT_ICON 数据 → 标准 ICO 字节。
fn build_ico_from_group(group: &[u8], icons: &[(u32, &[u8])]) -> Option<Vec<u8>> {
    if group.len() < 6 {
        return None;
    }
    let count = rd16(group, 4)? as usize;
    let mut entries = Vec::new();
    for i in 0..count {
        let e = 6 + i * 14;
        if e + 14 > group.len() {
            break;
        }
        let id = rd16(group, e + 12)?;
        // 组里偶尔引用外部模块的图标：缺一帧就跳过它，不要整组报废。
        let Some(data) = icons.iter().find(|(n, _)| *n == id as u32).map(|(_, d)| *d) else {
            continue;
        };
        entries.push((
            [
                group[e],
                group[e + 1],
                group[e + 2],
                group[e + 3],
                group[e + 4],
                group[e + 5],
                group[e + 6],
                group[e + 7],
            ],
            data,
        ));
    }
    if entries.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len();
    for (head, data) in &entries {
        out.extend_from_slice(&head[..8]); // w,h,colors,reserved,planes,bpp
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in &entries {
        out.extend_from_slice(data);
    }
    Some(out)
}

fn rd16(d: &[u8], o: usize) -> Option<u16> {
    let s = d.get(o..o + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn rd32(d: &[u8], o: usize) -> Option<u32> {
    let s = d.get(o..o + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
