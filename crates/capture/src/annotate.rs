//! 标注模型：十种工具、纯几何、可再次编辑。
//!
//! 标注保存为数据而不是像素：每条记录保留几何、颜色与线宽，导出时按顺序重画。
//! 这样选中、移动、改色、撤销都不需要像素级反向操作。
//!
//! 坐标以选区左上角为原点，与屏幕坐标解耦；选区拖动、裁剪、贴图缩放都不会改变标注本身。

use zx_ui::Canvas;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Highlighter,
    Mosaic,
    Text,
    Counter,
}

impl Tool {
    pub const ALL: [Tool; 10] = [
        Tool::Select,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Arrow,
        Tool::Line,
        Tool::Pen,
        Tool::Highlighter,
        Tool::Mosaic,
        Tool::Text,
        Tool::Counter,
    ];

    /// 工具栏单键快捷键。
    pub fn from_key(vk: u16) -> Option<Tool> {
        Some(match vk {
            0x56 => Tool::Select,
            0x52 => Tool::Rect,
            0x4F => Tool::Ellipse,
            0x41 => Tool::Arrow,
            0x4C => Tool::Line,
            0x50 => Tool::Pen,
            0x48 => Tool::Highlighter,
            0x4D => Tool::Mosaic,
            0x54 => Tool::Text,
            0x4E => Tool::Counter,
            _ => return None,
        })
    }

    /// 切换工具时的默认线宽。
    pub fn default_width(&self) -> f32 {
        match self {
            Tool::Highlighter => 18.0,
            Tool::Pen => 3.0,
            Tool::Counter => 11.0,
            Tool::Mosaic => 0.0,
            _ => 3.0,
        }
    }
}

/// 一条标注：矩形/线段用 `a`、`b`，画笔用 `pts`。
#[derive(Clone, Debug)]
pub struct Anno {
    pub tool: Tool,
    pub a: (i32, i32),
    pub b: (i32, i32),
    pub pts: Vec<(i32, i32)>,
    pub text: String,
    pub color: u32,
    pub width: f32,
    /// 序号工具的编号。
    pub n: u32,
}

impl Anno {
    pub fn new(tool: Tool, at: (i32, i32), color: u32, n: u32) -> Anno {
        Anno {
            tool,
            a: at,
            b: at,
            pts: vec![at],
            text: String::new(),
            color,
            width: tool.default_width(),
            n,
        }
    }

    pub fn set_end(&mut self, at: (i32, i32)) {
        self.b = at;
        if matches!(self.tool, Tool::Pen | Tool::Highlighter) {
            if let Some(&last) = self.pts.last() {
                let dx = (at.0 - last.0) as f32;
                let dy = (at.1 - last.1) as f32;
                if dx * dx + dy * dy < 2.25 {
                    return;
                }
            }
            self.pts.push(at);
        }
    }

    /// 几何包围盒，不含线宽。
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let (mut x0, mut y0, mut x1, mut y1) = match self.tool {
            Tool::Pen | Tool::Highlighter => {
                if self.pts.is_empty() {
                    (0.0, 0.0, 0.0, 0.0)
                } else {
                    let mut lo = (f32::MAX, f32::MAX);
                    let mut hi = (f32::MIN, f32::MIN);
                    for p in &self.pts {
                        lo.0 = lo.0.min(p.0 as f32);
                        lo.1 = lo.1.min(p.1 as f32);
                        hi.0 = hi.0.max(p.0 as f32);
                        hi.1 = hi.1.max(p.1 as f32);
                    }
                    (lo.0, lo.1, hi.0, hi.1)
                }
            }
            Tool::Text => {
                let size = (self.width * 4.0).max(14.0);
                let w = (self.text.chars().count() as f32 * size * 0.72).max(size);
                (
                    self.a.0 as f32,
                    self.a.1 as f32,
                    self.a.0 as f32 + w,
                    self.a.1 as f32 + size * 1.4,
                )
            }
            Tool::Counter => {
                let r = self.counter_r();
                (
                    self.a.0 as f32 - r,
                    self.a.1 as f32 - r,
                    self.a.0 as f32 + r,
                    self.a.1 as f32 + r,
                )
            }
            _ => (
                self.a.0.min(self.b.0) as f32,
                self.a.1.min(self.b.1) as f32,
                self.a.0.max(self.b.0) as f32,
                self.a.1.max(self.b.1) as f32,
            ),
        };
        if x1 < x0 {
            std::mem::swap(&mut x0, &mut x1);
        }
        if y1 < y0 {
            std::mem::swap(&mut y0, &mut y1);
        }
        (x0, y0, x1, y1)
    }

    /// 序号圆半径，绘制与命中共用。
    pub fn counter_r(&self) -> f32 {
        (self.width * 2.4).max(12.0)
    }

    pub fn translate(&mut self, dx: i32, dy: i32) {
        self.a = (self.a.0 + dx, self.a.1 + dy);
        self.b = (self.b.0 + dx, self.b.1 + dy);
        for p in &mut self.pts {
            *p = (p.0 + dx, p.1 + dy);
        }
    }

    pub fn hits(&self, x: f32, y: f32) -> bool {
        let (x0, y0, x1, y1) = self.bounds();
        let pad = self.width.max(4.0) + 3.0;
        x >= x0 - pad && x <= x1 + pad && y >= y0 - pad && y <= y1 + pad
    }

    /// 拖动误触产生的小图形不保留。
    pub fn is_meaningful(&self) -> bool {
        match self.tool {
            Tool::Text => !self.text.trim().is_empty(),
            Tool::Pen | Tool::Highlighter => self.pts.len() > 1,
            Tool::Counter => true,
            _ => {
                let (x0, y0, x1, y1) = self.bounds();
                (x1 - x0) >= 3.0 || (y1 - y0) >= 3.0
            }
        }
    }
}

/// 标注调色板：高对比六色 + 黑/白。
pub const PALETTE: [u32; 8] = [
    0xFFFF4D4F,
    0xFFFFA940,
    0xFFFFD600,
    0xFF22C55E,
    0xFF2E90FA,
    0xFFA855F7,
    0xFF111111,
    0xFFFFFFFF,
];

/// 画一条标注。`ox/oy` 是图片在画布上的偏移，`scale` 是显示缩放。
pub fn draw_one(canvas: &mut Canvas, a: &Anno, ox: f32, oy: f32, scale: f32) {
    let s = scale.max(0.05);
    let map_x = |v: f32| v * s + ox;
    let map_y = |v: f32| v * s + oy;
    let map_w = |v: f32| v * s;

    match a.tool {
        Tool::Rect | Tool::Mosaic => {
            let (x0, y0, x1, y1) = a.bounds();
            if a.tool == Tool::Mosaic {
                mosaic(
                    canvas,
                    map_x(x0),
                    map_y(y0),
                    map_w(x1 - x0),
                    map_w(y1 - y0),
                    ((8.0 * s) as i32).max(3),
                );
            } else {
                canvas.stroke_round(
                    map_x(x0),
                    map_y(y0),
                    map_w(x1 - x0),
                    map_w(y1 - y0),
                    0.0,
                    map_w(a.width),
                    a.color,
                );
            }
        }
        Tool::Ellipse => {
            let (x0, y0, x1, y1) = a.bounds();
            canvas.stroke_ellipse(
                map_x(x0),
                map_y(y0),
                map_w(x1 - x0),
                map_w(y1 - y0),
                map_w(a.width),
                a.color,
            );
        }
        Tool::Line => {
            canvas.line(
                map_x(a.a.0 as f32),
                map_y(a.a.1 as f32),
                map_x(a.b.0 as f32),
                map_y(a.b.1 as f32),
                map_w(a.width),
                a.color,
            );
        }
        Tool::Arrow => {
            let (ax, ay) = (a.a.0 as f32, a.a.1 as f32);
            let (bx, by) = (a.b.0 as f32, a.b.1 as f32);
            canvas.line(map_x(ax), map_y(ay), map_x(bx), map_y(by), map_w(a.width), a.color);
            let (dx, dy) = (bx - ax, by - ay);
            let len = dx.hypot(dy);
            if len > 1.0 {
                let head = (len * 0.32).min(a.width * 4.0 + 12.0);
                let angle = dy.atan2(dx);
                for side in [0.5f32, -0.5] {
                    let hx = bx - head * (angle + side).cos();
                    let hy = by - head * (angle + side).sin();
                    canvas.line(map_x(bx), map_y(by), map_x(hx), map_y(hy), map_w(a.width), a.color);
                }
            }
        }
        Tool::Pen | Tool::Highlighter => {
            if a.pts.len() < 2 {
                return;
            }
            let width = if a.tool == Tool::Highlighter {
                a.width.max(12.0)
            } else {
                a.width
            };
            let color = if a.tool == Tool::Highlighter {
                zx_ui::with_alpha(a.color, 0x66)
            } else {
                a.color
            };
            let pts: Vec<(f32, f32)> =
                a.pts.iter().map(|p| (map_x(p.0 as f32), map_y(p.1 as f32))).collect();
            canvas.polyline(&pts, map_w(width), color);
        }
        Tool::Text => {
            let size = (a.width * 4.0).max(14.0);
            let (x0, y0, _x1, y1) = a.bounds();
            let tw = canvas.measure(&a.text, map_w(size), true).max(map_w(size)) + 12.0 * s;
            canvas.text_ex(
                &a.text,
                map_x(x0),
                map_y(y0),
                tw,
                map_w(y1 - y0),
                map_w(size),
                a.color,
                zx_ui::TextAlign::Near,
                zx_ui::VAlign::Top,
                true,
                false,
            );
        }
        Tool::Counter => {
            let r = a.counter_r();
            canvas.fill_ellipse(
                map_x(a.a.0 as f32) - map_w(r),
                map_y(a.a.1 as f32) - map_w(r),
                map_w(r * 2.0),
                map_w(r * 2.0),
                a.color,
            );
            let light = zx_ui::luma(a.color) > 0.62;
            let fg = if light { 0xFF111111 } else { 0xFFFFFFFF };
            if light {
                canvas.stroke_ellipse(
                    map_x(a.a.0 as f32) - map_w(r),
                    map_y(a.a.1 as f32) - map_w(r),
                    map_w(r * 2.0),
                    map_w(r * 2.0),
                    1.0,
                    0x40000000,
                );
            }
            canvas.text_ex(
                &a.n.to_string(),
                map_x(a.a.0 as f32) - map_w(r),
                map_y(a.a.1 as f32) - map_w(r),
                map_w(r * 2.0),
                map_w(r * 2.0),
                map_w((r * 1.35).max(10.0)),
                fg,
                zx_ui::TextAlign::Center,
                zx_ui::VAlign::Center,
                true,
                false,
            );
        }
        Tool::Select => {}
    }
}

/// 画全部标注，并给选中的一条加选框。
pub fn draw_all(
    canvas: &mut Canvas,
    annos: &[Anno],
    ox: f32,
    oy: f32,
    selected: Option<usize>,
    scale: f32,
) {
    let s = scale.max(0.05);
    for a in annos {
        draw_one(canvas, a, ox, oy, s);
    }
    let Some(a) = selected.and_then(|i| annos.get(i)) else { return };
    if a.tool == Tool::Counter {
        let r = a.counter_r() * s + 4.0;
        let cx = a.a.0 as f32 * s + ox;
        let cy = a.a.1 as f32 * s + oy;
        canvas.stroke_ellipse(cx - r, cy - r, r * 2.0, r * 2.0, 1.5, ACCENT);
        return;
    }

    let (x0, y0, x1, y1) = a.bounds();
    let pad = 4.0;
    let (bx, by) = (x0 * s + ox - pad, y0 * s + oy - pad);
    let (bw, bh) = ((x1 - x0) * s + pad * 2.0, (y1 - y0) * s + pad * 2.0);
    canvas.stroke_round(bx, by, bw, bh, 0.0, 1.5, ACCENT);
    // 控制点画大一点：尺寸调整全靠它们（和它们之间的边带），太小点不中。
    let r = 5.0;
    for ((hx, hy), _) in handles(a) {
        let (px, py) = (hx * s + ox, hy * s + oy);
        canvas.fill_ellipse(px - r, py - r, r * 2.0, r * 2.0, 0xFFFFFFFF);
        canvas.stroke_ellipse(px - r, py - r, r * 2.0, r * 2.0, 1.5, ACCENT);
    }
}

const ACCENT: u32 = 0xFF2E90FA;

/// 选中标注上的控制点。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AHdl {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    P0,
    P1,
}

/// 选中态的控制点集合。
pub fn handles(a: &Anno) -> Vec<((f32, f32), AHdl)> {
    if matches!(a.tool, Tool::Arrow | Tool::Line) {
        return vec![
            ((a.a.0 as f32, a.a.1 as f32), AHdl::P0),
            ((a.b.0 as f32, a.b.1 as f32), AHdl::P1),
        ];
    }
    if a.tool == Tool::Counter {
        return Vec::new();
    }

    let (x0, y0, x1, y1) = a.bounds();
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    vec![
        ((x0, y0), AHdl::NW),
        ((cx, y0), AHdl::N),
        ((x1, y0), AHdl::NE),
        ((x1, cy), AHdl::E),
        ((x1, y1), AHdl::SE),
        ((cx, y1), AHdl::S),
        ((x0, y1), AHdl::SW),
        ((x0, cy), AHdl::W),
    ]
}

pub fn handle_at(a: &Anno, x: f32, y: f32, tol: f32) -> Option<AHdl> {
    if let Some((_, h)) = handles(a)
        .into_iter()
        .find(|((hx, hy), _)| (hx - x).abs() <= tol && (hy - y).abs() <= tol)
    {
        return Some(h);
    }
    // 八个控制点之间的空档以前什么都不响应：用户在框线上拖、想改大小，
    // 结果要么新画一个图形，要么整块被拖走。这里把包围盒的四条边也当成控制点，
    // 只有「小图形」才跳过——那尺寸下边带会把内部全吃掉，拖动就没法移动了。
    edge_at(a, x, y, tol)
}

/// 包围盒边带的命中：贴着哪条边就返回那条边的方向。
///
/// 线段/箭头只有两个端点可拖（它们没有"宽高"）；序号是圆点，不参与缩放。
fn edge_at(a: &Anno, x: f32, y: f32, tol: f32) -> Option<AHdl> {
    if matches!(a.tool, Tool::Arrow | Tool::Line | Tool::Counter | Tool::Select) {
        return None;
    }
    let (x0, y0, x1, y1) = a.bounds();
    if x < x0 - tol || x > x1 + tol || y < y0 - tol || y > y1 + tol {
        return None;
    }
    // 图形本身太小时，边带会连中间一起盖住，移动就没了着力点。
    if x1 - x0 <= tol * 3.0 && y1 - y0 <= tol * 3.0 {
        return None;
    }
    let (dl, dr) = ((x - x0).abs(), (x1 - x).abs());
    let (dt, db) = ((y - y0).abs(), (y1 - y).abs());
    let m = dl.min(dr).min(dt).min(db);
    if m > tol {
        return None;
    }
    Some(if m == dt {
        AHdl::N
    } else if m == db {
        AHdl::S
    } else if m == dl {
        AHdl::W
    } else {
        AHdl::E
    })
}

/// 拖控制点改尺寸；`orig` 是按下时的快照，避免逐帧累乘漂移。
pub fn resize(a: &mut Anno, h: AHdl, x: f32, y: f32, orig: &Anno) {
    match h {
        AHdl::P0 => {
            a.a = (x.round() as i32, y.round() as i32);
            return;
        }
        AHdl::P1 => {
            a.b = (x.round() as i32, y.round() as i32);
            return;
        }
        _ => {}
    }

    let (ox0, oy0, ox1, oy1) = orig.bounds();
    let (mut x0, mut y0, mut x1, mut y1) = (ox0, oy0, ox1, oy1);
    match h {
        AHdl::NW => {
            x0 = x;
            y0 = y;
        }
        AHdl::N => y0 = y,
        AHdl::NE => {
            x1 = x;
            y0 = y;
        }
        AHdl::E => x1 = x,
        AHdl::SE => {
            x1 = x;
            y1 = y;
        }
        AHdl::S => y1 = y,
        AHdl::SW => {
            x0 = x;
            y1 = y;
        }
        AHdl::W => x0 = x,
        _ => {}
    }

    // 防止反向拖穿；最小 4px。
    if x1 - x0 < 4.0 {
        if matches!(h, AHdl::NW | AHdl::W | AHdl::SW) {
            x0 = x1 - 4.0;
        } else {
            x1 = x0 + 4.0;
        }
    }
    if y1 - y0 < 4.0 {
        if matches!(h, AHdl::NW | AHdl::N | AHdl::NE) {
            y0 = y1 - 4.0;
        } else {
            y1 = y0 + 4.0;
        }
    }

    match a.tool {
        Tool::Pen | Tool::Highlighter => {
            let sx = (x1 - x0) / (ox1 - ox0).max(1.0);
            let sy = (y1 - y0) / (oy1 - oy0).max(1.0);
            a.pts = orig
                .pts
                .iter()
                .map(|p| {
                    (
                        ((p.0 as f32 - ox0) * sx + x0).round() as i32,
                        ((p.1 as f32 - oy0) * sy + y0).round() as i32,
                    )
                })
                .collect();
            if let Some(first) = a.pts.first().copied() {
                a.a = first;
            }
            if let Some(last) = a.pts.last().copied() {
                a.b = last;
            }
        }
        Tool::Text => {
            if matches!(h, AHdl::NW | AHdl::NE | AHdl::SE | AHdl::SW) {
                let k = ((x1 - x0) / (ox1 - ox0).max(1.0))
                    .min((y1 - y0) / (oy1 - oy0).max(1.0));
                a.width = (orig.width * k).clamp(1.0, 48.0);
            }
            a.a = (x0.round() as i32, y0.round() as i32);
            a.b = (x1.round() as i32, y1.round() as i32);
        }
        _ => {
            a.a = (x0.round() as i32, y0.round() as i32);
            a.b = (x1.round() as i32, y1.round() as i32);
        }
    }
}

/// 下一个序号 = 现有序号最大值 + 1，撤销/删除后不会跳号。
pub fn next_counter(annos: &[Anno]) -> u32 {
    annos
        .iter()
        .filter(|a| a.tool == Tool::Counter)
        .map(|a| a.n)
        .max()
        .unwrap_or(0)
        + 1
}

/// 命中哪条标注；后画的在上层，所以倒序找。
pub fn hit(annos: &[Anno], x: f32, y: f32) -> Option<usize> {
    annos.iter().enumerate().rev().find(|(_, a)| a.hits(x, y)).map(|(i, _)| i)
}

/// 马赛克：先算完每块平均色，再统一填，避免读到已覆盖的像素。
fn mosaic(canvas: &mut Canvas, x0: f32, y0: f32, w: f32, h: f32, block: i32) {
    let (cw, ch) = (canvas.w, canvas.h);
    let bx0 = (x0.floor() as i32).clamp(0, cw);
    let by0 = (y0.floor() as i32).clamp(0, ch);
    let bx1 = ((x0 + w).ceil() as i32).clamp(0, cw);
    let by1 = ((y0 + h).ceil() as i32).clamp(0, ch);
    if bx1 <= bx0 || by1 <= by0 {
        return;
    }

    let stride = canvas.stride();
    let blocks: Vec<(i32, i32, i32, i32, u32)> = {
        let px = canvas.pixels();
        let mut out = Vec::new();
        let mut by = by0;
        while by < by1 {
            let mut bx = bx0;
            while bx < bx1 {
                let ex = (bx + block).min(bx1);
                let ey = (by + block).min(by1);
                let step_x = ((ex - bx) / 8).max(1);
                let step_y = ((ey - by) / 8).max(1);
                let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
                let mut sy = by;
                while sy < ey {
                    let mut sx = bx;
                    while sx < ex {
                        let o = sy as usize * stride + sx as usize * 4;
                        b += px[o] as u64;
                        g += px[o + 1] as u64;
                        r += px[o + 2] as u64;
                        n += 1;
                        sx += step_x;
                    }
                    sy += step_y;
                }
                if n > 0 {
                    let argb = 0xFF00_0000
                        | (((r / n) as u32) << 16)
                        | (((g / n) as u32) << 8)
                        | ((b / n) as u32);
                    out.push((bx, by, ex, ey, argb));
                }
                bx += block;
            }
            by += block;
        }
        out
    };

    for (bx, by, ex, ey, argb) in blocks {
        canvas.fill_rect(bx as f32, by as f32, (ex - bx) as f32, (ey - by) as f32, argb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_ordered() {
        let mut a = Anno::new(Tool::Rect, (50, 60), 0xFFFF0000, 0);
        a.set_end((10, 20));
        assert_eq!(a.bounds(), (10.0, 20.0, 50.0, 60.0));
    }

    #[test]
    fn tiny_shapes_are_dropped() {
        let mut a = Anno::new(Tool::Rect, (10, 10), 0xFFFF0000, 0);
        a.set_end((11, 11));
        assert!(!a.is_meaningful());
        a.set_end((40, 40));
        assert!(a.is_meaningful());
    }

    #[test]
    fn next_counter_fills_gaps() {
        assert_eq!(next_counter(&[]), 1);
        let mut a = Anno::new(Tool::Counter, (0, 0), 0xFFFFFFFF, 1);
        a.width = 11.0;
        let b = Anno::new(Tool::Counter, (0, 0), 0xFFFFFFFF, 3);
        assert_eq!(next_counter(&[a, b]), 4);
        let rect = Anno::new(Tool::Rect, (0, 0), 0xFFFFFFFF, 9);
        assert_eq!(next_counter(&[rect]), 1);
    }

    #[test]
    fn resize_drags_the_right_corner() {
        let mut a = Anno::new(Tool::Rect, (10, 10), 0xFFFF0000, 0);
        a.set_end((60, 40));
        let orig = a.clone();
        resize(&mut a, AHdl::SE, 100.0, 80.0, &orig);
        assert_eq!(a.bounds(), (10.0, 10.0, 100.0, 80.0));
        resize(&mut a, AHdl::NW, 0.0, 5.0, &orig);
        assert_eq!(a.bounds(), (0.0, 5.0, 60.0, 40.0));
    }

    #[test]
    fn resize_keeps_a_minimum_size() {
        let mut a = Anno::new(Tool::Rect, (10, 10), 0xFFFF0000, 0);
        a.set_end((60, 40));
        let orig = a.clone();
        resize(&mut a, AHdl::SE, -50.0, 40.0, &orig);
        let (x0, _, x1, _) = a.bounds();
        assert!((x1 - x0 - 4.0).abs() < 0.01, "宽度应被夹到 4，实际 {}", x1 - x0);
    }

    #[test]
    fn resize_scales_pen_points() {
        let mut a = Anno::new(Tool::Pen, (10, 10), 0xFFFF0000, 0);
        a.set_end((20, 10));
        a.set_end((30, 20));
        let orig = a.clone();
        resize(&mut a, AHdl::E, 50.0, 20.0, &orig);
        assert_eq!(a.pts.first().copied(), Some((10, 10)));
        assert_eq!(a.pts.last().copied(), Some((50, 20)));
    }

    #[test]
    fn handles_follow_the_shape_kind() {
        let mut line = Anno::new(Tool::Line, (0, 0), 0xFFFFFFFF, 0);
        line.set_end((100, 100));
        assert_eq!(handles(&line).len(), 2);
        let mut counter = Anno::new(Tool::Counter, (5, 5), 0xFFFFFFFF, 1);
        counter.width = 11.0;
        assert!(handles(&counter).is_empty());
        let mut rect = Anno::new(Tool::Rect, (0, 0), 0xFFFFFFFF, 0);
        rect.set_end((50, 50));
        assert_eq!(handles(&rect).len(), 8);
        let a = rect.clone();
        assert_eq!(handle_at(&a, 50.0, 25.0, 6.0), Some(AHdl::E));
        assert_eq!(handle_at(&a, 25.0, 25.0, 6.0), None);
    }

    #[test]
    fn text_without_content_is_dropped() {
        let a = Anno::new(Tool::Text, (10, 10), 0xFFFFFFFF, 0);
        assert!(!a.is_meaningful());
    }

    #[test]
    fn edges_of_a_selected_shape_are_draggable() {
        let mut a = Anno::new(Tool::Ellipse, (20, 20), 0xFFFFFFFF, 0);
        a.set_end((200, 140));
        // 贴着四条边（不是控制点位置）也要能抓到，否则用户拖边线改不了尺寸
        assert_eq!(handle_at(&a, 110.0, 22.0, 8.0), Some(AHdl::N));
        assert_eq!(handle_at(&a, 110.0, 138.0, 8.0), Some(AHdl::S));
        assert_eq!(handle_at(&a, 22.0, 80.0, 8.0), Some(AHdl::W));
        assert_eq!(handle_at(&a, 198.0, 80.0, 8.0), Some(AHdl::E));
        // 中间仍然是空的：那里要留给「整块拖动」
        assert_eq!(handle_at(&a, 110.0, 80.0, 8.0), None);
    }

    #[test]
    fn small_shapes_still_take_corner_handles() {
        let mut a = Anno::new(Tool::Rect, (100, 100), 0xFFFFFFFF, 0);
        a.set_end((108, 108));
        // 8px 的图形本来就没有"内部"可言，命中控制点即可（拖动仍然能改尺寸）
        assert_eq!(handle_at(&a, 104.0, 104.0, 8.0), Some(AHdl::NW));
        assert_eq!(handle_at(&a, 100.0, 100.0, 8.0), Some(AHdl::NW));
    }

    #[test]
    fn lines_do_not_get_edge_handles() {
        let mut a = Anno::new(Tool::Line, (0, 0), 0xFFFFFFFF, 0);
        a.set_end((200, 200));
        // 线段中点不该被当成「边」
        assert_eq!(handle_at(&a, 100.0, 100.0, 8.0), None);
    }

    #[test]
    fn edge_resize_changes_size_of_a_rect() {
        let mut a = Anno::new(Tool::Rect, (10, 10), 0xFFFF0000, 0);
        a.set_end((110, 60));
        let orig = a.clone();
        let h = handle_at(&a, 60.0, 60.0, 8.0).expect("底边可抓");
        resize(&mut a, h, 60.0, 120.0, &orig);
        assert_eq!(a.bounds(), (10.0, 10.0, 110.0, 120.0));
    }
}
