//! 快捷方式（`.lnk`）提取：先看它指向的图标，再退到目标程序的图标。
//!
//! LNK 只是"一层指针"：真正好看的图标在它指向的 exe/dll/ico 里。顺序是
//! **IconLocation → 目标路径 → 自身所在目录的同名图标**。直接读 `%SystemRoot%`
//! 这类环境变量、`shell32.dll,3` 这类索引都要处理，否则大量快捷方式会"没图标"。

use std::path::{Path, PathBuf};

use crate::error::IconError;

pub fn is_lnk(data: &[u8]) -> bool {
    data.len() >= 0x4C && &data[..4] == &[0x4C, 0x00, 0x00, 0x00]
}

#[derive(Debug, Default)]
pub struct Link {
    pub icon_location: Option<String>,
    pub target: Option<String>,
    pub working_dir: Option<String>,
}

/// 只做结构解析，不碰文件系统（方便单测）。
pub fn parse(data: &[u8]) -> Result<Link, IconError> {
    if data.len() < 0x4C {
        return Err(IconError::Corrupt("LNK 头过短".into()));
    }
    if &data[4..20] != &[
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
    ] {
        return Err(IconError::Corrupt("LNK CLSID 不匹配".into()));
    }
    let flags = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let unicode = flags & 0x80 != 0;
    let has_idlist = flags & 0x01 != 0;
    let has_linkinfo = flags & 0x02 != 0;
    let has_name = flags & 0x04 != 0;
    let has_relpath = flags & 0x08 != 0;
    let has_workdir = flags & 0x10 != 0;
    let has_args = flags & 0x20 != 0;
    let has_icon = flags & 0x40 != 0;

    let mut pos = 0x4Cusize;
    if has_idlist {
        let n = rd16(data, pos).ok_or_else(|| IconError::Corrupt("IDList 长度缺失".into()))? as usize;
        pos = pos.saturating_add(2 + n);
    }

    let mut link = Link::default();
    if has_linkinfo && pos + 4 <= data.len() {
        let size = rd32(data, pos).unwrap_or(0) as usize;
        if size >= 0x1C && pos + size <= data.len() {
            let hdr = rd32(data, pos + 4).unwrap_or(0) as usize;
            let li_flags = rd32(data, pos + 8).unwrap_or(0);
            let local_off = rd32(data, pos + 16).unwrap_or(0) as usize;
            let suffix_off = rd32(data, pos + 24).unwrap_or(0) as usize;
            let local_uni = if hdr >= 0x24 { rd32(data, pos + 28).unwrap_or(0) as usize } else { 0 };
            let suffix_uni = if hdr >= 0x24 { rd32(data, pos + 32).unwrap_or(0) as usize } else { 0 };

            let mut base: Option<String> = None;
            if local_uni != 0 && li_flags & 1 != 0 {
                base = read_cstr16(data, pos + local_uni);
            } else if local_off != 0 {
                base = read_cstr8(data, pos + local_off);
            }
            if let Some(mut b) = base {
                if suffix_uni != 0 {
                    if let Some(s) = read_cstr16(data, pos + suffix_uni) {
                        b.push_str(&s);
                    }
                } else if suffix_off != 0 {
                    if let Some(s) = read_cstr8(data, pos + suffix_off) {
                        b.push_str(&s);
                    }
                }
                link.target = Some(b);
            }
            pos += size;
        }
    }

    // StringData 顺序固定：NAME / RELATIVE_PATH / WORKING_DIR / ARGUMENTS / ICON_LOCATION
    let mut vals: [Option<String>; 5] = Default::default();
    for (i, present) in [has_name, has_relpath, has_workdir, has_args, has_icon]
        .into_iter()
        .enumerate()
    {
        if !present {
            continue;
        }
        let Some(s) = read_counted(data, &mut pos, unicode) else { break };
        vals[i] = Some(s);
    }
    if has_workdir {
        link.working_dir = vals[2].clone().filter(|s| !s.is_empty());
    }
    if has_icon {
        link.icon_location = vals[4].clone().filter(|s| !s.is_empty());
    }
    if link.target.is_none() {
        if let Some(rel) = vals[1].clone().filter(|s| !s.is_empty()) {
            link.target = Some(rel);
        }
    }

    // ExtraData：IconEnvironmentDataBlock（0xA0000007）里还有一份展开后的图标路径。
    let mut p = pos;
    while p + 8 <= data.len() {
        let size = rd32(data, p).unwrap_or(0) as usize;
        let sig = rd32(data, p + 4).unwrap_or(0);
        if size < 8 || p + size > data.len() {
            break;
        }
        if sig == 0xA000_0007 && size >= 8 + 260 + 520 {
            if let Some(s) = read_cstr16(data, p + 8 + 260) {
                if !s.is_empty() && link.icon_location.is_none() {
                    link.icon_location = Some(s);
                }
            }
        }
        p += size;
    }
    Ok(link)
}

fn read_counted(data: &[u8], pos: &mut usize, unicode: bool) -> Option<String> {
    let n = rd16(data, *pos)? as usize;
    *pos += 2;
    if unicode {
        let need = n * 2;
        let s = data.get(*pos..*pos + need)?;
        *pos += need;
        Some(String::from_utf16_lossy(
            &s.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        ))
    } else {
        let s = data.get(*pos..*pos + n)?;
        *pos += n;
        Some(String::from_utf8_lossy(s).to_string())
    }
}

fn read_cstr16(data: &[u8], off: usize) -> Option<String> {
    let mut out = Vec::new();
    let mut p = off;
    while p + 2 <= data.len() {
        let c = u16::from_le_bytes([data[p], data[p + 1]]);
        if c == 0 {
            break;
        }
        out.push(c);
        p += 2;
    }
    if out.is_empty() {
        None
    } else {
        Some(String::from_utf16_lossy(&out))
    }
}

fn read_cstr8(data: &[u8], off: usize) -> Option<String> {
    let s = data.get(off..)?;
    let end = s.iter().position(|b| *b == 0).unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(String::from_utf8_lossy(&s[..end]).to_string())
    }
}

fn rd16(d: &[u8], o: usize) -> Option<u16> {
    let s = d.get(o..o + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}
fn rd32(d: &[u8], o: usize) -> Option<u32> {
    let s = d.get(o..o + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// 从快捷方式找图标：图标路径 → 目标路径。返回第一个存在的文件。
pub fn resolve(lnk: &Link, lnk_path: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(s) = &lnk.icon_location {
        candidates.push(strip_index(s));
    }
    if let Some(s) = &lnk.target {
        candidates.push(strip_index(s));
    }
    if let Some(w) = &lnk.working_dir {
        if let Some(name) = lnk_path.file_stem().and_then(|s| s.to_str()) {
            candidates.push(format!("{w}\\{name}.ico"));
        }
    }
    for c in candidates {
        let expanded = expand_env(&c);
        let p = PathBuf::from(expanded);
        let p = if p.is_absolute() {
            p
        } else {
            lnk_path.parent().map(|d| d.join(&p)).unwrap_or(p)
        };
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// `"C:\x\app.exe",0` / `C:\x\app.exe,0` → 路径。
fn strip_index(s: &str) -> String {
    let s = s.trim().trim_matches('"');
    if let Some((head, tail)) = s.rsplit_once(',') {
        if tail.trim().chars().all(|c| c.is_ascii_digit() || c == '-' || c == ' ') {
            return head.trim().trim_matches('"').to_string();
        }
    }
    s.to_string()
}

/// 展开 `%SystemRoot%` 这类变量（不依赖 shell）。
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(j) = after.find('%') {
            let name = &after[..j];
            match std::env::var(name) {
                Ok(v) => out.push_str(&v),
                Err(_) => {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
            rest = &after[j + 1..];
        } else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}
