//! 覆盖层：任务栏常驻条 / 悬浮卡片。
//!
//! 一个窗口类、一套布局与绘制，两种形态（任务栏嵌入的替代方案是"贴到任务栏上的
//! 顶层分层窗口"，见 docs/ARCHITECTURE.md 的取舍说明）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_MOUSELEAVE_MSG: u32 = 0x02A3;

use zx_core::fmt;
use zx_ui::painter::Canvas;
use zx_ui::theme;
use zx_ui::window::{self, WM_REPOSITION, WM_SET_METRICS, WM_SNAPSHOT};
use zx_ui::{TextAlign, VAlign};

use crate::layout::{self, compute, Layout};
use crate::snapshot::Snapshot;

pub const CLASS: &str = "ZhuxiaTaskbarOverlay";
pub const CARD_RADIUS: f32 = 8.0;
const TIMER_TICK: usize = 1;
const TIMER_EDGE: usize = 2;
const TIMER_ANIM: usize = 3;
/// 贴边隐藏的滑动帧数与帧间隔：7 帧 × 16ms ≈ 110ms，和上游的手感接近。
const ANIM_STEPS: u8 = 7;
const ANIM_MS: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Form { Taskbar, Floating }

thread_local! {
    static MAP: RefCell<HashMap<isize, *mut Overlay>> = RefCell::new(HashMap::new());
    static NEXT_FORM: std::cell::Cell<Form> = const { std::cell::Cell::new(Form::Taskbar) };
    /// 下一个要建的覆盖层该贴哪根任务栏（多屏时每屏一个覆盖层，靠它区分）。
    static NEXT_TASK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

fn slot_of(hwnd: HWND) -> Option<*mut Overlay> {
    MAP.with(|m| m.borrow().get(&(hwnd.0 as isize)).copied())
}

pub struct Overlay {
    pub hwnd: HWND,
    pub form: Form,
    pub taskbar: HWND,
    pub classical: bool,
    pub vertical: bool,
    pub canvas: Canvas,
    pub dpi: u32,
    pub phys_w: i32,
    pub phys_h: i32,
    pub logical_h: f32,
    pub avail_dip: f32,
    pub hovered: i32,
    pub tracking: bool,
    pub light_taskbar: bool,
    pub snapshot: Arc<Snapshot>,
    // 悬浮形态
    pub fx: i32,
    pub fy: i32,
    pub press: bool,
    pub dock_edge: i32,
    pub edge_hidden: bool,
    pub fullscreen_hidden: bool,
    pub grab: (i32, i32),
    // 经典任务栏
    pub band: HWND,
    pub band_rect: RECT,
    pub band_shrunk: bool,
    pub logged: bool,
    /// 贴边隐藏：吸附时的"家"位置（屏幕坐标）
    pub home: (i32, i32),
    /// 贴边隐藏的滑动动画（None = 静止）
    pub anim: Option<Anim>,
}

/// 一次滑动的插值状态：从 `from` 缓动到 `to`，`step` 是已经走完的帧数。
#[derive(Clone, Copy)]
pub struct Anim { from: (i32, i32), to: (i32, i32), step: u8 }

impl Overlay {
    fn layout(&self) -> Layout {
        let cfg = zx_core::cfg::cfg();
        let gpu_content = match cfg.gpu_content() {
            zx_core::GpuTaskbarContent::Usage => 0u8,
            zx_core::GpuTaskbarContent::Temperature => 1,
            zx_core::GpuTaskbarContent::Both => 2,
        };
        compute(cfg.mask(), gpu_content, self.vertical, self.avail_dip, cfg.compact(), &cfg.normalized_order())
    }

    fn is_floating(&self) -> bool { self.form == Form::Floating }

    fn palette(&self) -> theme::Palette {
        if self.is_floating() {
            theme::for_window(zx_core::win::apps_light())
        } else {
            theme::for_taskbar(self.light_taskbar)
        }
    }

    /// 重算尺寸并（必要时）重建画布，然后重定位。
    fn relayout(&mut self) {
        let l = self.layout();
        let dpi = self.dpi;
        let (want_w, want_h) = if self.is_floating() {
            (zx_core::win::dpi_scale_i(l.width, dpi), zx_core::win::dpi_scale_i(if l.width > 0.0 { self.logical_h.max(40.0) } else { 40.0 }, dpi))
        } else if self.vertical {
            (zx_core::win::dpi_scale_i(l.width.max(1.0), dpi), zx_core::win::dpi_scale_i(l.height.max(1.0), dpi))
        } else {
            (zx_core::win::dpi_scale_i(l.width.max(1.0), dpi), self.phys_h.max(1))
        };
        if want_w != self.phys_w || want_h != self.phys_h {
            self.phys_w = want_w.max(1);
            self.phys_h = want_h.max(1);
            self.canvas.resize(self.phys_w, self.phys_h);
            if self.is_floating() {
                self.logical_h = self.phys_h as f32 * 96.0 / dpi as f32;
            }
            if self.is_floating() {
                zx_ui::painter::set_round_region(self.hwnd, self.phys_w, self.phys_h,
                    zx_core::win::dpi_scale_i(CARD_RADIUS, dpi));
            }
        }
        self.reposition(true);
    }


    fn reposition(&mut self, force: bool) {
        if self.is_floating() { self.reposition_floating(force); return; }
        let Some(tb) = zx_core::win::rect_of(self.taskbar) else {
            // 该屏的任务栏没了（拔掉显示器 / 用户在设置里关掉那屏的任务栏）→ 收起来，
            // 别把窗口留在已经不存在的位置上。
            unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); }
            return;
        };
        let cfg = zx_core::cfg::cfg();

        // 方向翻转：宽 < 高 = 侧边任务栏
        let vertical_now = (tb.right - tb.left) < (tb.bottom - tb.top) && tb.right > tb.left && tb.bottom > tb.top;
        if vertical_now != self.vertical {
            self.vertical = vertical_now;
            self.relayout();
            return;
        }
        if self.vertical {
            let (band_w, band_x, h) = self.band_cross(&tb);
            self.avail_dip = layout::vertical_avail_width(band_w, self.dpi);
            let l = self.layout();
            let want_w = zx_core::win::dpi_scale_i(l.width.max(1.0), self.dpi);
            let want_h = zx_core::win::dpi_scale_i(l.height.max(1.0), self.dpi);
            if want_w != self.phys_w || want_h != self.phys_h {
                self.phys_w = want_w;
                self.phys_h = want_h;
                self.canvas.resize(self.phys_w, self.phys_h);
            }
            let (x, y) = self.anchor_vertical(&tb, band_w, band_x, &cfg);
            let _ = h;
            self.place(x, y, force);
            return;
        }
        // 横向：高度跟任务栏走
        let band_h = self.band_height(&tb);
        if band_h > 0 && band_h != self.phys_h {
            self.phys_h = band_h;
            let l = self.layout();
            self.phys_w = zx_core::win::dpi_scale_i(l.width.max(1.0), self.dpi);
            self.canvas.resize(self.phys_w, self.phys_h);
        }
        self.logical_h = self.phys_h as f32 * 96.0 / self.dpi as f32;
        let (x, y) = self.anchor_horizontal(&tb, &cfg);
        self.place(x, y, force);
    }

    fn place(&mut self, x: i32, y: i32, force: bool) {
        unsafe {
            let cur = zx_core::win::rect_of(self.hwnd).unwrap_or_default();
            if !force && cur.left == x && cur.top == y && (cur.right - cur.left) == self.phys_w && (cur.bottom - cur.top) == self.phys_h {
                return;
            }
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, self.phys_w, self.phys_h,
                                 SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    fn anchor_horizontal(&self, tb: &RECT, cfg: &zx_core::Settings) -> (i32, i32) {
        let spacing = zx_core::win::dpi_scale_i(2.0, self.dpi);
        let y = tb.bottom - self.phys_h;
        let mut x = i32::MIN;
        let center_aligned = zx_core::win::hkcu_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarAl", 1) != 0;

        if cfg.on_left() && center_aligned {
            if cfg.snap_to_start() {
                let start = zx_core::win::find_child_by_class(self.taskbar, "Start");
                if !start.is_invalid() {
                    if let Some(r) = zx_core::win::rect_of(start) {
                        x = r.left - self.phys_w - spacing;
                    }
                }
            } else {
                let widgets = zx_core::win::hkcu_dword(
                    r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarDa", 1) != 0;
                x = tb.left + spacing + if widgets { zx_core::win::dpi_scale_i(160.0, self.dpi) } else { 0 };
            }
            if x != i32::MIN && x + self.phys_w <= tb.right { return (x, y); }
        }

        let tray = zx_core::win::find_child_by_class(self.taskbar, "TrayNotifyWnd");
        if !tray.is_invalid() {
            if let Some(r) = zx_core::win::rect_of(tray) {
                x = r.left - self.phys_w - spacing;
            }
        }
        if x == i32::MIN {
            x = tb.right - self.phys_w - zx_core::win::dpi_scale_i(88.0, self.dpi);
        }
        if !center_aligned && zx_core::win::hkcu_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarDa", 1) != 0 {
            x -= zx_core::win::dpi_scale_i(160.0, self.dpi);
        }
        x = x.clamp(tb.left, (tb.right - self.phys_w).max(tb.left));
        (x, y)
    }

    fn anchor_vertical(&self, tb: &RECT, band_w: i32, band_x: i32, cfg: &zx_core::Settings) -> (i32, i32) {
        let spacing = zx_core::win::dpi_scale_i(2.0, self.dpi);
        let x = tb.left + band_x + ((band_w - self.phys_w) / 2).max(0);
        let center_aligned = zx_core::win::hkcu_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarAl", 1) != 0;
        if cfg.on_left() && center_aligned {
            let mut y = tb.top + spacing;
            if cfg.snap_to_start() {
                y = (tb.top + spacing).max(tb.top + spacing);
            }
            return (x, y);
        }
        let tray = zx_core::win::find_child_by_class(self.taskbar, "TrayNotifyWnd");
        let mut y = if !tray.is_invalid() {
            zx_core::win::rect_of(tray).map(|r| r.top - self.phys_h - spacing)
                .unwrap_or(tb.bottom - self.phys_h - zx_core::win::dpi_scale_i(88.0, self.dpi))
        } else {
            tb.bottom - self.phys_h - zx_core::win::dpi_scale_i(88.0, self.dpi)
        };
        y = y.clamp(tb.top, (tb.bottom - self.phys_h).max(tb.top));
        (x, y)
    }

    /// 任务栏横截面（侧边任务栏的厚度与起始 x）。
    fn band_cross(&self, tb: &RECT) -> (i32, i32, i32) {
        (tb.right - tb.left, 0, tb.bottom - tb.top)
    }

    fn band_height(&self, tb: &RECT) -> i32 {
        if self.classical && !self.band.is_invalid() {
            if let Some(r) = zx_core::win::rect_of(self.band) { return r.bottom - r.top; }
        }
        tb.bottom - tb.top
    }

    fn reposition_floating(&mut self, force: bool) {
        if self.fx == i32::MIN {
            let work = window::work_area(self.hwnd);
            self.fx = work.right - self.phys_w - zx_core::win::dpi_scale_i(24.0, self.dpi);
            self.fy = work.top + zx_core::win::dpi_scale_i(24.0, self.dpi);
        }
        unsafe {
            let _ = SetWindowPos(self.hwnd, if zx_core::cfg::cfg().floating_topmost() { HWND_TOPMOST } else { HWND_NOTOPMOST },
                self.fx, self.fy, self.phys_w, self.phys_h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = force;
        }
    }

    // ---------- 绘制 ----------
    fn draw(&mut self) {
        let l = self.layout();
        if l.width <= 0.0 { return; }
        let dpi = self.dpi as f32 / 96.0;
        let pal = self.palette();
        self.canvas.clear(0x0000_0000);
        if std::env::var("ZHUXIA_DEBUG_BG").ok().as_deref() == Some("1") {
            self.canvas.fill_rect(0.0, 0.0, self.phys_w as f32, self.phys_h as f32, 0xFF00FF00);
        }

        let floating = self.is_floating();
        if floating {
            let a = (zx_core::cfg::cfg().floating_opacity() * 255.0) as u8;
            let fill = zx_ui::with_alpha(pal.card, a);
            self.canvas.fill_round(0.0, 0.0, self.phys_w as f32, self.phys_h as f32,
                                   CARD_RADIUS * dpi, fill);
        } else {
            // Win11 任务栏：视觉上要"透出任务栏"，所以这里不铺实色背景。
            // 但分层窗口的鼠标命中是**逐像素按 alpha 判断**的：alpha = 0 的区域会直接
            // 穿透到下面的任务栏，连 WM_MOUSEMOVE 都收不到——结果只有文字笔画能点中，
            // "点网络块没反应"就是这么来的。铺一层 alpha = 1 的底（肉眼不可见）即可。
            self.canvas.fill_rect(0.0, 0.0, self.phys_w as f32, self.phys_h as f32, 0x0100_0000);
        }

        if l.vertical { self.draw_vertical(&l, &pal, dpi); } else { self.draw_horizontal(&l, &pal, dpi); }

        if floating {
            self.canvas.stroke_round(0.5, 0.5, self.phys_w as f32 - 1.0, self.phys_h as f32 - 1.0,
                                     CARD_RADIUS * dpi, 1.0, pal.card_border);
        }
        let (px, py) = self.fx_or_rect();
        self.canvas.present(self.hwnd, px, py);
    }

    fn fx_or_rect(&self) -> (i32, i32) {
        if self.is_floating() {
            (self.fx, self.fy)
        } else {
            zx_core::win::rect_of(self.hwnd).map(|r| (r.left, r.top)).unwrap_or((0, 0))
        }
    }

    fn draw_horizontal(&mut self, l: &Layout, pal: &theme::Palette, k: f32) {
        let pad = if self.is_floating() { 0.0 } else { 4.0 * k };
        let top = pad;
        let bottom = self.phys_h as f32 - pad;
        let mid = (top + bottom) / 2.0;

        for slot in 0..5i32 {
            let brush = if slot == crate::app::selected() {
                Some(pal.highlight)
            } else if slot == self.hovered {
                Some(pal.hover)
            } else { None };
            let Some(color) = brush else { continue };
            let Some(r) = l.slot_rect(slot, top, mid, bottom) else { continue };
            let (x0, y0, x1, y1) = scale_rect(r, k);
            if self.is_floating() {
                self.canvas.fill_rect(x0, y0, x1 - x0, y1 - y0, color);
            } else {
                self.canvas.fill_round(x0, y0, x1 - x0, y1 - y0, 6.0 * k, color);
            }
        }

        if zx_core::cfg::cfg().separators() {
            let half = 0.5 * k;
            let gpu_block_group = if l.gpu_both && l.slot_visible(3) { Some(l.position_of(3) / 2) } else { None };
            let net_block_group = if l.net_pos >= 0 { Some(l.net_pos / 2) } else { None };
            let mut boundary = false;
            for g in 0..4usize {
                if !l.group_visible(g) { continue; }
                let gx0 = l.group_left(g) * k;
                let gx1 = gx0 + l.group_w(g) * k;
                if boundary {
                    self.canvas.fill_rect(gx0 - half, top, 1.0 * k, bottom - top, pal.separator);
                }
                if Some(g as i32) != net_block_group && Some(g as i32) != gpu_block_group {
                    self.canvas.fill_rect(gx0, mid - half, gx1 - gx0, 1.0 * k, pal.separator);
                }
                boundary = true;
            }
        }

        let snap = self.snapshot.clone();
        let label_px = 13.0 * k;
        // CPU / 内存 / 磁盘
        for (slot, label, value) in [
            (0i32, "CPU", fmt::pct(snap.cpu)),
            (1, "内存", fmt::pct(snap.ram)),
            (2, "磁盘", fmt::pct(snap.disk_pct)),
        ] {
            self.row(l, pal, slot, label, &value, top, mid, bottom, k, false);
        }

        // GPU
        let usage = if snap.gpu_ok { fmt::pct(snap.gpu_pct) } else { "--".into() };
        let temp = fmt::temp(if snap.gpu_temp_valid { snap.gpu_temp } else { f64::NAN },
                             zx_core::cfg::cfg().gpu_temp_unit() == zx_core::GpuTempUnit::Fahrenheit);
        if l.gpu_both {
            if let Some((x0, y0, x1, y1)) = l.slot_rect(3, top, mid, bottom) {
                let (sx0, sy0, sx1, sy1) = scale_rect((x0, y0, x1, y1), k);
                let inset = layout::row_inset(zx_core::cfg::cfg().compact()) * k;
                let cell = (sx0 + inset, sy0, sx1 - inset, sy1);
                let m = (cell.1 + cell.3) / 2.0;
                let box_w = (cell.2 - cell.0).max(1.0);
                // "GPU" 竖向跨两行；值右对齐，但和 "GPU" 撞上时先缩字号再省略——
                // 之前两者共用整块宽度，`100%` 会一路左伸直接压在 "GPU" 上。
                self.canvas.text("GPU", cell.0, cell.1, box_w, cell.3 - cell.1,
                                 label_px, pal.label, TextAlign::Near, VAlign::Center);
                let lw = self.canvas.measure("GPU", label_px, false);
                for (txt, y0, y1) in [(&usage, cell.1, m), (&temp, m, cell.3)] {
                    let fpx = fit_px(&mut self.canvas, txt, box_w - lw - 3.0 * k, label_px);
                    let trim = self.canvas.measure(txt, fpx, false) > box_w - lw;
                    self.canvas.text_ex(txt, cell.0, y0, box_w, y1 - y0,
                                        fpx, pal.text, TextAlign::Far, VAlign::Center, false, trim);
                }
            }
        } else if l.temp_pos >= 0 {
            self.row_pos(l, pal, l.temp_pos, "GPU", &temp, top, mid, bottom, k);
        } else {
            self.row(l, pal, 3, "GPU", &usage, top, mid, bottom, k, false);
        }

        // 网络
        if let Some((x0, y0, x1, y1)) = l.slot_rect(4, top, mid, bottom) {
            let (sx0, sy0, sx1, sy1) = scale_rect((x0, y0, x1, y1), k);
            let inset = layout::row_inset(zx_core::cfg::cfg().compact()) * k;
            let m = (sy0 + sy1) / 2.0;
            let cell = (sx0 + inset, 0.0, sx1 - inset, 0.0);
            arrow_metric(&mut self.canvas, (cell.0, sy0, cell.2, m),
                         "↑", &dbg_speed(snap.net_up, "99.8 MB/s"), label_px, pal);
            arrow_metric(&mut self.canvas, (cell.0, m, cell.2, sy1),
                         "↓", &dbg_speed(snap.net_down, "12.3 KB/s"), label_px, pal);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(&mut self, l: &Layout, pal: &theme::Palette, slot: i32, label: &str, value: &str,
           top: f32, mid: f32, bottom: f32, k: f32, strip: bool) {
        let pos = l.position_of(slot);
        self.row_pos(l, pal, pos, label, value, top, mid, bottom, k);
        let _ = strip;
    }

    #[allow(clippy::too_many_arguments)]
    fn row_pos(&mut self, l: &Layout, pal: &theme::Palette, pos: i32, label: &str, value: &str,
               top: f32, mid: f32, bottom: f32, k: f32) {
        let Some(r) = l.pos_rect_at(pos, top, mid, bottom) else { return };
        let (x0, y0, x1, y1) = scale_rect(r, k);
        let inset = layout::row_inset(zx_core::cfg::cfg().compact()) * k;
        let px = 13.0 * k;
        let box_w = (x1 - x0 - inset * 2.0).max(1.0);
        self.canvas.text(label, x0 + inset, y0, box_w, y1 - y0, px, pal.label, TextAlign::Near, VAlign::Center);
        // 值默认用整块宽度（右对齐，视觉上贴着右边）；只有和标签快要撞上时才缩字号。
        let lw = self.canvas.measure(label, px, false);
        let fpx = fit_px(&mut self.canvas, value, box_w - lw - 3.0 * k, px);
        let trim = self.canvas.measure(value, fpx, false) > box_w - lw;
        self.canvas.text_ex(value, x0 + inset, y0, box_w, y1 - y0,
                            fpx, pal.text, TextAlign::Far, VAlign::Center, false, trim);
    }

    fn draw_vertical(&mut self, l: &Layout, pal: &theme::Palette, k: f32) {
        // 悬停/选中
        for slot in 0..5i32 {
            let color = if slot == crate::app::selected() { Some(pal.highlight) }
                        else if slot == self.hovered { Some(pal.hover) } else { None };
            let Some(color) = color else { continue };
            let Some(r) = l.slot_rect(slot, 0.0, 0.0, 0.0) else { continue };
            let (x0, y0, x1, y1) = scale_rect(r, k);
            self.canvas.fill_round(x0, y0, x1 - x0, y1 - y0, 6.0 * k, color);
        }

        if zx_core::cfg::cfg().separators() {
            let half = 0.5 * k;
            if l.two_line {
                let mut p = 1;
                while p < layout::PACKED && l.slot_at(p as i32) >= 0 {
                    let skip = (l.gpu_both && p as i32 == l.temp_pos) || (l.net_pos >= 0 && p as i32 == l.net_pos + 1);
                    if !skip {
                        let y = l.strip_pos_top(p as i32) * k;
                        self.canvas.fill_rect(0.0, y - half, l.width * k, 1.0 * k, pal.separator);
                    }
                    p += 1;
                }
            } else {
                for c in 0..l.columns {
                    let x0 = l.column_left(c) * k;
                    let x1 = x0 + l.column_width(c) * k;
                    if c > 0 {
                        self.canvas.fill_rect(x0 - half, 0.0, 1.0 * k, l.height * k, pal.separator);
                    }
                    for r in 1..l.col_rows[c as usize] {
                        let inside_net = l.net_col == c && l.net_row == r - 1;
                        let temp_col = if l.temp_pos >= 0 { l.pos_col[l.temp_pos as usize] } else { -1 };
                        let temp_row = if l.temp_pos >= 0 { l.pos_row[l.temp_pos as usize] } else { -1 };
                        let inside_gpu = l.gpu_both && temp_col == c && temp_row == r;
                        if inside_net || inside_gpu { continue; }
                        let y = r as f32 * l.row_h * k;
                        self.canvas.fill_rect(x0, y - half, x1 - x0, 1.0 * k, pal.separator);
                    }
                }
            }
        }

        let snap = self.snapshot.clone();
        let px = 13.0 * k;
        if !l.two_line {
            for (slot, label, value) in [
                (0i32, "CPU", fmt::pct(snap.cpu)),
                (1, "内存", fmt::pct(snap.ram)),
                (2, "磁盘", fmt::pct(snap.disk_pct)),
            ] {
                self.row_pos(l, pal, l.position_of(slot), label, &value, 0.0, 0.0, 0.0, k);
            }
            let usage = if snap.gpu_ok { fmt::pct(snap.gpu_pct) } else { "--".into() };
            let temp = fmt::temp(if snap.gpu_temp_valid { snap.gpu_temp } else { f64::NAN },
                                 zx_core::cfg::cfg().gpu_temp_unit() == zx_core::GpuTempUnit::Fahrenheit);
            if l.gpu_both {
                if let Some((x0, y0, x1, y1)) = l.slot_rect(3, 0.0, 0.0, 0.0) {
                    let (sx0, sy0, sx1, sy1) = scale_rect((x0, y0, x1, y1), k);
                    let inset = layout::row_inset(zx_core::cfg::cfg().compact()) * k;
                    let m = (sy0 + sy1) / 2.0;
                    self.canvas.text("GPU", sx0 + inset, sy0, sx1 - sx0 - inset * 2.0, sy1 - sy0,
                                     px, pal.label, TextAlign::Near, VAlign::Center);
                    self.canvas.text_ex(&usage, sx0 + inset, sy0, sx1 - sx0 - inset * 2.0, m - sy0,
                                        px, pal.text, TextAlign::Far, VAlign::Center, false, false);
                    self.canvas.text_ex(&temp, sx0 + inset, m, sx1 - sx0 - inset * 2.0, sy1 - m,
                                        px, pal.text, TextAlign::Far, VAlign::Center, false, false);
                }
            } else if l.temp_pos >= 0 {
                self.row_pos(l, pal, l.temp_pos, "GPU", &temp, 0.0, 0.0, 0.0, k);
            } else {
                self.row_pos(l, pal, l.position_of(3), "GPU", &usage, 0.0, 0.0, 0.0, k);
            }
            if let Some((x0, y0, x1, y1)) = l.slot_rect(4, 0.0, 0.0, 0.0) {
                let (sx0, sy0, sx1, sy1) = scale_rect((x0, y0, x1, y1), k);
                let inset = layout::row_inset(zx_core::cfg::cfg().compact()) * k;
                let m = sy0 + l.row_h * k;
                arrow_metric(&mut self.canvas, (sx0 + inset, sy0, sx1 - inset, m),
                             "↑", &dbg_speed(snap.net_up, "99.8 MB/s"), px, pal);
                arrow_metric(&mut self.canvas, (sx0 + inset, m, sx1 - inset, sy1),
                             "↓", &dbg_speed(snap.net_down, "12.3 KB/s"), px, pal);
            }
            return;
        }

        // 双行条带
        let label_px = 11.0 * k;
        for (slot, label, value) in [
            (0i32, "CPU", fmt::pct(snap.cpu)),
            (1, "内存", fmt::pct(snap.ram)),
            (2, "磁盘", fmt::pct(snap.disk_pct)),
        ] {
            if let Some((x0, y0, x1, y1)) = l.pos_rect(l.position_of(slot)) {
                self.strip_block((x0, y0, x1, y1), label, &value, label_px, px, pal, k);
            }
        }
        let usage = if snap.gpu_ok { fmt::pct(snap.gpu_pct) } else { "--".into() };
        let temp = fmt::temp(if snap.gpu_temp_valid { snap.gpu_temp } else { f64::NAN },
                             zx_core::cfg::cfg().gpu_temp_unit() == zx_core::GpuTempUnit::Fahrenheit);
        if l.gpu_both {
            if let Some((x0, y0, x1, y1)) = l.slot_rect(3, 0.0, 0.0, 0.0) {
                let (sx0, sy0, sx1, sy1) = scale_rect((x0, y0, x1, y1), k);
                let pad = layout::strip_block_pad(zx_core::cfg::cfg().compact()) * k;
                let inset = 2.0 * k;
                let h = (sy1 - sy0 - pad * 2.0 - 2.0 * layout::STRIP_LINE_GAP * k) / 3.0;
                let mut y = sy0 + pad;
                self.canvas.text("GPU", sx0 + inset, y, sx1 - sx0 - inset * 2.0, h, label_px, pal.label, TextAlign::Center, VAlign::Center);
                y += h + layout::STRIP_LINE_GAP * k;
                self.canvas.text(&usage, sx0 + inset, y, sx1 - sx0 - inset * 2.0, h, px, pal.text, TextAlign::Center, VAlign::Center);
                y += h + layout::STRIP_LINE_GAP * k;
                self.canvas.text(&temp, sx0 + inset, y, sx1 - sx0 - inset * 2.0, h, px, pal.text, TextAlign::Center, VAlign::Center);
            }
        } else if l.temp_pos >= 0 {
            if let Some((x0, y0, x1, y1)) = l.pos_rect(l.temp_pos) {
                self.strip_block((x0, y0, x1, y1), "GPU", &temp, label_px, px, pal, k);
            }
        } else if let Some((x0, y0, x1, y1)) = l.pos_rect(l.position_of(3)) {
            self.strip_block((x0, y0, x1, y1), "GPU", &usage, label_px, px, pal, k);
        }
        if let Some((x0, y0, x1, y1)) = l.slot_rect(4, 0.0, 0.0, 0.0) {
            let (sx0, sy0, sx1, sy1) = scale_rect((x0, y0, x1, y1), k);
            let pad = layout::strip_block_pad(zx_core::cfg::cfg().compact()) * k;
            let inset = 2.0 * k;
            let band = (sy1 - sy0 - pad * 2.0 - 3.0 * layout::STRIP_LINE_GAP * k) / 4.0;
            let mut y = sy0 + pad;
            self.canvas.text("↑", sx0 + inset, y, sx1 - sx0 - inset * 2.0, band, label_px, pal.label, TextAlign::Center, VAlign::Center);
            y += band + layout::STRIP_LINE_GAP * k;
            self.canvas.text_ex(&fmt::net_rate_compact(snap.net_up), sx0 + inset, y, sx1 - sx0 - inset * 2.0, band,
                                px, pal.text, TextAlign::Center, VAlign::Center, false, true);
            y += band + layout::STRIP_LINE_GAP * k;
            self.canvas.text("↓", sx0 + inset, y, sx1 - sx0 - inset * 2.0, band, label_px, pal.label, TextAlign::Center, VAlign::Center);
            y += band + layout::STRIP_LINE_GAP * k;
            self.canvas.text_ex(&fmt::net_rate_compact(snap.net_down), sx0 + inset, y, sx1 - sx0 - inset * 2.0, band,
                                px, pal.text, TextAlign::Center, VAlign::Center, false, true);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn strip_block(&mut self, r: (f32, f32, f32, f32), label: &str, value: &str,
                   label_px: f32, px: f32, pal: &theme::Palette, k: f32) {
        let (sx0, sy0, sx1, sy1) = scale_rect(r, k);
        let pad = layout::strip_block_pad(zx_core::cfg::cfg().compact()) * k;
        let inset = 2.0 * k;
        let content_top = sy0 + pad;
        let content_bottom = sy1 - pad;
        let line_h = (content_bottom - content_top - layout::STRIP_LINE_GAP * k) / 2.0;
        self.canvas.text(label, sx0 + inset, content_top, sx1 - sx0 - inset * 2.0, line_h,
                         label_px, pal.label, TextAlign::Center, VAlign::Center);
        self.canvas.text(value, sx0 + inset, content_top + line_h + layout::STRIP_LINE_GAP * k,
                         sx1 - sx0 - inset * 2.0, line_h, px, pal.text, TextAlign::Center, VAlign::Center);
    }

    /// 贴边隐藏：根据"家"位置判断吸附边（左/右/上），并决定是否启动 100ms 轮询。
    fn update_dock(&mut self) {
        let cfg = zx_core::cfg::cfg();
        self.dock_edge = 0;
        if self.is_floating() && cfg.floating_edge_hide() {
            let work = window::work_area(self.hwnd);
            let margin = zx_core::win::dpi_scale_i(12.0, self.dpi);
            let (hx, hy) = if self.home.0 == i32::MIN { (self.fx, self.fy) } else { self.home };
            if hx <= work.left + margin { self.dock_edge = 1; }
            else if hx + self.phys_w >= work.right - margin { self.dock_edge = 2; }
            else if hy <= work.top + margin { self.dock_edge = 3; }
            unsafe {
                if self.dock_edge != 0 {
                    SetTimer(self.hwnd, TIMER_EDGE, 100, None);
                } else {
                    let _ = KillTimer(self.hwnd, TIMER_EDGE);
                }
            }
            if self.dock_edge == 0 && self.edge_hidden {
                self.edge_hidden = false;
                self.slide_to(hx, hy);
            }
        }
    }

    /// 贴边隐藏的显隐切换（100ms 轮询）。
    fn edge_tick(&mut self) {
        if !self.is_floating() || !zx_core::cfg::cfg().floating_edge_hide() { return; }
        self.update_dock();
        if self.dock_edge == 0 { return; }
        let work = window::work_area(self.hwnd);
        let mut pt = POINT::default();
        unsafe { let _ = GetCursorPos(&mut pt); }
        let sliver = zx_core::win::dpi_scale_i(3.0, self.dpi);
        let on_window = zx_core::win::rect_of(self.hwnd).map(|r| {
            pt.x >= r.left - sliver && pt.x <= r.right + sliver && pt.y >= r.top - sliver && pt.y <= r.bottom + sliver
        }).unwrap_or(false);

        if self.edge_hidden {
            // 光标贴到边缘的细条上 → 滑出来
            let near = match self.dock_edge {
                1 => pt.x <= work.left + sliver,
                2 => pt.x >= work.right - sliver,
                _ => pt.y <= work.top + sliver,
            };
            if near {
                self.edge_hidden = false;
                self.slide_to(self.home.0, self.home.1);
            }
        } else if !on_window && self.anim.is_none() {
            // 光标离开 → 收起，只留 3px 细条
            let hidden = match self.dock_edge {
                1 => (work.left - self.phys_w + sliver, self.home.1),
                2 => (work.right - sliver, self.home.1),
                _ => (self.home.0, work.top - self.phys_h + sliver),
            };
            self.edge_hidden = true;
            self.slide_to(hidden.0, hidden.1);
        }
    }

    /// 平滑移动到目标位置。
    ///
    /// 之前是直接 SetWindowPos 跳过去，收起/滑出的动作很生硬；这里按缓出曲线插值，
    /// 每 16ms 一帧、共 7 帧（≈110ms）。中途改目标就从当前位置重新起算，不会回弹。
    fn slide_to(&mut self, x: i32, y: i32) {
        if self.fx == x && self.fy == y {
            self.end_anim();
            self.reposition(true);
            return;
        }
        self.anim = Some(Anim { from: (self.fx, self.fy), to: (x, y), step: 0 });
        unsafe { SetTimer(self.hwnd, TIMER_ANIM, ANIM_MS, None); }
        self.anim_step();
    }

    /// 推进一帧。到达终点时把位置钉死在目标值（避免浮点累计误差）。
    fn anim_step(&mut self) {
        let Some(a) = self.anim else { return };
        let step = a.step.saturating_add(1);
        let t = (step as f32 / ANIM_STEPS as f32).min(1.0);
        let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic：起步快、收尾稳
        self.fx = a.from.0 + ((a.to.0 - a.from.0) as f32 * e).round() as i32;
        self.fy = a.from.1 + ((a.to.1 - a.from.1) as f32 * e).round() as i32;
        if step >= ANIM_STEPS {
            self.fx = a.to.0;
            self.fy = a.to.1;
            self.end_anim();
        } else {
            self.anim = Some(Anim { step, ..a });
        }
        self.reposition(true);
    }

    fn end_anim(&mut self) {
        self.anim = None;
        unsafe { let _ = KillTimer(self.hwnd, TIMER_ANIM); }
    }

    fn hit_test(&self, x: i32, y: i32) -> i32 {
        let l = self.layout();
        if l.vertical {
            if x < 0 || y < 0 { return -1; }
            let k = self.dpi as f32 / 96.0;
            let (xd, yd) = (x as f32 / k, y as f32 / k);
            for pos in 0..layout::PACKED as i32 {
                let slot = l.slot_at(pos);
                if slot < 0 { continue; }
                if let Some((x0, y0, x1, y1)) = l.pos_rect(pos) {
                    if xd >= x0 && xd < x1 && yd >= y0 && yd < y1 { return slot; }
                }
            }
            return -1;
        }
        // 每一列覆盖 2 个格子位置（pos = 列*2, 列*2+1）。
        //
        // 早先的写法是 `slot_at(列*2 + (y 在上半 ? 0 : 1))`，默认"每列两个不同项目"，
        // 但磁盘/GPU/网络这类单行或跨行项目并不是这样：
        //   · 磁盘只占 pos 2，pos 3 是空的 → 点下半直接返回 -1（点了没反应）
        //   · GPU/网络两行是**同一个** slot，按上下算会跳到隔壁项目上
        // 这里逐个看该列的两个格子，才既不会误判也不会漏判。
        let k = self.dpi as f32 / 96.0;
        let mid = self.phys_h / 2;
        for g in 0..4usize {
            if !l.group_visible(g) { continue; }
            let left = (l.group_left(g) * k) as i32;
            let last = l.group_left(g) + l.group_w(g) >= l.width - 0.01;
            let right = if last { self.phys_w } else { ((l.group_left(g) + l.group_w(g)) * k) as i32 };
            if x < left || x >= right { continue; }
            let top_slot = l.slot_at(g as i32 * 2);
            let bottom_slot = l.slot_at(g as i32 * 2 + 1);
            if top_slot < 0 && bottom_slot < 0 { return -1; }
            if top_slot == bottom_slot { return top_slot; }
            if bottom_slot < 0 { return top_slot; }      // 上半有、下半空 → 整列都算它
            if top_slot < 0 { return bottom_slot; }
            return if y < mid { top_slot } else { bottom_slot };
        }
        -1
    }
}

fn scale_rect(r: (f32, f32, f32, f32), k: f32) -> (f32, f32, f32, f32) {
    (r.0 * k, r.1 * k, r.2 * k, r.3 * k)
}

// ---------------- 窗口过程 ----------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    match msg {
        WM_NCCREATE => LRESULT(1),
        WM_CREATE => {
            let cfg = zx_core::cfg::cfg();
            let form = NEXT_FORM.with(|c| c.get());
            // 任务栏形态：用建窗前指定的那根（多屏各一个覆盖层）；
            // 悬浮形态不需要，兜底找主任务栏只为读 DPI / 高度参考。
            let picked = NEXT_TASK.with(|c| c.get());
            let taskbar = if picked != 0 {
                HWND(picked as *mut _)
            } else {
                FindWindowW(windows::core::w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default()
            };
            let classical = !is_win11_taskbar(taskbar);
            let dpi = window::dpi_of(taskbar);
            let vertical = zx_core::win::rect_of(taskbar)
                .map(|r| (r.right - r.left) < (r.bottom - r.top)).unwrap_or(false);
            let phys_h = zx_core::win::rect_of(taskbar).map(|r| r.bottom - r.top).unwrap_or(48).max(24);
            let logical_h = phys_h as f32 * 96.0 / dpi.max(1) as f32;

            let mut o = Box::new(Overlay {
                hwnd, form, taskbar, classical, vertical,
                canvas: Canvas::new(64, phys_h, dpi),
                dpi, phys_w: 64, phys_h, logical_h, avail_dip: 0.0,
                hovered: -1, tracking: false, light_taskbar: zx_core::win::taskbar_light(),
                snapshot: crate::snapshot::latest(),
                fx: cfg.floating_x.unwrap_or(i32::MIN), fy: cfg.floating_y.unwrap_or(i32::MIN), press: false, dock_edge: 0,
                edge_hidden: false, fullscreen_hidden: false, grab: (0, 0),
                band: HWND::default(), band_rect: RECT::default(), band_shrunk: false, logged: false,
                home: (i32::MIN, i32::MIN),
                anim: None,
            });
            if form == Form::Floating { o.vertical = false; o.classical = false; }
            o.relayout();
            if form == Form::Floating { o.home = (o.fx, o.fy); o.update_dock(); }
            if form == Form::Taskbar && classical { shrink_classic_band(&mut o); }
            zx_core::log::info(format!("覆盖层创建 hwnd={:?} form={:?} taskbar={:?} dpi={} vertical={} {}x{}", hwnd, form as u8, taskbar, dpi, vertical, o.phys_w, o.phys_h));
            zx_core::log::info(format!("覆盖层形态：win11={} classical={} vertical={}", !classical, classical, vertical));
            let ptr = Box::into_raw(o);
            MAP.with(|m| m.borrow_mut().insert(hwnd.0 as isize, ptr));
            crate::runtime::register_snapshot_window(hwnd);
            let _ = cfg;
            SetTimer(hwnd, TIMER_TICK, 1000, None);
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_EDGE => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) { o.edge_tick(); }
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_ANIM => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) { o.anim_step(); }
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_TICK => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                // 主题 / 几何 / 全屏状态每秒复核一次
                let light = zx_core::win::taskbar_light();
                let mut redraw = false;
                if light != o.light_taskbar { o.light_taskbar = light; redraw = true; }
                o.reposition(false);
                let fs = window::foreground_fullscreen();
                if o.is_floating() && zx_core::cfg::cfg().floating_fullscreen_hide() && fs != o.fullscreen_hidden {
                    o.fullscreen_hidden = fs;
                    let _ = ShowWindow(hwnd, if fs { SW_HIDE } else { SW_SHOWNOACTIVATE });
                }
                if redraw { o.draw(); }
            }
            LRESULT(0)
        }
        WM_SNAPSHOT => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                o.snapshot = crate::snapshot::latest();
                o.draw();
            }
            LRESULT(0)
        }
        WM_SET_METRICS | WM_REPOSITION => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) { o.relayout(); o.draw(); }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                if !o.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut t);
                    o.tracking = true;
                }
                if o.press && o.is_floating() {
                    let moved = (x - o.grab.0).abs() > zx_core::win::dpi_scale_i(4.0, o.dpi)
                        || (y - o.grab.1).abs() > zx_core::win::dpi_scale_i(4.0, o.dpi);
                    if moved {
                        let mut r = zx_core::win::rect_of(hwnd).unwrap_or_default();
                        r.left += x - o.grab.0;
                        r.top += y - o.grab.1;
                        o.fx = r.left;
                        o.fy = r.top;
                        o.reposition(true);
                        return LRESULT(0);
                    }
                }
                let slot = o.hit_test(x, y);
                if slot != o.hovered {
                    o.hovered = slot;
                    o.draw();
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                o.tracking = false;
                if o.hovered != -1 { o.hovered = -1; o.draw(); }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                o.press = true;
                o.grab = (x, y);
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                let moved = (x - o.grab.0).abs() > zx_core::win::dpi_scale_i(4.0, o.dpi)
                    || (y - o.grab.1).abs() > zx_core::win::dpi_scale_i(4.0, o.dpi);
                o.press = false;
                if o.is_floating() && moved {
                    let mut r = zx_core::win::rect_of(hwnd).unwrap_or_default();
                    r.left += x - o.grab.0;
                    r.top += y - o.grab.1;
                    o.fx = r.left;
                    o.fy = r.top;
                    o.home = (o.fx, o.fy);
                    crate::cfg_set(|c| { c.floating_x = Some(o.fx); c.floating_y = Some(o.fy); });
                    o.reposition(true);
                    o.update_dock();
                    return LRESULT(0);
                }
                let slot = o.hit_test(x, y);
                if slot >= 0 { crate::app::toggle_detail(slot, hwnd); }
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            if let Some(o) = slot_of(hwnd).map(|p| &mut *p) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                show_menu(hwnd, pt.x, pt.y, o.is_floating());
            }
            LRESULT(0)
        }
        zx_ui::window::WM_APP_UPDATE_READY => {
            if let Some(tag) = crate::update::take() {
                let pick = crate::dialogs::update(&tag);
                match pick {
                    0 => crate::cfg_set(|c| c.ignored_update_version = Some(tag.clone())),
                    2 => {
                        // 只在 GitHub 分发：直接开 Releases 页面，不需要用户配更新源
                        let url = zx_core::cfg::UPDATE_PAGE;
                        unsafe {
                            let u = zx_core::win::wide(url);
                            windows::Win32::UI::Shell::ShellExecuteW(
                                HWND::default(), windows::core::w!("open"),
                                PCWSTR(u.as_ptr()), PCWSTR::null(), PCWSTR::null(),
                                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL);
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        zx_ui::window::WM_APP_OPEN_SETTINGS => {
            zx_core::log::info("收到打开设置面板的通知");
            crate::open_settings();
            LRESULT(0)
        }
        zx_ui::window::WM_APP_QUIT => {
            crate::app::exit();
            LRESULT(0)
        }
        zx_ui::window::WM_APP_RELOAD => {
            crate::reload_and_apply();
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = MAP.with(|m| m.borrow_mut().remove(&(hwnd.0 as isize)));
            if let Some(p) = ptr {
                let mut o = Box::from_raw(p);
                restore_classic_band(&mut o);
                crate::runtime::unregister_snapshot_window(hwnd);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32, floating: bool) {
    use zx_ui::menu::{popup, Item};
    let cfg = zx_core::cfg::cfg();
    let items = vec![
        Item::new(100, "悬浮模式").checked(cfg.floating()),
        Item::new(101, "置顶显示").checked(cfg.floating_topmost()).enabled(floating),
        Item::new(102, "贴边隐藏").checked(cfg.floating_edge_hide()).enabled(floating),
        Item::new(103, "全屏时隐藏").checked(cfg.floating_fullscreen_hide()).enabled(floating),
        Item::sep(),
        Item::new(104, "紧凑模式").checked(cfg.compact()),
        Item::sep(),
        Item::new(105, "设置"),
        Item::new(106, "退出"),
    ];
    match popup(hwnd, x, y, &items) {
        100 => { crate::cfg_set(|c| c.floating_mode = Some(!cfg.floating())); crate::app::rebuild_overlays(); }
        101 => { crate::cfg_set(|c| c.floating_topmost = Some(!cfg.floating_topmost())); crate::app::apply_live(); }
        102 => { crate::cfg_set(|c| c.floating_edge_hide = Some(!cfg.floating_edge_hide())); crate::app::apply_live(); }
        103 => { crate::cfg_set(|c| c.floating_fullscreen_hide = Some(!cfg.floating_fullscreen_hide())); crate::app::apply_live(); }
        104 => { crate::cfg_set(|c| c.compact_layout = Some(!cfg.compact())); crate::app::apply_live(); }
        105 => crate::app::open_settings(),
        106 => crate::app::exit(),
        _ => {}
    }
}

pub fn is_win11_taskbar(taskbar: HWND) -> bool {
    // Win11 的任务栏是 XAML 岛：Shell_TrayWnd 下挂着
    // "Windows.UI.Composition.DesktopWindowContentBridge"（不同 build 前后缀会变，
    // 所以按子串匹配类名）。Win10 / ExplorerPatcher 的经典任务栏没有这个子窗口。
    use windows::Win32::UI::WindowsAndMessaging::*;
    if taskbar.is_invalid() { return false; }
    unsafe {
        let mut cur = GetWindow(taskbar, GW_CHILD).unwrap_or_default();
        while !cur.is_invalid() {
            let mut buf = [0u16; 256];
            let n = GetClassNameW(cur, &mut buf);
            let name = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if name.contains("DesktopWindowContentBridge") { return true; }
            cur = match GetWindow(cur, GW_HWNDNEXT) { Ok(v) => v, Err(_) => break };
        }
    }
    false
}

/// 经典任务栏（Win10 / ExplorerPatcher）：把任务按钮带（MSTaskSwWClass）缩窄，
/// 让出覆盖层的位置，退出时还原。上游 TrafficMonitor 的 CClassicalTaskbarDlg 同一套做法。
fn shrink_classic_band(o: &mut Overlay) {
    let bar = {
        let b = zx_core::win::find_child_by_class(o.taskbar, "ReBarWindow32");
        if b.is_invalid() { zx_core::win::find_child_by_class(o.taskbar, "WorkerW") } else { b }
    };
    if bar.is_invalid() { return; }
    let min = {
        let m = zx_core::win::find_child_by_class(bar, "MSTaskSwWClass");
        if m.is_invalid() { zx_core::win::find_child_by_class(bar, "MSTaskListWClass") } else { m }
    };
    if min.is_invalid() { return; }
    let Some(rc) = zx_core::win::rect_of(min) else { return };
    if rc.right - rc.left <= o.phys_w + 32 { return; } // 带子太短，别硬塞

    o.band = min;
    o.band_rect = rc;
    o.band_shrunk = true;
    let w = (rc.right - rc.left) - o.phys_w - zx_core::win::dpi_scale_i(2.0, o.dpi);
    unsafe {
        let _ = SetWindowPos(min, HWND_TOP, rc.left, rc.top, w, rc.bottom - rc.top,
                             SWP_NOACTIVATE | SWP_NOZORDER);
    }
    zx_core::log::info(format!("经典任务栏：任务按钮带缩窄到 {w}px，为覆盖层让位"));
}

fn restore_classic_band(o: &mut Overlay) {
    if !o.band_shrunk || o.band.is_invalid() { return; }
    unsafe {
        let _ = SetWindowPos(o.band, HWND_TOP, o.band_rect.left, o.band_rect.top,
                             o.band_rect.right - o.band_rect.left, o.band_rect.bottom - o.band_rect.top,
                             SWP_NOACTIVATE | SWP_NOZORDER);
    }
    o.band_shrunk = false;
}

// ---------------- 创建 / 销毁 ----------------

/// 建任务栏形态覆盖层：每个选中的显示器各一个。
///
/// `taskbarDisplays` 为空 = 只跟主屏（上游旧配置的默认语义）；非空时按设备接口名匹配，
/// 断开的那台先跳过，等它回来下一次重建时会重新挂上（配置项会保留）。
/// 副屏没开"在此显示器上显示任务栏"时该屏没有任务栏窗口，也就没有位置可贴，直接跳过。
pub fn create_taskbar() -> Option<HWND> {
    NEXT_FORM.with(|c| c.set(Form::Taskbar));
    let cfg = zx_core::cfg::cfg();
    let displays = zx_core::monitor::displays();

    let mut picks: Vec<isize> = Vec::new();
    match cfg.taskbar_displays.as_ref().filter(|v| !v.is_empty()) {
        Some(sel) => {
            for s in sel {
                if let Some(d) = displays.iter().find(|d| d.id.eq_ignore_ascii_case(&s.id)) {
                    if d.has_taskbar() { picks.push(d.taskbar); }
                }
            }
        }
        None => {
            // 默认只主屏
            picks.extend(displays.iter().filter(|d| d.primary && d.has_taskbar()).map(|d| d.taskbar));
        }
    }
    if picks.is_empty() {
        // 配置里的显示器全断了 / 枚举失败：退回主任务栏，至少不要什么都不显示
        if let Some(d) = displays.iter().find(|d| d.primary) {
            if d.has_taskbar() { picks.push(d.taskbar); }
        }
    }

    let mut first = None;
    for tb in picks {
        NEXT_TASK.with(|c| c.set(tb));
        let hwnd = window::create_window(CLASS, "Zhuxia", WS_POPUP,
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, 380, 48);
        if let Some(hwnd) = hwnd {
            unsafe { let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE); }
            if first.is_none() { first = Some(hwnd); }
        }
    }
    NEXT_TASK.with(|c| c.set(0));
    first
}

/// 任务栏形态下，当前应挂几个覆盖层（供托盘自愈判断"是不是少了屏"）。
pub fn expected_taskbar_count() -> usize {
    let cfg = zx_core::cfg::cfg();
    let displays = zx_core::monitor::displays();
    match cfg.taskbar_displays.as_ref().filter(|v| !v.is_empty()) {
        Some(sel) => sel.iter().filter(|s| {
            displays.iter().any(|d| d.id.eq_ignore_ascii_case(&s.id) && d.has_taskbar())
        }).count(),
        None => displays.iter().filter(|d| d.primary && d.has_taskbar()).count(),
    }
}

pub fn create_floating() -> Option<HWND> {
    NEXT_FORM.with(|c| c.set(Form::Floating));
    let hwnd = window::create_window(CLASS, "Zhuxia", WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, 380, 48)?;
    unsafe { let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE); }
    Some(hwnd)
}

pub fn register() {
    window::register_class(CLASS, wndproc, None);
}

pub fn redraw_all() {
    MAP.with(|m| {
        for p in m.borrow().values() {
            unsafe { let _ = PostMessageW((**p).hwnd, WM_SNAPSHOT, WPARAM(0), LPARAM(0)); }
        }
    });
}

pub fn any() -> bool { MAP.with(|m| !m.borrow().is_empty()) }

pub fn count() -> usize { MAP.with(|m| m.borrow().len()) }

/// 把覆盖层重新顶到 z 序最前（任务栏有时会把自己抬上来盖住我们）。
pub fn reassert_topmost() {
    MAP.with(|m| {
        for p in m.borrow().values() {
            unsafe {
                let o = &**p;
                // 所贴的任务栏已经不存在了（副屏拔掉），别硬 SHOWWINDOW 把它翻出来
                if !o.is_floating() && zx_core::win::rect_of(o.taskbar).is_none() { continue; }
                let _ = SetWindowPos(o.hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
        }
    });
}

pub fn first_hwnd() -> Option<HWND> {
    MAP.with(|m| m.borrow().values().next().map(|p| unsafe { (**p).hwnd }))
}

pub fn destroy_all() {
    let list: Vec<HWND> = MAP.with(|m| m.borrow().values().map(|p| unsafe { (**p).hwnd }).collect());
    for h in list { unsafe { let _ = DestroyWindow(h); } }
}

/// 让所有覆盖层重新按当前配置布局并重绘（设置变更后调用）。
pub fn refresh_all() {
    MAP.with(|m| {
        for p in m.borrow().values() {
            unsafe {
                let o = &mut **p;
                o.light_taskbar = zx_core::win::taskbar_light();
                o.relayout();
                o.draw();
            }
        }
    });
}

/// 调试用：`ZHUXIA_DEBUG_BIGSPEED=1` 把网速伪造成 12.3/99.8 MB/s，验证极端宽度下的排版。
fn dbg_speed(real: i64, fake: &'static str) -> String {
    if std::env::var("ZHUXIA_DEBUG_BIGSPEED").ok().as_deref() == Some("1") {
        fake.to_string()
    } else {
        fmt::net_rate(real)
    }
}

/// 画"箭头 + 数值"：箭头单独占一段宽度，数值只在剩下的宽度里排，并按需缩字号。
///
/// 之前两者共用一个矩形（箭头 Near、数值 Far），网速奔到 `12.3 MB/s` 时
/// 数值会从右边一路左扩，直接压在箭头上——这就是"覆盖上下箭头"的成因。
fn arrow_metric(c: &mut Canvas, r: (f32, f32, f32, f32), arrow: &str, value: &str,
                px: f32, pal: &theme::Palette) {
    let (x0, y0, x1, y1) = r;
    let aw = (c.measure(arrow, px, false) + 1.0 * (px / 11.0)).max(px * 0.55);
    c.text(arrow, x0, y0, aw, y1 - y0, px, pal.label, TextAlign::Near, VAlign::Center);
    let vx = x0 + aw;
    let vw = (x1 - vx).max(1.0);
    let mut fpx = px;
    // 最小缩到 72%，再放不下 GDI+ 会自己裁（不会溢出到箭头区域）
    while fpx > px * 0.72 && c.measure(value, fpx, false) > vw {
        fpx -= 0.5;
    }
    c.text_ex(value, vx, y0, vw, y1 - y0, fpx, pal.text, TextAlign::Far, VAlign::Center, false, true);
}

/// 在给定宽度里放得下就用原字号，放不下就往下缩（最低 80%）。
fn fit_px(c: &mut Canvas, s: &str, avail: f32, px: f32) -> f32 {
    if avail <= 0.0 { return px * 0.8; }
    let mut f = px;
    while f > px * 0.8 && c.measure(s, f, false) > avail {
        f -= 0.5;
    }
    f
}
