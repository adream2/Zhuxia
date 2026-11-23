//! 编码：PNG 走 `png` crate（能控压缩级别），JPG/GIF/BMP/TIFF 走 GDI+，WebP 走 libwebp，
//! ICO 自己拼容器（GDI+ 没有 ICO 编码器）。
//!
//! 与 Filee `ImageEncoder.ApplyOptions` 对应的三个动作都在这里：
//! - 无 alpha 的格式先压平背景（否则透明区变黑）；
//! - 有损格式把 quality 夹在 1..=100 再交给编码器；
//! - 8bit/通道，不写 16bit（GDI+ 的 32bpp 位图本来就是 8bit）。

use std::ffi::c_void;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Graphics::GdiPlus::*;

use crate::error::ConvertError;
use crate::fmt::Target;
use crate::decode::Bitmap;

const PIXFMT_ARGB: i32 = 0x0026_200A;

/// 一次编码的参数（从配置来）。
#[derive(Clone, Copy, Debug)]
pub struct Quality {
    pub quality: u8,
    pub webp_lossless: bool,
    pub background: [u8; 3],
}

impl Default for Quality {
    fn default() -> Self {
        Quality { quality: 90, webp_lossless: false, background: [255, 255, 255] }
    }
}

/// 把像素写成 `path` 指向的文件。调用方负责临时文件名与改名。
///
/// `ico_sizes` 只在 `target == Ico` 时生效：一个 .ico 里塞哪几个规格。
pub fn write(
    img: &Bitmap,
    target: Target,
    q: Quality,
    ico_sizes: &[u32],
    path: &Path,
) -> Result<(), ConvertError> {
    if img.w == 0 || img.h == 0 {
        return Err(ConvertError::Encode("空图像".into()));
    }
    // 格式不带 alpha 时先压平：JPG 上透明区变黑是最常见的"转换后图变丑"投诉。
    let pixels = if target.keeps_alpha() {
        std::borrow::Cow::Borrowed(&img.rgba)
    } else {
        std::borrow::Cow::Owned(img.flatten(q.background))
    };
    match target {
        Target::Png => write_png(img.w, img.h, &pixels, path),
        Target::WebP => write_webp(img.w, img.h, &pixels, q, path),
        Target::Ico => write_ico(img, ico_sizes, path),
        Target::Jpg | Target::Gif | Target::Bmp | Target::Tiff => {
            write_gdiplus(img.w, img.h, &pixels, target, q.quality, path)
        }
    }
}

/// PNG：全不透明就写 RGB，省一条 alpha 通道；压缩级别固定 6（Balanced），
/// 9 在小图上只小几个百分点、耗时翻倍，右键菜单要的是"点完就好"。
fn write_png(w: u32, h: u32, rgba: &[u8], path: &Path) -> Result<(), ConvertError> {
    let bytes = png_bytes(w, h, rgba, false)?;
    std::fs::write(path, bytes).map_err(|e| crate::error::io_error(&e, path))
}

/// 编码成 PNG 字节。`force_alpha = true` 时即使全不透明也保留 RGBA（ICO 的帧需要）。
fn png_bytes(w: u32, h: u32, rgba: &[u8], force_alpha: bool) -> Result<Vec<u8>, ConvertError> {
    let opaque = !force_alpha && rgba.chunks_exact(4).all(|p| p[3] == 255);
    let data: std::borrow::Cow<[u8]> = if opaque {
        let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
        for p in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&p[..3]);
        }
        std::borrow::Cow::Owned(rgb)
    } else {
        std::borrow::Cow::Borrowed(rgba)
    };

    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(std::io::Cursor::new(&mut out), w, h);
        enc.set_color(if opaque { png::ColorType::Rgb } else { png::ColorType::Rgba });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        let mut writer = enc
            .write_header()
            .map_err(|e| ConvertError::Encode(format!("PNG 头：{e}")))?;
        writer
            .write_image_data(&data)
            .map_err(|e| ConvertError::Encode(format!("PNG 数据：{e}")))?;
        writer
            .finish()
            .map_err(|e| ConvertError::Encode(format!("PNG 收尾：{e}")))?;
    }
    Result::Ok(out)
}

/// ICO：GDI+ 没有 ICO 编码器，自己拼容器。
///
/// 每帧都写成 PNG 压缩的单张图（Vista 起支持，Win10/11 全规格都认）。
/// 图标必须正方形：非正方形先透明补齐、居中；每个规格用 HighQualityBicubic 单独缩。
fn write_ico(img: &Bitmap, sizes: &[u32], path: &Path) -> Result<(), ConvertError> {
    let sizes = normalize_ico_sizes(sizes);
    let side = img.w.max(img.h);
    let square = Bitmap { w: side, h: side, rgba: pad_square(&img.rgba, img.w, img.h, side) };

    let mut frames: Vec<(u32, Vec<u8>)> = Vec::with_capacity(sizes.len());
    for s in sizes {
        let small = crate::decode::resize(&square, s, s)?;
        frames.push((s, png_bytes(s, s, &small.rgba, true)?));
    }

    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len();
    for (s, data) in &frames {
        // ICONDIR 的宽高是 u8，256 写成 0
        let dim = if *s >= 256 { 0u8 } else { *s as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0); // 调色板颜色数（真彩 = 0）
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bit count
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in &frames {
        out.extend_from_slice(data);
    }
    std::fs::write(path, out).map_err(|e| crate::error::io_error(&e, path))
}

/// 规格清洗：8–256、去重、从大到小、最多 8 个；空列表回落到最常用的四档。
pub fn normalize_ico_sizes(sizes: &[u32]) -> Vec<u32> {
    let mut v: Vec<u32> = sizes
        .iter()
        .copied()
        .filter(|s| (8..=256).contains(s))
        .collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    v.truncate(8);
    if v.is_empty() {
        v = vec![256, 48, 32, 16];
    }
    v
}

/// 把 w×h 的 RGBA 居中贴到 side×side 的透明画布上（图标必须正方形）。
fn pad_square(rgba: &[u8], w: u32, h: u32, side: u32) -> Vec<u8> {
    if w == side && h == side {
        return rgba.to_vec();
    }
    let mut out = vec![0u8; side as usize * side as usize * 4];
    let dx = (side - w) / 2;
    let dy = (side - h) / 2;
    for y in 0..h as usize {
        let src = &rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
        let dst_off = ((y as u32 + dy) as usize * side as usize + dx as usize) * 4;
        out[dst_off..dst_off + src.len()].copy_from_slice(src);
    }
    out
}

/// WebP：libwebp 直接吃 RGBA。有损用 quality，无损走 lossless 编码器。
fn write_webp(w: u32, h: u32, rgba: &[u8], q: Quality, path: &Path) -> Result<(), ConvertError> {
    let stride = (w as i32) * 4;
    let mut out: *mut u8 = std::ptr::null_mut();
    let size = unsafe {
        if q.webp_lossless {
            libwebp_sys::WebPEncodeLosslessRGBA(rgba.as_ptr(), w as i32, h as i32, stride, &mut out)
        } else {
            let f = (q.quality.clamp(1, 100)) as f32;
            libwebp_sys::WebPEncodeRGBA(rgba.as_ptr(), w as i32, h as i32, stride, f, &mut out)
        }
    };
    if size == 0 || out.is_null() {
        return Err(ConvertError::Encode("libwebp 编码失败".into()));
    }
    let bytes = unsafe {
        let v = std::slice::from_raw_parts(out, size).to_vec();
        libwebp_sys::WebPFree(out as *mut _);
        v
    };
    std::fs::write(path, bytes).map_err(|e| crate::error::io_error(&e, path))
}

/// GDI+ 编码（JPG/GIF/BMP/TIFF）。quality 只对 JPEG 生效，其余格式忽略。
fn write_gdiplus(
    w: u32,
    h: u32,
    rgba: &[u8],
    target: Target,
    quality: u8,
    path: &Path,
) -> Result<(), ConvertError> {
    zx_ui::painter::init_gdiplus();

    // GDI+ 的 32bppARGB 在内存里是 BGRA：换一次字节序，别让红蓝互换。
    let mut bgra = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }

    let mime = match target {
        Target::Jpg => "image/jpeg",
        Target::Gif => "image/gif",
        Target::Bmp => "image/bmp",
        Target::Tiff => "image/tiff",
        _ => return Err(ConvertError::Encode("该格式不走 GDI+".into())),
    };
    let clsid = encoder_clsid(mime)
        .ok_or_else(|| ConvertError::Encode(format!("系统缺少 {mime} 编码器")))?;

    unsafe {
        let mut bmp: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(
            w as i32,
            h as i32,
            (w * 4) as i32,
            PIXFMT_ARGB,
            Some(bgra.as_ptr()),
            &mut bmp,
        )
        .0 != 0
            || bmp.is_null()
        {
            return Err(ConvertError::Encode("创建编码位图失败".into()));
        }
        let guard = EncGuard(bmp);

        let mut qv: i32 = quality.clamp(1, 100) as i32;
        let mut params = EncoderParameters::default();
        params.Count = 1;
        params.Parameter[0].Guid = EncoderQuality;
        params.Parameter[0].NumberOfValues = 1;
        params.Parameter[0].Type = EncoderParameterValueTypeLong.0 as u32;
        params.Parameter[0].Value = &mut qv as *mut i32 as *mut c_void;

        let wide = zx_core::win::wide(&path.to_string_lossy());
        let st = GdipSaveImageToFile(
            guard.0 as *mut GpImage,
            PCWSTR(wide.as_ptr()),
            &clsid,
            &params,
        );
        if st.0 != 0 {
            return Err(ConvertError::Encode(format!("GDI+ 写入失败 status={}", st.0)));
        }
    }
    Result::Ok(())
}

struct EncGuard(*mut GpBitmap);
impl Drop for EncGuard {
    fn drop(&mut self) {
        unsafe { GdipDisposeImage(self.0 as *mut GpImage); }
    }
}

/// 按 MIME 找系统编码器 CLSID（编码器列表随 GDI+ 版本走，不能写死）。
fn encoder_clsid(mime: &str) -> Option<windows::core::GUID> {
    use windows::Win32::Graphics::GdiPlus::{GdipGetImageEncoders, GdipGetImageEncodersSize, ImageCodecInfo};
    unsafe {
        let mut num = 0u32;
        let mut size = 0u32;
        if GdipGetImageEncodersSize(&mut num, &mut size).0 != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let encoders = buf.as_mut_ptr() as *mut ImageCodecInfo;
        if GdipGetImageEncoders(num, size, encoders).0 != 0 {
            return None;
        }
        for i in 0..num as usize {
            let info = &*encoders.add(i);
            if info.MimeType.is_null() {
                continue;
            }
            let mut len = 0usize;
            while *info.MimeType.0.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(info.MimeType.0, len));
            if s.eq_ignore_ascii_case(mime) {
                return Some(info.Clsid);
            }
        }
        None
    }
}
