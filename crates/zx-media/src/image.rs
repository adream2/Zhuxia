//! 解码 + 缩放。

use std::io::Cursor;

#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// 非预乘 RGBA8，逐行紧凑排列
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    Unsupported(&'static str),
    Decode(String),
    Encode(String),
    TooLarge,
}

impl MediaError {
    pub fn text(&self) -> String {
        match self {
            MediaError::Unsupported(what) => format!("{what} 暂不支持转换（保留原格式上传）"),
            MediaError::Decode(e) => format!("解码失败：{e}"),
            MediaError::Encode(e) => format!("编码失败：{e}"),
            MediaError::TooLarge => "图片像素太多（上限 6000 万），不转换，按原图上传".into(),
        }
    }
}

const MAX_PIXELS: u64 = 60_000_000;

/// 只读文件头，拿宽高。拿不到就返回 None（不为了显示尺寸去解码整张图）。
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        if bytes.len() >= 24 {
            let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
            let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }
        return None;
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_size(bytes);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        unsafe {
            let (mut w, mut h) = (0i32, 0i32);
            if libwebp_sys::WebPGetInfo(bytes.as_ptr(), bytes.len(), &mut w, &mut h) != 0
                && w > 0
                && h > 0
            {
                return Some((w as u32, h as u32));
            }
        }
        return None;
    }
    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) && bytes.len() >= 10 {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }
    None
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // SOF0..SOF15，排除 DHT(C4) / JPG(C8) / DAC(CC)
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            if w > 0 && h > 0 {
                return Some((w, h));
            }
            return None;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

pub fn decode(bytes: &[u8]) -> Result<Image, MediaError> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return decode_png(bytes);
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return decode_jpeg(bytes);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return decode_webp(bytes);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Err(MediaError::Unsupported("GIF"));
    }
    if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        return Err(MediaError::Unsupported("SVG"));
    }
    Err(MediaError::Unsupported("这种格式"))
}

fn decode_png(bytes: &[u8]) -> Result<Image, MediaError> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    // 调色板展开 + 16 位降到 8 位，剩下的颜色类型自己补成 RGBA
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| MediaError::Decode(e.to_string()))?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| MediaError::Decode(e.to_string()))?;
    let (w, h) = (info.width, info.height);
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(MediaError::TooLarge);
    }
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => rgb_to_rgba(data),
        png::ColorType::GrayscaleAlpha => gray_alpha_to_rgba(data),
        png::ColorType::Grayscale => gray_to_rgba(data),
        png::ColorType::Indexed => {
            return Err(MediaError::Decode("调色板没有被展开".into()));
        }
    };
    Ok(Image { width: w, height: h, rgba })
}

fn rgb_to_rgba(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / 3 * 4);
    for px in src.as_chunks::<3>().0 {
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    out
}

fn gray_to_rgba(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() * 4);
    for g in src {
        out.extend_from_slice(&[*g, *g, *g, 255]);
    }
    out
}

fn gray_alpha_to_rgba(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / 2 * 4);
    for px in src.as_chunks::<2>().0 {
        out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
    }
    out
}

fn decode_jpeg(bytes: &[u8]) -> Result<Image, MediaError> {
    use zune_jpeg::zune_core::{colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(bytes), opts);
    let pixels = dec.decode().map_err(|e| MediaError::Decode(format!("{e:?}")))?;
    let info = dec.info().ok_or_else(|| MediaError::Decode("缺少尺寸信息".into()))?;
    let (w, h) = (info.width as u32, info.height as u32);
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(MediaError::TooLarge);
    }
    if pixels.len() != (w * h * 4) as usize {
        return Err(MediaError::Decode(format!(
            "JPEG 输出字节数 {} 与 {}x{} 不符",
            pixels.len(),
            w,
            h
        )));
    }
    Ok(Image { width: w, height: h, rgba: pixels })
}

fn decode_webp(bytes: &[u8]) -> Result<Image, MediaError> {
    unsafe {
        let (mut w, mut h) = (0i32, 0i32);
        let ptr = libwebp_sys::WebPDecodeRGBA(bytes.as_ptr(), bytes.len(), &mut w, &mut h);
        if ptr.is_null() || w <= 0 || h <= 0 {
            return Err(MediaError::Decode("libwebp 解码失败".into()));
        }
        if w as u64 * h as u64 > MAX_PIXELS {
            libwebp_sys::WebPFree(ptr as *mut _);
            return Err(MediaError::TooLarge);
        }
        let len = (w as usize) * (h as usize) * 4;
        let rgba = std::slice::from_raw_parts(ptr, len).to_vec();
        libwebp_sys::WebPFree(ptr as *mut _);
        Ok(Image { width: w as u32, height: h as u32, rgba })
    }
}

/// 只缩不放。`max_side == 0`（不限制）或本来就小 → 原样返回。
///
/// `max_side` 是**长边**上限，缩放按长边等比，短边跟着算，
/// 不会把 4:3 的图压成方图。
pub fn resize(img: &Image, max_side: u32) -> Image {
    if max_side == 0 {
        return img.clone();
    }
    let longest = img.width.max(img.height);
    if longest <= max_side {
        return img.clone();
    }
    let scale = max_side as f32 / longest as f32;
    let dw = ((img.width as f32 * scale).round() as u32).max(1);
    let dh = ((img.height as f32 * scale).round() as u32).max(1);

    // 横趟：src(w,h) → tmp(dw,h)，u8 中间缓冲，内存只多一份目标宽度
    let mut tmp = vec![0u8; (dw as usize) * (img.height as usize) * 4];
    let (contrib, radius) = contributions(img.width, dw);
    for y in 0..img.height as usize {
        let row = &img.rgba[y * img.width as usize * 4..(y + 1) * img.width as usize * 4];
        for (x, taps) in contrib.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(sx, w) in taps {
                let o = sx * 4;
                acc[0] += row[o] as f32 * w;
                acc[1] += row[o + 1] as f32 * w;
                acc[2] += row[o + 2] as f32 * w;
                acc[3] += row[o + 3] as f32 * w;
            }
            let d = (y * dw as usize + x) * 4;
            tmp[d] = acc[0].clamp(0.0, 255.0).round() as u8;
            tmp[d + 1] = acc[1].clamp(0.0, 255.0).round() as u8;
            tmp[d + 2] = acc[2].clamp(0.0, 255.0).round() as u8;
            tmp[d + 3] = acc[3].clamp(0.0, 255.0).round() as u8;
        }
    }
    let _ = radius;

    // 纵趟：tmp(dw,h) → out(dw,dh)
    let (contrib, _) = contributions(img.height, dh);
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 4];
    for (y, taps) in contrib.iter().enumerate() {
        for x in 0..dw as usize {
            let mut acc = [0f32; 4];
            for &(sy, w) in taps {
                let o = (sy * dw as usize + x) * 4;
                acc[0] += tmp[o] as f32 * w;
                acc[1] += tmp[o + 1] as f32 * w;
                acc[2] += tmp[o + 2] as f32 * w;
                acc[3] += tmp[o + 3] as f32 * w;
            }
            let d = (y * dw as usize + x) * 4;
            out[d] = acc[0].clamp(0.0, 255.0).round() as u8;
            out[d + 1] = acc[1].clamp(0.0, 255.0).round() as u8;
            out[d + 2] = acc[2].clamp(0.0, 255.0).round() as u8;
            out[d + 3] = acc[3].clamp(0.0, 255.0).round() as u8;
        }
    }
    Image { width: dw, height: dh, rgba: out }
}

/// 每个目标像素对应的源像素与权重。返回 (taps, 半径)。
fn contributions(src: u32, dst: u32) -> (Vec<Vec<(usize, f32)>>, f32) {
    let ratio = src as f32 / dst as f32;
    // 缩小时半径跟着放大，避免只采样 3 个点导致锯齿
    let support = 3.0 * ratio.max(1.0);
    let mut out = Vec::with_capacity(dst as usize);
    for i in 0..dst {
        let center = (i as f32 + 0.5) * ratio - 0.5;
        let lo = (center - support).floor() as i32;
        let hi = (center + support).ceil() as i32;
        let mut taps: Vec<(usize, f32)> = Vec::new();
        let mut sum = 0.0f32;
        for j in lo..=hi {
            let x = (center - j as f32) / ratio.max(1.0);
            let w = lanczos3(x);
            if w.abs() < 1e-5 {
                continue;
            }
            let sx = j.clamp(0, src as i32 - 1) as usize;
            taps.push((sx, w));
            sum += w;
        }
        if sum.abs() < 1e-6 {
            let sx = center.round().clamp(0.0, src as f32 - 1.0) as usize;
            taps = vec![(sx, 1.0)];
        } else {
            for t in &mut taps {
                t.1 /= sum;
            }
        }
        out.push(taps);
    }
    (out, support)
}

fn lanczos3(x: f32) -> f32 {
    let x = x.abs();
    if x < 1e-6 {
        return 1.0;
    }
    if x >= 3.0 {
        return 0.0;
    }
    let px = std::f32::consts::PI * x;
    (px.sin() / px) * ((px / 3.0).sin() / (px / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_rgba(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(px).unwrap();
        }
        out
    }

    #[test]
    fn decodes_png_and_reports_size() {
        let raw = [255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 255];
        let bytes = png_rgba(2, 2, &raw);
        assert_eq!(dimensions(&bytes), Some((2, 2)));
        let img = decode(&bytes).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(&img.rgba, &raw);
    }

    #[test]
    fn resize_keeps_aspect_and_does_not_upscale() {
        let img = Image { width: 400, height: 200, rgba: vec![128; 400 * 200 * 4] };
        let small = resize(&img, 200);
        assert_eq!((small.width, small.height), (200, 100));
        let same = resize(&small, 400);
        assert_eq!((same.width, same.height), (200, 100));
    }

    #[test]
    fn resize_zero_means_unlimited() {
        let img = Image { width: 3000, height: 1000, rgba: vec![7; 3000 * 1000 * 4] };
        let same = resize(&img, 0);
        assert_eq!((same.width, same.height), (3000, 1000));
        // 短边跟着长边等比例：3000×1000 → 1200×400
        let half = resize(&img, 1200);
        assert_eq!((half.width, half.height), (1200, 400));
        // 竖图同样按长边算
        let tall = Image { width: 1000, height: 3000, rgba: vec![7; 1000 * 3000 * 4] };
        let t = resize(&tall, 1200);
        assert_eq!((t.width, t.height), (400, 1200));
    }

    #[test]
    fn resize_averages_solid_color() {
        let img = Image {
            width: 8,
            height: 8,
            rgba: vec![10u8, 20, 30, 255].repeat(64),
        };
        let small = resize(&img, 4);
        assert_eq!((small.width, small.height), (4, 4));
        for px in small.rgba.chunks_exact(4) {
            assert!((px[0] as i32 - 10).abs() <= 1);
            assert!((px[1] as i32 - 20).abs() <= 1);
            assert!((px[2] as i32 - 30).abs() <= 1);
        }
    }

    #[test]
    fn rejects_gif() {
        assert!(matches!(decode(b"GIF89a......"), Err(MediaError::Unsupported("GIF"))));
        assert!(matches!(decode(b"<svg></svg>"), Err(MediaError::Unsupported("SVG"))));
    }
}
