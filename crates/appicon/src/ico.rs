//! ICO 容器读写。
//!
//! 读：ICO 是"一个文件里装多张图"，必须**全部帧都读出来**——只取最大帧会把
//! 16/32/48 的清晰小图丢掉，而 Windows 在列表视图里用的正是它们。
//! 写：按 ICONDIR 规范自己拼，每帧 PNG 压缩（Vista 起全认）。

use crate::error::IconError;
use crate::model::{decode_dib, decode_png, Frame, IconSet};

pub fn is_ico(data: &[u8]) -> bool {
    data.len() >= 6 && data[0] == 0 && data[1] == 0 && data[2] == 1 && data[3] == 0
}

/// 读 ICO：逐帧解码，PNG 帧保留原始字节。
pub fn read(data: &[u8]) -> Result<IconSet, IconError> {
    if !is_ico(data) {
        return Err(IconError::Corrupt("不是 ICO 容器".into()));
    }
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    if count == 0 || count > 256 {
        return Err(IconError::Corrupt(format!("ICO 帧数异常：{count}")));
    }
    let mut set = IconSet::new("ICO".to_string());
    for i in 0..count {
        let e = 6 + i * 16;
        if e + 16 > data.len() {
            break;
        }
        let w = if data[e] == 0 { 256u32 } else { data[e] as u32 };
        let h = if data[e + 1] == 0 { 256u32 } else { data[e + 1] as u32 };
        let len = u32::from_le_bytes([data[e + 8], data[e + 9], data[e + 10], data[e + 11]]) as usize;
        let off = u32::from_le_bytes([data[e + 12], data[e + 13], data[e + 14], data[e + 15]]) as usize;
        if len == 0 || off.saturating_add(len) > data.len() {
            continue;
        }
        let raw = &data[off..off + len];
        let hint = format!("ICO {w}×{h}");
        let frame = if raw.starts_with(b"\x89PNG\r\n\x1a\n") {
            decode_png(raw, &hint)
        } else {
            decode_dib(raw, true, &hint)
        };
        match frame {
            Ok(f) => set.push(f),
            Err(e) => {
                // 单帧坏了不影响其它帧：老 ICO 里混着 8 位调色板帧很常见。
                zx_core::log::warn(format!("ICO 第 {i} 帧跳过：{}", e.reason()));
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("ICO 里没有可解码的帧".into()));
    }
    set.detail = format!("ICO · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

/// 拼一个 ICO。`frames` 是 (边长, PNG 字节)，调用方保证已按大小降序。
pub fn build(frames: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, IconError> {
    if frames.is_empty() {
        return Err(IconError::NoIcon("没有可写入 ICO 的帧".into()));
    }
    let mut out = Vec::with_capacity(64 * 1024);
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len();
    for (side, png) in frames {
        let dim = if *side >= 256 { 0u8 } else { *side as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0); // 调色板颜色数
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in frames {
        out.extend_from_slice(png);
    }
    Ok(out)
}

/// 把一组帧按"边长"整理成 ICO 写入清单：能复用原生帧就复用，
/// 缺的档位从最大帧缩下来，绝不把 16px 放大成 256px。
pub fn plan(frames: &[Frame], sizes: &[u32]) -> Vec<u32> {
    let native: Vec<u32> = {
        let mut v: Vec<u32> = frames
            .iter()
            .filter(|f| f.w == f.h && f.w <= 256)
            .map(|f| f.w)
            .collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.dedup();
        v
    };
    let max = frames.iter().map(|f| f.long_side()).max().unwrap_or(0).min(256);
    let mut out: Vec<u32> = Vec::new();
    for s in sizes {
        let s = *s;
        if (8..=256).contains(&s) && s <= max.max(native.iter().copied().max().unwrap_or(0)) {
            out.push(s);
        }
    }
    out.extend(native.iter().copied().filter(|s| (8..=256).contains(s)));
    out.sort_unstable_by(|a, b| b.cmp(a));
    out.dedup();
    out.truncate(8);
    if out.is_empty() {
        out.push(max.clamp(8, 256));
    }
    out
}
