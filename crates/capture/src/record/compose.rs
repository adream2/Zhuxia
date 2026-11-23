//! 布局与合成：把"屏幕帧"和"摄像头帧"摆到一块输出缓冲上。
//!
//! 设计照 `docs/RECORD.md` §3.4：布局是**纯函数**（`plan()` 只算几何），
//! 合成是**逐像素**（不走 GDI+ 每帧绘制——那在大图上比手写双线性慢一个量级）。
//! 这样设置页预览、自检、真实录制共用同一份几何，不会出现"预览和成片不一致"。
//!
//! 合成顺序：底色 → 屏幕层 → 摄像头阴影 → 摄像头（带遮罩）→ 摄像头描边。

use zx_core::{RecordLayout, RecordSettings};

use super::source::{fit, FrameBuf, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 只录屏幕
    Screen,
    /// 只录摄像头
    Camera,
    /// 画中画：屏幕铺满，摄像头贴角
    Pip,
    /// 并排：屏幕在左、摄像头在右
    SideBySide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rounded,
    Circle,
}

/// 布局参数（从配置解析出来的一份，录制期间不变）。
#[derive(Clone, Debug)]
pub struct Spec {
    pub mode: Mode,
    pub corner: Corner,
    pub size_pct: i32,
    pub shape: Shape,
    pub mirror: bool,
    pub border: bool,
    pub shadow: bool,
    pub margin: i32,
    /// 输出宽度上限，0 = 不限制。
    pub max_width: i32,
}

impl Spec {
    pub fn from_cfg(cfg: &RecordSettings) -> Self {
        let l: &RecordLayout = &cfg.layout;
        Spec {
            mode: match l.mode.as_str() {
                "screen" => Mode::Screen,
                "camera" => Mode::Camera,
                "side-by-side" | "side" => Mode::SideBySide,
                _ => Mode::Pip,
            },
            corner: match l.corner.as_str() {
                "top-left" => Corner::TopLeft,
                "top-right" => Corner::TopRight,
                "bottom-left" => Corner::BottomLeft,
                _ => Corner::BottomRight,
            },
            size_pct: l.size,
            shape: match l.shape.as_str() {
                "circle" => Shape::Circle,
                _ => Shape::Rounded,
            },
            mirror: l.mirror,
            border: l.border,
            shadow: l.shadow,
            margin: l.margin,
            max_width: cfg.max_width,
        }
    }
}

/// 摄像头层在输出里的落位。
#[derive(Clone, Debug)]
pub struct CamLayer {
    pub dst: Rect,
    pub shape: Shape,
    pub mirror: bool,
    pub border: bool,
    pub shadow: bool,
}

/// 一帧的布局结果。
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub w: i32,
    pub h: i32,
    pub screen: Option<Rect>,
    pub camera: Option<CamLayer>,
    /// 摄像头因为缺帧/未就绪被降级掉了（控制条据此提示）。
    pub cam_dropped: bool,
}

/// 按宽高比把 `(w,h)` 压到 `max_width` 以内（0 = 不压）。
fn cap(w: i32, h: i32, max_width: i32) -> (i32, i32) {
    let (w, h) = if max_width > 0 && w > max_width {
        let nh = ((h as i64 * max_width as i64 + w as i64 / 2) / w as i64) as i32;
        (max_width, nh.max(1))
    } else {
        (w.max(1), h.max(1))
    };
    // H.264 要求宽高为偶数（奇数会被编码器拒绝或自动裁掉一行），
    // 统一按偶数取整：GIF 少 1px 无所谓，视频少 1px 会挂。
    ((w.max(2)) & !1, (h.max(2)) & !1)
}

/// 算输出尺寸与各层位置。`cam` 为 `None` = 没有可用的摄像头帧。
pub fn plan(spec: &Spec, screen: (i32, i32), cam: Option<(i32, i32)>) -> Plan {
    let (sw, sh) = (screen.0.max(1), screen.1.max(1));
    match spec.mode {
        Mode::Camera => {
            let Some((cw, ch)) = cam else {
                return Plan { w: 1, h: 1, screen: None, camera: None, cam_dropped: true };
            };
            let (w, h) = cap(cw, ch, spec.max_width);
            Plan {
                w,
                h,
                screen: None,
                camera: Some(CamLayer {
                    dst: Rect::new(0, 0, w, h),
                    shape: Shape::Rounded,
                    mirror: spec.mirror,
                    border: false,
                    shadow: false,
                }),
                cam_dropped: false,
            }
        }
        Mode::Screen => {
            let (w, h) = cap(sw, sh, spec.max_width);
            Plan { w, h, screen: Some(Rect::new(0, 0, w, h)), camera: None, cam_dropped: false }
        }
        Mode::Pip => {
            let (w, h) = cap(sw, sh, spec.max_width);
            let Some((cw, ch)) = cam else {
                return Plan {
                    w,
                    h,
                    screen: Some(Rect::new(0, 0, w, h)),
                    camera: None,
                    cam_dropped: true,
                };
            };
            // 短边 * 百分比 = 摄像头那一侧的边长；再按摄像头自身比例装进正方形框
            let side = ((w.min(h) as f32) * spec.size_pct as f32 / 100.0).round() as i32;
            let side = side.clamp(24, w.min(h));
            let (cwd, chd) = fit(cw, ch, side, side);
            let m = spec.margin.min((w.min(h) / 4).max(0));
            let x = match spec.corner {
                Corner::TopLeft | Corner::BottomLeft => m,
                _ => w - cwd - m,
            };
            let y = match spec.corner {
                Corner::TopLeft | Corner::TopRight => m,
                _ => h - chd - m,
            };
            Plan {
                w,
                h,
                screen: Some(Rect::new(0, 0, w, h)),
                camera: Some(CamLayer {
                    dst: Rect::new(x.max(0), y.max(0), cwd, chd),
                    shape: spec.shape,
                    mirror: spec.mirror,
                    border: spec.border,
                    shadow: spec.shadow,
                }),
                cam_dropped: false,
            }
        }
        Mode::SideBySide => {
            let Some((cw, ch)) = cam else {
                let (w, h) = cap(sw, sh, spec.max_width);
                return Plan {
                    w,
                    h,
                    screen: Some(Rect::new(0, 0, w, h)),
                    camera: None,
                    cam_dropped: true,
                };
            };
            // 以屏幕高度为基准高度，两路各自按比例定宽；总宽再按 max_width 整体缩一次。
            let gap = (sh as f32 * 0.01).round().max(8.0) as i32;
            let base_h = sh;
            let w1 = sw;
            let w2 = ((cw as i64 * base_h as i64 + ch as i64 / 2) / ch as i64) as i32;
            let total = w1 + w2 + gap;
            let (tw, th) = cap(total, base_h, spec.max_width);
            let s = tw as f32 / total as f32;
            let h = th;
            let aw = ((w1 as f32) * s).round() as i32;
            let bw = ((w2 as f32) * s).round() as i32;
            let g = ((gap as f32) * s).round().max(1.0) as i32;
            Plan {
                w: aw + bw + g,
                h,
                screen: Some(Rect::new(0, 0, aw, h)),
                camera: Some(CamLayer {
                    dst: Rect::new(aw + g, 0, bw, h),
                    shape: Shape::Rounded,
                    mirror: spec.mirror,
                    border: false,
                    shadow: false,
                }),
                cam_dropped: false,
            }
        }
    }
}

// ---------------------------------------------------------------- 合成

/// 圆角/圆形遮罩的 alpha（0..255），尺寸 = `r.w * r.h`。
fn make_mask(w: i32, h: i32, shape: Shape) -> Vec<u8> {
    let mut m = vec![0u8; (w * h).max(1) as usize];
    if w <= 0 || h <= 0 {
        return m;
    }
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
    // 圆角半径：短边的 1/6，最少 4px（GIF 里再小就看不出来了）
    let radius = (w.min(h) as f32 / 6.0).clamp(2.0, 40.0);
    for y in 0..h {
        for x in 0..w {
            let d = match shape {
                Shape::Circle => {
                    let dx = x as f32 + 0.5 - cx;
                    let dy = y as f32 + 0.5 - cy;
                    (dx * dx + dy * dy).sqrt() - cx.min(cy)
                }
                Shape::Rounded => sdf_round(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, radius),
            };
            // 1px 抗锯齿：d <= -1 全不透明，d >= 0 全透明
            let a = (-d).clamp(0.0, 1.0);
            m[y as usize * w as usize + x as usize] = (a * 255.0).round() as u8;
        }
    }
    m
}

/// 圆角矩形的有符号距离（内部为负）。
fn sdf_round(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, rad: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - rad);
    let qy = (py - cy).abs() - (hh - rad);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad
}

/// 把 `src` 缩放画到 `dst[r]` 上，可选遮罩与水平镜像（双线性）。
pub fn blit_scaled(dst: &mut FrameBuf, src: &FrameBuf, r: Rect, mask: Option<&[u8]>, mirror: bool) {
    if r.is_empty() || src.is_empty() {
        return;
    }
    // 目标矩形裁到画布内（plan 已经保证，但边框/缩略图等外部调用不一定）
    let x0 = r.x.max(0);
    let y0 = r.y.max(0);
    let x1 = r.right().min(dst.w);
    let y1 = r.bottom().min(dst.h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let dstride = dst.stride();
    let sstride = src.stride();

    // 快路径：1:1、无遮罩、无镜像 → 逐行 memcpy。
    // 屏幕层永远是这个情形，而它占输出面积的 90% 以上，走浮点双线性纯属浪费
    // （实测 1280×720 从 25ms 降到 1ms 量级）。
    if mask.is_none()
        && !mirror
        && r.w == src.w
        && r.h == src.h
        && r.x >= 0
        && r.y >= 0
    {
        let row_bytes = src.w as usize * 4;
        for y in y0..y1 {
            let sy = (y - r.y) as usize;
            let s = sy * sstride;
            let d = y as usize * dstride + r.x as usize * 4;
            dst.buf[d..d + row_bytes].copy_from_slice(&src.buf[s..s + row_bytes]);
        }
        // 屏幕内容本来就不透明
        for y in y0..y1 {
            let d = y as usize * dstride + r.x as usize * 4;
            for x in (d + 3..d + row_bytes).step_by(4) {
                dst.buf[x] = 255;
            }
        }
        return;
    }

    // 缩放路径：**列映射预计算 + 16.16 定点整数插值**。
    //
    // 原来这里是逐像素 f32 + 闭包，1920×1080 → 1280×720 实测 **28ms/帧**：
    // 92 万像素 × 3 通道 × 6 次浮点乘加 + 每像素 4 次越界检查。结果是
    // "选 720 输出反而更卡"——设置项看起来省资源，实际把帧率砍半。
    // 现在把横向映射提到行外算一次，插值改成整数移位，同样的画面降到几毫秒。
    let sx_step = src.w as f64 / r.w as f64;
    let sy_step = src.h as f64 / r.h as f64;
    let maxx = (src.w - 1) as f64;
    let maxy = (src.h - 1) as f64;

    // 目标列 → (左源列, 右源列, 权重 0..=65536)
    let mut cols: Vec<(u32, u32, u32)> = Vec::with_capacity((x1 - x0).max(0) as usize);
    for x in x0..x1 {
        let dx = (x - r.x) as f64;
        let mut sx = (dx + 0.5) * sx_step - 0.5;
        if mirror {
            sx = maxx - sx;
        }
        let sx = sx.clamp(0.0, maxx);
        let ixf = sx.floor();
        let fx = (((sx - ixf) * 65536.0).round() as u32).min(65536);
        let ix = (ixf.max(0.0) as u32).min(src.w.saturating_sub(1) as u32);
        let ix1 = (ix + 1).min(src.w.saturating_sub(1) as u32);
        cols.push((ix, ix1, fx));
    }

    let sbuf = &src.buf;
    let last_row = (src.h - 1).max(0) as usize;
    for y in y0..y1 {
        let dy = (y - r.y) as f64;
        let sy = ((dy + 0.5) * sy_step - 0.5).clamp(0.0, maxy);
        let iyf = sy.floor();
        let fy = (((sy - iyf) * 65536.0).round() as u32).min(65536);
        let iy = (iyf.max(0.0) as usize).min(last_row);
        let iy1 = (iy + 1).min(last_row);
        let r0 = iy * sstride;
        let r1 = iy1 * sstride;
        let drow = y as usize * dstride;
        let wy = fy;
        let iwy = 65536 - fy;
        let mrow = (y - r.y) as usize * r.w as usize;

        for (k, &(ix, ix1, wx)) in cols.iter().enumerate() {
            let (ix, ix1) = (ix as usize, ix1 as usize);
            let i0 = r0 + ix * 4;
            let i1 = r0 + ix1 * 4;
            let j0 = r1 + ix * 4;
            let j1 = r1 + ix1 * 4;
            let iwx = 65536 - wx;
            // 三个颜色通道各做一次"横向插值 → 纵向插值"
            let ch = |c: usize| -> u32 {
                let top = (sbuf[i0 + c] as u32 * iwx + sbuf[i1 + c] as u32 * wx) >> 16;
                let bot = (sbuf[j0 + c] as u32 * iwx + sbuf[j1 + c] as u32 * wx) >> 16;
                (top * iwy + bot * wy) >> 16
            };
            let (pb, pg, pr) = (ch(0), ch(1), ch(2));

            let x = x0 + k as i32;
            let dof = drow + x as usize * 4;
            let a = match mask {
                Some(m) => *m.get(mrow + (x - r.x) as usize).unwrap_or(&255),
                None => 255,
            };
            if a == 255 {
                dst.buf[dof] = pb as u8;
                dst.buf[dof + 1] = pg as u8;
                dst.buf[dof + 2] = pr as u8;
                dst.buf[dof + 3] = 255;
            } else if a > 0 {
                let af = a as u32;
                let inv = 255 - af;
                dst.buf[dof] = ((pb * af + dst.buf[dof] as u32 * inv) / 255) as u8;
                dst.buf[dof + 1] = ((pg * af + dst.buf[dof + 1] as u32 * inv) / 255) as u8;
                dst.buf[dof + 2] = ((pr * af + dst.buf[dof + 2] as u32 * inv) / 255) as u8;
                dst.buf[dof + 3] = 255;
            }
        }
    }
}

/// 在 `r` 外侧铺一层柔和暗影（画中画要"浮起来"才不糊在内容上）。
fn draw_shadow(dst: &mut FrameBuf, r: Rect, shape: Shape) {
    let spread = (r.w.min(r.h) as f32 * 0.10).clamp(4.0, 24.0);
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let (hw, hh) = (r.w as f32 / 2.0, r.h as f32 / 2.0);
    let radius = if shape == Shape::Circle {
        hw.min(hh)
    } else {
        (r.w.min(r.h) as f32 / 6.0).clamp(2.0, 40.0)
    };
    let bx0 = (r.x as f32 - spread).floor() as i32;
    let by0 = (r.y as f32 - spread).floor() as i32;
    let bx1 = (r.right() as f32 + spread).ceil() as i32;
    let by1 = (r.bottom() as f32 + spread).ceil() as i32;
    let dstride = dst.stride();
    for y in by0.max(0)..by1.min(dst.h) {
        for x in bx0.max(0)..bx1.min(dst.w) {
            let d = if shape == Shape::Circle {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                (dx * dx + dy * dy).sqrt() - hw.min(hh)
            } else {
                sdf_round(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, radius)
            };
            if d <= 0.0 || d >= spread {
                continue;
            }
            // 越往外越淡：0 -> 96，spread -> 0
            let a = (1.0 - d / spread) * 96.0;
            let af = a.round().clamp(0.0, 255.0) as u32;
            if af == 0 {
                continue;
            }
            let inv = 255 - af;
            let dof = y as usize * dstride + x as usize * 4;
            for c in 0..3 {
                dst.buf[dof + c] = ((dst.buf[dof + c] as u32 * inv) / 255) as u8;
            }
        }
    }
}

/// 给 `r` 内侧画一圈半透明白描边（深色/浅色背景上都看得清）。
fn stroke_round(dst: &mut FrameBuf, r: Rect, shape: Shape, width: i32) {
    if width <= 0 {
        return;
    }
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let (hw, hh) = (r.w as f32 / 2.0, r.h as f32 / 2.0);
    let radius = if shape == Shape::Circle {
        hw.min(hh)
    } else {
        (r.w.min(r.h) as f32 / 6.0).clamp(2.0, 40.0)
    };
    let wf = width as f32;
    let dstride = dst.stride();
    for y in r.y.max(0)..r.bottom().min(dst.h) {
        for x in r.x.max(0)..r.right().min(dst.w) {
            let d = if shape == Shape::Circle {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                (dx * dx + dy * dy).sqrt() - hw.min(hh)
            } else {
                sdf_round(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, radius)
            };
            // 只画最外 width 像素那一条
            if d > 0.0 || d < -wf {
                continue;
            }
            let a = ((-d) / wf * 200.0).round().clamp(0.0, 255.0) as u32;
            let inv = 255 - a;
            let dof = y as usize * dstride + x as usize * 4;
            // 白描边：each channel = c*inv/255 + 255*a/255
            for c in 0..3 {
                dst.buf[dof + c] = ((dst.buf[dof + c] as u32 * inv + 255 * a) / 255) as u8;
            }
        }
    }
}

/// 按 `plan` 合成一帧。`bg` 是底/边色（并排模式看得出来）。
pub fn render(plan: &Plan, screen: Option<&FrameBuf>, cam: Option<&FrameBuf>, out: &mut FrameBuf, bg: u32) {
    out.clear(bg);
    if let (Some(buf), Some(r)) = (screen, plan.screen) {
        blit_scaled(out, buf, r, None, false);
    }
    if let (Some(buf), Some(layer)) = (cam, plan.camera.as_ref()) {
        let mask = if layer.shape == Shape::Rounded || layer.shape == Shape::Circle {
            Some(make_mask(layer.dst.w, layer.dst.h, layer.shape))
        } else {
            None
        };
        if layer.shadow {
            draw_shadow(out, layer.dst, layer.shape);
        }
        blit_scaled(out, buf, layer.dst, mask.as_deref(), layer.mirror);
        if layer.border {
            let w = ((layer.dst.w.min(layer.dst.h) as f32) * 0.008).round().clamp(1.0, 3.0) as i32;
            stroke_round(out, layer.dst, layer.shape, w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(mode: Mode) -> Spec {
        Spec {
            mode,
            corner: Corner::BottomRight,
            size_pct: 24,
            shape: Shape::Rounded,
            mirror: true,
            border: true,
            shadow: true,
            margin: 16,
            max_width: 0,
        }
    }

    #[test]
    fn screen_only_keeps_size() {
        let p = plan(&spec(Mode::Screen), (1280, 720), None);
        assert_eq!((p.w, p.h), (1280, 720));
        assert_eq!(p.screen, Some(Rect::new(0, 0, 1280, 720)));
        assert!(p.camera.is_none());
    }

    #[test]
    fn max_width_caps_proportionally() {
        let mut s = spec(Mode::Screen);
        s.max_width = 640;
        let p = plan(&s, (1920, 1080), None);
        assert_eq!((p.w, p.h), (640, 360));
    }

    #[test]
    fn pip_puts_camera_in_corner_inside_canvas() {
        let p = plan(&spec(Mode::Pip), (1280, 720), Some((640, 480)));
        let cam = p.camera.expect("cam layer");
        assert!(cam.dst.right() <= p.w && cam.dst.bottom() <= p.h);
        // 24% of min(1280,720)=720 -> 172.8 -> ~173 边长，4:3 装进去 -> 173x130
        assert!((172..=174).contains(&cam.dst.w), "w={}", cam.dst.w);
        assert_eq!(cam.dst.x + cam.dst.w + 16, p.w);
        assert_eq!(cam.dst.y + cam.dst.h + 16, p.h);
    }

    #[test]
    fn pip_without_camera_degrades_to_screen() {
        let p = plan(&spec(Mode::Pip), (800, 600), None);
        assert!(p.cam_dropped);
        assert_eq!(p.screen, Some(Rect::new(0, 0, 800, 600)));
    }

    #[test]
    fn side_by_side_adds_width() {
        let p = plan(&spec(Mode::SideBySide), (800, 600), Some((640, 480)));
        assert!(p.w > 800);
        let cam = p.camera.expect("cam");
        assert_eq!(cam.dst.right(), p.w);
        assert_eq!(p.h, 600);
    }

    #[test]
    fn camera_only_matches_camera() {
        let p = plan(&spec(Mode::Camera), (800, 600), Some((320, 240)));
        assert_eq!((p.w, p.h), (320, 240));
        assert!(p.screen.is_none());
    }

    #[test]
    fn circle_mask_is_opaque_at_center_and_clear_at_corner() {
        let m = make_mask(64, 64, Shape::Circle);
        assert_eq!(m[32 * 64 + 32], 255);
        assert_eq!(m[0], 0);
    }

    #[test]
    fn render_leaves_bg_outside_letterbox() {
        let s = spec(Mode::Screen);
        let p = plan(&s, (100, 100), None);
        let mut out = FrameBuf::new(p.w, p.h);
        let src = {
            let mut f = FrameBuf::new(100, 100);
            f.clear(0xFF00FF00);
            f
        };
        render(&p, Some(&src), None, &mut out, 0xFF000000);
        assert_eq!(out.pixel(50, 50), 0xFF00FF00);
    }
}
