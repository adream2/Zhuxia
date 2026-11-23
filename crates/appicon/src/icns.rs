//! ICNS（macOS 图标）读取。
//!
//! 现代 icns 就是一堆 PNG 块（ic07–ic14、icp4–icp6），`iconutil` 生成的包全是这种；
//! 老包可能只有 RLE 的 is32/il32/it32。这里支持 PNG 与 ic04/ic05 的原始 ARGB，
//! 老 RLE 块跳过——对应的应用基本都是 2007 年前的老软件，取不到图标会明确报"没有图标"。

use crate::error::IconError;
use crate::model::{decode_png, Frame, IconSet};

pub fn is_icns(data: &[u8]) -> bool {
    data.len() >= 8 && &data[..4] == b"icns"
}

/// 块类型 → 边长（PNG / ARGB 两类）。
fn chunk_side(ty: &[u8; 4]) -> Option<u32> {
    Some(match ty {
        b"icp4" => 16,
        b"icp5" => 32,
        b"icp6" => 64,
        b"ic07" => 128,
        b"ic08" => 256,
        b"ic09" => 512,
        b"ic10" => 1024,
        b"ic11" => 32,
        b"ic12" => 64,
        b"ic13" => 256,
        b"ic14" => 512,
        b"ic04" => 16,
        b"ic05" => 32,
        _ => return None,
    })
}

pub fn read(data: &[u8]) -> Result<IconSet, IconError> {
    if !is_icns(data) {
        return Err(IconError::Corrupt("不是 ICNS 容器".into()));
    }
    let total = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let end = total.min(data.len());
    let mut set = IconSet::new("ICNS".to_string());
    let mut skipped = 0usize;
    let mut pos = 8usize;
    while pos + 8 <= end {
        let ty: [u8; 4] = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
        let len = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        if len < 8 || pos + len > end {
            break;
        }
        let body = &data[pos + 8..pos + len];
        match &ty {
            b"ic04" | b"ic05" => {
                let side = if &ty == b"ic04" { 16 } else { 32 };
                let hint = format!("ICNS {} {}×{} ARGB", String::from_utf8_lossy(&ty), side, side);
                match decode_argb(body, side, side, &hint) {
                    Ok(f) => set.push(f),
                    Err(e) => zx_core::log::warn(format!("{hint} 解码失败：{}", e.reason())),
                }
            }
            t if chunk_side(t).is_some() => {
                let side = chunk_side(t).unwrap_or(0);
                let hint = format!("ICNS {} {}×{}", String::from_utf8_lossy(t), side, side);
                if body.starts_with(b"\x89PNG\r\n\x1a\n") {
                    match decode_png(body, &hint) {
                        Ok(f) => set.push(f),
                        Err(e) => zx_core::log::warn(format!("{hint} 解码失败：{}", e.reason())),
                    }
                } else {
                    // ic08/ic09 也允许 JPEG 2000，Windows 没有系统解码器。
                    skipped += 1;
                }
            }
            t if matches!(t, b"is32" | b"il32" | b"it32" | b"ih32" | b"ics#" | b"ICN#") => {
                skipped += 1;
            }
            _ => {}
        }
        pos += len;
    }
    if set.is_empty() {
        return Err(IconError::NoIcon(if skipped > 0 {
            format!("{skipped} 个老式 RLE/JPEG2000 块，当前版本不解")
        } else {
            "ICNS 里没有图标块".into()
        }));
    }
    set.detail = format!(
        "ICNS · {} 帧（{}）{}",
        set.frames.len(),
        set.size_list(),
        if skipped > 0 { format!("，跳过 {skipped} 个老格式块") } else { String::new() }
    );
    Ok(set.finish())
}

/// ic04/ic05：每像素 4 字节，顺序 A,R,G,B。
fn decode_argb(data: &[u8], w: u32, h: u32, hint: &str) -> Result<Frame, IconError> {
    let need = w as usize * h as usize * 4;
    if data.len() < need {
        return Err(IconError::Corrupt(format!("{hint}：ARGB 数据长度不足")));
    }
    let mut rgba = Vec::with_capacity(need);
    let mut any = false;
    for p in data[..need].as_chunks::<4>().0 {
        any |= p[0] != 0;
        rgba.extend_from_slice(&[p[1], p[2], p[3], p[0]]);
    }
    if !any {
        for p in rgba.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
    }
    let mut f = Frame::new(w, h, rgba, hint)?;
    f.rank = 20;
    Ok(f)
}
