//! 一块离屏画布：32bpp 预乘 ARGB DIB + GDI+ Graphics。
//!
//! 所有坐标都是物理像素（调用方按 dpi 缩放）。画完 `present()` 一次
//! UpdateLayeredWindow 送上屏——没有中间合成、没有额外窗口、没有渲染进程。

use std::collections::HashMap;
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, COLORREF, HGLOBAL, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::GdiPlus::*;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::*;

const PIXFMT_PARGB: i32 = 0x000E_200B; // PixelFormat32bppPARGB

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextAlign { Near, Center, Far }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VAlign { Top, Center, Bottom }

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub dpi: u32,
    dib: HBITMAP,
    memdc: HDC,
    old: HGDIOBJ,
    bits: *mut u8,
    bmp: *mut GpBitmap,
    g: *mut GpGraphics,
    family: *mut GpFontFamily,
    fonts: HashMap<(u16, bool), *mut GpFont>,
    /// 按 (字号, 字重, 字体名) 缓存的字体：水印要 light/regular/bold 三档字重，
    /// GDI+ 的 FontStyle 只有"粗/不粗"，所以走 GDI 的 LOGFONT。
    named: HashMap<(u16, i32, String), *mut GpFont>,
    fmts: HashMap<(u8, u8, bool), *mut GpStringFormat>,
    /// 纯色画刷按 ARGB 缓存：任务栏每帧要画几十个矩形，每次都 Create/Delete 一次
    /// 纯属浪费（字体和 StringFormat 早就这么做了）。Drop 里统一释放。
    brushes: HashMap<u32, *mut GpSolidFill>,
}

// SAFETY: 这个 `Send` **只是为了满足 `static Mutex<T>` 的约束**（`static` 要求 `T: Send`），
// 不代表可以把值 move 到别的线程去用。Canvas 握着 HBITMAP / HDC 和 GDI+ 的
// Graphics / Font / Brush，其中 GDI+ 是**按线程初始化**的（`GdiplusStartup` 每个使用
// 线程各一次），跨线程使用或 `Drop` 会随机崩在 GDI+ 内部。
//
// 真正的用法约定：**创建、绘制、销毁都在同一个线程**（UI 线程，或某个专职工作线程
// 自己 new 自己 drop）。目前所有调用点都满足；这条一旦被破坏，debug 构建也未必报错，
// 而是变成偶发崩溃，所以改代码时请守住它。
unsafe impl Send for Canvas {}

/// 进程级 GDI+ 初始化。模块自己也会用到 GDI+（例如剪贴板把 DIB 编码成 PNG），
/// 这时面板可能还没创建过 Canvas，所以对外暴露一个幂等的入口。
pub fn init_gdiplus() { startup(); }

/// 0xAARRGGBB 的感知亮度（Rec.709 系数，0..1）。
///
/// 四个模块原来各写了一份（其中一份用 BT.601 系数），同一个颜色在不同地方
/// 可能被判成"亮"或"暗"——统一到这里，免得"白字配亮底"这类问题各修各的。
pub fn luma(argb: u32) -> f32 {
    let r = ((argb >> 16) & 0xFF) as f32;
    let g = ((argb >> 8) & 0xFF) as f32;
    let b = (argb & 0xFF) as f32;
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
}

/// 系统里有没有这个字体族。
///
/// GDI+ / GDI 遇到不存在的字体名会**静默**换成默认字体，所以"切了字体没变化"
/// 十有八九是名字写错了或者系统没装。这里给业务层一个显式检查。
pub fn font_available(name: &str) -> bool {
    startup();
    let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut fam: *mut GpFontFamily = std::ptr::null_mut();
    unsafe {
        let st = GdipCreateFontFamilyFromName(PCWSTR(w.as_ptr()), std::ptr::null_mut(), &mut fam);
        if st.0 == 0 && !fam.is_null() {
            GdipDeleteFontFamily(fam);
            return true;
        }
    }
    false
}

// GDI+ 只需要进程级初始化一次。
fn startup() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let mut token: usize = 0;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: windows::Win32::Foundation::BOOL(0),
            SuppressExternalCodecs: windows::Win32::Foundation::BOOL(0),
        };
        let _ = GdiplusStartup(&mut token, &input, std::ptr::null_mut());
    });
}

fn pick_family(_dpi: u32) -> *mut GpFontFamily {
    // 中文优先：GDI+ 不做字体回退，Segoe UI 没有汉字字形。
    for name in ["Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI", "Tahoma"] {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut fam: *mut GpFontFamily = std::ptr::null_mut();
        unsafe {
            let st = GdipCreateFontFamilyFromName(PCWSTR(w.as_ptr()), std::ptr::null_mut(), &mut fam);
            if st.0 == 0 && !fam.is_null() { return fam; }
        }
    }
    std::ptr::null_mut()
}

impl Canvas {
    pub fn new(w: i32, h: i32, dpi: u32) -> Canvas {
        startup();
        let w = w.max(1);
        let h = h.max(1);
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
            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                .unwrap_or_default();
            let memdc = CreateCompatibleDC(screen);
            let old = SelectObject(memdc, dib);
            let mut bmp: *mut GpBitmap = std::ptr::null_mut();
            GdipCreateBitmapFromScan0(w, h, w * 4, PIXFMT_PARGB, Some(bits as *mut u8), &mut bmp);
            let mut g: *mut GpGraphics = std::ptr::null_mut();
            GdipGetImageGraphicsContext(bmp as *mut _, &mut g);
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
            GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
            GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
            ReleaseDC(None, screen);

            // 超大画布（长截图 / 拼接）在内存紧张时可能失败：这时 bits/bmp/g 会是 NULL，
            // 后续任何绘制或像素访问都是未定义行为。退化成 1×1 空图，让调用方拿到
            // 一张（空白但合法的）画布，而不是把 NULL 一路传下去。
            if dib.is_invalid() || bits.is_null() || bmp.is_null() || g.is_null() {
                if !g.is_null() { GdipDeleteGraphics(g); }
                if !bmp.is_null() { GdipDisposeImage(bmp as *mut _); }
                if !dib.is_invalid() {
                    SelectObject(memdc, old);
                    let _ = DeleteObject(dib);
                }
                if !memdc.is_invalid() { let _ = DeleteDC(memdc); }
                if w > 1 || h > 1 {
                    return Canvas::new(1, 1, dpi);
                }
            }

            Canvas {
                w, h, dpi, dib, memdc, old, bits: bits as *mut u8, bmp, g,
                family: pick_family(dpi), fonts: HashMap::new(), named: HashMap::new(),
                fmts: HashMap::new(), brushes: HashMap::new(),
            }
        }
    }

    /// 换尺寸：重建 DIB 与 GDI+ 位图（字体/格式缓存保留）。
    ///
    /// 注意：新画布的资源必须"接管"而不是被 Drop 掉——旧实现把字段拷出来后
    /// 让临时 Canvas 正常析构，结果刚接管的 DIB/DC 被立刻删除，
    /// UpdateLayeredWindow 于是返回 ERROR_GEN_FAILURE(31)。
    pub fn resize(&mut self, w: i32, h: i32) {
        let (w, h) = (w.max(1), h.max(1));
        if w == self.w && h == self.h { return; }
        unsafe {
            GdipDeleteGraphics(self.g);
            GdipDisposeImage(self.bmp as *mut _);
            SelectObject(self.memdc, self.old);
            let _ = DeleteObject(self.dib);
            let _ = DeleteDC(self.memdc);
        }
        let fresh = Canvas::new(w, h, self.dpi);
        let family = fresh.family;
        self.dib = fresh.dib;
        self.memdc = fresh.memdc;
        self.old = fresh.old;
        self.bits = fresh.bits;
        self.bmp = fresh.bmp;
        self.g = fresh.g;
        self.w = w;
        self.h = h;
        std::mem::forget(fresh);
        // 新画布的字体族不要（继续用已缓存的旧家族，字体缓存仍然有效）。
        unsafe { if !family.is_null() { GdipDeleteFontFamily(family); } }
    }

    pub fn clear(&mut self, argb: u32) {
        unsafe { GdipGraphicsClear(self.g, argb); }
    }

    fn brush(&mut self, argb: u32) -> *mut GpSolidFill {
        if let Some(&b) = self.brushes.get(&argb) { return b; }
        let mut b: *mut GpSolidFill = std::ptr::null_mut();
        unsafe { GdipCreateSolidFill(argb, &mut b); }
        // 创建失败（内存不足）时不入缓存，返回空指针让调用方画不出来即可
        if !b.is_null() { self.brushes.insert(argb, b); }
        b
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        let b = self.brush(argb);
        unsafe {
            GdipFillRectangle(self.g, b as *mut _, x, y, w, h);
        }
    }

    pub fn fill_round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        let r = r.min(w / 2.0).min(h / 2.0);
        unsafe {
            let path = round_path(x, y, w, h, r);
            let b = self.brush(argb);
            GdipFillPath(self.g, b as *mut _, path);
            GdipDeletePath(path);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        let r = r.min(w / 2.0).min(h / 2.0);
        unsafe {
            let path = round_path(x, y, w, h, r);
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1(argb, width, UnitPixel, &mut pen);
            GdipDrawPath(self.g, pen, path);
            GdipDeletePen(pen);
            GdipDeletePath(path);
        }
    }

    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, argb: u32) {
        if (argb >> 24) == 0 { return; }
        unsafe {
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1(argb, width, UnitPixel, &mut pen);
            GdipDrawLine(self.g, pen, x1, y1, x2, y2);
            GdipDeletePen(pen);
        }
    }

    /// 折线（画笔轨迹用）。`pts` 少于两个点直接返回。
    pub fn polyline(&mut self, pts: &[(f32, f32)], width: f32, argb: u32) {
        if pts.len() < 2 || (argb >> 24) == 0 { return; }
        unsafe {
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1(argb, width, UnitPixel, &mut pen);
            // 圆头圆角：手绘笔迹接缝不要出现尖角
            GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapFlat);
            GdipSetPenLineJoin(pen, LineJoinRound);
            for w in pts.windows(2) {
                GdipDrawLine(self.g, pen, w[0].0, w[0].1, w[1].0, w[1].1);
            }
            GdipDeletePen(pen);
        }
    }

    /// 椭圆描边（标注用）。
    pub fn stroke_ellipse(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        unsafe {
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1(argb, width, UnitPixel, &mut pen);
            GdipDrawEllipse(self.g, pen, x, y, w, h);
            GdipDeletePen(pen);
        }
    }

    /// 线性渐变填充（美化模板的背景用）。`angle` 是角度制，0° 从左到右。
    #[allow(clippy::too_many_arguments)]
    pub fn gradient_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c1: u32, c2: u32, angle: f32) {
        if w <= 0.0 || h <= 0.0 { return; }
        unsafe {
            let rect = RectF { X: x, Y: y, Width: w, Height: h };
            let mut brush: *mut GpLineGradient = std::ptr::null_mut();
            // isAngleScalable = FALSE：角度按设备像素走，不随 rect 拉伸
            if GdipCreateLineBrushFromRectWithAngle(&rect, c1, c2, angle, BOOL(0), WrapModeTile, &mut brush).0 != 0 {
                return;
            }
            GdipFillRectangle(self.g, brush as *mut _, x, y, w, h);
            GdipDeleteBrush(brush as *mut _);
        }
    }

    /// 把另一块画布画到本画布上（可缩放）。用于美化合成与拼接预览。
    pub fn draw_canvas(&mut self, src: &Canvas, x: f32, y: f32, w: f32, h: f32) {
        unsafe {
            GdipDrawImageRectI(self.g, src.bmp as *mut GpImage, x as i32, y as i32, w as i32, h as i32);
        }
    }

    /// 圆角裁剪，配合 `draw_canvas` 用：先裁，再画，最后 `reset_clip`。
    pub fn clip_round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        if w <= 0.0 || h <= 0.0 { return; }
        let r = r.min(w / 2.0).min(h / 2.0);
        unsafe {
            let path = round_path(x, y, w, h, r);
            GdipSetClipPath(self.g, path, CombineModeReplace);
            GdipDeletePath(path);
        }
    }

    pub fn reset_clip(&mut self) {
        unsafe { GdipResetClip(self.g); }
    }

    pub fn fill_ellipse(&mut self, x: f32, y: f32, w: f32, h: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        unsafe {
            let b = self.brush(argb);
            GdipFillEllipse(self.g, b as *mut _, x, y, w, h);
        }
    }

    /// 填充多边形（面积图用）。点按顺序给出，自动闭合。
    pub fn fill_polygon(&mut self, pts: &[(f32, f32)], argb: u32) {
        if (argb >> 24) == 0 || pts.len() < 3 { return; }
        let v: Vec<PointF> = pts.iter().map(|&(x, y)| PointF { X: x, Y: y }).collect();
        let brush = self.brush(argb);
        unsafe {
            GdipFillPolygon(self.g, brush as *mut GpBrush, v.as_ptr(), v.len() as i32,
                            FillModeAlternate);
        }
    }

    /// 多边形填充 + 顶边描线，一次画完（面积图的"面 + 线"组合，省一次遍历）。
    pub fn fill_polygon_stroked(&mut self, pts: &[(f32, f32)], fill: u32, line: u32, width: f32) {
        self.fill_polygon(pts, fill);
        if (line >> 24) == 0 { return; }
        unsafe {
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1(line, width, UnitPixel, &mut pen);
            for w in pts.windows(2) {
                GdipDrawLine(self.g, pen, w[0].0, w[0].1, w[1].0, w[1].1);
            }
            GdipDeletePen(pen);
        }
    }

    fn font(&mut self, px: f32, bold: bool) -> *mut GpFont {
        let key = (px.max(1.0).round() as u16, bold);
        if let Some(&f) = self.fonts.get(&key) { return f; }
        let mut f: *mut GpFont = std::ptr::null_mut();
        unsafe {
            let style = if bold { 1 } else { 0 };
            GdipCreateFont(self.family, key.0 as f32, style, UnitPixel, &mut f);
        }
        self.fonts.insert(key, f);
        f
    }

    fn fmt(&mut self, align: TextAlign, valign: VAlign, trim: bool) -> *mut GpStringFormat {
        let key = (align as u8, valign as u8, trim);
        if let Some(&f) = self.fmts.get(&key) { return f; }
        let mut f: *mut GpStringFormat = std::ptr::null_mut();
        unsafe {
            // 0x1000 = StringFormatFlagsNoWrap
            GdipCreateStringFormat(0x1000, 0, &mut f);
            GdipSetStringFormatAlign(f, match align {
                TextAlign::Near => StringAlignmentNear,
                TextAlign::Center => StringAlignmentCenter,
                TextAlign::Far => StringAlignmentFar,
            });
            GdipSetStringFormatLineAlign(f, match valign {
                VAlign::Top => StringAlignmentNear,
                VAlign::Center => StringAlignmentCenter,
                VAlign::Bottom => StringAlignmentFar,
            });
            if trim { GdipSetStringFormatTrimming(f, StringTrimmingEllipsisCharacter); }
        }
        self.fmts.insert(key, f);
        f
    }

    /// 画一行文本。`w/h` 是文本盒（物理像素）。
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, s: &str, x: f32, y: f32, w: f32, h: f32, px: f32, argb: u32, align: TextAlign, valign: VAlign) {
        self.text_ex(s, x, y, w, h, px, argb, align, valign, false, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_ex(&mut self, s: &str, x: f32, y: f32, w: f32, h: f32, px: f32, argb: u32,
                   align: TextAlign, valign: VAlign, bold: bool, trim: bool) {
        if s.is_empty() || (argb >> 24) == 0 { return; }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let font = self.font(px, bold);
        let fmt = self.fmt(align, valign, trim);
        let b = self.brush(argb);
        let rect = RectF { X: x, Y: y, Width: w, Height: h };
        unsafe {
            GdipDrawString(self.g, PCWSTR(wide.as_ptr()), wide.len() as i32, font, &rect, fmt, b as *const _);
        }
    }

    /// 自动换行的多行文本，超出 `h` 的部分裁掉（剪贴板面板的预览用）。
    #[allow(clippy::too_many_arguments)]
    pub fn text_wrapped(&mut self, s: &str, x: f32, y: f32, w: f32, h: f32, px: f32, argb: u32,
                        _max_lines: i32) {
        if s.is_empty() || (argb >> 24) == 0 { return; }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let font = self.font(px, false);
        let b = self.brush(argb);
        unsafe {
            // 0x4000 = LineLimit：自动换行，但只在给定的矩形里排，超出裁掉
            let mut fmt: *mut GpStringFormat = std::ptr::null_mut();
            GdipCreateStringFormat(0x4000, 0, &mut fmt);
            GdipSetStringFormatAlign(fmt, StringAlignmentNear);
            GdipSetStringFormatLineAlign(fmt, StringAlignmentNear);
            GdipSetStringFormatTrimming(fmt, StringTrimmingEllipsisWord);
            let rect = RectF { X: x, Y: y, Width: w, Height: h };
            GdipDrawString(self.g, PCWSTR(wide.as_ptr()), wide.len() as i32, font, &rect, fmt, b as *const _);
            GdipDeleteStringFormat(fmt);
        }
    }

    pub fn measure(&mut self, s: &str, px: f32, bold: bool) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let font = self.font(px, bold);
        let fmt = self.fmt(TextAlign::Near, VAlign::Center, false);
        let layout = RectF { X: 0.0, Y: 0.0, Width: 100000.0, Height: 100.0 };
        let mut bbox = RectF::default();
        let mut fitted = 0i32;
        let mut lines = 0i32;
        unsafe {
            if GdipMeasureString(self.g, PCWSTR(wide.as_ptr()), wide.len() as i32, font, &layout, fmt,
                                 &mut bbox, &mut fitted, &mut lines).0 != 0 {
                return 0.0;
            }
        }
        bbox.Width
    }

    /// 按字体名 + 字重取字体（300/400/500/700 都能表达）。
    fn font_named(&mut self, px: f32, family: &str, weight: i32) -> *mut GpFont {
        let key = (px.max(1.0).round() as u16, weight.clamp(100, 900), family.to_string());
        if let Some(&f) = self.named.get(&key) {
            return f;
        }
        let mut lf = windows::Win32::Graphics::Gdi::LOGFONTW {
            lfHeight: -(key.0 as i32),
            lfWeight: key.1,
            lfCharSet: windows::Win32::Graphics::Gdi::DEFAULT_CHARSET,
            lfQuality: windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY,
            ..Default::default()
        };
        let name: Vec<u16> = family.encode_utf16().take(31).collect();
        lf.lfFaceName[..name.len()].copy_from_slice(&name);
        let mut f: *mut GpFont = std::ptr::null_mut();
        unsafe {
            // 必须给一个真实的 DC：传 NULL 时 GDI+ 可能返回空字体，
            // 而空字体去 GdipDeleteFont 会直接崩（之前就是这样 0xC0000005）。
            let hdc = GetDC(None);
            GdipCreateFontFromLogfontW(hdc, &lf, &mut f);
            ReleaseDC(None, hdc);
            if f.is_null() {
                // 系统没有这个字体就退回默认字体族；**另建一份**，不要和缓存共用指针
                GdipCreateFont(self.family, px.max(1.0), if weight >= 600 { 1 } else { 0 },
                               UnitPixel, &mut f);
            }
        }
        if f.is_null() {
            return self.font(px, weight >= 600);
        }
        self.named.insert(key, f);
        f
    }

    /// 指定字体族与字重的单行文本（水印用）。
    #[allow(clippy::too_many_arguments)]
    pub fn text_named(&mut self, s: &str, x: f32, y: f32, w: f32, h: f32, px: f32, argb: u32,
                      align: TextAlign, valign: VAlign, family: &str, weight: i32) {
        if s.is_empty() || (argb >> 24) == 0 { return; }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let font = self.font_named(px, family, weight);
        let fmt = self.fmt(align, valign, false);
        let b = self.brush(argb);
        let rect = RectF { X: x, Y: y, Width: w, Height: h };
        unsafe {
            GdipDrawString(self.g, PCWSTR(wide.as_ptr()), wide.len() as i32, font, &rect, fmt, b as *const _);
        }
    }

    pub fn measure_named(&mut self, s: &str, px: f32, family: &str, weight: i32) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let font = self.font_named(px, family, weight);
        let fmt = self.fmt(TextAlign::Near, VAlign::Center, false);
        let layout = RectF { X: 0.0, Y: 0.0, Width: 100000.0, Height: 100.0 };
        let mut bbox = RectF::default();
        let mut fitted = 0i32;
        let mut lines = 0i32;
        unsafe {
            if GdipMeasureString(self.g, PCWSTR(wide.as_ptr()), wide.len() as i32, font, &layout, fmt,
                                 &mut bbox, &mut fitted, &mut lines).0 != 0 {
                return 0.0;
            }
        }
        bbox.Width
    }

    /// 四角半径可以不同的裁剪（图片上方有标题栏时，上圆角=0、下圆角=radius）。
    #[allow(clippy::too_many_arguments)]
    pub fn clip_round4(&mut self, x: f32, y: f32, w: f32, h: f32,
                       tl: f32, tr: f32, br: f32, bl: f32) {
        if w <= 0.0 || h <= 0.0 { return; }
        let m = (w / 2.0).min(h / 2.0);
        let (tl, tr, br, bl) = (tl.min(m).max(0.0), tr.min(m).max(0.0), br.min(m).max(0.0), bl.min(m).max(0.0));
        unsafe {
            let path = round_path4(x, y, w, h, tl, tr, br, bl);
            GdipSetClipPath(self.g, path, CombineModeReplace);
            GdipDeletePath(path);
        }
    }

    /// 四角半径不同的实心填充。
    #[allow(clippy::too_many_arguments)]
    pub fn fill_round4(&mut self, x: f32, y: f32, w: f32, h: f32,
                       tl: f32, tr: f32, br: f32, bl: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        let m = (w / 2.0).min(h / 2.0);
        let (tl, tr, br, bl) = (tl.min(m).max(0.0), tr.min(m).max(0.0), br.min(m).max(0.0), bl.min(m).max(0.0));
        unsafe {
            let path = round_path4(x, y, w, h, tl, tr, br, bl);
            let b = self.brush(argb);
            GdipFillPath(self.g, b as *mut _, path);
            GdipDeletePath(path);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_round4(&mut self, x: f32, y: f32, w: f32, h: f32,
                         tl: f32, tr: f32, br: f32, bl: f32, width: f32, argb: u32) {
        if w <= 0.0 || h <= 0.0 || (argb >> 24) == 0 { return; }
        let m = (w / 2.0).min(h / 2.0);
        let (tl, tr, br, bl) = (tl.min(m).max(0.0), tr.min(m).max(0.0), br.min(m).max(0.0), bl.min(m).max(0.0));
        unsafe {
            let path = round_path4(x, y, w, h, tl, tr, br, bl);
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1(argb, width, UnitPixel, &mut pen);
            GdipDrawPath(self.g, pen, path);
            GdipDeletePen(pen);
            GdipDeletePath(path);
        }
    }

    /// 画布内容画进一个四角半径不同的矩形（截图贴进"窗口"时用）。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_canvas_rounded4(&mut self, src: &Canvas, x: f32, y: f32, w: f32, h: f32,
                                tl: f32, tr: f32, br: f32, bl: f32) {
        self.clip_round4(x, y, w, h, tl, tr, br, bl);
        self.draw_canvas(src, x, y, w, h);
        self.reset_clip();
    }

    /// 画一张 GDI+ 图（图标缓存里的指针）。
    ///
    /// `img` 是 `icon_for()` 缓存的裸句柄，生命周期由缓存管理；这里只做非空检查。
    /// 之所以不标 `unsafe`：调用方拿到的就是缓存给的指针，没有更安全的封装必要。
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn draw_image(&mut self, img: *mut GpImage, x: f32, y: f32, w: f32, h: f32) {
        if img.is_null() { return; }
        unsafe { GdipDrawImageRectI(self.g, img, x as i32, y as i32, w as i32, h as i32); }
    }

    /// 把画布送上屏（分层窗口，逐像素 alpha）。
    pub fn present(&mut self, hwnd: HWND, x: i32, y: i32) {
        unsafe {
            GdipFlush(self.g, FlushIntentionSync);
            let screen = GetDC(None);
            let size = SIZE { cx: self.w, cy: self.h };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT { x, y };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let r = UpdateLayeredWindow(hwnd, screen, Some(&dst), Some(&size), self.memdc, Some(&src),
                                        COLORREF(0), Some(&blend), ULW_ALPHA);
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                zx_core::log::info(format!("UpdateLayeredWindow ok={:?} err={:?} size={}x{} dst=({},{})",
                    r.is_ok(), windows::Win32::Foundation::GetLastError(), self.w, self.h, x, y));
            });
            ReleaseDC(None, screen);
        }
    }

    /// 非分层窗口的 WM_PAINT：直接把内存 DC 拷到窗口 DC。
    pub fn blit(&mut self, target: HDC, x: i32, y: i32) {
        unsafe {
            GdipFlush(self.g, FlushIntentionSync);
            let _ = BitBlt(target, x, y, self.w, self.h, self.memdc, 0, 0, SRCCOPY);
        }
    }

    /// 行字节数（32bpp，恒等于 `w * 4`，这里显式给出来免得调用方自己算）。
    pub fn stride(&self) -> usize { self.w as usize * 4 }

    /// 像素缓冲（32bpp，预乘 ARGB，**逐行自上而下**）。
    ///
    /// 截图模块用它做两件 GDI+ 做起来很慢的事：把抓到的整屏像素直接 memcpy 进画布，
    /// 以及按 8×8 块平均算马赛克。写下这块内存后不需要再碰 GDI+。
    pub fn pixels(&self) -> &[u8] {
        if self.bits.is_null() { return &[]; }
        unsafe { std::slice::from_raw_parts(self.bits, self.stride() * self.h as usize) }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        if self.bits.is_null() { return &mut []; }
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.stride() * self.h as usize) }
    }

    /// 画布内容编码成 PNG 字节（内存里完成，不落临时文件）。
    ///
    /// 画布是预乘 ARGB；GDI+ 的 PNG 编码器按 `PixelFormat32bppPARGB` 会写成带 alpha 的
    /// PNG，截图内容全不透明，所以结果就是普通 24/32 位 PNG。
    pub fn png_bytes(&self) -> Option<Vec<u8>> {
        use windows::Win32::System::Com::StructuredStorage::{
            CreateStreamOnHGlobal, GetHGlobalFromStream,
        };
        unsafe {
            GdipFlush(self.g, FlushIntentionSync);
            let clsid = png_encoder_clsid()?;
            let stream = CreateStreamOnHGlobal(HGLOBAL(std::ptr::null_mut()), BOOL(1)).ok()?;
            if GdipSaveImageToStream(self.bmp as *mut GpImage, &stream, &clsid, std::ptr::null()).0 != 0 {
                return None;
            }
            let hg = GetHGlobalFromStream(&stream).ok()?;
            let size = GlobalSize(hg);
            if size == 0 { return None; }
            let p = GlobalLock(hg) as *const u8;
            if p.is_null() { return None; }
            let bytes = std::slice::from_raw_parts(p, size).to_vec();
            let _ = GlobalUnlock(hg);
            Some(bytes)
        }
    }

}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            // 同一个 GpFont 指针可能同时出现在两个缓存里（回退路径），按地址去重
            let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
            for (_, f) in self.fonts.drain() {
                if seen.insert(f as usize) { GdipDeleteFont(f); }
            }
            for (_, f) in self.named.drain() {
                if seen.insert(f as usize) { GdipDeleteFont(f); }
            }
            for (_, f) in self.fmts.drain() { GdipDeleteStringFormat(f); }
            for (_, b) in self.brushes.drain() { if !b.is_null() { GdipDeleteBrush(b as *mut GpBrush); } }
            if !self.family.is_null() { GdipDeleteFontFamily(self.family); }
            GdipDeleteGraphics(self.g);
            GdipDisposeImage(self.bmp as *mut _);
            SelectObject(self.memdc, self.old);
            let _ = DeleteObject(self.dib);
            let _ = DeleteDC(self.memdc);
        }
    }
}

unsafe fn round_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> *mut GpPath {
    let mut path: *mut GpPath = std::ptr::null_mut();
    GdipCreatePath(FillModeAlternate, &mut path);
    if r <= 0.5 {
        GdipAddPathRectangle(path, x, y, w, h);
        return path;
    }
    let d = r * 2.0;
    GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
    GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
    GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
    GdipClosePathFigure(path);
    path
}

/// 四角半径各自指定的圆角路径（顺时针）。
#[allow(clippy::too_many_arguments)]
unsafe fn round_path4(x: f32, y: f32, w: f32, h: f32,
                      tl: f32, tr: f32, br: f32, bl: f32) -> *mut GpPath {
    let mut path: *mut GpPath = std::ptr::null_mut();
    GdipCreatePath(FillModeAlternate, &mut path);
    GdipAddPathLine(path, x + tl, y, x + w - tr, y);
    if tr > 0.5 {
        GdipAddPathArc(path, x + w - tr * 2.0, y, tr * 2.0, tr * 2.0, 270.0, 90.0);
    }
    GdipAddPathLine(path, x + w, y + tr, x + w, y + h - br);
    if br > 0.5 {
        GdipAddPathArc(path, x + w - br * 2.0, y + h - br * 2.0, br * 2.0, br * 2.0, 0.0, 90.0);
    }
    GdipAddPathLine(path, x + w - br, y + h, x + bl, y + h);
    if bl > 0.5 {
        GdipAddPathArc(path, x, y + h - bl * 2.0, bl * 2.0, bl * 2.0, 90.0, 90.0);
    }
    GdipAddPathLine(path, x, y + h - bl, x, y + tl);
    if tl > 0.5 {
        GdipAddPathArc(path, x, y, tl * 2.0, tl * 2.0, 180.0, 90.0);
    }
    GdipClosePathFigure(path);
    path
}

/// GDI+ 的 PNG 编码器 CLSID（系统注册表里查一次，编码器列表随 GDI+ 版本走）。
fn png_encoder_clsid() -> Option<windows::core::GUID> {
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
            while *info.MimeType.0.add(len) != 0 { len += 1; }
            let mime = String::from_utf16_lossy(std::slice::from_raw_parts(info.MimeType.0, len));
            if mime.eq_ignore_ascii_case("image/png") {
                return Some(info.Clsid);
            }
        }
        None
    }
}

/// 给窗口加圆角区域（分层窗口也可用，用来裁掉窗口四角的硬边）。
pub fn set_round_region(hwnd: HWND, w: i32, h: i32, radius: i32) {
    unsafe {
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2);
        let _ = SetWindowRgn(hwnd, rgn, true);
    }
}
