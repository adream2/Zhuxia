//! `resources.arsc` 解析：把 `android:icon="@0x7f0e0001"` 这样的资源 id 换成
//! `res/mipmap-xxxhdpi-v4/ic_launcher.png` 这样的真实路径。
//!
//! 只实现图标需要的三件事：全局字符串池、包/类型字符串池、type 表里的条目查找。
//! 配置维度只关心 density——同一张图有 mdpi/hdpi/xhdpi/xxhdpi/xxxhdpi 五份，
//! 取密度最高的那份（APK 里通常就是最清晰的）。

use crate::error::IconError;

#[derive(Clone, Debug)]
pub enum Resolved {
    Str(String),
    Ref(u32),
    Int(i32),
}

struct Pool {
    strings: Vec<String>,
}

impl Pool {
    fn get(&self, i: u32) -> Option<String> {
        self.strings.get(i as usize).cloned()
    }
}

pub struct Arsc {
    data: Vec<u8>,
    global: Pool,
    packages: Vec<Package>,
}

struct Package {
    id: u32,
    entries: Vec<TypeEntry>,
}

struct TypeEntry {
    type_id: u8,
    index: u32,
    density: u16,
    dtype: u8,
    data: u32,
}

impl Arsc {
    pub fn parse(data: &[u8]) -> Result<Arsc, IconError> {
        if data.len() < 12 || u16::from_le_bytes([data[0], data[1]]) != 0x0002 {
            return Err(IconError::Corrupt("不是 resources.arsc".into()));
        }
        let hdr = u16::from_le_bytes([data[2], data[3]]) as usize;
        let total = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let end = total.min(data.len());
        let global = parse_pool(data, hdr, end).ok_or_else(|| IconError::Corrupt("ARSC 全局字符串池缺失".into()))?;
        let mut packages = Vec::new();
        let mut p = hdr;
        // 全局字符串池后面紧跟包
        if let Some(sz) = chunk_size(data, p) {
            p += sz.max(8);
        }
        while p + 8 <= end {
            let ty = u16::from_le_bytes([data[p], data[p + 1]]);
            let Some(size) = chunk_size(data, p) else { break };
            if size < 8 || p + size > end {
                break;
            }
            if ty == 0x0200 {
                if let Some(pkg) = parse_package(data, p, size, end) {
                    packages.push(pkg);
                }
            }
            p += size;
        }
        Ok(Arsc { data: data.to_vec(), global, packages })
    }

    pub fn resolve(&self, id: u32) -> Option<Resolved> {
        self.resolve_depth(id, 0)
    }

    fn resolve_depth(&self, id: u32, depth: u32) -> Option<Resolved> {
        if depth > 4 {
            return None;
        }
        let pkg_id = (id >> 24) & 0xFF;
        let type_id = ((id >> 16) & 0xFF) as u8;
        let index = id & 0xFFFF;
        let pkg = self.packages.iter().find(|p| p.id == pkg_id)?;
        let best = pkg
            .entries
            .iter()
            .filter(|e| e.type_id == type_id && e.index == index)
            .max_by_key(|e| e.density)?;
        match best.dtype {
            0x03 => self.global.get(best.data).map(Resolved::Str),
            0x01 => self.resolve_depth(best.data, depth + 1),
            0x10 | 0x11 => Some(Resolved::Int(best.data as i32)),
            _ => None,
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

fn parse_package(data: &[u8], base: usize, size: usize, end: usize) -> Option<Package> {
    if size < 288 || base + 288 > end {
        return None;
    }
    let id = rd32(data, base + 8)?;
    // 类型池 / 键池只在"按名字反查资源"时才需要；当前实现只按 resource id 取图标，
    // 解析出来没人用（以前 `type_name` 读取过 types，那个调试函数已删），不再做无谓解析。

    let hdr = u16::from_le_bytes([data[base + 2], data[base + 3]]) as usize;
    let mut entries = Vec::new();
    let mut p = base + hdr;
    while p + 8 <= base + size {
        let ty = u16::from_le_bytes([data[p], data[p + 1]]);
        let Some(csize) = chunk_size(data, p) else { break };
        if csize < 8 || p + csize > base + size {
            break;
        }
        if ty == 0x0201 {
            parse_type(data, p, csize, &mut entries);
        }
        p += csize;
    }
    Some(Package { id, entries })
}

fn parse_type(data: &[u8], base: usize, size: usize, out: &mut Vec<TypeEntry>) {
    let type_id = match data.get(base + 8) {
        Some(v) => *v,
        None => return,
    };
    let flags = data.get(base + 9).copied().unwrap_or(0);
    let entry_count = rd32(data, base + 12).unwrap_or(0) as usize;
    let entries_start = rd32(data, base + 16).unwrap_or(0) as usize;
    let config = base + 20;
    let density = if rd32(data, config).unwrap_or(0) >= 16 {
        rd16(data, config + 14).unwrap_or(0)
    } else {
        0
    };
    if entry_count > 200_000 {
        return;
    }
    let offsets = base + 20 + rd32(data, config).unwrap_or(0) as usize;
    let ebase = base + entries_start;
    for i in 0..entry_count {
        let (index, off) = if flags & 0x01 != 0 {
            // sparse：u16 索引 + u16 offset/4
            let o = offsets + i * 4;
            let idx = rd16(data, o).unwrap_or(0) as u32;
            let off = rd16(data, o + 2).unwrap_or(0xFFFF) as u32;
            (idx, if off == 0xFFFF { None } else { Some(off as usize * 4) })
        } else if flags & 0x02 != 0 {
            let o = offsets + i * 2;
            let v = rd16(data, o).unwrap_or(0xFFFF) as u32;
            (i as u32, if v == 0xFFFF { None } else { Some(v as usize * 4) })
        } else {
            let o = offsets + i * 4;
            let v = rd32(data, o).unwrap_or(0xFFFF_FFFF);
            (i as u32, if v == 0xFFFF_FFFF { None } else { Some(v as usize) })
        };
        let Some(off) = off else { continue };
        let e = ebase + off;
        if e + 8 > base + size || e + 8 > data.len() {
            continue;
        }
        let esize = rd16(data, e).unwrap_or(0) as usize;
        let eflags = rd16(data, e + 2).unwrap_or(0);
        if eflags & 0x01 != 0 {
            continue; // 复杂条目（bag），图标不会用它
        }
        let v = e + esize.max(8);
        if v + 8 > data.len() || v + 8 > base + size {
            continue;
        }
        let dtype = data.get(v + 3).copied().unwrap_or(0);
        let ddata = rd32(data, v + 4).unwrap_or(0);
        out.push(TypeEntry { type_id, index, density, dtype, data: ddata });
    }
}

/* ------------------------------ 字符串池 ------------------------------ */

fn parse_pool(data: &[u8], base: usize, end: usize) -> Option<Pool> {
    if base + 28 > data.len() || base + 28 > end {
        return None;
    }
    if u16::from_le_bytes([data[base], data[base + 1]]) != 0x0001 {
        return None;
    }
    let size = u32::from_le_bytes([data[base + 4], data[base + 5], data[base + 6], data[base + 7]]) as usize;
    if size < 28 || base + size > end {
        return None;
    }
    let count = rd32(data, base + 8)? as usize;
    let flags = rd32(data, base + 16)?;
    let strings_start = rd32(data, base + 20)? as usize;
    let utf8 = flags & 0x100 != 0;
    if count > 500_000 {
        return None;
    }
    let mut strings = Vec::with_capacity(count);
    for i in 0..count {
        let off = rd32(data, base + 28 + i * 4)? as usize;
        let at = base + strings_start + off;
        if at >= base + size {
            strings.push(String::new());
            continue;
        }
        let s = if utf8 {
            read_utf8(data, at, base + size)
        } else {
            read_utf16(data, at, base + size)
        };
        strings.push(s.unwrap_or_default());
    }
    Some(Pool { strings })
}

fn read_utf8(data: &[u8], at: usize, end: usize) -> Option<String> {
    let mut p = at;
    let (_u16len, n) = read_len8(data, p)?;
    p += n;
    let (blen, n) = read_len8(data, p)?;
    p += n;
    if p + blen > end || p + blen > data.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&data[p..p + blen]).to_string())
}

fn read_len8(data: &[u8], p: usize) -> Option<(usize, usize)> {
    let b = *data.get(p)?;
    if b & 0x80 != 0 {
        Some((((b as usize & 0x7F) << 8) | *data.get(p + 1)? as usize, 2))
    } else {
        Some((b as usize, 1))
    }
}

fn read_utf16(data: &[u8], at: usize, end: usize) -> Option<String> {
    let mut p = at;
    let first = rd16(data, p)? as usize;
    p += 2;
    let len = if first & 0x8000 != 0 {
        let second = rd16(data, p)? as usize;
        p += 2;
        ((first & 0x7FFF) << 16) | second
    } else {
        first
    };
    if len > 200_000 || p + len * 2 > end || p + len * 2 > data.len() {
        return None;
    }
    let s: Vec<u16> = (0..len)
        .map(|i| u16::from_le_bytes([data[p + i * 2], data[p + i * 2 + 1]]))
        .collect();
    Some(String::from_utf16_lossy(&s))
}

fn chunk_size(data: &[u8], p: usize) -> Option<usize> {
    rd32(data, p + 4).map(|v| v as usize)
}
fn rd16(d: &[u8], o: usize) -> Option<u16> {
    let s = d.get(o..o + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}
fn rd32(d: &[u8], o: usize) -> Option<u32> {
    let s = d.get(o..o + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
