//! 自绘取色面板（HSV）：饱和度/明度方块 + 竖直色相条 + 最近使用色 + `#RRGGBB`。
//!
//! 为什么不用系统的 `ChooseColorW`：那是个 1990 年代的浅色对话框，往深色工具条上弹一次
//! 很扎眼；而且截图标注、水印两个地方都要用，自绘一份还能共享"最近使用"。
//!
//! 调用是**阻塞**的（内部跑一段局部消息循环），和 `prompt::text` 一个模式：
//! 选色这几秒用户只会跟这个面板交互，宿主暂停是合理的。

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::Canvas;

const CLASS: &str = "ZhuxiaCapturePicker";
const WM_PICK_DONE: u32 = WM_APP + 111;

const W: i32 = 260;
const H: i32 = 218;

/// 饱和度/明度方块
const SV: (i32, i32, i32, i32) = (12, 12, 150, 110);
/// 竖直色相条
const HUE: (i32, i32, i32, i32) = (170, 12, 16, 110);
/// 当前色预览
const PREVIEW: (i32, i32, i32, i32) = (194, 12, 54, 32);
/// 最近使用色：最多 6 个，每个 22
const RECENT_N: usize = 6;
const DOT: i32 = 22;
const DOT_GAP: i32 = 6;
const RECENT_X: i32 = 48;
const RECENT_Y: i32 = 148;
/// 底部按钮
const BTN_Y: i32 = 180;
const BTN_H: i32 = 26;

const BG: u32 = 0xF21A1A1A;
const BORDER: u32 = 0x33FFFFFF;
const DIM: u32 = 0x99F2F2F2;

/// 进程级"最近使用"，两个面板共享（截图里选过的色，水印面板里也能直接用）。
static RECENT: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
/// 最近一次确定的颜色（下次打开默认停在它上面）。
static LAST: AtomicU32 = AtomicU32::new(0xFF2E90FA);

#[derive(Clone, Copy, PartialEq)]
enum Region {
    Sv,
    Hue,
    Recent(usize),
    Ok,
    Cancel,
}

struct Picker {
    hwnd: HWND,
    canvas: Canvas,
    h: f32,
    s: f32,
    v: f32,
    /// DPI 缩放（1.0 = 96 DPI）。所有布局常量都是逻辑像素，绘制与命中都要乘/除它。
    zoom: f32,
    drag: Option<Region>,
    result: Option<u32>,
    recent: Vec<u32>,
    hover: Option<Region>,
}

thread_local! {
    static PICKER: Cell<*mut Picker> = const { Cell::new(std::ptr::null_mut()) };
}

fn with_picker<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICKER.with(|c| {
        let p = c.get();
        if p.is_null() { None } else { Some(unsafe { f(&mut *p) }) }
    })
}

pub fn register() {
    crate::register_class_dbl(CLASS, wndproc);
}

/// 打开取色面板，返回用户确定的颜色（`None` = 取消）。
pub fn pick(initial: u32, near: POINT) -> Option<u32> {
    register();
    let start = if (initial >> 24) == 0 { LAST.load(Ordering::Relaxed) } else { initial };
    let (h, s, v) = rgb_to_hsv(start);
    let recent: Vec<u32> = RECENT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .copied()
        .take(RECENT_N)
        .collect();

    let hwnd = zx_ui::window::create_window(
        CLASS, "驻匣 · 取色", WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, W, H,
    )?;

    let dpi = unsafe {
        let d = GetDpiForWindow(hwnd);
        if d == 0 { 96 } else { d }
    };
    let zoom = (dpi as f32 / 96.0).max(1.0);
    let (wpx, hpx) = (((W as f32) * zoom).round() as i32, ((H as f32) * zoom).round() as i32);
    let mut p = Box::new(Picker {
        hwnd,
        canvas: Canvas::new(wpx, hpx, dpi),
        h, s, v,
        zoom,
        drag: None,
        result: None,
        recent,
        hover: None,
    });
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let x = (near.x + 12).clamp(vx + 8, (vx + vw - wpx - 8).max(vx + 8));
    let y = (near.y + 12).clamp(vy + 8, (vy + vh - hpx - 8).max(vy + 8));
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, wpx, hpx, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
    }
    p.draw();
    let ptr = Box::into_raw(p);
    PICKER.with(|c| c.set(ptr));
    // 自检：`ZHUXIA_CAPTURE_SELFTEST_PICKER=<毫秒>` 时到点自动确定，用于截图核对
    if let Ok(ms) = std::env::var("ZHUXIA_CAPTURE_SELFTEST_PICKER") {
        let ms = ms.parse::<u32>().unwrap_or(1500).max(300);
        unsafe { let _ = SetTimer(hwnd, 1, ms, None); }
    }

    unsafe {
        // 上一次如果"确定"和定时器同时触发，队列里会多留一条 WM_PICK_DONE，
        // 新面板的第一轮取到它就会立刻返回（表现为"点了自定义没反应"）。
        let mut stale = MSG::default();
        while PeekMessageW(&mut stale, None, WM_PICK_DONE, WM_PICK_DONE, PM_REMOVE).as_bool() {}

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_PICK_DONE {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let out = with_picker(|p| p.result).flatten();
    let raw = PICKER.with(|c| c.replace(std::ptr::null_mut()));
    if !raw.is_null() {
        unsafe {
            let b = Box::from_raw(raw);
            let _ = DestroyWindow(b.hwnd);
        }
    }
    if let Some(c) = out {
        LAST.store(c, Ordering::Relaxed);
        let mut r = RECENT.lock().unwrap_or_else(|p| p.into_inner());
        r.retain(|x| *x != c);
        r.insert(0, c);
        r.truncate(RECENT_N);
    }
    out
}

impl Picker {
    fn color(&self) -> u32 { hsv_to_rgb(self.h, self.s, self.v) }

    fn set_from(&mut self, region: Region, x: i32, y: i32) {
        match region {
            Region::Sv => {
                let (sx, sy, sw, sh) = SV;
                self.s = ((x - sx) as f32 / sw as f32).clamp(0.0, 1.0);
                self.v = 1.0 - ((y - sy) as f32 / sh as f32).clamp(0.0, 1.0);
            }
            Region::Hue => {
                let (_, hy, _, hh) = HUE;
                self.h = (((y - hy) as f32 / hh as f32).clamp(0.0, 0.999) * 360.0).round();
            }
            _ => {}
        }
    }

    fn draw(&mut self) {
        let cur = self.color();
        let z = self.zoom;
        let (cw, ch) = (self.canvas.w as f32, self.canvas.h as f32);
        let c = &mut self.canvas;
        c.clear(0x00000000);
        c.fill_round(0.0, 0.0, cw, ch, 10.0 * z, BG);
        c.stroke_round(0.5 * z, 0.5 * z, cw - z, ch - z, 10.0 * z, z, BORDER);

        // ---- 饱和度 / 明度方块：纯色底 +「白→透明」横 + 「透明→黑」竖 ----
        let (sx, sy, sw, sh) = SV;
        let (sf, syf) = (sx as f32 * z, sy as f32 * z);
        let (swf, shf) = (sw as f32 * z, sh as f32 * z);
        c.fill_rect(sf, syf, swf, shf, hsv_to_rgb(self.h, 1.0, 1.0));
        c.gradient_rect(sf, syf, swf, shf, 0xFFFFFFFF, 0x00FFFFFF, 0.0);
        c.gradient_rect(sf, syf, swf, shf, 0x00000000, 0xFF000000, 90.0);
        c.stroke_round(sf - 0.5 * z, syf - 0.5 * z, swf + z, shf + z, 0.0, z, 0x40FFFFFF);

        // 指示器：白色圆环 + 内圈深色，深浅底上都看得见
        let px = sf + self.s * swf;
        let py = syf + (1.0 - self.v) * shf;
        c.stroke_ellipse(px - 6.0 * z, py - 6.0 * z, 12.0 * z, 12.0 * z, 2.0 * z, 0xFFFFFFFF);
        c.stroke_ellipse(px - 7.5 * z, py - 7.5 * z, 15.0 * z, 15.0 * z, 1.0 * z, 0x66000000);

        // ---- 色相条（按物理像素行画，缩放下也不出条纹）----
        let (hx, hy, hw, hh) = HUE;
        let (hxf, hyf) = (hx as f32 * z, hy as f32 * z);
        let (hwf, hhf) = (hw as f32 * z, hh as f32 * z);
        let steps = (hhf.round() as i32).max(1);
        for i in 0..steps {
            let hue = i as f32 / steps as f32 * 360.0;
            c.fill_rect(hxf, hyf + i as f32, hwf, 1.0, hsv_to_rgb(hue, 1.0, 1.0));
        }
        c.stroke_round(hxf - 0.5 * z, hyf - 0.5 * z, hwf + z, hhf + z, 0.0, z, 0x40FFFFFF);
        let ty = hyf + self.h / 360.0 * hhf;
        c.stroke_round(hxf - 2.0 * z, ty - 2.5 * z, hwf + 4.0 * z, 5.0 * z, 2.0 * z, 2.0 * z, 0xFFFFFFFF);
        c.stroke_round(hxf - 3.0 * z, ty - 3.5 * z, hwf + 6.0 * z, 7.0 * z, 3.0 * z, z, 0x66000000);

        // ---- 当前色 + 十六进制 ----
        let (px0, py0, pw, ph) = PREVIEW;
        let (pf, pyf) = (px0 as f32 * z, py0 as f32 * z);
        let (pwf, phf) = (pw as f32 * z, ph as f32 * z);
        c.fill_round(pf, pyf, pwf, phf, 5.0 * z, cur);
        c.stroke_round(pf, pyf, pwf, phf, 5.0 * z, z, 0x40FFFFFF);
        c.text(&hex(cur), pf, pyf + phf + 3.0 * z, pwf, 16.0 * z, 10.5 * z, DIM,
               zx_ui::TextAlign::Center, zx_ui::VAlign::Center);

        // ---- 最近使用 ----
        // 标签底边要留出和色点的间距，否则会被第一行圆点压掉一条
        c.text("最近", 12.0 * z, (RECENT_Y as f32 - 18.0) * z, 34.0 * z, 14.0 * z,
               10.5 * z, DIM, zx_ui::TextAlign::Near, zx_ui::VAlign::Center);
        let dot = DOT as f32 * z;
        let step = (DOT + DOT_GAP) as f32 * z;
        let (rx0, ry0) = (RECENT_X as f32 * z, RECENT_Y as f32 * z);
        for i in 0..RECENT_N {
            let x = rx0 + i as f32 * step;
            match self.recent.get(i) {
                Some(col) => {
                    if self.hover == Some(Region::Recent(i)) {
                        c.fill_round(x - 2.0 * z, ry0 - 2.0 * z, dot + 4.0 * z, dot + 4.0 * z,
                                     6.0 * z, 0x33FFFFFF);
                    }
                    c.fill_round(x, ry0, dot, dot, 4.0 * z, *col);
                    c.stroke_round(x, ry0, dot, dot, 4.0 * z, z, 0x40FFFFFF);
                }
                None => {
                    c.stroke_round(x + 0.5 * z, ry0 + 0.5 * z, dot - z, dot - z, 4.0 * z, z,
                                   0x22FFFFFF);
                }
            }
        }

        // ---- 按钮 ----
        for (region, x, w, label) in
            [(Region::Cancel, 12, 110, "取消"), (Region::Ok, 134, 114, "确定")]
        {
            let hot = self.hover == Some(region);
            let fill = if region == Region::Ok {
                if hot { 0xFF3B9BFB } else { 0xFF2E90FA }
            } else if hot {
                0x33FFFFFF
            } else {
                0x1AFFFFFF
            };
            let (xf, wf) = (x as f32 * z, w as f32 * z);
            let (yf, hf) = (BTN_Y as f32 * z, BTN_H as f32 * z);
            c.fill_round(xf, yf, wf, hf, 6.0 * z, fill);
            c.text(label, xf, yf, wf, hf, 12.0 * z, 0xFFFFFFFF,
                   zx_ui::TextAlign::Center, zx_ui::VAlign::Center);
        }

        let mut r = RECT::default();
        unsafe { let _ = GetWindowRect(self.hwnd, &mut r); }
        self.canvas.present(self.hwnd, r.left, r.top);
    }
}

fn inside(r: (i32, i32, i32, i32), x: i32, y: i32) -> bool {
    let (rx, ry, rw, rh) = r;
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

fn hit_region(x: i32, y: i32, recent_len: usize) -> Option<Region> {
    if inside(SV, x, y) {
        return Some(Region::Sv);
    }
    if inside(HUE, x, y) {
        return Some(Region::Hue);
    }
    if (RECENT_Y..RECENT_Y + DOT).contains(&y) {
        for i in 0..recent_len.min(RECENT_N) {
            let rx = RECENT_X + i as i32 * (DOT + DOT_GAP);
            if x >= rx && x < rx + DOT {
                return Some(Region::Recent(i));
            }
        }
    }
    if (BTN_Y..BTN_Y + BTN_H).contains(&y) {
        if (12..122).contains(&x) {
            return Some(Region::Cancel);
        }
        if (134..248).contains(&x) {
            return Some(Region::Ok);
        }
    }
    None
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_picker(|p| {
                let mut r = RECT::default();
                let _ = GetWindowRect(p.hwnd, &mut r);
                let z = p.zoom;
                let (x, y) = (((pt.x - r.left) as f32 / z) as i32,
                              ((pt.y - r.top) as f32 / z) as i32);
                if let Some(drag) = p.drag {
                    p.set_from(drag, x, y);
                    p.draw();
                    return;
                }
                let h = hit_region(x, y, p.recent.len());
                if h != p.hover {
                    p.hover = h;
                    p.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_picker(|p| {
                let mut r = RECT::default();
                let _ = GetWindowRect(p.hwnd, &mut r);
                let z = p.zoom;
                let (x, y) = (((pt.x - r.left) as f32 / z) as i32,
                              ((pt.y - r.top) as f32 / z) as i32);
                match hit_region(x, y, p.recent.len()) {
                    Some(Region::Ok) => {
                        p.result = Some(p.color());
                        let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
                    }
                    Some(Region::Cancel) => {
                        let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
                    }
                    Some(Region::Recent(i)) => {
                        if let Some(c) = p.recent.get(i).copied() {
                            let (h, s, v) = rgb_to_hsv(c);
                            p.h = h;
                            p.s = s;
                            p.v = v;
                            p.result = Some(c);
                            let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
                        }
                    }
                    Some(region @ (Region::Sv | Region::Hue)) => {
                        p.drag = Some(region);
                        p.set_from(region, x, y);
                        unsafe { SetCapture(hwnd); }
                        p.draw();
                    }
                    None => {}
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with_picker(|p| p.drag = None);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = w.0 as u16;
            if key == VK_ESCAPE.0 {
                let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
            } else if key == VK_RETURN.0 {
                with_picker(|p| {
                    if p.result.is_none() {
                        p.result = Some(p.color());
                    }
                });
                let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_TIMER => {
            unsafe { let _ = KillTimer(hwnd, 1); }
            with_picker(|p| {
                if p.result.is_none() {
                    p.result = Some(p.color());
                }
            });
            let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = PostMessageW(hwnd, WM_PICK_DONE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

// ---------------------------------------------------------------- 颜色换算

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let f = |q: f32| ((q + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    0xFF00_0000 | (f(r) << 16) | (f(g) << 8) | f(b)
}

pub fn rgb_to_hsv(color: u32) -> (f32, f32, f32) {
    let r = ((color >> 16) & 0xFF) as f32 / 255.0;
    let g = ((color >> 8) & 0xFF) as f32 / 255.0;
    let b = (color & 0xFF) as f32 / 255.0;
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let h = if d <= f32::EPSILON {
        0.0
    } else if mx == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.rem_euclid(360.0), if mx <= f32::EPSILON { 0.0 } else { d / mx }, mx)
}

pub fn hex(color: u32) -> String {
    format!("#{:02X}{:02X}{:02X}", (color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_primary_colors() {
        for (h, s, v) in [(0.0, 1.0, 1.0), (60.0, 1.0, 1.0), (120.0, 1.0, 1.0), (240.0, 1.0, 1.0)] {
            let c = hsv_to_rgb(h, s, v);
            let (h2, s2, v2) = rgb_to_hsv(c);
            assert!((h2 - h).abs() < 0.01 || (h2 - 360.0).abs() < 0.01, "{h} → {h2}");
            assert!((s2 - s).abs() < 0.01 && (v2 - v).abs() < 0.01);
        }
    }

    #[test]
    fn greys_have_no_saturation() {
        let (h, s, v) = rgb_to_hsv(0xFF808080);
        assert_eq!(h, 0.0);
        assert!(s < 0.01);
        assert!((v - 0.502).abs() < 0.01);
    }

    #[test]
    fn hex_formats_without_alpha() {
        assert_eq!(hex(0xFF2E90FA), "#2E90FA");
        assert_eq!(hex(0x00123456), "#123456");
    }
}
