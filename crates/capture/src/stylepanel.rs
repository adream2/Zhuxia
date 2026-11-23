//! 美化面板：背景 / 边框 / 水印三个分页，左侧实时预览。
//!
//! 面板是独立分层窗口，不阻塞截图覆盖层；所有改动先写内存，回车/关闭才落盘。

use std::cell::Cell;
use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::window::create_window;
use zx_ui::Canvas;

use crate::annotate::PALETTE;
use crate::annotate::Anno;
use crate::design::{self, Icon};
use crate::prompt;
use crate::register_class_dbl;
use crate::style::{
    self, Aspect, Bg, Frame, Style, WmFont, WmPlace, WmWeight, GRADIENTS, SOLIDS, WM_COLORS,
};
use crate::target;

const CLASS: &str = "ZhuxiaCaptureStyle";
const W: i32 = 760;
const H: i32 = 560;

const PREVIEW: (i32, i32, i32, i32) = (16, 52, 352, 506);
const TABS: [(i32, i32); 3] = [(368, 56), (466, 56), (564, 56)];
const TAB_W: i32 = 90;
const TAB_H: i32 = 28;
const COL: i32 = 368;
const COL_W: i32 = 376;
const TRACK_W: f32 = 200.0;

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Bg(Bg),
    Frame(Frame),
    Aspect(usize),
    Beautify,
    WmEnable,
    WmPlace(usize),
    WmText,
    WmColorAuto,
    WmColorPreset(usize),
    WmColorCustom,
    BgCustom,
    WmFont(usize),
    WmWeight(usize),
    Tab(usize),
    Close,
    Slider(u8),
}

enum Pending {
    Text(String),
    Pick(u32),
    PickBg(u32),
}

struct Panel {
    hwnd: HWND,
    canvas: Canvas,
    thumb: Canvas,
    swatches: Vec<Canvas>,
    style: Style,
    orig: Style,
    tab: usize,
    drag: Option<(u8, f32, f32, f32, f32)>,
    cursor: (i32, i32),
    hits: Vec<(RECT, Hit)>,
    preview: Option<Canvas>,
}

thread_local! {
    static PANEL: Cell<*mut Panel> = const { Cell::new(std::ptr::null_mut()) };
}

fn with_panel<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|cell| {
        let p = cell.get();
        if p.is_null() { None } else { Some(unsafe { f(&mut *p) }) }
    })
}

pub fn is_open() -> bool {
    PANEL.with(|c| !c.get().is_null())
}

pub fn close() {
    let p = PANEL.with(|c| c.replace(std::ptr::null_mut()));
    if p.is_null() {
        return;
    }
    let panel = unsafe { Box::from_raw(p) };
    unsafe {
        let _ = DestroyWindow(panel.hwnd);
    }
}

pub fn open(doc: &Canvas, annos: &[Anno], avoid: RECT, near: POINT) {
    register();
    if is_open() {
        close();
    }

    let thumb = make_thumb(doc, annos);
    let style = style::current();
    let Some(hwnd) = create_window(
        CLASS,
        "驻匣 · 美化",
        WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        W,
        H,
    ) else {
        return;
    };

    let swatches = (0..GRADIENTS.len())
        .map(|i| style::make_swatch(Bg::Gradient(i), 58, 38))
        .collect();
    let mut panel = Box::new(Panel {
        hwnd,
        canvas: Canvas::new(W, H, 96),
        thumb,
        swatches,
        style: style.clone(),
        orig: style,
        tab: std::env::var("ZHUXIA_CAPTURE_SELFTEST_STYLEPANEL_TAB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .min(2),
        drag: None,
        cursor: (0, 0),
        hits: Vec::new(),
        preview: None,
    });

    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    let y = (near.y - 40).clamp(vy + 8, (vy + vh - H - 8).max(vy + 8));
    let (x, y) = if avoid.right + W + 16 <= vx + vw - 8 {
        (avoid.right + 16, y)
    } else if avoid.left - W - 16 >= vx + 8 {
        (avoid.left - W - 16, y)
    } else {
        ((vx + vw - W - 8).max(vx + 8), vy + 8)
    };

    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, W, H, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
    }
    panel.draw();
    let ptr = Box::into_raw(panel);
    PANEL.with(|c| c.set(ptr));
}

fn make_thumb(doc: &Canvas, annos: &[Anno]) -> Canvas {
    let k = (480.0 / doc.w as f32).min(480.0 / doc.h as f32).min(1.0);
    let w = ((doc.w as f32 * k) as i32).max(8);
    let h = ((doc.h as f32 * k) as i32).max(8);
    let mut out = Canvas::new(w, h, 96);
    out.draw_canvas(doc, 0.0, 0.0, w as f32, h as f32);
    crate::annotate::draw_all(&mut out, annos, 0.0, 0.0, None, k);
    out
}

fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| register_class_dbl(CLASS, wndproc));
}

impl Panel {
    fn invalidate(&mut self) {
        self.preview = None;
        self.draw();
    }

    fn commit(&mut self) {
        style::set_mem(self.style.clone());
        self.notify();
    }

    fn notify(&self) {
        if let Some(h) = crate::overlay::hwnd() {
            unsafe {
                let _ = PostMessageW(h, crate::overlay::WM_STYLE_CHANGED, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn revert(&mut self) {
        self.style = self.orig.clone();
        style::set_mem(self.style.clone());
        self.notify();
    }

    fn draw(&mut self) {
        let t = design::tokens();
        self.canvas.clear(0x00000000);
        design::solid_panel(&mut self.canvas, 0.0, 0.0, W as f32, H as f32, 12.0, t.panel, &t, 1.0);

        design::text(
            &mut self.canvas,
            "美化",
            16.0,
            10.0,
            120.0,
            30.0,
            13.0,
            t.text,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            600,
        );
        let (state, color) = if self.style.enabled {
            ("已启用", t.accent)
        } else {
            ("已关闭", t.text3)
        };
        design::text(
            &mut self.canvas,
            state,
            54.0,
            10.0,
            120.0,
            30.0,
            11.5,
            color,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            500,
        );

        let close = RECT { left: W - 40, top: 10, right: W - 12, bottom: 38 };
        let hovered = target::hit(&close, self.cursor.0, self.cursor.1);
        if hovered {
            self.canvas.fill_round(close.left as f32, close.top as f32, 28.0, 28.0, 7.0, t.hover);
        }
        design::draw_icon(
            &mut self.canvas,
            Icon::Close,
            close.left as f32 + 14.0,
            close.top as f32 + 14.0,
            16.0,
            if hovered { t.text } else { t.text2 },
            1.0,
        );
        self.hits.clear();
        self.hits.push((close, Hit::Close));

        self.draw_preview();
        self.draw_tabs();
        match self.tab {
            0 => self.tab_background(),
            1 => self.tab_frame(),
            _ => self.tab_watermark(),
        }

        let (tx, ty) = (16.0, (H - 44) as f32);
        let sw = RECT {
            left: tx as i32,
            top: ty as i32 + 6,
            right: tx as i32 + 30,
            bottom: ty as i32 + 24,
        };
        design::switch(&mut self.canvas, tx, ty + 6.0, self.style.enabled, &t, 1.0);
        self.hits.push((sw, Hit::Beautify));
        design::text(
            &mut self.canvas,
            "启用美化",
            tx + 40.0,
            ty,
            90.0,
            30.0,
            12.0,
            t.text2,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            500,
        );
        let tip = RECT {
            left: tx as i32 + 130,
            top: ty as i32,
            right: tx as i32 + 150,
            bottom: ty as i32 + 30,
        };
        self.hits.push((tip, Hit::Beautify));
        design::text(
            &mut self.canvas,
            "回车确定 · Esc 取消 · 复制/保存自动套用",
            tx + 136.0,
            ty,
            420.0,
            30.0,
            11.0,
            t.text3,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            400,
        );

        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        self.canvas.present(self.hwnd, rect.left, rect.top);
    }

    fn draw_preview(&mut self) {
        let t = design::tokens();
        let (px, py, pw, ph) = PREVIEW;
        let (cw, ch) = ((pw - px) as f32, (ph - py) as f32);
        self.canvas.fill_round(px as f32, py as f32, cw, ch, 10.0, t.stage);

        let cell = 8.0f32;
        self.canvas.clip_round(px as f32, py as f32, cw, ch, 10.0);
        let mut y = 0.0;
        let mut row = 0;
        while y < ch {
            let mut x = 0.0;
            let mut col = 0;
            while x < cw {
                let color = if (row + col) % 2 == 1 { t.checker_b } else { t.checker_a };
                self.canvas.fill_rect(px as f32 + x, py as f32 + y, cell, cell, color);
                x += cell;
                col += 1;
            }
            y += cell;
            row += 1;
        }
        self.canvas.reset_clip();
        self.canvas.stroke_round(px as f32, py as f32, cw, ch, 10.0, 1.0, t.border);

        if self.preview.is_none() {
            let styled = style::render(&self.thumb, &[], &self.style, 1.0);
            let bw = (pw - px - 24) as f32;
            let bh = (ph - py - 24) as f32;
            let k = (bw / styled.w as f32).min(bh / styled.h as f32).min(1.0);
            let dw = (styled.w as f32 * k).max(1.0);
            let dh = (styled.h as f32 * k).max(1.0);
            let mut small = Canvas::new(dw as i32, dh as i32, 96);
            small.draw_canvas(&styled, 0.0, 0.0, dw, dh);
            self.preview = Some(small);
        }
        if let Some(p) = &self.preview {
            let x = px as f32 + ((pw - px) as f32 - p.w as f32) / 2.0;
            let y = py as f32 + ((ph - py) as f32 - p.h as f32) / 2.0;
            self.canvas.draw_canvas(p, x, y, p.w as f32, p.h as f32);
        }
    }

    fn draw_tabs(&mut self) {
        let t = design::tokens();
        let (fx, fy) = (TABS[0].0 as f32, TABS[0].1 as f32);
        let last = TABS[TABS.len() - 1];
        let cw = (last.0 + TAB_W) as f32 - fx + 6.0;
        design::segmented(&mut self.canvas, fx - 3.0, fy - 3.0, cw, TAB_H as f32 + 6.0, &t, 1.0);

        for (i, name) in ["背景", "边框", "水印"].iter().enumerate() {
            let (x, y) = TABS[i];
            let active = self.tab == i;
            if active {
                design::segmented_item(
                    &mut self.canvas,
                    x as f32,
                    y as f32,
                    TAB_W as f32,
                    TAB_H as f32,
                    &t,
                    1.0,
                );
            }
            let rc = RECT { left: x, top: y, right: x + TAB_W, bottom: y + TAB_H };
            let hovered = target::hit(&rc, self.cursor.0, self.cursor.1);
            let color = if active || hovered { t.text } else { t.text2 };
            design::text(
                &mut self.canvas,
                name,
                x as f32,
                y as f32,
                TAB_W as f32,
                TAB_H as f32,
                12.0,
                color,
                zx_ui::TextAlign::Center,
                zx_ui::VAlign::Center,
                if active { 600 } else { 500 },
            );
            self.hits.push((rc, Hit::Tab(i)));
        }
    }

    fn label(&mut self, text: &str, y: f32) {
        let t = design::tokens();
        design::text(
            &mut self.canvas,
            text,
            COL as f32,
            y,
            200.0,
            18.0,
            11.0,
            t.text3,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            600,
        );
    }

    fn tab_background(&mut self) {
        let t = design::tokens();
        self.label("渐变", 104.0);
        for i in 0..GRADIENTS.len() {
            let x = COL + (i as i32 % 6) * 64;
            let y = 126 + (i as i32 / 6) * 46;
            let rc = RECT { left: x, top: y, right: x + 58, bottom: y + 38 };
            if let Some(sw) = self.swatches.get(i) {
                self.canvas.draw_canvas(sw, x as f32, y as f32, 58.0, 38.0);
            }
            let selected = self.style.bg == Bg::Gradient(i);
            let hovered = target::hit(&rc, self.cursor.0, self.cursor.1);
            self.swatch_ring(x as f32, y as f32, 58.0, 38.0, 8.0, selected, hovered, &t);
            self.hits.push((rc, Hit::Bg(Bg::Gradient(i))));
        }

        self.label("纯色", 236.0);
        for (i, (_, color)) in SOLIDS.iter().enumerate() {
            let x = COL + i as i32 * 39;
            let y = 258;
            let rc = RECT { left: x, top: y, right: x + 34, bottom: y + 26 };
            self.canvas.fill_round(x as f32, y as f32, 34.0, 26.0, 6.0, *color);
            let selected = self.style.bg == Bg::Solid(i);
            let hovered = target::hit(&rc, self.cursor.0, self.cursor.1);
            self.canvas.stroke_round(x as f32, y as f32, 34.0, 26.0, 6.0, 1.0, t.border_strong);
            self.swatch_ring(x as f32, y as f32, 34.0, 26.0, 6.0, selected, hovered, &t);
            self.hits.push((rc, Hit::Bg(Bg::Solid(i))));
        }

        let custom = match self.style.bg {
            Bg::SolidCustom(c) => Some(c),
            _ => None,
        };
        let cx = COL + SOLIDS.len() as i32 * 39 + 9;
        let (xf, yf) = (cx as f32, 258.0);
        let d = 26.0;
        match custom {
            Some(c) => {
                self.canvas.fill_ellipse(xf, yf, d, d, c);
                let ring = if zx_ui::luma(c) > 0.62 { 0x33000000 } else { 0x40FFFFFF };
                self.canvas.stroke_ellipse(xf, yf, d, d, 1.0, ring);
            }
            None => {
                self.canvas.clip_round(xf, yf, d, d, d / 2.0);
                self.canvas.gradient_rect(xf, yf, d, d, 0xFFFF4D4F, 0xFF2E90FA, 45.0);
                self.canvas.reset_clip();
                self.plus(xf + d / 2.0, yf + d / 2.0);
                self.canvas.stroke_ellipse(xf, yf, d, d, 1.0, 0x40FFFFFF);
            }
        }
        let rc = RECT { left: cx - 2, top: 256, right: cx + d as i32 + 2, bottom: 286 };
        let hovered = target::hit(&rc, self.cursor.0, self.cursor.1);
        self.swatch_ring(xf, yf, d, d, d / 2.0, custom.is_some(), hovered, &t);
        self.hits.push((rc, Hit::BgCustom));

        self.label("其它", 300.0);
        for (i, (bg, label)) in
            [(Bg::Blur, "截图模糊"), (Bg::Transparent, "透明")].iter().enumerate()
        {
            let x = COL + i as i32 * 128;
            self.button(x, 322, 120, 30, label, Hit::Bg(*bg), self.style.bg == *bg);
        }
    }

    fn tab_frame(&mut self) {
        self.label("窗口样式", 104.0);
        for (i, f) in Frame::ALL.iter().enumerate() {
            let x = COL + (i % 3) as i32 * 126;
            let y = 126 + (i / 3) as i32 * 52;
            self.button(x, y, 118, 44, f.name(), Hit::Frame(*f), self.style.frame == *f);
        }

        self.slider("边距", 250.0, 0.0, 160.0, self.style.padding as f32, 0);
        self.slider("圆角", 292.0, 0.0, 36.0, self.style.radius as f32, 1);
        self.slider("阴影", 334.0, 0.0, 100.0, self.style.shadow as f32, 2);

        self.label("比例", 378.0);
        for (i, a) in Aspect::ALL.iter().enumerate() {
            let x = COL + (i % 3) as i32 * 126;
            let y = 400 + (i / 3) as i32 * 30;
            self.button(x, y, 118, 24, a.name(), Hit::Aspect(i), self.style.aspect == *a);
        }
    }

    fn tab_watermark(&mut self) {
        self.button(
            COL,
            104,
            110,
            28,
            if self.style.wm.enabled { "已启用" } else { "已关闭" },
            Hit::WmEnable,
            self.style.wm.enabled,
        );
        self.label("位置", 140.0);
        for (i, place) in [WmPlace::Margin, WmPlace::Image, WmPlace::Tile].iter().enumerate() {
            self.button(
                COL + 64 + i as i32 * 92,
                156,
                88,
                26,
                place.name(),
                Hit::WmPlace(i),
                self.style.wm.place == *place,
            );
        }

        let text = if self.style.wm.text.trim().is_empty() {
            "设置水印文字…".to_string()
        } else {
            format!("文字：{}", truncate(&self.style.wm.text, 14))
        };
        self.button(COL, 194, COL_W, 28, &text, Hit::WmText, false);

        self.label("颜色", 236.0);
        self.color_dot(COL + 44, 248, None, self.style.wm.color.is_none(), "自动", Hit::WmColorAuto);
        let preset_x = COL + 112;
        for (i, color) in WM_COLORS.iter().enumerate() {
            let x = preset_x + i as i32 * 28;
            self.color_dot(
                x,
                248,
                Some(*color),
                self.style.wm.color == Some(*color),
                "",
                Hit::WmColorPreset(i),
            );
        }
        self.custom_dot(preset_x + WM_COLORS.len() as i32 * 28 + 6, 248, self.style.wm.color);

        self.slider("透明度", 300.0, 24.0, 80.0, self.style.wm.opacity as f32, 3);

        self.label("字体", 326.0);
        for (i, f) in WmFont::ALL.iter().enumerate() {
            let x = COL + i as i32 * 62;
            self.button(x, 342, 58, 26, f.name(), Hit::WmFont(i), self.style.wm.font == *f);
        }
        self.label("字重", 382.0);
        for (i, w) in WmWeight::ALL.iter().enumerate() {
            let x = COL + i as i32 * 74;
            self.button(x, 398, 68, 26, w.name(), Hit::WmWeight(i), self.style.wm.weight == *w);
        }
        self.slider("字号%", 456.0, 60.0, 200.0, self.style.wm.size as f32, 4);
    }

    #[allow(clippy::too_many_arguments)]
    fn swatch_ring(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        selected: bool,
        hovered: bool,
        t: &design::Tokens,
    ) {
        if selected {
            self.canvas.stroke_round(x - 3.0, y - 3.0, w + 6.0, h + 6.0, r + 3.0, 1.5, t.text);
        } else if hovered {
            self.canvas.stroke_round(
                x - 3.0,
                y - 3.0,
                w + 6.0,
                h + 6.0,
                r + 3.0,
                1.5,
                t.border_strong,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, x: i32, y: i32, w: i32, h: i32, label: &str, hit: Hit, active: bool) {
        let t = design::tokens();
        let rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
        let hovered = target::hit(&rc, self.cursor.0, self.cursor.1);
        let bg = if active {
            t.active
        } else if hovered {
            t.hover
        } else {
            t.field
        };
        self.canvas.fill_round(x as f32, y as f32, w as f32, h as f32, 7.0, bg);
        if !active {
            self.canvas.stroke_round(x as f32, y as f32, w as f32, h as f32, 7.0, 1.0, t.border);
        }
        let color = if active { t.text } else { t.text2 };
        design::text(
            &mut self.canvas,
            label,
            x as f32,
            y as f32,
            w as f32,
            h as f32,
            12.0,
            color,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            if active { 600 } else { 500 },
        );
        self.hits.push((rc, hit));
    }

    fn plus(&mut self, cx: f32, cy: f32) {
        self.canvas.line(cx - 5.0, cy, cx + 5.0, cy, 1.8, 0xFFFFFFFF);
        self.canvas.line(cx, cy - 5.0, cx, cy + 5.0, 1.8, 0xFFFFFFFF);
    }

    fn custom_dot(&mut self, x: i32, y: i32, current: Option<u32>) {
        let t = design::tokens();
        let d = 22.0;
        let (xf, yf) = (x as f32, y as f32);
        self.canvas.clip_round(xf, yf, d, d, d / 2.0);
        self.canvas.gradient_rect(xf, yf, d, d, 0xFFFF4D4F, 0xFF2E90FA, 45.0);
        self.canvas.reset_clip();
        self.plus(xf + d / 2.0, yf + d / 2.0);
        self.canvas.stroke_ellipse(xf, yf, d, d, 1.0, 0x40FFFFFF);
        if let Some(c) = current {
            if !WM_COLORS.contains(&c) {
                self.canvas.stroke_ellipse(xf - 3.0, yf - 3.0, d + 6.0, d + 6.0, 1.5, t.text);
            }
        }
        self.hits.push((
            RECT { left: x - 4, top: y - 4, right: x + d as i32 + 4, bottom: y + d as i32 + 4 },
            Hit::WmColorCustom,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn color_dot(
        &mut self,
        x: i32,
        y: i32,
        color: Option<u32>,
        active: bool,
        label: &str,
        hit: Hit,
    ) {
        let t = design::tokens();
        let d = 22.0;
        let (xf, yf) = (x as f32, y as f32);
        match color {
            Some(c) => {
                self.canvas.fill_ellipse(xf, yf, d, d, c);
                let ring = if zx_ui::luma(c) > 0.62 { 0x33000000 } else { 0x40FFFFFF };
                self.canvas.stroke_ellipse(xf, yf, d, d, 1.0, ring);
            }
            None => {
                self.canvas.fill_ellipse(xf, yf, d, d, 0xFFFFFFFF);
                self.canvas.clip_round(xf, yf, d / 2.0 + 1.0, d, d / 2.0);
                self.canvas.fill_rect(xf, yf, d, d, 0xFF111827);
                self.canvas.reset_clip();
                self.canvas.stroke_ellipse(xf, yf, d, d, 1.0, 0x40FFFFFF);
            }
        }
        if active {
            self.canvas.stroke_ellipse(xf - 3.0, yf - 3.0, d + 6.0, d + 6.0, 1.5, t.text);
        }
        if !label.is_empty() {
            design::text(
                &mut self.canvas,
                label,
                xf + d + 6.0,
                yf,
                34.0,
                d,
                11.0,
                t.text3,
                zx_ui::TextAlign::Near,
                zx_ui::VAlign::Center,
                500,
            );
        }
        self.hits.push((
            RECT { left: x - 4, top: y - 4, right: x + d as i32 + 4, bottom: y + d as i32 + 4 },
            hit,
        ));
    }

    fn slider(&mut self, label: &str, y: f32, min: f32, max: f32, value: f32, id: u8) {
        let t = design::tokens();
        let x = COL as f32;
        design::text(
            &mut self.canvas,
            label,
            x,
            y - 18.0,
            TRACK_W - 40.0,
            18.0,
            12.0,
            t.text2,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            500,
        );
        design::text(
            &mut self.canvas,
            &format!("{}", value as i32),
            x,
            y - 18.0,
            TRACK_W,
            18.0,
            12.0,
            t.text3,
            zx_ui::TextAlign::Far,
            zx_ui::VAlign::Center,
            500,
        );
        let k = ((value - min) / (max - min)).clamp(0.0, 1.0);
        design::slider(&mut self.canvas, x, y, TRACK_W, k, &t, 1.0);
        self.hits.push((
            RECT {
                left: x as i32,
                top: (y - 2.0) as i32,
                right: (x + TRACK_W) as i32,
                bottom: (y + 16.0) as i32,
            },
            Hit::Slider(id),
        ));
    }

    fn set_slider(&mut self, id: u8, value: f32) {
        let v = value.round() as i32;
        match id {
            0 => self.style.padding = v,
            1 => self.style.radius = v,
            2 => self.style.shadow = v,
            3 => self.style.wm.opacity = v,
            4 => self.style.wm.size = v,
            _ => {}
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}…") } else { t }
}

fn slider_range(id: u8) -> (f32, f32) {
    match id {
        0 => (0.0, 160.0),
        1 => (0.0, 36.0),
        2 => (0.0, 100.0),
        3 => (24.0, 80.0),
        _ => (60.0, 200.0),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_panel(|p| {
                let mut r = RECT::default();
                let _ = GetWindowRect(p.hwnd, &mut r);
                p.cursor = (pt.x - r.left, pt.y - r.top);
                if let Some((id, x0, tw, min, max)) = p.drag {
                    let rel = ((p.cursor.0 as f32 - x0) / tw).clamp(0.0, 1.0);
                    p.set_slider(id, min + (max - min) * rel);
                    p.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut pending: Option<Pending> = None;
            with_panel(|p| {
                let mut r = RECT::default();
                let _ = GetWindowRect(p.hwnd, &mut r);
                let (lx, ly) = (pt.x - r.left, pt.y - r.top);
                let hit = p
                    .hits
                    .iter()
                    .find(|(rc, _)| {
                        lx >= rc.left && lx < rc.right && ly >= rc.top && ly < rc.bottom
                    })
                    .map(|(_, h)| *h);

                match hit {
                    Some(Hit::Close) => {
                        let _ = PostMessageW(p.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    Some(Hit::Tab(i)) => {
                        p.tab = i.min(2);
                        p.invalidate();
                    }
                    Some(Hit::Bg(bg)) => {
                        p.style.bg = bg;
                        p.style.enabled = true;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::Frame(f)) => {
                        p.style.frame = f;
                        p.style.enabled = true;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::Aspect(i)) => {
                        if let Some(a) = Aspect::ALL.get(i) {
                            p.style.aspect = *a;
                            p.commit();
                            p.invalidate();
                        }
                    }
                    Some(Hit::Beautify) => {
                        p.style.enabled = !p.style.enabled;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::WmEnable) => {
                        p.style.wm.enabled = !p.style.wm.enabled;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::WmPlace(i)) => {
                        p.style.wm.place = match i {
                            0 => WmPlace::Margin,
                            1 => WmPlace::Image,
                            _ => WmPlace::Tile,
                        };
                        p.style.wm.enabled = true;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::WmText) => pending = Some(Pending::Text(p.style.wm.text.clone())),
                    Some(Hit::WmColorAuto) => {
                        p.style.wm.color = None;
                        p.style.wm.enabled = true;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::WmColorPreset(k)) => {
                        p.style.wm.color = WM_COLORS.get(k).copied();
                        p.style.wm.enabled = true;
                        p.commit();
                        p.invalidate();
                    }
                    Some(Hit::WmColorCustom) => {
                        pending = Some(Pending::Pick(p.style.wm.color.unwrap_or(PALETTE[5])));
                    }
                    Some(Hit::BgCustom) => {
                        let cur = match p.style.bg {
                            Bg::SolidCustom(c) => c,
                            _ => 0xFF3F4756,
                        };
                        pending = Some(Pending::PickBg(cur));
                    }
                    Some(Hit::WmFont(i)) => {
                        if let Some(f) = WmFont::ALL.get(i) {
                            p.style.wm.font = *f;
                            p.style.wm.enabled = true;
                            p.commit();
                            p.invalidate();
                        }
                    }
                    Some(Hit::WmWeight(i)) => {
                        if let Some(x) = WmWeight::ALL.get(i) {
                            p.style.wm.weight = *x;
                            p.style.wm.enabled = true;
                            p.commit();
                            p.invalidate();
                        }
                    }
                    Some(Hit::Slider(id)) => {
                        let (min, max) = slider_range(id);
                        p.drag = Some((id, COL as f32, TRACK_W, min, max));
                        unsafe {
                            SetCapture(p.hwnd);
                        }
                        let rel = ((lx as f32 - COL as f32) / TRACK_W).clamp(0.0, 1.0);
                        p.set_slider(id, min + (max - min) * rel);
                        p.invalidate();
                    }
                    None => {
                        p.drag = None;
                        drag_window(p.hwnd, pt);
                    }
                }
            });

            match pending {
                Some(Pending::Text(current)) => {
                    let near = POINT { x: pt.x, y: pt.y + 24 };
                    if let Some(v) = prompt::text("水印文字（留空 = 不加水印）", &current, near) {
                        with_panel(|p| {
                            p.style.wm.text = v;
                            p.style.wm.enabled = true;
                            p.commit();
                            p.invalidate();
                        });
                    }
                }
                Some(Pending::PickBg(cur)) => {
                    let near = POINT { x: pt.x + 20, y: pt.y + 20 };
                    let picked = crate::picker::pick(cur, near);
                    with_panel(|p| {
                        if let Some(c) = picked {
                            p.style.bg = Bg::SolidCustom(c);
                            p.commit();
                        }
                    });
                }
                Some(Pending::Pick(cur)) => {
                    let near = POINT { x: pt.x + 20, y: pt.y + 20 };
                    let picked = crate::picker::pick(cur, near);
                    with_panel(|p| {
                        if let Some(c) = picked {
                            p.style.wm.color = Some(c);
                            p.style.wm.enabled = true;
                            p.commit();
                        }
                        p.invalidate();
                    });
                }
                None => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            if with_panel(|p| p.drag.take()).flatten().is_some() {
                style::persist();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = w.0 as u16;
            if key == VK_ESCAPE.0 {
                with_panel(|p| p.revert());
                style::persist();
                close();
            } else if key == VK_RETURN.0 {
                style::persist();
                close();
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let _ = SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CLOSE => {
            style::persist();
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn drag_window(hwnd: HWND, start: POINT) {
    unsafe {
        let _ = ReleaseCapture();
        let xy = ((start.y as isize) << 16) | (start.x as isize & 0xFFFF);
        let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(2), LPARAM(xy));
    }
}
