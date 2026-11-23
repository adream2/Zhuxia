//! Linux 三种主流包：deb / rpm / AppImage。
//!
//! 图标都遵循 freedesktop 约定：`.desktop` 里写 `Icon=名字`，真正的文件在
//! `usr/share/icons/hicolor/<尺寸>/apps/名字.png`（或 `usr/share/pixmaps/名字.png`）。
//! 所以提取逻辑是"先找桌面文件里的名字，再按名字收所有尺寸"，比"随便挑一张最大的 PNG"
//! 准确得多——包里的图标经常不止一个（状态图标、菜单图标、MIME 图标）。
//!
//! 老包只有 SVG 图标（尤其 hicolor/scalable），这里用 resvg 光栅化到 512px。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::IconError;
use crate::model::{decode_image, raster_svg, IconSet};

const MAX_ENTRY: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;

/* --------------------------------- deb --------------------------------- */

pub fn is_ar(data: &[u8]) -> bool {
    data.starts_with(b"!<arch>\n")
}

pub fn deb(path: &Path) -> Result<IconSet, IconError> {
    let members = ar_members(path)?;
    let (name, off, size) = members
        .into_iter()
        .find(|(n, _, _)| n.starts_with("data.tar"))
        .ok_or_else(|| IconError::Corrupt("deb 里没有 data.tar".into()))?;
    let mut f = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
    f.seek(SeekFrom::Start(off)).map_err(|e| crate::error::io_error(&e, path))?;
    let rd: Box<dyn Read> = decompress(Box::new(f.take(size)), &name)?;
    let mut tar = tar::Archive::new(rd);
    let mut entries = Vec::new();
    let mut total = 0u64;
    for e in tar.entries().map_err(|e| IconError::Corrupt(format!("tar：{e}")))? {
        let Ok(mut e) = e else { continue };
        if !e.header().entry_type().is_file() {
            continue;
        }
        let p = e.path().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        if !wanted(&p) {
            continue;
        }
        if e.size() > MAX_ENTRY {
            continue;
        }
        let mut buf = Vec::new();
        if e.read_to_end(&mut buf).is_err() {
            continue;
        }
        total += buf.len() as u64;
        if total > MAX_TOTAL {
            break;
        }
        entries.push((p, buf));
    }
    let set = pick_icons(&entries, "deb")?;
    Ok(set)
}

fn ar_members(path: &Path) -> Result<Vec<(String, u64, u64)>, IconError> {
    let mut f = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic).map_err(|_| IconError::Corrupt("ar 头截断".into()))?;
    if &magic != b"!<arch>\n" {
        return Err(IconError::Corrupt("不是 ar 归档".into()));
    }
    let mut out = Vec::new();
    let mut pos = 8u64;
    loop {
        f.seek(SeekFrom::Start(pos)).map_err(|e| crate::error::io_error(&e, path))?;
        let mut hdr = [0u8; 60];
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        if &hdr[58..60] != b"`\n" {
            break;
        }
        let name = String::from_utf8_lossy(&hdr[0..16]).trim().trim_end_matches('/').to_string();
        let size_s = String::from_utf8_lossy(&hdr[48..58]).trim().to_string();
        let size: u64 = size_s.parse().unwrap_or(0);
        let data = pos + 60;
        if !name.is_empty() && name != "/" && !name.starts_with("//") {
            out.push((name, data, size));
        }
        pos = data + size + (size & 1);
    }
    Ok(out)
}

/* --------------------------------- rpm --------------------------------- */

pub fn is_rpm(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == [0xED, 0xAB, 0xEE, 0xDB]
}

pub fn rpm(path: &Path) -> Result<IconSet, IconError> {
    let mut f = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    // 只读前 4MB 用来解析 lead/签名头/主头；负载再按偏移流式读，不整包进内存。
    let head_len = len.min(4 * 1024 * 1024) as usize;
    let mut head = vec![0u8; head_len];
    f.read_exact(&mut head).map_err(|e| crate::error::io_error(&e, path))?;
    if !is_rpm(&head) {
        return Err(IconError::Corrupt("不是 rpm 包".into()));
    }
    if head.len() < 112 {
        return Err(IconError::Corrupt("rpm 头截断".into()));
    }
    // 只有签名头之后需要 8 字节对齐；主头后面直接就是负载（多对齐 1–7 字节会
    // 错过压缩流的魔数，前面就是踩过这个坑）。
    let mut pos = 96usize;
    for i in 0..2 {
        if pos + 16 > head.len() {
            return Err(IconError::Corrupt("rpm 头超过 4MB（异常）".into()));
        }
        let nindex = u32::from_be_bytes([head[pos + 8], head[pos + 9], head[pos + 10], head[pos + 11]]) as usize;
        let hsize = u32::from_be_bytes([head[pos + 12], head[pos + 13], head[pos + 14], head[pos + 15]]) as usize;
        pos = pos.saturating_add(16 + nindex * 16 + hsize);
        if i == 0 {
            pos = (pos + 7) & !7;
        }
    }
    let start = pos as u64;
    if start >= len {
        return Err(IconError::Corrupt("找不到 rpm 负载".into()));
    }
    f.seek(SeekFrom::Start(start)).map_err(|e| crate::error::io_error(&e, path))?;
    let mut magic = [0u8; 6];
    let got = f.read(&mut magic).unwrap_or(0);
    let suffix = if magic[..got.min(2)] == [0x1F, 0x8B] {
        ".gz"
    } else if magic[..got.min(6)] == [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00] {
        ".xz"
    } else if magic[..got.min(4)] == [0x28, 0xB5, 0x2F, 0xFD] {
        ".zst"
    } else if &magic[..got.min(3)] == b"BZh" {
        ".bz2"
    } else {
        ""
    };
    f.seek(SeekFrom::Start(start)).map_err(|e| crate::error::io_error(&e, path))?;
    let rd: Box<dyn Read> = decompress(Box::new(f.take(len - start)), &format!("payload{suffix}"))?;
    let mut entries = Vec::new();
    cpio_entries(rd, &mut entries)?;
    pick_icons(&entries, "rpm")
}

/// cpio newc（rpm 负载）。
fn cpio_entries(mut rd: Box<dyn Read>, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), IconError> {
    let mut total = 0u64;
    loop {
        let mut hdr = [0u8; 110];
        if read_full(&mut rd, &mut hdr).is_err() {
            break;
        }
        if &hdr[..6] != b"070701" && &hdr[..6] != b"070702" {
            return Err(IconError::Corrupt("cpio 头不匹配".into()));
        }
        let hex = |o: usize| -> u64 {
            std::str::from_utf8(&hdr[o..o + 8])
                .ok()
                .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
                .unwrap_or(0)
        };
        let mode = hex(14);
        let filesize = hex(54) as usize;
        let namesize = hex(94) as usize;
        if namesize == 0 || namesize > 4096 {
            return Err(IconError::Corrupt("cpio 文件名长度异常".into()));
        }
        let mut namebuf = vec![0u8; namesize];
        if read_full(&mut rd, &mut namebuf).is_err() {
            break;
        }
        let name = String::from_utf8_lossy(&namebuf[..namesize.saturating_sub(1)]).to_string();
        let pad = (4 - (110 + namesize) % 4) % 4;
        let mut skip = [0u8; 4];
        if pad > 0 && read_full(&mut rd, &mut skip[..pad]).is_err() {
            break;
        }
        if name == "TRAILER!!!" {
            break;
        }
        let is_file = mode & 0o170000 == 0o100000;
        if is_file && wanted(&name) && filesize as u64 <= MAX_ENTRY {
            let mut buf = vec![0u8; filesize];
            if read_full(&mut rd, &mut buf).is_err() {
                break;
            }
            total += buf.len() as u64;
            out.push((name, buf));
        } else if filesize > 0 {
            if std::io::copy(&mut rd.by_ref().take(filesize as u64), &mut std::io::sink()).is_err() {
                break;
            }
        }
        let dpad = (4 - filesize % 4) % 4;
        if dpad > 0 && std::io::copy(&mut rd.by_ref().take(dpad as u64), &mut std::io::sink()).is_err() {
            break;
        }
        if total > MAX_TOTAL {
            break;
        }
    }
    Ok(())
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let n = r.read(&mut buf[off..])?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
        }
        off += n;
    }
    Ok(())
}

/* ------------------------------- AppImage ------------------------------- */

pub fn appimage(path: &Path) -> Result<IconSet, IconError> {
    let mut f = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < 1024 * 1024 {
        return Err(IconError::Corrupt("AppImage 过小".into()));
    }
    // squashfs 魔数在文件尾部（AppImage 运行时在前，文件系统在后）。
    let mut offsets = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    while pos < len && offsets.len() < 16 {
        f.seek(SeekFrom::Start(pos)).map_err(|e| crate::error::io_error(&e, path))?;
        let n = f.read(&mut buf).map_err(|e| crate::error::io_error(&e, path))?;
        if n == 0 {
            break;
        }
        for i in 0..n.saturating_sub(4) {
            if &buf[i..i + 4] == b"hsqs" {
                offsets.push(pos + i as u64);
                if offsets.len() >= 16 {
                    break;
                }
            }
        }
        if n < buf.len() {
            break;
        }
        pos += n as u64 - 3;
    }
    if offsets.is_empty() {
        return Err(IconError::Unsupported("不是 type-2 AppImage（没有 squashfs）".into()));
    }
    offsets.reverse(); // 通常最后一个是文件系统
    let mut last_err = None;
    for off in offsets {
        let file = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
        let rd = std::io::BufReader::new(file);
        match backhand::FilesystemReader::from_reader_with_offset(rd, off) {
            Ok(fs) => match appimage_icons(&fs) {
                Ok(set) => return Ok(set),
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(IconError::Corrupt(format!("squashfs 解析失败：{e}"))),
        }
    }
    Err(last_err.unwrap_or_else(|| IconError::NoIcon("AppImage 里没有图标".into())))
}

fn appimage_icons(fs: &backhand::FilesystemReader<'_>) -> Result<IconSet, IconError> {
    use backhand::InnerNode;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0u64;
    let mut desk_name = None;
    let mut dir_link: Option<std::path::PathBuf> = None;
    for node in fs.files() {
        let path = node.fullpath.to_string_lossy().replace('\\', "/");
        if path == "/.DirIcon" {
            if let InnerNode::Symlink(s) = &node.inner {
                dir_link = Some(s.link.clone());
                continue;
            }
        }
        let is_file = matches!(node.inner, InnerNode::File(_));
        if !is_file {
            continue;
        }
        let want = path.ends_with(".desktop") || wanted(&path) || path == "/.DirIcon";
        if !want {
            continue;
        }
        let InnerNode::File(f) = &node.inner else { continue };
        let mut buf = Vec::new();
        if fs.file(f).reader().take(MAX_ENTRY).read_to_end(&mut buf).is_err() {
            continue;
        }
        total += buf.len() as u64;
        if total > MAX_TOTAL {
            break;
        }
        if path.ends_with(".desktop") {
            if let Some(n) = desktop_icon(&buf) {
                desk_name = Some(n);
            }
        }
        entries.push((path, buf));
    }
    let mut set = pick_icons_with_name(&entries, "AppImage", desk_name.as_deref())?;
    // .DirIcon 是打包者指定的图标：和已有帧同尺寸时由 push 去重，不够大才补。
    // 它通常是符号链接，指向根目录或 usr/share/icons 里的真实文件。
    if let Some((_, data)) = entries.iter().find(|(p, _)| p == "/.DirIcon") {
        if let Ok(f) = decode_any_icon(data, ".DirIcon") {
            set.push(f);
        }
    } else if let Some(link) = dir_link {
        let target = if link.is_absolute() { link } else { std::path::PathBuf::from("/").join(link) };
        if let Some(node) = fs.files().find(|n| n.fullpath == target) {
            if let InnerNode::File(f) = &node.inner {
                let mut buf = Vec::new();
                if fs.file(f).reader().take(MAX_ENTRY).read_to_end(&mut buf).is_ok() {
                    if let Ok(frame) = decode_any_icon(&buf, ".DirIcon") {
                        set.push(frame);
                    }
                }
            }
        }
    }
    set.detail = format!("AppImage · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

/* ------------------------------ 共同选择逻辑 ------------------------------ */

/// 候选文件：桌面文件或图标图片。
fn wanted(p: &str) -> bool {
    let low = p.to_ascii_lowercase();
    if low.ends_with(".desktop") {
        return low.contains("/applications/") || !low.contains('/');
    }
    let is_img = low.ends_with(".png")
        || low.ends_with(".webp")
        || low.ends_with(".jpg")
        || low.ends_with(".jpeg")
        || low.ends_with(".svg");
    if !is_img {
        return false;
    }
    low.contains("/icons/") || low.contains("/pixmaps/")
}

fn pick_icons(entries: &[(String, Vec<u8>)], tag: &str) -> Result<IconSet, IconError> {
    let name = entries
        .iter()
        .filter(|(p, _)| p.to_ascii_lowercase().ends_with(".desktop"))
        .find_map(|(_, d)| desktop_icon(d));
    pick_icons_with_name(entries, tag, name.as_deref())
}

fn pick_icons_with_name(
    entries: &[(String, Vec<u8>)],
    tag: &str,
    desktop_name: Option<&str>,
) -> Result<IconSet, IconError> {
    let images: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, (p, _))| {
            let low = p.to_ascii_lowercase();
            low.ends_with(".png")
                || low.ends_with(".webp")
                || low.ends_with(".jpg")
                || low.ends_with(".jpeg")
                || low.ends_with(".svg")
        })
        .map(|(i, _)| i)
        .collect();
    if images.is_empty() {
        return Err(IconError::NoIcon(format!("{tag}：包里没有图标图片")));
    }

    let mut chosen: Vec<usize> = Vec::new();
    if let Some(name) = desktop_name {
        let base = name.trim().trim_end_matches(".png").trim_end_matches(".svg");
        let base = base.rsplit('/').next().unwrap_or(base).to_ascii_lowercase();
        for &i in &images {
            let stem = stem_of(&entries[i].0).to_ascii_lowercase();
            if stem == base {
                chosen.push(i);
            }
        }
    }
    if chosen.is_empty() {
        // 没有桌面文件/名字对不上：按基名分组，选"尺寸最大且份数最多"的那组。
        use std::collections::HashMap;
        let mut groups: HashMap<String, (u32, usize)> = HashMap::new();
        for &i in &images {
            let stem = stem_of(&entries[i].0);
            let hint = size_hint(&entries[i].0);
            let e = groups.entry(stem).or_insert((0, 0));
            e.0 = e.0.max(hint);
            e.1 += 1;
        }
        let best = groups
            .iter()
            .max_by_key(|(_, (hint, count))| (*hint, *count))
            .map(|(k, _)| k.clone());
        if let Some(best) = best {
            for &i in &images {
                if stem_of(&entries[i].0) == best {
                    chosen.push(i);
                }
            }
        }
    }
    if chosen.is_empty() {
        return Err(IconError::NoIcon(format!("{tag}：没有与 Icon= 匹配的文件")));
    }

    let mut set = IconSet::new(format!("{tag} · {}", stem_of(&entries[chosen[0]].0)));
    for i in chosen.into_iter().take(16) {
        let (p, data) = &entries[i];
        match decode_any_icon(data, p) {
            Ok(f) => set.push(f),
            Err(e) => zx_core::log::warn(format!("{tag} 图标 {p} 跳过：{}", e.reason())),
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon(format!("{tag}：候选图标都无法解码")));
    }
    set.detail = format!("{tag} · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

fn decode_any_icon(data: &[u8], name: &str) -> Result<crate::model::Frame, IconError> {
    if name.to_ascii_lowercase().ends_with(".svg") {
        raster_svg(data, 512, name)
    } else {
        decode_image(data, name)
    }
}

/// 从 `hicolor/256x256/apps/foo.png` 里取 256；pixmaps 没有尺寸线索返回 0。
fn size_hint(p: &str) -> u32 {
    let low = p.to_ascii_lowercase();
    for seg in low.split('/') {
        if let Some((a, b)) = seg.split_once('x') {
            if let (Ok(x), Ok(y)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if x == y {
                    return x;
                }
            }
        }
    }
    if low.contains("scalable") {
        return 1024;
    }
    0
}

fn stem_of(p: &str) -> String {
    Path::new(p)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(p)
        .to_string()
}

fn desktop_icon(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Icon=") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/* ------------------------------- 解压分派 ------------------------------- */

fn decompress(rd: Box<dyn Read>, name: &str) -> Result<Box<dyn Read>, IconError> {
    let low = name.to_ascii_lowercase();
    if low.ends_with(".gz") || low.ends_with(".tgz") {
        Ok(Box::new(flate2::read::GzDecoder::new(rd)))
    } else if low.ends_with(".xz") || low.ends_with(".lzma") {
        Ok(Box::new(liblzma::read::XzDecoder::new(rd)))
    } else if low.ends_with(".zst") || low.ends_with(".zstd") {
        let d = ruzstd::decoding::StreamingDecoder::new(rd)
            .map_err(|e| IconError::Corrupt(format!("zstd：{e}")))?;
        Ok(Box::new(d))
    } else if low.ends_with(".bz2") {
        Ok(Box::new(bzip2::read::BzDecoder::new(rd)))
    } else {
        Ok(rd)
    }
}
