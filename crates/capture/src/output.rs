//! 导出：把「冻结帧 + 标注」合成一张图，然后复制到剪贴板 / 存成 PNG / 交给贴图窗口。
//!
//! 合成只有三步：`memcpy` 裁一块源图 → 按顺序重画标注 → 编码。美化模板在
//! `style.rs`（对这里产出的图再套一层），拼接在 `stitch.rs`，多图/长图都先合成
//! 成一张 `Canvas` 再走同一条导出路径。

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use zx_ui::Canvas;

use crate::annotate::{self, Anno};
use crate::screen::Frame;

/// 从冻结帧里裁出一块（选区 → 一张独立的图）。
pub fn crop(frame: &Frame, sel: windows::Win32::Foundation::RECT) -> Option<Canvas> {
    // 选区可能压在屏外（拖到边缘、或显示器热插拔后），先裁进帧内
    let sel = crate::screen::clamp_rect(sel, frame.bounds());
    let w = sel.right - sel.left;
    let h = sel.bottom - sel.top;
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut canvas = Canvas::new(w, h, 96);
    {
        let src_stride = frame.w as usize * 4;
        let (fx, fy) = (sel.left - frame.x, sel.top - frame.y);
        let dst_stride = canvas.stride();
        let src = frame.pixels_ptr();
        let dst = canvas.pixels_mut();
        if src.is_null() || dst.len() < (h as usize) * dst_stride {
            return None;
        }
        unsafe {
            for row in 0..h as usize {
                let s = (fy as usize + row) * src_stride + fx as usize * 4;
                let d = row * dst_stride;
                std::ptr::copy_nonoverlapping(src.add(s), dst.as_mut_ptr().add(d), w as usize * 4);
            }
        }
    }
    Some(canvas)
}

/// 从一张 Canvas 里裁一块（OCR 调试、拼接预览都会用到）。
pub fn crop_canvas(src: &Canvas, rect: (i32, i32, i32, i32)) -> Option<Canvas> {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 || x < 0 || y < 0 {
        return None;
    }
    if x + w > src.w || y + h > src.h {
        return None;
    }
    let mut out = Canvas::new(w, h, 96);
    let stride = src.stride();
    let dstride = out.stride();
    let px = src.pixels();
    let dst = out.pixels_mut();
    if px.len() < (y + h) as usize * stride || dst.len() < h as usize * dstride {
        return None;
    }
    for row in 0..h as usize {
        let s = (y as usize + row) * stride + x as usize * 4;
        let d = row * dstride;
        dst[d..d + dstride].copy_from_slice(&px[s..s + dstride]);
    }
    Some(out)
}

/// 把标注画到图上（导出用；不改原图）。
pub fn compose_doc(doc: &Canvas, annos: &[Anno]) -> Canvas {
    let mut out = Canvas::new(doc.w, doc.h, 96);
    out.draw_canvas(doc, 0.0, 0.0, doc.w as f32, doc.h as f32);
    annotate::draw_all(&mut out, annos, 0.0, 0.0, None, 1.0);
    out
}

/// 剪贴板格式号。windows 0.58 把 CF_* 放在了 Gdi 的 DrawText 常量里（不含剪贴板那几个），
/// 与 `clipboard` 模块一样直接写数值：CF_BITMAP=2、CF_DIB=8。
const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;

/// 复制到剪贴板：`CF_DIB`（兼容性最好）+ `PNG` 注册格式（保留无损原图）+ `CF_BITMAP`。
///
/// 剪贴板历史模块监听的正是这两个格式，所以截完图会自然出现在历史里——
/// 这是模块化带来的顺手好处，不需要两个模块互相调用。
pub fn copy_to_clipboard(canvas: &Canvas) -> bool {
    let Some(png) = canvas.png_bytes() else { return false };
    let dib = dib_bytes(canvas);
    let hbmp = make_bitmap(canvas);
    unsafe {
        if OpenClipboard(None).is_err() {
            zx_core::log::warn("截图复制失败：剪贴板被别的程序占用");
            return false;
        }
        let _ = EmptyClipboard();
        let mut ok = set_bytes(CF_DIB, &dib);
        let png_id = RegisterClipboardFormatW(windows::core::w!("PNG"));
        if png_id != 0 {
            ok |= set_bytes(png_id, &png);
        }
        if !hbmp.is_invalid() {
            if SetClipboardData(CF_BITMAP, HANDLE(hbmp.0)).is_ok() {
                ok = true;
                // 成功时所有权归系统，不能再删
            } else {
                let _ = DeleteObject(hbmp);
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

/// 存 PNG。文件名带时间戳，重名自动加后缀。
pub fn save_png(canvas: &Canvas, dir: &Path) -> Option<PathBuf> {
    let png = canvas.png_bytes()?;
    if std::fs::create_dir_all(dir).is_err() {
        zx_core::log::warn(format!("截图目录创建失败：{}", dir.display()));
        return None;
    }
    // 时间戳只取一次：之前每轮都重新取，跨秒时会生成一组不一致的文件名
    let base = stamp();
    let mut path = dir.join(format!("截图_{base}.png"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        if n > 9999 {
            zx_core::log::warn("截图保存失败：同一秒内重名次数过多");
            return None;
        }
        path = dir.join(format!("截图_{base}-{n}.png"));
    }
    match std::fs::write(&path, &png) {
        Ok(()) => Some(path),
        Err(e) => {
            zx_core::log::warn(format!("截图写盘失败 {}：{e}", path.display()));
            None
        }
    }
}

fn stamp() -> String {
    // 2026-11-21 11:00:07 → 2026-11-21_11-00-07
    let now = zx_core::win::local_time_string();
    now.replace(':', "-").replace(' ', "_")
}

/// 复制/贴图时按设置"顺手存一份"。
pub fn auto_save(canvas: &Canvas) -> Option<PathBuf> {
    let c = crate::cfg();
    if !c.auto_save {
        return None;
    }
    save_png(canvas, &c.save_dir)
}

// ---------------------------------------------------------------- 剪贴板底层

fn set_bytes(id: u32, bytes: &[u8]) -> bool {
    unsafe {
        let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else { return false };
        let p = GlobalLock(hmem) as *mut u8;
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(hmem);
        if SetClipboardData(id, HANDLE(hmem.0)).is_err() {
            let _ = windows::Win32::Foundation::GlobalFree(HGLOBAL(hmem.0));
            return false;
        }
        true
    }
}

/// 生成 `CF_DIB` 需要的字节：40 字节头 + 自下而上的 BGRA 行。
fn dib_bytes(canvas: &Canvas) -> Vec<u8> {
    let (w, h) = (canvas.w, canvas.h);
    let stride = canvas.stride();
    let mut out = vec![0u8; 40 + stride * h as usize];
    let hdr = &mut out[..40];
    hdr[0..4].copy_from_slice(&40u32.to_le_bytes());
    hdr[4..8].copy_from_slice(&w.to_le_bytes());
    hdr[8..12].copy_from_slice(&h.to_le_bytes()); // 正数 = 自下而上
    hdr[12..14].copy_from_slice(&1u16.to_le_bytes());
    hdr[14..16].copy_from_slice(&32u16.to_le_bytes());
    // biCompression = BI_RGB(0)、biSizeImage
    hdr[20..24].copy_from_slice(&((stride * h as usize) as u32).to_le_bytes());
    let px = canvas.pixels();
    for row in 0..h as usize {
        let src = (h as usize - 1 - row) * stride;
        let dst = 40 + row * stride;
        out[dst..dst + stride].copy_from_slice(&px[src..src + stride]);
    }
    out
}

/// 建一张和画布同内容的 DIB section，用来填 `CF_BITMAP`。
fn make_bitmap(canvas: &Canvas) -> HBITMAP {
    let (w, h) = (canvas.w, canvas.h);
    let stride = canvas.stride();
    unsafe {
        let screen = GetDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        ReleaseDC(None, screen);
        if bmp.is_invalid() || bits.is_null() {
            return HBITMAP::default();
        }
        std::ptr::copy_nonoverlapping(canvas.pixels().as_ptr(), bits as *mut u8, stride * h as usize);
        bmp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_header_matches_canvas() {
        let mut c = Canvas::new(4, 3, 96);
        c.clear(0xFF112233);
        let dib = dib_bytes(&c);
        assert_eq!(dib.len(), 40 + 4 * 4 * 3);
        assert_eq!(u32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]), 4);
        assert_eq!(i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]), 3);
        // BI_RGB + 32bpp
        assert_eq!(u16::from_le_bytes([dib[14], dib[15]]), 32);
        // 第一行（文件里是最下面那行）应是 BGR = 33 22 11
        assert_eq!(&dib[40..43], &[0x33, 0x22, 0x11]);
    }
}
