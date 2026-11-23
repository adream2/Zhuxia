//! MSI（Windows Installer）：OLE 复合文档里的二进制图标。
//!
//! 图标通常在 `Icon` 存储里，流名就是 Icon 表的 Name 列。**但 MSI 的流名是压缩编码
//! 过的**（不是可读 UTF-16），部分安装包/缓存 MSI 还会把表拆到别处，按名字找
//! `Icon/xxx` 会直接失效。所以这里按**内容**扫：先读 16 字节看 magic，是 PE/ICO/
//! PNG/JPEG 才继续读。代价是每个流多读十几字节，收益是不挑包。
//!
//! 多个图标时取面积最大的那组：主程序图标几乎总是最大、颜色最多的那一个。

use std::io::Read;

use crate::error::IconError;
use crate::model::IconSet;

const MAX_STREAM: usize = 32 * 1024 * 1024;
const MAX_TOTAL: u64 = 128 * 1024 * 1024;

pub fn is_cfb(data: &[u8]) -> bool {
    data.len() >= 8 && data[..8] == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
}

pub fn extract(data: &[u8]) -> Result<IconSet, IconError> {
    if !is_cfb(data) {
        return Err(IconError::Corrupt("不是 OLE 复合文档".into()));
    }
    let mut cf = cfb::CompoundFile::open(std::io::Cursor::new(data.to_vec()))
        .map_err(|e| IconError::Corrupt(format!("OLE 打开失败：{e}")))?;

    let mut paths: Vec<String> = Vec::new();
    for entry in cf.walk() {
        if entry.is_stream() {
            paths.push(entry.path().to_string_lossy().replace('\\', "/"));
        }
    }
    if paths.is_empty() {
        return Err(IconError::NoIcon("MSI 里没有流".into()));
    }

    let mut best: Option<(u32, IconSet)> = None;
    let mut scanned = 0usize;
    let mut total = 0u64;
    for p in paths {
        if scanned >= 4096 || total > MAX_TOTAL {
            break;
        }
        scanned += 1;
        let Ok(mut s) = cf.open_stream(&p) else { continue };
        let mut head = [0u8; 16];
        let mut got = 0usize;
        while got < head.len() {
            match s.read(&mut head[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => break,
            }
        }
        if !looks_icon(&head[..got]) {
            continue;
        }
        let mut buf = head[..got].to_vec();
        if s.by_ref().take(MAX_STREAM as u64).read_to_end(&mut buf).is_err() || buf.len() > MAX_STREAM {
            continue;
        }
        total += buf.len() as u64;
        if let Ok(set) = decode_stream(&buf, &p) {
            if set.is_empty() {
                continue;
            }
            let side = set.max_side();
            if best.as_ref().map(|(b, _)| side > *b).unwrap_or(true) {
                best = Some((side, set));
            }
        }
    }
    let Some((_, mut set)) = best else {
        return Err(IconError::NoIcon("MSI 的流里没有图标（Icon/Binary 表可能为空）".into()));
    };
    set.detail = format!("MSI · {} · {} 帧（{}）", set.detail, set.frames.len(), set.size_list());
    Ok(set.finish())
}

fn looks_icon(d: &[u8]) -> bool {
    d.starts_with(b"MZ")
        || (d.len() >= 4 && d[..4] == [0, 0, 1, 0])
        || d.starts_with(b"\x89PNG")
        || d.starts_with(b"\xFF\xD8\xFF")
        || (d.len() > 12 && &d[..4] == b"RIFF" && &d[8..12] == b"WEBP")
        || d.starts_with(b"BM")
}

fn decode_stream(data: &[u8], name: &str) -> Result<IconSet, IconError> {
    if data.starts_with(b"MZ") {
        return crate::pe::extract(data);
    }
    if crate::ico::is_ico(data) {
        return crate::ico::read(data);
    }
    if data.starts_with(b"\x89PNG")
        || data.starts_with(b"\xFF\xD8\xFF")
        || data.starts_with(b"RIFF")
        || data.starts_with(b"BM")
    {
        let mut set = IconSet::new(name.to_string());
        set.push(crate::model::decode_image(data, name)?);
        return Ok(set.finish());
    }
    Err(IconError::Corrupt(format!("{name}：不认识的图标数据")))
}
