//! 剪贴板：写链接、读图片。
//!
//! 读图优先级：`PNG` 注册格式 → `CF_DIBV5` → `CF_DIB` → `CF_BITMAP`。
//! 为什么先看 PNG：浏览器/Electron 应用（以及本项目的截图模块）会同时放一份 PNG，
//! 已经是压缩好的文件，直接拿到就能传；DIB 还要自己转像素、自己压。
//!
//! 全部在本地完成：不经过临时文件，也不依赖 GDI+ 的编码器（`CF_DIB` 不是文件格式，
//! GitHub 不认，必须先转成 PNG）。

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, GetDC, GetDIBits, ReleaseDC,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};

const CF_DIB: u32 = 8;
/// CF_DIBV5 是预定义格式（带 alpha 掩码的 32 位 DIB）
const CF_DIBV5: u32 = 17;
const CF_BITMAP: u32 = 2;
const CF_UNICODETEXT: u32 = 13;

/// 把文本放进剪贴板（只写 CF_UNICODETEXT）。
pub fn set_text(text: &str) -> bool {
    unsafe {
        if OpenClipboard(None).is_err() {
            zx_core::log::warn("图床：剪贴板被别的程序占用，链接没写进去");
            return false;
        }
        let _ = EmptyClipboard();
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * 2;
        let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            let _ = CloseClipboard();
            return false;
        };
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            let _ = CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr, bytes);
        let _ = GlobalUnlock(hmem);
        // 成功后所有权归系统，不能再 GlobalFree
        let ok = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// 读剪贴板里的图片，返回 PNG 字节。
pub fn read_image_png() -> Option<Vec<u8>> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let out = read_locked();
        let _ = CloseClipboard();
        out
    }
}

unsafe fn read_locked() -> Option<Vec<u8>> {
    // 1) PNG 注册格式：已经是文件字节
    let png_id = unsafe { RegisterClipboardFormatW(windows::core::w!("PNG")) };
    if png_id != 0 {
        if let Ok(h) = unsafe { GetClipboardData(png_id) } {
            if let Some(bytes) = unsafe { handle_bytes(h) } {
                if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                    return Some(bytes);
                }
            }
        }
    }

    // 2) DIB（含 V5）。V5 只是头更长，像素布局同 DIB_RGB_COLORS。
    for fmt in [CF_DIBV5, CF_DIB] {
        if let Ok(h) = unsafe { GetClipboardData(fmt) } {
            if let Some(bytes) = unsafe { handle_bytes(h) } {
                if let Some(png) = unsafe { dib_to_png(&bytes) } {
                    return Some(png);
                }
            }
        }
    }

    // 3) CF_BITMAP：老程序（Word/画图）只给位图句柄
    if unsafe { IsClipboardFormatAvailable(CF_BITMAP) }.is_ok() {
        if let Ok(h) = unsafe { GetClipboardData(CF_BITMAP) } {
            if let Some(png) = unsafe { bitmap_to_png(h) } {
                return Some(png);
            }
        }
    }

    // 4) 最后再扫一遍所有格式：有些程序用自定义名注册 PNG
    let mut fmt = 0u32;
    loop {
        fmt = unsafe { EnumClipboardFormats(fmt) };
        if fmt == 0 {
            break;
        }
        if let Ok(h) = unsafe { GetClipboardData(fmt) } {
            if let Some(bytes) = unsafe { handle_bytes(h) } {
                if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

unsafe fn handle_bytes(h: HANDLE) -> Option<Vec<u8>> {
    if h.is_invalid() {
        return None;
    }
    let hg = HGLOBAL(h.0);
    let size = unsafe { GlobalSize(hg) };
    if size == 0 || size > 128 * 1024 * 1024 {
        return None;
    }
    let ptr = unsafe { GlobalLock(hg) } as *const u8;
    if ptr.is_null() {
        return None;
    }
    let out = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    let _ = unsafe { GlobalUnlock(hg) };
    Some(out)
}

/// CF_DIB → RGBA → PNG。只支持 BI_RGB / BI_BITFIELDS 的 24/32 位（覆盖 99% 的粘贴场景）。
unsafe fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let rd_u32 = |o: usize| u32::from_le_bytes(dib[o..o + 4].try_into().unwrap());
    let rd_i32 = |o: usize| i32::from_le_bytes(dib[o..o + 4].try_into().unwrap());
    let rd_u16 = |o: usize| u16::from_le_bytes(dib[o..o + 2].try_into().unwrap());
    let header = rd_u32(0) as usize;
    let w = rd_i32(4);
    let h_raw = rd_i32(8);
    let bpp = rd_u16(14) as u32;
    let compression = rd_u32(16);
    if w <= 0 || h_raw == 0 || (bpp != 24 && bpp != 32) || header >= dib.len() {
        return None;
    }
    // 0 = BI_RGB；3 = BI_BITFIELDS（DIBV5 也是这个，掩码在头里，这里按 BGRA 处理）
    if compression != 0 && compression != 3 {
        return None;
    }
    let top_down = h_raw < 0;
    let h = h_raw.unsigned_abs() as usize;
    let w = w as usize;
    if w > 20_000 || h > 20_000 {
        return None;
    }
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let need = header + stride * h;
    if dib.len() < need {
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        let src_y = if top_down { y } else { h - 1 - y };
        let row = header + src_y * stride;
        for x in 0..w {
            let s = row + x * (bpp as usize / 8);
            let b = dib[s];
            let g = dib[s + 1];
            let r = dib[s + 2];
            let a = if bpp == 32 { dib[s + 3] } else { 255 };
            if bpp == 32 && a != 0 {
                any_alpha = true;
            }
            let d = (y * w + x) * 4;
            rgba[d] = r;
            rgba[d + 1] = g;
            rgba[d + 2] = b;
            rgba[d + 3] = a;
        }
    }
    // 很多 32 位 DIB 的 alpha 位全是 0（未定义），这时当不透明处理，否则整张图透明。
    if bpp == 32 && !any_alpha {
        for px in rgba.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }
    encode_png(&rgba, w as u32, h as u32)
}

unsafe fn bitmap_to_png(h: HANDLE) -> Option<Vec<u8>> {
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        // 先问尺寸
        let got = GetDIBits(hdc, windows::Win32::Graphics::Gdi::HBITMAP(h.0), 0, 0, None, &mut bi, DIB_RGB_COLORS);
        if got == 0 || bi.bmiHeader.biWidth <= 0 || bi.bmiHeader.biHeight == 0 {
            ReleaseDC(None, hdc);
            return None;
        }
        let w = bi.bmiHeader.biWidth as u32;
        let hgt = bi.bmiHeader.biHeight.unsigned_abs();
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let mut buf = vec![0u8; (w as usize) * (hgt as usize) * 4];
        let got = GetDIBits(
            hdc,
            windows::Win32::Graphics::Gdi::HBITMAP(h.0),
            0,
            hgt,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        if got == 0 {
            return None;
        }
        for px in buf.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            px[3] = 255;
        }
        // GetDIBits 给的是自底向上，翻过来
        let stride = (w as usize) * 4;
        let mut rgba = vec![0u8; buf.len()];
        for y in 0..hgt as usize {
            let src = (hgt as usize - 1 - y) * stride;
            rgba[y * stride..(y + 1) * stride].copy_from_slice(&buf[src..src + stride]);
        }
        encode_png(&rgba, w, hgt)
    }
}

pub fn encode_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

/// 生成一张 64×64 的渐变测试图（"测试上传"按钮用：不读屏幕、不碰剪贴板）。
pub fn test_png() -> Vec<u8> {
    let w = 64usize;
    let h = 64usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let d = (y * w + x) * 4;
            rgba[d] = (x * 4) as u8;
            rgba[d + 1] = (y * 4) as u8;
            rgba[d + 2] = 200;
            rgba[d + 3] = 255;
        }
    }
    encode_png(&rgba, w as u32, h as u32).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dib(w: i32, h: i32, bpp: u16, top_down: bool) -> Vec<u8> {
        let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
        let hh = h.unsigned_abs() as usize;
        let mut v = vec![0u8; 40 + stride * hh];
        v[0..4].copy_from_slice(&40u32.to_le_bytes());
        v[4..8].copy_from_slice(&w.to_le_bytes());
        v[8..12].copy_from_slice(&(if top_down { -h } else { h }).to_le_bytes());
        v[12..14].copy_from_slice(&1u16.to_le_bytes());
        v[14..16].copy_from_slice(&bpp.to_le_bytes());
        // 每个像素填成 (B=10, G=20, R=30, A=255)
        for y in 0..hh {
            // 自底向上时，第一行是图像最后一行；这里只关心颜色不是位置
            let _ = y;
            for x in 0..w as usize {
                let s = 40 + y * stride + x * (bpp as usize / 8);
                v[s] = 10;
                v[s + 1] = 20;
                v[s + 2] = 30;
                if bpp == 32 {
                    v[s + 3] = 255;
                }
            }
        }
        v
    }

    #[test]
    fn dib24_becomes_rgba_png() {
        let png = unsafe { dib_to_png(&dib(3, 2, 24, false)) }.expect("24bpp");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let dec = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(&buf[..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn dib32_top_down_is_not_flipped() {
        // 只上一行涂红，另一行涂蓝；top-down 时第一行应保持红
        let mut v = dib(2, 2, 32, true);
        let stride = 8usize;
        for x in 0..2 {
            let s = 40 + x * 4;
            v[s] = 0;
            v[s + 1] = 0;
            v[s + 2] = 255;
            let t = 40 + stride + x * 4;
            v[t] = 255;
            v[t + 1] = 0;
            v[t + 2] = 0;
        }
        let png = unsafe { dib_to_png(&v) }.expect("32bpp");
        let dec = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..4], &[255, 0, 0, 255], "第一行应是红色");
        assert_eq!(&buf[8..12], &[0, 0, 255, 255], "第二行应是蓝色");
    }

    #[test]
    fn zero_alpha_is_treated_as_opaque() {
        let mut v = dib(1, 1, 32, false);
        v[43] = 0;
        let png = unsafe { dib_to_png(&v) }.expect("alpha=0");
        let dec = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf[3], 255);
    }

    #[test]
    fn test_png_is_valid() {
        let png = test_png();
        let dec = png::Decoder::new(std::io::Cursor::new(&png));
        let reader = dec.read_info().unwrap();
        assert_eq!(reader.info().width, 64);
        assert_eq!(reader.info().height, 64);
    }
}
