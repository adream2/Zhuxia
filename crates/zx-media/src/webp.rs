//! WebP 编码与缩略图。

use crate::image::{resize, Image, MediaError};

/// 有损 WebP。质量 1–100；`method=4` 是速度/体积的折中档。
pub fn encode_webp(img: &Image, quality: u8) -> Result<Vec<u8>, MediaError> {
    if img.width == 0 || img.height == 0 || img.rgba.len() != (img.width * img.height * 4) as usize {
        return Err(MediaError::Encode("像素缓冲与尺寸不一致".into()));
    }
    unsafe {
        let mut out: *mut u8 = std::ptr::null_mut();
        let size = libwebp_sys::WebPEncodeRGBA(
            img.rgba.as_ptr(),
            img.width as i32,
            img.height as i32,
            (img.width * 4) as i32,
            quality.clamp(1, 100) as f32,
            &mut out,
        );
        if size == 0 || out.is_null() {
            return Err(MediaError::Encode("libwebp 返回 0 字节".into()));
        }
        let bytes = std::slice::from_raw_parts(out, size).to_vec();
        libwebp_sys::WebPFree(out as *mut _);
        Ok(bytes)
    }
}

/// 网格用缩略图：长边缩到 `side`，WebP 质量 78。
pub fn thumbnail_webp(img: &Image, side: u32) -> Result<Vec<u8>, MediaError> {
    let small = resize(img, side.max(32));
    encode_webp(&small, 78)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::decode;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
        Image { width: w, height: h, rgba: rgba.repeat((w * h) as usize) }
    }

    #[test]
    fn encodes_and_decodes_back() {
        let img = solid(32, 16, [20, 40, 200, 255]);
        let webp = encode_webp(&img, 80).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        let back = decode(&webp).unwrap();
        assert_eq!((back.width, back.height), (32, 16));
        // 有损编码，允许 ±6
        let px = &back.rgba[..4];
        assert!((px[0] as i32 - 20).abs() <= 6, "{px:?}");
        assert!((px[2] as i32 - 200).abs() <= 8, "{px:?}");
    }

    #[test]
    fn thumbnail_limits_long_side() {
        let img = solid(400, 200, [1, 2, 3, 255]);
        let webp = thumbnail_webp(&img, 100).unwrap();
        let back = decode(&webp).unwrap();
        assert_eq!((back.width, back.height), (100, 50));
    }

    #[test]
    fn rejects_bad_buffer() {
        let img = Image { width: 10, height: 10, rgba: vec![0; 10] };
        assert!(encode_webp(&img, 80).is_err());
    }
}
