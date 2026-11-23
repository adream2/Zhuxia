//! 解码：GDI+ 负责常见位图（含 CMYK JPEG、带 ICC 的照片），WebP 交给 libwebp，
//! SVG 交给 resvg（系统没有可直接调用的矢量光栅化器）。
//!
//! 为什么不用 `image` crate 一把梭：GDI+ 是系统库，零体积、零编译时间，而且
//! CMYK/ICC 的处理比纯 Rust 解码器靠谱（手机照片、印刷稿都常见）。
//! 参考 Filee 的 `MagickImageConverter`：解码后**立刻按 EXIF 定向**、按目标格式
//! 的边上限缩放，再把像素交给编码器——顺序不能反，否则旋转会把缩放后的边缘糊掉。

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Graphics::GdiPlus::*;

use crate::error::ConvertError;

/// 解出来的一张图：RGBA8，行优先，无 padding。全流程只用这一种内存布局。
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

impl Bitmap {
    /// 整张图是否全不透明（PNG 可以据此省掉 alpha 通道，文件小 25% 左右）。
    pub fn opaque(&self) -> bool {
        self.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255)
    }

    /// 把 alpha 压到背景色上（JPG/GIF/BMP 没有透明通道）。
    /// 就地改 RGBA 并返回新缓冲：`out = src * a + bg * (1 - a)`。
    pub fn flatten(&self, bg: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rgba.len());
        for p in self.rgba.as_chunks::<4>().0 {
            let a = p[3] as u32;
            if a == 255 {
                out.extend_from_slice(&p[..3]);
            } else if a == 0 {
                out.extend_from_slice(&bg);
            } else {
                let ia = 255 - a;
                for i in 0..3 {
                    out.push(((p[i] as u32 * a + bg[i] as u32 * ia) / 255) as u8);
                }
            }
            out.push(255);
        }
        out
    }
}

const PIXFMT_ARGB: i32 = 0x0026_200A; // PixelFormat32bppARGB（内存里是 BGRA 字节序）
/// 解码上限：80 MP ≈ 320 MB RGBA。再大就该用户自己先缩，硬扛只会换页到死。
const MAX_PIXELS: u64 = 80_000_000;
/// 单文件读取上限（WebP 走内存解码才需要；GDI+ 自己管 IO）。
const MAX_FILE: u64 = 512 * 1024 * 1024;
/// SVG 渲染下限：24×24 的图标 SVG 按原尺寸转出来没法用，长边至少给到 256px。
const SVG_MIN_LONG: f32 = 256.0;
/// SVG 渲染上限：再大就交给目标格式自己的边上限，避免一张矢量图画成上亿像素。
const SVG_MAX_LONG: f32 = 4096.0;

/// 解码入口。`max_side` 是目标格式的边上限（0 = 不缩放）。
pub fn load(path: &Path, max_side: u32) -> Result<Bitmap, ConvertError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "webp" {
        return load_webp(path);
    }
    if ext == "svg" || ext == "svgz" {
        return load_svg(path, max_side);
    }
    if ext == "ico" {
        return load_ico(path, max_side);
    }
    unsafe { load_gdiplus(path, max_side) }
}

unsafe fn load_gdiplus(path: &Path, max_side: u32) -> Result<Bitmap, ConvertError> {
    zx_ui::painter::init_gdiplus();
    let wide = zx_core::win::wide(&path.to_string_lossy());
    let mut img: *mut GpImage = std::ptr::null_mut();
    // ICM 版本会套用内嵌色彩配置：手机照片的 Display P3 不套会偏色。
    let st = GdipLoadImageFromFileICM(PCWSTR(wide.as_ptr()), &mut img);
    if st.0 != 0 || img.is_null() {
        return Err(ConvertError::Corrupt(format!("GDI+ 载入失败 status={:?}", st.0)));
    }
    let _img = ImageGuard(img);

    let (mut w, mut h) = (0u32, 0u32);
    let _ = GdipGetImageWidth(img, &mut w);
    let _ = GdipGetImageHeight(img, &mut h);
    if w == 0 || h == 0 {
        return Err(ConvertError::Corrupt("图片尺寸为 0".into()));
    }

    // ---- EXIF 方向 ----
    // GDI+ 不会自动转正，手机的 portrait 照片直接重编码就会躺倒。
    let orientation = read_orientation(img);
    if orientation > 1 {
        let rft = match orientation {
            2 => RotateNoneFlipX,
            3 => Rotate180FlipNone,
            4 => RotateNoneFlipY,
            5 => Rotate90FlipX,
            6 => Rotate90FlipNone,
            7 => Rotate270FlipX,
            8 => Rotate270FlipNone,
            _ => RotateNoneFlipNone,
        };
        if GdipImageRotateFlip(img, rft).0 == 0 && orientation >= 5 {
            std::mem::swap(&mut w, &mut h);
        }
    }

    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(ConvertError::TooLarge { w, h });
    }

    // ---- 缩放到目标格式的边上限（只在超限时缩，绝不放大）----
    let (tw, th) = fit(w, h, max_side);

    let mut dst: *mut GpBitmap = std::ptr::null_mut();
    if GdipCreateBitmapFromScan0(tw as i32, th as i32, 0, PIXFMT_ARGB, None, &mut dst).0 != 0
        || dst.is_null()
    {
        return Err(ConvertError::Corrupt("创建目标位图失败（内存不足？）".into()));
    }
    let _dst = BitmapGuard(dst);

    let mut g: *mut GpGraphics = std::ptr::null_mut();
    if GdipGetImageGraphicsContext(dst as *mut GpImage, &mut g).0 == 0 && !g.is_null() {
        let _ = GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
        // Half 像素偏移 + 高质量插值：缩小时不会把 1px 边框吃掉。
        let _ = GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);
        let _ = GdipDrawImageRectI(g, img, 0, 0, tw as i32, th as i32);
        let _ = GdipFlush(g, FlushIntentionSync);
        let _ = GdipDeleteGraphics(g);
    } else {
        return Err(ConvertError::Corrupt("取绘图上下文失败".into()));
    }

    // ---- 取像素 ----
    let rect = Rect { X: 0, Y: 0, Width: tw as i32, Height: th as i32 };
    let mut data = BitmapData::default();
    if GdipBitmapLockBits(dst, &rect, 1 /* ImageLockModeRead */, PIXFMT_ARGB, &mut data).0 != 0 {
        return Err(ConvertError::Corrupt("锁定像素失败".into()));
    }
    let mut rgba = vec![0u8; tw as usize * th as usize * 4];
    {
        let stride = data.Stride as usize;
        let src = data.Scan0 as *const u8;
        for y in 0..th as usize {
            let row = src.add(y * stride);
            let out = &mut rgba[y * tw as usize * 4..(y + 1) * tw as usize * 4];
            std::ptr::copy_nonoverlapping(row, out.as_mut_ptr(), tw as usize * 4);
        }
    }
    let _ = GdipBitmapUnlockBits(dst, &mut data);

    // GDI+ 的 32bppARGB 在内存里是 BGRA，转成 RGBA 交给编码器（PNG/WebP 都要 RGBA）。
    for p in rgba.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    Result::Ok(Bitmap { w: tw, h: th, rgba })
}

/// 读 EXIF Orientation（PropertyTagOrientation = 0x0112）；读不到按 1 处理。
unsafe fn read_orientation(img: *mut GpImage) -> u16 {
    const PID_ORIENTATION: u32 = 0x0112;
    let mut size = 0u32;
    if GdipGetPropertyItemSize(img, PID_ORIENTATION, &mut size).0 != 0 || size == 0 {
        return 1;
    }
    // 用 u64 缓冲保证 8 字节对齐（PropertyItem 里有指针字段）。
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let item = buf.as_mut_ptr() as *mut PropertyItem;
    if GdipGetPropertyItem(img, PID_ORIENTATION, size, item).0 != 0 {
        return 1;
    }
    let it = &*item;
    if it.length >= 2 && !it.value.is_null() {
        let v = *(it.value as *const u16);
        if (1..=8).contains(&v) { return v; }
    }
    1
}

/// SVG 光栅化（resvg）。
///
/// 尺寸策略：按 SVG 自己声明的尺寸渲染，但**长边至少 256px**（图标类 SVG 才有用），
/// 上限 4096px 或目标格式的边上限。渲染结果是预乘 alpha，交出去之前要反预乘，
/// 否则半透明边缘会发黑。
fn load_svg(path: &Path, max_side: u32) -> Result<Bitmap, ConvertError> {
    let data = std::fs::read(path).map_err(|e| crate::error::io_error(&e, path))?;
    if data.len() as u64 > MAX_FILE {
        return Err(ConvertError::Corrupt("文件过大，拒绝渲染".into()));
    }

    // 字体库进程内只加载一次：load_system_fonts 要扫几千个字体文件，每次转换都扫会明显卡。
    let opt = resvg::usvg::Options {
        fontdb: system_fontdb(),
        resources_dir: path.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };

    let tree = resvg::usvg::Tree::from_data(&data, &opt)
        .map_err(|e| ConvertError::Corrupt(format!("SVG 解析失败：{e}")))?;
    let size = tree.size();
    let (iw, ih) = (size.width(), size.height());
    if !(iw.is_finite() && ih.is_finite()) || iw <= 0.0 || ih <= 0.0 {
        return Err(ConvertError::Corrupt("SVG 没有有效的宽高".into()));
    }

    let cap = if max_side > 0 {
        (max_side as f32).min(SVG_MAX_LONG)
    } else {
        SVG_MAX_LONG
    }
    .max(SVG_MIN_LONG);
    let long = iw.max(ih);
    let target_long = long.max(SVG_MIN_LONG).min(cap);
    let scale = target_long / long;
    let w = ((iw * scale).round() as u32).max(1);
    let h = ((ih * scale).round() as u32).max(1);
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(ConvertError::TooLarge { w, h });
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| ConvertError::Corrupt("SVG 画布创建失败（内存不足？）".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let mut rgba = pixmap.data().to_vec();
    // tiny-skia 输出预乘 RGBA；PNG / WebP 要的是直通 alpha。
    for p in rgba.as_chunks_mut::<4>().0 {
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
    Result::Ok(Bitmap { w, h, rgba })
}

/// 进程内共享一份系统字体库（usvg 的 `fontdb`）。
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

/// 用 GDI+ 把图缩放到指定尺寸。
///
/// ICO 要一次产出好几个规格，不能只依赖解码时那一次缩放，所以单独暴露一个缩放入口。
/// 缩小走 HighQualityBicubic + Half 像素偏移，细线不会丢。
pub fn resize(src: &Bitmap, w: u32, h: u32) -> Result<Bitmap, ConvertError> {
    if src.w == w && src.h == h {
        return Result::Ok(Bitmap { w, h, rgba: src.rgba.clone() });
    }
    zx_ui::painter::init_gdiplus();
    let mut bgra = Vec::with_capacity(src.rgba.len());
    for p in src.rgba.as_chunks::<4>().0 {
        bgra.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    unsafe {
        let mut s: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(
            src.w as i32,
            src.h as i32,
            (src.w * 4) as i32,
            PIXFMT_ARGB,
            Some(bgra.as_ptr()),
            &mut s,
        )
        .0 != 0
            || s.is_null()
        {
            return Err(ConvertError::Corrupt("缩放：源位图创建失败".into()));
        }
        let _s = BitmapGuard(s);
        let mut d: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(w as i32, h as i32, 0, PIXFMT_ARGB, None, &mut d).0 != 0
            || d.is_null()
        {
            return Err(ConvertError::Corrupt("缩放：目标位图创建失败".into()));
        }
        let _d = BitmapGuard(d);
        let mut g: *mut GpGraphics = std::ptr::null_mut();
        if GdipGetImageGraphicsContext(d as *mut GpImage, &mut g).0 != 0 || g.is_null() {
            return Err(ConvertError::Corrupt("缩放：取绘图上下文失败".into()));
        }
        let _ = GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
        let _ = GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);
        let _ = GdipDrawImageRectI(g, s as *mut GpImage, 0, 0, w as i32, h as i32);
        let _ = GdipFlush(g, FlushIntentionSync);
        let _ = GdipDeleteGraphics(g);

        let rect = Rect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
        let mut data = BitmapData::default();
        if GdipBitmapLockBits(d, &rect, 1, PIXFMT_ARGB, &mut data).0 != 0 {
            return Err(ConvertError::Corrupt("缩放：锁定像素失败".into()));
        }
        let stride = data.Stride as usize;
        let mut out = vec![0u8; w as usize * h as usize * 4];
        {
            let base = data.Scan0 as *const u8;
            for y in 0..h as usize {
                let row = base.add(y * stride);
                std::ptr::copy_nonoverlapping(
                    row,
                    out[y * w as usize * 4..(y + 1) * w as usize * 4].as_mut_ptr(),
                    w as usize * 4,
                );
            }
        }
        let _ = GdipBitmapUnlockBits(d, &mut data);
        for p in out.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        Result::Ok(Bitmap { w, h, rgba: out })
    }
}

/// ICO 解码：**取最大的一帧**。
///
/// GDI+ 读 ICO 只给系统默认尺寸（通常 16×16），拿它转 PNG 等于把图标毁了。
/// 这里自己解 ICONDIR：优先用最大帧里的 PNG 数据（Vista 之后 256px 基本都是 PNG），
/// 其次解 32/24 位 BMP 帧，都不行才退回 GDI+。
fn load_ico(path: &Path, max_side: u32) -> Result<Bitmap, ConvertError> {
    let bytes = std::fs::read(path).map_err(|e| crate::error::io_error(&e, path))?;
    let mut best: Option<(u32, usize, usize)> = None; // (面积, 偏移, 长度)
    if bytes.len() >= 6 && u16::from_le_bytes([bytes[2], bytes[3]]) == 1 {
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        for i in 0..count {
            let e = 6 + i * 16;
            if e + 16 > bytes.len() {
                break;
            }
            let w = if bytes[e] == 0 { 256u32 } else { bytes[e] as u32 };
            let h = if bytes[e + 1] == 0 { 256u32 } else { bytes[e + 1] as u32 };
            let len = u32::from_le_bytes([bytes[e + 8], bytes[e + 9], bytes[e + 10], bytes[e + 11]]);
            let off = u32::from_le_bytes([bytes[e + 12], bytes[e + 13], bytes[e + 14], bytes[e + 15]]);
            let (len, off) = (len as usize, off as usize);
            if len == 0 || off.saturating_add(len) > bytes.len() {
                continue;
            }
            let area = w * h;
            if best.map(|(a, _, _)| area > a).unwrap_or(true) {
                best = Some((area, off, len));
            }
        }
    }

    if let Some((_, off, len)) = best {
        let data = &bytes[off..off + len];
        let mut img = if data.len() >= 8 && &data[..8] == b"\x89PNG\r\n\x1a\n" {
            decode_png_slice(data)?
        } else {
            decode_bmp_frame(data)?
        };
        if max_side > 0 && img.w.max(img.h) > max_side {
            let (tw, th) = fit(img.w, img.h, max_side);
            img = resize(&img, tw, th)?;
        }
        return Result::Ok(img);
    }
    unsafe { load_gdiplus(path, max_side) }
}

/// 解码一段 PNG 字节（ICO 里的大图标就是这个）。
fn decode_png_slice(data: &[u8]) -> Result<Bitmap, ConvertError> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| ConvertError::Corrupt(format!("ICO 内的 PNG 解析失败：{e}")))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| ConvertError::Corrupt("ICO 内的 PNG 尺寸异常".into()))?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| ConvertError::Corrupt(format!("ICO 内的 PNG 解码失败：{e}")))?;
    let (w, h) = (info.width, info.height);
    let px = &buf[..info.line_size * h as usize];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px
            .as_chunks::<3>().0.iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => px
            .as_chunks::<2>().0.iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => {
            return Err(ConvertError::Corrupt("ICO 内的 PNG 调色板未展开".into()))
        }
    };
    Result::Ok(Bitmap { w, h, rgba })
}

/// 解 ICO 里的 BMP 帧（BITMAPINFOHEADER + 自下而上的行，可选 AND 掩码）。
/// 只处理未压缩的 32/24 位：图标里这两种最常见，其余交给 GDI+。
fn decode_bmp_frame(data: &[u8]) -> Result<Bitmap, ConvertError> {
    const HDR: usize = 40;
    if data.len() < HDR + 4 {
        return Err(ConvertError::Corrupt("ICO 内的 BMP 帧过短".into()));
    }
    let rd_u32 = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let hdr_size = rd_u32(0) as usize;
    let w = rd_u32(4) as i32;
    let h2 = rd_u32(8) as i32; // 含 AND 掩码，实际高度是它的一半
    let bpp = u16::from_le_bytes([data[14], data[15]]);
    let compress = rd_u32(16);
    if w <= 0 || h2 <= 0 || hdr_size < HDR || compress != 0 || !matches!(bpp, 24 | 32) {
        return Err(ConvertError::Corrupt("ICO 内的 BMP 帧格式不支持".into()));
    }
    let h = (h2 / 2) as u32;
    let w = w as u32;
    let stride = ((w * bpp as u32).div_ceil(32) * 4) as usize;
    let need = hdr_size + stride * h as usize;
    if data.len() < need {
        return Err(ConvertError::Corrupt("ICO 内的 BMP 像素不完整".into()));
    }

    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        // BMP 自下而上：源行 = h-1-y
        let src_row = hdr_size + (h as usize - 1 - y) * stride;
        let dst_row = y * w as usize * 4;
        for x in 0..w as usize {
            let s = src_row + x * (bpp as usize / 8);
            let d = dst_row + x * 4;
            rgba[d] = data[s + 2];
            rgba[d + 1] = data[s + 1];
            rgba[d + 2] = data[s];
            rgba[d + 3] = if bpp == 32 { data[s + 3] } else { 255 };
        }
    }
    // 32 位帧如果没有 alpha（全 0），按不透明处理；24 位帧由 AND 掩码给透明。
    if bpp == 32 && rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        for p in rgba.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
    }
    Result::Ok(Bitmap { w, h, rgba })
}

/// libwebp 解码（GDI+ 不支持 WebP）。
fn load_webp(path: &Path) -> Result<Bitmap, ConvertError> {
    let meta = std::fs::metadata(path).map_err(|e| crate::error::io_error(&e, path))?;
    if meta.len() > MAX_FILE {
        return Err(ConvertError::Corrupt("文件过大，拒绝解码".into()));
    }
    let data = std::fs::read(path).map_err(|e| crate::error::io_error(&e, path))?;
    let (mut w, mut h) = (0i32, 0i32);
    unsafe {
        if libwebp_sys::WebPGetInfo(data.as_ptr(), data.len(), &mut w, &mut h) == 0 || w <= 0 || h <= 0 {
            return Err(ConvertError::Corrupt("WebP 头解析失败".into()));
        }
        if (w as u64) * (h as u64) > MAX_PIXELS {
            return Err(ConvertError::TooLarge { w: w as u32, h: h as u32 });
        }
        let mut dw = 0i32;
        let mut dh = 0i32;
        let ptr = libwebp_sys::WebPDecodeRGBA(data.as_ptr(), data.len(), &mut dw, &mut dh);
        if ptr.is_null() {
            return Err(ConvertError::Corrupt("WebP 解码失败".into()));
        }
        let len = dw as usize * dh as usize * 4;
        let rgba = std::slice::from_raw_parts(ptr, len).to_vec();
        libwebp_sys::WebPFree(ptr as *mut _);
        Result::Ok(Bitmap { w: dw as u32, h: dh as u32, rgba })
    }
}

/// 按边上限求目标尺寸；`max_side == 0` 或没超限时原样返回。
fn fit(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    if max_side == 0 {
        return (w, h);
    }
    let long = w.max(h);
    if long <= max_side {
        return (w, h);
    }
    let scale = max_side as f64 / long as f64;
    (
        ((w as f64 * scale).floor() as u32).max(1),
        ((h as f64 * scale).floor() as u32).max(1),
    )
}

struct ImageGuard(*mut GpImage);
impl Drop for ImageGuard {
    fn drop(&mut self) {
        unsafe { GdipDisposeImage(self.0); }
    }
}
struct BitmapGuard(*mut GpBitmap);
impl Drop for BitmapGuard {
    fn drop(&mut self) {
        unsafe { GdipDisposeImage(self.0 as *mut GpImage); }
    }
}
