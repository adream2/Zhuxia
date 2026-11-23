//! 图标帧模型与解码：所有提取器最后都产出一组 RGBA 帧。
//!
//! 帧的原始格式有 PNG / 32 位 DIB / WebP / JPEG / SVG 五种（PE、ICO、ICNS、APK、
//! 各平台包里就这么几种）。统一在这里解成 RGBA8，再做尺寸策略与落盘。
//!
//! 上限是硬约束：一个恶意构造的 ICO 可以声明 65535×65535，不设限就是一次 OOM。

use std::path::Path;

use crate::error::IconError;

/// 单帧像素上限（32 MP ≈ 128 MB RGBA）。正常图标最大 1024×1024，给足余量。
const MAX_FRAME_PIXELS: u64 = 32_000_000;
/// 单帧压缩数据上限。
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// 一个图标集最多保留多少帧（ICO 通常 <15，ICNS <20）。
const MAX_FRAMES: usize = 64;
/// SVG 光栅化的下限与上限（与 imgconv 保持一致：24×24 的图标画出来没法用）。
const SVG_MIN_LONG: f32 = 256.0;
const SVG_MAX_LONG: f32 = 1024.0;

/// 一张图标：RGBA8、行优先、无 padding。
#[derive(Clone, Debug)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// 原始 PNG 字节（有就直接塞进 ICO，省一次重编码，也避免再压缩带来的色差）。
    pub png: Option<Vec<u8>>,
    /// 来自哪里（"PE RT_GROUP_ICON" / "ICNS ic09" / "mipmap-xxxhdpi"）。
    pub origin: String,
    /// 同一尺寸下挑哪一帧：PNG > 32 位 DIB > 24 位 > 调色板。数字越大越优先。
    pub rank: u8,
}

impl Frame {
    pub fn new(w: u32, h: u32, rgba: Vec<u8>, origin: impl Into<String>) -> Result<Self, IconError> {
        if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_FRAME_PIXELS {
            return Err(IconError::TooLarge(format!("{w}×{h}")));
        }
        let need = w as usize * h as usize * 4;
        if rgba.len() != need {
            return Err(IconError::Corrupt(format!(
                "像素长度不符：{w}×{h} 需要 {need}，实际 {}",
                rgba.len()
            )));
        }
        Ok(Frame { w, h, rgba, png: None, origin: origin.into(), rank: 0 })
    }

    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    pub fn long_side(&self) -> u32 {
        self.w.max(self.h)
    }

    /// 全不透明（PNG 可省 alpha 通道）。
    pub fn opaque(&self) -> bool {
        self.rgba.chunks_exact(4).all(|p| p[3] == 255)
    }
}

/// 一次提取的结果：若干分辨率 + 一句"从哪来的"。
#[derive(Clone, Default, Debug)]
pub struct IconSet {
    pub frames: Vec<Frame>,
    /// 来源说明，例如 "APK · mipmap-xxxhdpi/ic_launcher.png"。
    pub detail: String,
}

impl IconSet {
    pub fn new(detail: impl Into<String>) -> Self {
        IconSet { frames: Vec::new(), detail: detail.into() }
    }

    /// 加一帧。完全相同的帧（同尺寸同像素）只留一份，避免 APK 里同一个图标
    /// 在多个 density 目录重复出现时输出一堆一模一样的文件。
    pub fn push(&mut self, f: Frame) {
        if self.frames.len() >= MAX_FRAMES {
            return;
        }
        // 同一尺寸只留一帧：PE 的图标组常带 8/24/32 位三个版本，
        // 全留下会让"导出全部尺寸"出现一堆同名不同后缀的重复图。
        if let Some(i) = self.frames.iter().position(|x| x.w == f.w && x.h == f.h) {
            if f.rank > self.frames[i].rank {
                self.frames[i] = f;
            }
            return;
        }
        self.frames.push(f);
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// 面积从大到小：输出时"最大的一帧"永远排第一。
    pub fn finish(mut self) -> Self {
        self.frames.sort_by(|a, b| b.area().cmp(&a.area()).then(b.w.cmp(&a.w)));
        self
    }

    pub fn largest(&self) -> Option<&Frame> {
        self.frames.iter().max_by_key(|f| f.area())
    }

    pub fn max_side(&self) -> u32 {
        self.frames.iter().map(|f| f.w.max(f.h)).max().unwrap_or(0)
    }

    /// "16/32/48/256" 这种给人看的规格列表。
    pub fn size_list(&self) -> String {
        let mut v: Vec<u32> = self.frames.iter().map(|f| f.w.max(f.h)).collect();
        v.sort_unstable();
        v.dedup();
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("/")
    }
}

/* ------------------------------- 解码入口 ------------------------------- */

/// 按 magic 判断字节流是什么图片，解成 RGBA。`hint` 只影响报错文案。
pub fn decode_image(data: &[u8], hint: &str) -> Result<Frame, IconError> {
    if data.len() > MAX_FRAME_BYTES {
        return Err(IconError::TooLarge(format!("{hint} {} B", data.len())));
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return decode_png(data, hint);
    }
    if data.starts_with(b"\xFF\xD8\xFF") {
        return decode_via_gdiplus(data, "jpg", hint);
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return decode_via_gdiplus(data, "gif", hint);
    }
    if data.starts_with(b"BM") && data.len() > 26 {
        return decode_dib(data, false, hint);
    }
    if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return decode_webp(data, hint);
    }
    if data.starts_with(&[0, 0, 1, 0]) {
        // 嵌套 ICO（MSI 的 Icon 表里可能直接存 ICO 文件）
        let set = crate::ico::read(data)?;
        return set
            .largest()
            .cloned()
            .ok_or_else(|| IconError::NoIcon(hint.to_string()));
    }
    if looks_like_svg(data) {
        return raster_svg(data, SVG_MAX_LONG as u32, hint);
    }
    if data.len() >= 12 && &data[4..12] == b"jP  \r\n\x87\n" {
        return Err(IconError::NeedExternal("JPEG 2000 图标".into()));
    }
    Err(IconError::Corrupt(format!("{hint}：不是可识别的图片数据")))
}

fn looks_like_svg(data: &[u8]) -> bool {
    let s = data.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(0);
    let head = &data[s..data.len().min(s + 256)];
    let text = String::from_utf8_lossy(head);
    text.starts_with("<?xml") || text.starts_with("<svg") || text.contains("<svg")
}

/// 解 PNG 字节。palette / 灰度都展开成 8 位 RGB(A)。
pub fn decode_png(data: &[u8], hint: &str) -> Result<Frame, IconError> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| IconError::Corrupt(format!("{hint}：PNG 头 {e}")))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| IconError::TooLarge(format!("{hint}：PNG 尺寸异常")))?;
    if size > MAX_FRAME_BYTES {
        return Err(IconError::TooLarge(format!("{hint}：PNG 解压后 {size} B")));
    }
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| IconError::Corrupt(format!("{hint}：PNG 数据 {e}")))?;
    let (w, h) = (info.width, info.height);
    if (w as u64) * (h as u64) > MAX_FRAME_PIXELS {
        return Err(IconError::TooLarge(format!("{hint}：{w}×{h}")));
    }
    let line = info.line_size;
    let px = &buf[..line * h as usize];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            px.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Indexed => {
            return Err(IconError::Corrupt(format!("{hint}：PNG 调色板未展开")));
        }
    };
    let mut f = Frame::new(w, h, rgba, hint)?;
    f.png = Some(data.to_vec());
    f.rank = 30;
    Ok(f)
}

/// 解 WebP（GDI+ 不支持）。
pub fn decode_webp(data: &[u8], hint: &str) -> Result<Frame, IconError> {
    let (mut w, mut h) = (0i32, 0i32);
    unsafe {
        if libwebp_sys::WebPGetInfo(data.as_ptr(), data.len(), &mut w, &mut h) == 0
            || w <= 0
            || h <= 0
        {
            return Err(IconError::Corrupt(format!("{hint}：WebP 头解析失败")));
        }
        if (w as u64) * (h as u64) > MAX_FRAME_PIXELS {
            return Err(IconError::TooLarge(format!("{hint}：{w}×{h}")));
        }
        let mut dw = 0i32;
        let mut dh = 0i32;
        let ptr = libwebp_sys::WebPDecodeRGBA(data.as_ptr(), data.len(), &mut dw, &mut dh);
        if ptr.is_null() {
            return Err(IconError::Corrupt(format!("{hint}：WebP 解码失败")));
        }
        let len = dw as usize * dh as usize * 4;
        let rgba = std::slice::from_raw_parts(ptr, len).to_vec();
        libwebp_sys::WebPFree(ptr as *mut _);
        let mut f = Frame::new(dw as u32, dh as u32, rgba, hint)?;
        f.rank = 25;
        Ok(f)
    }
}

/// SVG 光栅化。Linux 包里大量图标只有 SVG，这一步不做等于少半个平台。
pub fn raster_svg(data: &[u8], wanted: u32, hint: &str) -> Result<Frame, IconError> {
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb = system_fontdb();
    let tree = resvg::usvg::Tree::from_data(data, &opt)
        .map_err(|e| IconError::Corrupt(format!("{hint}：SVG 解析失败 {e}")))?;
    let size = tree.size();
    let (iw, ih) = (size.width(), size.height());
    if !(iw.is_finite() && ih.is_finite()) || iw <= 0.0 || ih <= 0.0 {
        return Err(IconError::Corrupt(format!("{hint}：SVG 没有有效宽高")));
    }
    let cap = (wanted as f32).clamp(SVG_MIN_LONG, SVG_MAX_LONG);
    let long = iw.max(ih);
    let target = long.max(SVG_MIN_LONG).min(cap);
    let scale = target / long;
    let w = ((iw * scale).round() as u32).max(1);
    let h = ((ih * scale).round() as u32).max(1);
    if (w as u64) * (h as u64) > MAX_FRAME_PIXELS {
        return Err(IconError::TooLarge(format!("{hint}：{w}×{h}")));
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| IconError::TooLarge(format!("{hint}：SVG 画布创建失败")))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = pixmap.data().to_vec();
    // tiny-skia 预乘 alpha，交出去之前反预乘，否则半透明边缘发黑。
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 {
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
        } else if a < 255 {
            for c in &mut p[..3] {
                *c = (((*c as u32) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let mut f = Frame::new(w, h, rgba, hint)?;
    f.rank = 30;
    Ok(f)
}

fn system_fontdb() -> std::sync::Arc<resvg::usvg::fontdb::Database> {
    static DB: std::sync::OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        std::sync::Arc::new(db)
    })
    .clone()
}

/// JPEG/GIF 这类 GDI+ 才能解的格式：落一个临时文件再走系统解码器。
fn decode_via_gdiplus(data: &[u8], ext: &str, hint: &str) -> Result<Frame, IconError> {
    let dir = std::env::temp_dir();
    let name = format!(
        ".zx-appicon-{}-{}.{ext}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let path = dir.join(name);
    std::fs::write(&path, data).map_err(|e| crate::error::io_error(&e, &path))?;
    let out = decode_file_gdiplus(&path, hint);
    let _ = std::fs::remove_file(&path);
    out
}

fn decode_file_gdiplus(path: &Path, hint: &str) -> Result<Frame, IconError> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::GdiPlus::*;

    zx_ui::painter::init_gdiplus();
    let wide = zx_core::win::wide(&path.to_string_lossy());
    let mut img: *mut GpImage = std::ptr::null_mut();
    unsafe {
        let st = GdipLoadImageFromFileICM(PCWSTR(wide.as_ptr()), &mut img);
        if st.0 != 0 || img.is_null() {
            return Err(IconError::Corrupt(format!("{hint}：GDI+ 载入失败 status={}", st.0)));
        }
        let mut w = 0u32;
        let mut h = 0u32;
        let _ = GdipGetImageWidth(img, &mut w);
        let _ = GdipGetImageHeight(img, &mut h);
        if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_FRAME_PIXELS {
            GdipDisposeImage(img);
            return Err(IconError::TooLarge(format!("{hint}：{w}×{h}")));
        }
        let mut dst: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(w as i32, h as i32, 0, 0x0026_200A, None, &mut dst).0 != 0
            || dst.is_null()
        {
            GdipDisposeImage(img);
            return Err(IconError::Corrupt(format!("{hint}：创建目标位图失败")));
        }
        let mut g: *mut GpGraphics = std::ptr::null_mut();
        if GdipGetImageGraphicsContext(dst as *mut GpImage, &mut g).0 != 0 || g.is_null() {
            GdipDisposeImage(dst as *mut GpImage);
            GdipDisposeImage(img);
            return Err(IconError::Corrupt(format!("{hint}：取绘图上下文失败")));
        }
        let _ = GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
        let _ = GdipDrawImageRectI(g, img, 0, 0, w as i32, h as i32);
        let _ = GdipFlush(g, FlushIntentionSync);
        let _ = GdipDeleteGraphics(g);

        let rect = Rect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
        let mut bd = BitmapData::default();
        if GdipBitmapLockBits(dst, &rect, 1, 0x0026_200A, &mut bd).0 != 0 {
            GdipDisposeImage(dst as *mut GpImage);
            GdipDisposeImage(img);
            return Err(IconError::Corrupt(format!("{hint}：锁定像素失败")));
        }
        let stride = bd.Stride as usize;
        let base = bd.Scan0 as *const u8;
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h as usize {
            std::ptr::copy_nonoverlapping(
                base.add(y * stride),
                rgba[y * w as usize * 4..(y + 1) * w as usize * 4].as_mut_ptr(),
                w as usize * 4,
            );
        }
        let _ = GdipBitmapUnlockBits(dst, &mut bd);
        GdipDisposeImage(dst as *mut GpImage);
        GdipDisposeImage(img);
        for p in rgba.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        let mut f = Frame::new(w, h, rgba, hint)?;
        f.rank = 20;
        Result::Ok(f)
    }
}

/// 解 DIB（BMP / ICO 内部帧）。支持 1/4/8/16/24/32 位与 ICO 的 AND 掩码。
pub fn decode_dib(data: &[u8], is_icon: bool, hint: &str) -> Result<Frame, IconError> {
    if data.len() < 40 {
        return Err(IconError::Corrupt(format!("{hint}：DIB 头过短")));
    }
    let rd_u16 = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
    let rd_u32 = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let hdr = rd_u32(0) as usize;
    if hdr < 40 || hdr > data.len() {
        return Err(IconError::Corrupt(format!("{hint}：DIB 头长度 {hdr}")));
    }
    let mut w = rd_u32(4) as i32;
    let mut h2 = rd_u32(8) as i32;
    let bpp = rd_u16(14);
    let compress = rd_u32(16);
    let colors_used = rd_u32(32) as usize;

    if w == 0 || h2 == 0 {
        return Err(IconError::Corrupt(format!("{hint}：DIB 尺寸为 0")));
    }
    let top_down = h2 < 0;
    if top_down {
        h2 = -h2;
    }
    w = w.abs();
    let h = if is_icon && !top_down && h2 % 2 == 0 { h2 / 2 } else { h2 };
    if w <= 0 || h <= 0 {
        return Err(IconError::Corrupt(format!("{hint}：DIB 尺寸异常")));
    }
    if (w as u64) * (h as u64) > MAX_FRAME_PIXELS {
        return Err(IconError::TooLarge(format!("{hint}：{w}×{h}")));
    }
    let w = w as u32;
    let h = h as u32;

    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(IconError::Unsupported(format!("DIB {bpp}bpp")));
    }
    if !matches!(compress, 0 | 3) {
        return Err(IconError::Unsupported(format!("DIB 压缩 {compress}")));
    }

    // BITFIELDS：40 字节头后面跟三/四个掩码
    let mut masks = [0u32; 4];
    let mut off = hdr;
    if compress == 3 {
        if hdr == 40 {
            if data.len() < off + 12 {
                return Err(IconError::Corrupt(format!("{hint}：BITFIELDS 掩码缺失")));
            }
            for m in masks.iter_mut().take(3) {
                *m = rd_u32(off);
                off += 4;
            }
        } else {
            for (i, m) in masks.iter_mut().enumerate() {
                *m = rd_u32(40 + i * 4);
            }
        }
    }
    if bpp == 16 && masks[0] == 0 {
        masks = [0x7C00, 0x03E0, 0x001F, 0];
    }

    let palette = if bpp <= 8 {
        let n = if colors_used > 0 { colors_used.min(256) } else { 1usize << bpp };
        let need = off + n * 4;
        if data.len() < need {
            return Err(IconError::Corrupt(format!("{hint}：调色板不完整")));
        }
        let mut pal = Vec::with_capacity(n);
        for i in 0..n {
            let o = off + i * 4;
            pal.push([data[o + 2], data[o + 1], data[o], data[o + 3]]);
        }
        off = need;
        Some(pal)
    } else {
        None
    };

    let stride = ((w as usize * bpp as usize + 31) / 32) * 4;
    let need = off + stride * h as usize;
    if data.len() < need {
        return Err(IconError::Corrupt(format!(
            "{hint}：像素数据不完整（需要 {need}，实际 {}）",
            data.len()
        )));
    }

    // AND 掩码紧跟在像素之后；有些 ICO 省略它。
    let mask_off = need;
    let mask_stride = ((w as usize + 31) / 32) * 4;
    let has_mask = is_icon && data.len() >= mask_off + mask_stride * h as usize;

    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    // 先扫一遍 32 位帧的 alpha：必须整帧判断，不能边解边看——
    // 否则第一行还是"没 alpha"，AND 掩码会把本该半透明的像素刷成不透明。
    let mut any_alpha = false;
    if bpp == 32 {
        for y in 0..h as usize {
            let row = off + y * stride;
            for x in 0..w as usize {
                if data[row + x * 4 + 3] != 0 {
                    any_alpha = true;
                    break;
                }
            }
            if any_alpha {
                break;
            }
        }
    }
    for y in 0..h as usize {
        let src_row = if top_down { y } else { h as usize - 1 - y };
        let row = off + src_row * stride;
        for x in 0..w as usize {
            let (r, g, b, mut a) = match bpp {
                1 | 4 | 8 => {
                    let bit = match bpp {
                        1 => (data[row + x / 8] >> (7 - (x % 8))) & 1,
                        4 => (data[row + x / 2] >> (4 * (1 - (x % 2)))) & 0xF,
                        _ => data[row + x],
                    } as usize;
                    let c = palette.as_ref().and_then(|p| p.get(bit)).copied().unwrap_or([0, 0, 0, 255]);
                    (c[0], c[1], c[2], if c[3] == 0 { 255 } else { c[3] })
                }
                16 => {
                    let v = rd_u16(row + x * 2) as u32;
                    let c = |m: u32| -> u8 {
                        if m == 0 {
                            return 0;
                        }
                        let shift = m.trailing_zeros();
                        let max = m >> shift;
                        let val = (v & m) >> shift;
                        ((val * 255 + max / 2) / max) as u8
                    };
                    (c(masks[0]), c(masks[1]), c(masks[2]), 255)
                }
                24 => {
                    let o = row + x * 3;
                    (data[o + 2], data[o + 1], data[o], 255)
                }
                _ => {
                    let o = row + x * 4;
                    (data[o + 2], data[o + 1], data[o], data[o + 3])
                }
            };
            if has_mask {
                // 有 alpha 的 32 位帧以 alpha 为准；其余（含 alpha 全 0 的 32 位帧）
                // 用 AND 掩码判透明，掩码没置位就是不透明。
                if bpp != 32 || !any_alpha {
                    let mo = mask_off + src_row * mask_stride + x / 8;
                    let masked = mo < data.len() && (data[mo] >> (7 - (x % 8))) & 1 == 1;
                    a = if masked { 0 } else { 255 };
                }
            }
            let d = (y * w as usize + x) * 4;
            rgba[d] = r;
            rgba[d + 1] = g;
            rgba[d + 2] = b;
            rgba[d + 3] = a;
        }
    }
    if bpp == 32 && !any_alpha && !has_mask {
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    let rank = match bpp {
        32 => 24,
        24 => 20,
        16 => 16,
        8 => 12,
        4 => 8,
        _ => 4,
    };
    let mut f = Frame::new(w, h, rgba, hint)?;
    f.rank = rank;
    Ok(f)
}

/* ------------------------------- 尺寸工具 ------------------------------- */

/// 用 GDI+ 做高质量缩放；失败时退回最近邻（宁可糊一点也不能失败）。
pub fn resize(src: &Frame, tw: u32, th: u32) -> Result<Frame, IconError> {
    if src.w == tw && src.h == th {
        return Ok(src.clone());
    }
    if let Some(f) = resize_gdiplus(src, tw, th) {
        return Ok(f);
    }
    Ok(resize_nearest(src, tw, th))
}

fn resize_gdiplus(src: &Frame, tw: u32, th: u32) -> Option<Frame> {
    use windows::Win32::Graphics::GdiPlus::*;
    zx_ui::painter::init_gdiplus();
    let mut bgra = Vec::with_capacity(src.rgba.len());
    for p in src.rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    unsafe {
        let mut s: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(
            src.w as i32,
            src.h as i32,
            (src.w * 4) as i32,
            0x0026_200A,
            Some(bgra.as_ptr()),
            &mut s,
        )
        .0 != 0
            || s.is_null()
        {
            return None;
        }
        let mut d: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(tw as i32, th as i32, 0, 0x0026_200A, None, &mut d).0 != 0
            || d.is_null()
        {
            GdipDisposeImage(s as *mut GpImage);
            return None;
        }
        let mut g: *mut GpGraphics = std::ptr::null_mut();
        if GdipGetImageGraphicsContext(d as *mut GpImage, &mut g).0 != 0 || g.is_null() {
            GdipDisposeImage(d as *mut GpImage);
            GdipDisposeImage(s as *mut GpImage);
            return None;
        }
        let _ = GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
        let _ = GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);
        let _ = GdipDrawImageRectI(g, s as *mut GpImage, 0, 0, tw as i32, th as i32);
        let _ = GdipFlush(g, FlushIntentionSync);
        let _ = GdipDeleteGraphics(g);

        let rect = Rect { X: 0, Y: 0, Width: tw as i32, Height: th as i32 };
        let mut bd = BitmapData::default();
        if GdipBitmapLockBits(d, &rect, 1, 0x0026_200A, &mut bd).0 != 0 {
            GdipDisposeImage(d as *mut GpImage);
            GdipDisposeImage(s as *mut GpImage);
            return None;
        }
        let stride = bd.Stride as usize;
        let base = bd.Scan0 as *const u8;
        let mut rgba = vec![0u8; tw as usize * th as usize * 4];
        for y in 0..th as usize {
            std::ptr::copy_nonoverlapping(
                base.add(y * stride),
                rgba[y * tw as usize * 4..(y + 1) * tw as usize * 4].as_mut_ptr(),
                tw as usize * 4,
            );
        }
        let _ = GdipBitmapUnlockBits(d, &mut bd);
        GdipDisposeImage(d as *mut GpImage);
        GdipDisposeImage(s as *mut GpImage);
        for p in rgba.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Frame::new(tw, th, rgba, "resize").ok()
    }
}

fn resize_nearest(src: &Frame, tw: u32, th: u32) -> Frame {
    let mut rgba = vec![0u8; tw as usize * th as usize * 4];
    for y in 0..th as usize {
        let sy = (y as u64 * src.h as u64 / th as u64) as usize;
        for x in 0..tw as usize {
            let sx = (x as u64 * src.w as u64 / tw as u64) as usize;
            let s = (sy * src.w as usize + sx) * 4;
            let d = (y * tw as usize + x) * 4;
            rgba[d..d + 4].copy_from_slice(&src.rgba[s..s + 4]);
        }
    }
    Frame { w: tw, h: th, rgba, png: None, origin: src.origin.clone(), rank: src.rank }
}

/// 非正方形图标居中补到正方形（透明填充）。ICO 的每一帧必须是正方形。
pub fn square_pad(src: &Frame) -> Frame {
    let side = src.w.max(src.h);
    if src.w == side && src.h == side {
        return src.clone();
    }
    let mut rgba = vec![0u8; side as usize * side as usize * 4];
    let dx = (side - src.w) / 2;
    let dy = (side - src.h) / 2;
    for y in 0..src.h as usize {
        let s = &src.rgba[y * src.w as usize * 4..(y + 1) * src.w as usize * 4];
        let d = ((y as u32 + dy) as usize * side as usize + dx as usize) * 4;
        rgba[d..d + s.len()].copy_from_slice(s);
    }
    Frame { w: side, h: side, rgba, png: None, origin: src.origin.clone(), rank: src.rank }
}

/// 等比缩到长边不超过 max（不放大）。
pub fn fit_long(src: &Frame, max: u32) -> Result<Frame, IconError> {
    let long = src.long_side();
    if max == 0 || long <= max {
        return Ok(src.clone());
    }
    let scale = max as f64 / long as f64;
    let w = ((src.w as f64 * scale).round() as u32).max(1);
    let h = ((src.h as f64 * scale).round() as u32).max(1);
    resize(src, w, h)
}

/// 编码 PNG。全不透明就写 RGB（省 25% 体积）。
pub fn png_bytes(f: &Frame, force_alpha: bool) -> Result<Vec<u8>, IconError> {
    let opaque = !force_alpha && f.opaque();
    let data: std::borrow::Cow<[u8]> = if opaque {
        let mut rgb = Vec::with_capacity((f.w as usize) * (f.h as usize) * 3);
        for p in f.rgba.chunks_exact(4) {
            rgb.extend_from_slice(&p[..3]);
        }
        std::borrow::Cow::Owned(rgb)
    } else {
        std::borrow::Cow::Borrowed(&f.rgba)
    };
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(std::io::Cursor::new(&mut out), f.w, f.h);
        enc.set_color(if opaque { png::ColorType::Rgb } else { png::ColorType::Rgba });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        let mut wr = enc.write_header().map_err(|e| IconError::Corrupt(format!("PNG 头 {e}")))?;
        wr.write_image_data(&data).map_err(|e| IconError::Corrupt(format!("PNG 数据 {e}")))?;
        wr.finish().map_err(|e| IconError::Corrupt(format!("PNG 收尾 {e}")))?;
    }
    Ok(out)
}
