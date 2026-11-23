//! 截图 UI 的设计系统：主题 token、基础控件与图标。
//!
//! 只负责“画什么”，不处理点击；交互在 `overlay.rs` / `stylepanel.rs`。
//! 所有尺寸按逻辑像素 × `s`（DPI 缩放）计算。

use std::sync::OnceLock;

use zx_ui::Canvas;

/// 一套主题 token。
#[derive(Clone, Copy)]
pub struct Tokens {
    pub bg: u32,
    pub panel: u32,
    pub raised: u32,
    pub float: u32,
    pub float_border: u32,
    pub field: u32,
    pub hover: u32,
    pub active: u32,
    pub border: u32,
    pub border_strong: u32,
    pub text: u32,
    pub text2: u32,
    pub text3: u32,
    pub accent: u32,
    pub primary: u32,
    pub primary_hover: u32,
    pub primary_text: u32,
    pub danger: u32,
    pub success: u32,
    pub bubble: u32,
    pub bubble_text: u32,
    pub bubble_border: u32,
    pub stage: u32,
    pub checker_a: u32,
    pub checker_b: u32,
}

const LIGHT: Tokens = Tokens {
    bg: 0xFFF5F5F7,
    panel: 0xFFFAFAFB,
    raised: 0xFFFFFFFF,
    float: 0xE6FFFFFF,
    float_border: 0x1A000000,
    field: 0x0B000000,
    hover: 0x0D000000,
    active: 0x16000000,
    border: 0x13000000,
    border_strong: 0x1F000000,
    text: 0xFF1D1D1F,
    text2: 0x9E1D1D1F,
    text3: 0x6B1D1D1F,
    accent: 0xFF007AFF,
    primary: 0xFF1D1D1F,
    primary_hover: 0xFF000000,
    primary_text: 0xFFFFFFFF,
    danger: 0xFFFF3B30,
    success: 0xFF34C759,
    bubble: 0xF01E1E20,
    bubble_text: 0xF0FFFFFF,
    bubble_border: 0x1FFFFFFF,
    stage: 0xFFE8E8EB,
    checker_a: 0xFFF0F0F2,
    checker_b: 0xFFDCDCE0,
};

const DARK: Tokens = Tokens {
    bg: 0xFF202022,
    panel: 0xFF1C1C1E,
    raised: 0xFF2C2C2F,
    float: 0xEB2C2C2F,
    float_border: 0x1FFFFFFF,
    field: 0x0FFFFFFF,
    hover: 0x12FFFFFF,
    active: 0x1FFFFFFF,
    border: 0x13FFFFFF,
    border_strong: 0x1FFFFFFF,
    text: 0xEBFFFFFF,
    text2: 0x99FFFFFF,
    text3: 0x61FFFFFF,
    accent: 0xFF0A84FF,
    primary: 0xFFF5F5F7,
    primary_hover: 0xFFFFFFFF,
    primary_text: 0xFF1D1D1F,
    danger: 0xFFFF453A,
    success: 0xFF30D158,
    bubble: 0xF01E1E20,
    bubble_text: 0xF0FFFFFF,
    bubble_border: 0x1FFFFFFF,
    stage: 0xFF161618,
    checker_a: 0xFF2A2A2D,
    checker_b: 0xFF38383C,
};

pub fn tokens() -> Tokens {
    if zx_core::win::apps_light() { LIGHT } else { DARK }
}

pub fn family() -> &'static str {
    static F: OnceLock<&'static str> = OnceLock::new();
    F.get_or_init(|| {
        for name in ["Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI"] {
            if zx_ui::font_available(name) {
                return name;
            }
        }
        "Microsoft YaHei UI"
    })
}

#[allow(clippy::too_many_arguments)]
pub fn text(
    cv: &mut Canvas,
    s: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    px: f32,
    color: u32,
    align: zx_ui::TextAlign,
    valign: zx_ui::VAlign,
    weight: i32,
) {
    cv.text_named(s, x, y, w, h, px, color, align, valign, family(), weight);
}

pub fn measure(cv: &mut Canvas, s: &str, px: f32, weight: i32) -> f32 {
    cv.measure_named(s, px, family(), weight)
}

pub fn hairline(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32, s: f32) {
    cv.stroke_round(x, y, w, h, r, (1.0 * s).max(1.0), color);
}

/// 用多层递减透明度的圆角矩形模拟投影（GDI+ 没有模糊阴影）。
pub fn shadow(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32) {
    for layer in (1..=6).rev() {
        let grow = layer as f32 * 2.2 * s;
        let alpha = (14 - layer * 2).max(2) as u8;
        cv.fill_round(
            x - grow,
            y - grow + 4.0 * s,
            w + grow * 2.0,
            h + grow * 2.0,
            r + grow,
            (alpha as u32) << 24,
        );
    }
}

/// 盒式模糊两遍，模拟面板背后的毛玻璃。
pub fn frost(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, radius: f32, s: f32) {
    let x0 = x.max(0.0) as i32;
    let y0 = y.max(0.0) as i32;
    let x1 = ((x + w) as i32).min(cv.w);
    let y1 = ((y + h) as i32).min(cv.h);
    if x1 - x0 < 2 || y1 - y0 < 2 {
        return;
    }

    let radius = ((radius * s) as i32 / 2).max(2) as usize;
    let iw = (x1 - x0) as usize;
    let ih = (y1 - y0) as usize;
    let stride = cv.stride();
    let pixels = cv.pixels_mut();

    let mut buf: Vec<[u32; 3]> = Vec::with_capacity(iw * ih);
    for row in 0..ih {
        let start = (y0 as usize + row) * stride + x0 as usize * 4;
        for col in 0..iw {
            let o = start + col * 4;
            buf.push([pixels[o] as u32, pixels[o + 1] as u32, pixels[o + 2] as u32]);
        }
    }

    let mut tmp = buf.clone();
    for _ in 0..2 {
        for row in 0..ih {
            let base = row * iw;
            for col in 0..iw {
                let lo = col.saturating_sub(radius);
                let hi = (col + radius).min(iw - 1);
                let n = (hi - lo + 1) as u32;
                let mut acc = [0u32; 3];
                for c in lo..=hi {
                    let p = buf[base + c];
                    acc[0] += p[0];
                    acc[1] += p[1];
                    acc[2] += p[2];
                }
                tmp[base + col] = [acc[0] / n, acc[1] / n, acc[2] / n];
            }
        }
        for col in 0..iw {
            for row in 0..ih {
                let lo = row.saturating_sub(radius);
                let hi = (row + radius).min(ih - 1);
                let n = (hi - lo + 1) as u32;
                let mut acc = [0u32; 3];
                for rr in lo..=hi {
                    let p = tmp[rr * iw + col];
                    acc[0] += p[0];
                    acc[1] += p[1];
                    acc[2] += p[2];
                }
                buf[row * iw + col] = [acc[0] / n, acc[1] / n, acc[2] / n];
            }
        }
    }

    for row in 0..ih {
        let start = (y0 as usize + row) * stride + x0 as usize * 4;
        for col in 0..iw {
            let p = buf[row * iw + col];
            let o = start + col * 4;
            pixels[o] = p[0].min(255) as u8;
            pixels[o + 1] = p[1].min(255) as u8;
            pixels[o + 2] = p[2].min(255) as u8;
        }
    }
}

pub fn floating_panel(
    cv: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    t: &Tokens,
    s: f32,
) {
    shadow(cv, x, y, w, h, r, s);
    let inset = r * 0.3;
    frost(cv, x + inset, y + inset, w - inset * 2.0, h - inset * 2.0, r, s);
    cv.fill_round(x, y, w, h, r, t.float);
    hairline(cv, x + 0.5 * s, y + 0.5 * s, w - s, h - s, r, t.float_border, s);
}

pub fn solid_panel(
    cv: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    bg: u32,
    t: &Tokens,
    s: f32,
) {
    shadow(cv, x, y, w, h, r, s);
    cv.fill_round(x, y, w, h, r, bg);
    hairline(cv, x + 0.5 * s, y + 0.5 * s, w - s, h - s, r, t.border_strong, s);
}

pub fn divider(cv: &mut Canvas, x: f32, y: f32, h: f32, t: &Tokens, s: f32) {
    cv.fill_rect(x, y, (1.0 * s).max(1.0), h, t.border_strong);
}

#[allow(clippy::too_many_arguments)]
pub fn bubble(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, t: &Tokens, s: f32) {
    cv.fill_round(x, y, w, h, 7.0 * s, t.bubble);
    hairline(cv, x + 0.5 * s, y + 0.5 * s, w - s, h - s, 7.0 * s, t.bubble_border, s);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Pointer,
    Square,
    Circle,
    ArrowUpRight,
    Slash,
    Pencil,
    Highlighter,
    Pixelate,
    Type,
    Counter,
    Undo,
    Redo,
    Droplet,
    Sparkles,
    ImagePlus,
    ScanText,
    UnfoldVertical,
    Record,
    Gif,
    Copy,
    Download,
    FolderDown,
    Pin,
    Close,
    Check,
    Plus,
    Upload,
}

/// 在中心 `(cx, cy)`、边长 `size` 的方框里画图标。
pub fn draw_icon(cv: &mut Canvas, icon: Icon, cx: f32, cy: f32, size: f32, color: u32, s: f32) {
    let k = size / 24.0;
    let left = cx - size / 2.0;
    let top = cy - size / 2.0;
    let at = |gx: f32, gy: f32| (left + gx * k, top + gy * k);
    let w = (1.75 * k).max(1.0 * s);

    fn stroke(
        cv: &mut Canvas,
        a: (f32, f32),
        b: (f32, f32),
        w: f32,
        color: u32,
    ) {
        cv.line(a.0, a.1, b.0, b.1, w, color);
        let r = w / 2.0;
        cv.fill_ellipse(a.0 - r, a.1 - r, w, w, color);
        cv.fill_ellipse(b.0 - r, b.1 - r, w, w, color);
    }

    let path = |cv: &mut Canvas, pts: &[(f32, f32)]| {
        for pair in pts.windows(2) {
            stroke(cv, pair[0], pair[1], w, color);
        }
    };

    match icon {
        Icon::Pointer => {
            // 经典鼠标指针：尖端左上、底角左下、右侧缺口。
            //
            // 旧的六点版本尾巴收成一个不到 1px 的尖刺（D(11,19)→E(8,12.5) 两条边只差
            // 两三个像素），圆弧描边在这个尺寸下直接消失，16px 时看着就像「缺了一块」。
            // 这里换成四点实心轮廓，整枚图标占满 24 格，任意缩放下都能闭合。
            let pts = [at(3.5, 3.0), at(10.8, 20.2), at(13.4, 12.9), at(20.5, 10.2)];
            cv.fill_polygon(&pts, color);
            // 描边必须显式闭合：`fill_polygon_stroked` 只连 windows(2)，会漏掉最后一条边。
            for i in 0..pts.len() {
                stroke(cv, pts[i], pts[(i + 1) % pts.len()], w, color);
            }
        }
        Icon::Square => {
            let (x, y) = at(3.5, 3.5);
            cv.stroke_round(x, y, 17.0 * k, 17.0 * k, 2.0 * k, w, color);
        }
        Icon::Circle => {
            let (x, y) = at(3.5, 3.5);
            cv.stroke_ellipse(x, y, 17.0 * k, 17.0 * k, w, color);
        }
        Icon::ArrowUpRight => {
            stroke(cv, at(7.0, 17.0), at(17.0, 7.0), w, color);
            path(cv, &[at(8.0, 7.0), at(17.0, 7.0), at(17.0, 16.0)]);
        }
        Icon::Slash => stroke(cv, at(5.0, 19.0), at(19.0, 5.0), w, color),
        Icon::Pencil => {
            path(
                cv,
                &[
                    at(4.0, 20.0),
                    at(4.5, 15.5),
                    at(16.5, 3.5),
                    at(20.5, 7.5),
                    at(8.5, 19.5),
                    at(4.0, 20.0),
                ],
            );
            stroke(cv, at(14.0, 6.0), at(18.0, 10.0), w, color);
        }
        Icon::Highlighter => {
            path(
                cv,
                &[at(9.0, 11.0), at(3.5, 16.5), at(3.5, 20.5), at(7.5, 20.5), at(13.0, 14.5)],
            );
            path(
                cv,
                &[at(14.5, 5.5), at(21.0, 12.0), at(16.5, 16.5), at(10.0, 10.0), at(14.5, 5.5)],
            );
        }
        Icon::Pixelate => {
            for (gx, gy, solid) in [
                (3.0, 3.0, true),
                (13.0, 3.0, false),
                (3.0, 13.0, false),
                (13.0, 13.0, true),
            ] {
                let (x, y) = at(gx, gy);
                if solid {
                    cv.fill_round(x, y, 8.0 * k, 8.0 * k, 1.2 * k, color);
                } else {
                    cv.stroke_round(x, y, 8.0 * k, 8.0 * k, 1.2 * k, w, color);
                }
            }
        }
        Icon::Type => {
            stroke(cv, at(5.0, 7.0), at(19.0, 7.0), w, color);
            stroke(cv, at(12.0, 7.0), at(12.0, 19.0), w, color);
            stroke(cv, at(9.0, 19.0), at(15.0, 19.0), w, color);
        }
        Icon::Counter => {
            let (x, y) = at(3.5, 3.5);
            cv.stroke_ellipse(x, y, 17.0 * k, 17.0 * k, w, color);
            stroke(cv, at(10.5, 9.5), at(12.5, 8.0), w, color);
            stroke(cv, at(12.5, 8.0), at(12.5, 16.0), w, color);
        }
        Icon::Undo => {
            path(cv, &[at(9.0, 6.0), at(4.0, 11.0), at(9.0, 16.0)]);
            stroke(cv, at(4.0, 11.0), at(14.0, 11.0), w, color);
            let pts: Vec<(f32, f32)> =
                arc(15.0, 14.0, 5.0, -90.0, 90.0, 10).iter().map(|p| at(p.0, p.1)).collect();
            path(cv, &pts);
        }
        Icon::Redo => {
            path(cv, &[at(15.0, 6.0), at(20.0, 11.0), at(15.0, 16.0)]);
            stroke(cv, at(20.0, 11.0), at(10.0, 11.0), w, color);
            let pts: Vec<(f32, f32)> =
                arc(9.0, 14.0, 5.0, 90.0, 270.0, 10).iter().map(|p| at(p.0, p.1)).collect();
            path(cv, &pts);
        }
        Icon::Droplet => {
            let (x, y) = at(3.5, 3.5);
            cv.stroke_ellipse(x, y, 17.0 * k, 17.0 * k, w, color);
            let (cx2, cy2) = at(12.0, 12.0);
            cv.fill_ellipse(cx2 - 2.0 * k, cy2 - 2.0 * k, 4.0 * k, 4.0 * k, color);
            for (gx, gy) in [(12.0, 4.8), (19.2, 12.0), (12.0, 19.2), (4.8, 12.0)] {
                let (x, y) = at(gx, gy);
                cv.fill_ellipse(x - 1.3 * k, y - 1.3 * k, 2.6 * k, 2.6 * k, color);
            }
        }
        Icon::Sparkles => {
            for (gx, gy, r) in [(10.0, 10.0, 6.0), (18.0, 18.0, 3.2)] {
                let pts = [
                    at(gx, gy - r),
                    at(gx + r * 0.3, gy - r * 0.3),
                    at(gx + r, gy),
                    at(gx + r * 0.3, gy + r * 0.3),
                    at(gx, gy + r),
                    at(gx - r * 0.3, gy + r * 0.3),
                    at(gx - r, gy),
                    at(gx - r * 0.3, gy - r * 0.3),
                    at(gx, gy - r),
                ];
                path(cv, &pts);
            }
        }
        Icon::ImagePlus => {
            let (x, y) = at(3.5, 3.5);
            cv.stroke_round(x, y, 17.0 * k, 17.0 * k, 2.5 * k, w, color);
            let (sx, sy) = at(7.5, 7.5);
            cv.stroke_ellipse(sx, sy, 3.2 * k, 3.2 * k, w, color);
            path(cv, &[at(4.5, 17.5), at(9.5, 11.5), at(13.5, 15.5)]);
            path(cv, &[at(15.5, 13.5), at(17.5, 11.5)]);
        }
        Icon::ScanText => {
            path(cv, &[at(3.5, 8.0), at(3.5, 3.5), at(8.0, 3.5)]);
            path(cv, &[at(16.0, 3.5), at(20.5, 3.5), at(20.5, 8.0)]);
            path(cv, &[at(20.5, 16.0), at(20.5, 20.5), at(16.0, 20.5)]);
            path(cv, &[at(8.0, 20.5), at(3.5, 20.5), at(3.5, 16.0)]);
            stroke(cv, at(7.0, 9.0), at(17.0, 9.0), w, color);
            stroke(cv, at(7.0, 12.0), at(17.0, 12.0), w, color);
            stroke(cv, at(7.0, 15.0), at(13.0, 15.0), w, color);
        }
        Icon::UnfoldVertical => {
            path(cv, &[at(8.0, 9.0), at(12.0, 5.0), at(16.0, 9.0)]);
            path(cv, &[at(8.0, 15.0), at(12.0, 19.0), at(16.0, 15.0)]);
            stroke(cv, at(12.0, 9.5), at(12.0, 11.0), w, color);
            stroke(cv, at(12.0, 13.0), at(12.0, 14.5), w, color);
        }
        Icon::Record => {
            let (x, y) = at(3.5, 3.5);
            cv.stroke_ellipse(x, y, 17.0 * k, 17.0 * k, w, color);
            let (x, y) = at(8.5, 8.5);
            cv.fill_ellipse(x, y, 7.0 * k, 7.0 * k, color);
        }
        Icon::Gif => {
            let (x, y) = at(2.5, 4.5);
            cv.stroke_round(x, y, 19.0 * k, 15.0 * k, 3.5 * k, w, color);
            text(
                cv,
                "GIF",
                x,
                y,
                19.0 * k,
                15.0 * k,
                (size * 0.44).max(6.0),
                color,
                zx_ui::TextAlign::Center,
                zx_ui::VAlign::Center,
                700,
            );
        }
        Icon::Copy => {
            let (x, y) = at(8.5, 8.5);
            cv.stroke_round(x, y, 12.0 * k, 12.0 * k, 2.4 * k, w, color);
            path(cv, &[at(5.5, 15.5), at(3.5, 15.5), at(3.5, 3.5), at(15.5, 3.5), at(15.5, 5.5)]);
        }
        Icon::Download => {
            stroke(cv, at(12.0, 3.5), at(12.0, 15.0), w, color);
            path(cv, &[at(7.5, 10.5), at(12.0, 15.0), at(16.5, 10.5)]);
            path(cv, &[at(3.5, 17.0), at(3.5, 20.5), at(20.5, 20.5), at(20.5, 17.0)]);
        }
        Icon::FolderDown => {
            path(
                cv,
                &[
                    at(3.5, 6.5),
                    at(9.5, 6.5),
                    at(11.5, 9.0),
                    at(20.5, 9.0),
                    at(20.5, 19.0),
                    at(3.5, 19.0),
                    at(3.5, 6.5),
                ],
            );
            stroke(cv, at(12.0, 10.5), at(12.0, 15.5), w, color);
            path(cv, &[at(9.8, 13.3), at(12.0, 15.5), at(14.2, 13.3)]);
        }
        Icon::Pin => {
            stroke(cv, at(12.0, 17.0), at(12.0, 21.5), w, color);
            path(
                cv,
                &[
                    at(7.5, 3.5),
                    at(16.5, 3.5),
                    at(15.0, 8.0),
                    at(18.0, 12.0),
                    at(6.0, 12.0),
                    at(9.0, 8.0),
                    at(7.5, 3.5),
                ],
            );
        }
        Icon::Close => {
            stroke(cv, at(6.0, 6.0), at(18.0, 18.0), w, color);
            stroke(cv, at(18.0, 6.0), at(6.0, 18.0), w, color);
        }
        Icon::Check => path(cv, &[at(5.0, 12.5), at(10.0, 17.5), at(19.0, 6.5)]),
        Icon::Plus => {
            stroke(cv, at(12.0, 5.0), at(12.0, 19.0), w, color);
            stroke(cv, at(5.0, 12.0), at(19.0, 12.0), w, color);
        }
        Icon::Upload => {
            stroke(cv, at(12.0, 15.5), at(12.0, 3.5), w, color);
            path(cv, &[at(7.5, 8.0), at(12.0, 3.5), at(16.5, 8.0)]);
            path(cv, &[at(3.5, 16.5), at(3.5, 20.5), at(20.5, 20.5), at(20.5, 16.5)]);
        }
    }
}

/// 圆弧取点。
pub fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32, n: usize) -> Vec<(f32, f32)> {
    (0..=n)
        .map(|i| {
            let t = a0 + (a1 - a0) * i as f32 / n as f32;
            (cx + r * t.to_radians().cos(), cy + r * t.to_radians().sin())
        })
        .collect()
}

pub fn segmented(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, t: &Tokens, s: f32) {
    cv.fill_round(x, y, w, h, 9.0 * s, t.field);
    hairline(cv, x + 0.5 * s, y + 0.5 * s, w - s, h - s, 9.0 * s, t.border, s);
}

pub fn segmented_item(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, t: &Tokens, s: f32) {
    cv.fill_round(x, y, w, h, 7.0 * s, t.raised);
    hairline(cv, x + 0.5 * s, y + 0.5 * s, w - s, h - s, 7.0 * s, t.border_strong, s);
}

pub fn switch(cv: &mut Canvas, x: f32, y: f32, on: bool, t: &Tokens, s: f32) {
    let (w, h) = (30.0 * s, 18.0 * s);
    cv.fill_round(x, y, w, h, h / 2.0, if on { t.accent } else { 0x3D787880 });
    let knob = 14.0 * s;
    let kx = x + if on { w - knob - 2.0 * s } else { 2.0 * s };
    cv.fill_ellipse(kx, y + 2.0 * s, knob, knob, 0xFFFFFFFF);
}

pub fn slider(cv: &mut Canvas, x: f32, y: f32, w: f32, fill: f32, t: &Tokens, s: f32) {
    let track_h = 4.0 * s;
    let ty = y + (14.0 * s - track_h) / 2.0;
    cv.fill_round(x, ty, w, track_h, track_h / 2.0, t.track_color());
    let fw = (w * fill).clamp(0.0, w);
    if fw > 0.0 {
        cv.fill_round(x, ty, fw.max(track_h), track_h, track_h / 2.0, t.text);
    }
    let knob = 14.0 * s;
    let kx = (x + fw - knob / 2.0).clamp(x - knob / 2.0, x + w - knob / 2.0);
    cv.fill_ellipse(kx, y, knob, knob, 0xFFFFFFFF);
    cv.stroke_ellipse(kx, y, knob, knob, (1.0 * s).max(1.0), 0x2E000000);
}

impl Tokens {
    fn track_color(&self) -> u32 {
        if (self.text & 0xFF) > 128 { 0x24FFFFFF } else { 0x1A000000 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_opaque_enough() {
        for t in [LIGHT, DARK] {
            assert!(t.float >> 24 > 0xB0);
            assert!(t.text >> 24 > 0xE0);
        }
    }

    #[test]
    fn arc_endpoints_are_on_circle() {
        let pts = arc(0.0, 0.0, 10.0, 0.0, 180.0, 4);
        assert_eq!(pts.len(), 5);
        assert!((pts[0].0 - 10.0).abs() < 0.01);
        assert!((pts[4].0 + 10.0).abs() < 0.01);
    }
}
