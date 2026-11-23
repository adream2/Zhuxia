//! 详情弹窗：一个无边框卡片窗口，按槽位渲染 5 种视图（CPU / 内存 / 磁盘 / GPU / 网络）。
//!
//! 每次打开都是新窗口、关闭即销毁（与上游一致：不"隐藏复用"，避免残影与状态腐化）。
//! 失焦自动关闭，钉住（📌）后常驻。

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::GdiPlus::{GdipCreateBitmapFromHICON, GdipDisposeImage, GpImage};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;

use zx_core::fmt;
use zx_ui::painter::Canvas;
use zx_ui::theme;
use zx_ui::window::{self, WM_SNAPSHOT};
use zx_ui::{TextAlign, VAlign};

use crate::snapshot::{ProcRow, Snapshot};

const CLASS: &str = "ZhuxiaDetail";
const W_DIP: f32 = 428.0;
const HEADER_H: f32 = 44.0;
const ROW_H: f32 = 27.0;
/// 界面里到处用的矩形：(x, y, w, h)，单位是 DIP×dpi 后的物理像素。
type Rect = (f32, f32, f32, f32);
/// 设备标签行（"全部" + 每台设备一个）
const TAB_H: f32 = 26.0;
const TAB_GAP: f32 = 6.0;
const TAB_Y: f32 = 214.0;

thread_local! {
    static MAP: RefCell<HashMap<isize, *mut Detail>> = RefCell::new(HashMap::new());
    static ICONS: RefCell<HashMap<String, IconSlot>> = RefCell::new(HashMap::new());
}

struct IconSlot { img: *mut GpImage }

struct Detail {
    hwnd: HWND,
    slot: i32,
    #[allow(dead_code)]
    anchor: HWND,
    pinned: bool,
    canvas: Canvas,
    dpi: u32,
    w: i32,
    h: i32,
    /// 0 = 全部设备总览；k(≥1) = 第 k-1 台设备（磁盘 / GPU）
    tab: usize,
    /// 网络页列表：false = 实时速率，true = 历史累计
    net_history: bool,
    /// 网络列表首行的 y（DIP）——绘制时记下来，点击命中时用同一个值
    list_y0: f32,
    /// 打开时快照可能还没出第一帧（devices=0），等首个 WM_SNAPSHOT 再决定落到哪个标签
    tab_pending: bool,
    activated: bool,
    hover: i32, // 1=pin 2=close 3..=tab
    snapshot: std::sync::Arc<Snapshot>,
}

pub fn open(slot: i32, anchor: HWND) -> bool {
    close_all();
    let dpi = window::dpi_of(anchor);
    // 磁盘 / GPU 多一行设备标签（+ 总览最多 5–6 行），网络最高
    let h_dip = match slot { 2 => 716.0, 4 => 768.0, 3 => 690.0, _ => 648.0 };
    let w = zx_core::win::dpi_scale_i(W_DIP, dpi);
    let h = zx_core::win::dpi_scale_i(h_dip, dpi);
    let Some(hwnd) = window::create_window(CLASS, "驻匣", WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w, h) else { return false };

    // 设置里指定了某块盘 / 某块 GPU 就直接跳到它；没指定就停在"全部"总览。
    let cfg = zx_core::cfg::cfg();
    let devices = match slot {
        2 => crate::snapshot::latest().disks.len(),
        3 => crate::snapshot::latest().gpus.len(),
        _ => 0,
    };
    let wants_specific = match slot {
        2 => cfg.disk_display() == zx_core::MetricDisplayMode::Specific,
        3 => cfg.gpu_display() == zx_core::MetricDisplayMode::Specific,
        _ => false,
    };
    // 第一帧还没出时设备清单是空的，先挂起，等 WM_SNAPSHOT 再定位
    let tab_pending = devices == 0 && wants_specific;
    let tab = match slot {
        2 if wants_specific => (cfg.disk_display_index.unwrap_or(0) + 1).min(devices),
        3 if wants_specific => (cfg.gpu_display_index.unwrap_or(0) + 1).min(devices),
        _ => 0,
    };
    let mut d = Box::new(Detail {
        hwnd, slot, anchor, pinned: false,
        canvas: Canvas::new(w, h, dpi),
        dpi, w, h, tab, tab_pending, net_history: false, list_y0: 0.0,
        activated: false, hover: 0,
        snapshot: crate::snapshot::latest(),
    });
    window::set_window_icon(hwnd);
    let (x, y) = anchor_pos(anchor, w, h);
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetForegroundWindow(hwnd);
    }
    zx_ui::painter::set_round_region(hwnd, w, h, zx_core::win::dpi_scale_i(10.0, dpi));
    d.draw();
    // 网络面板打开期间才允许碰 wlanapi（那几个调用会点亮任务栏定位图标）
    if slot == 4 { crate::sampling::net::set_wifi_wanted(true); }
    let p = Box::into_raw(d);
    MAP.with(|m| m.borrow_mut().insert(hwnd.0 as isize, p));
    crate::runtime::register_snapshot_window(hwnd);
    true
}

/// 贴着任务栏的屏幕边缘弹出（底边任务栏 → 弹窗在它上方）。
fn anchor_pos(anchor: HWND, w: i32, h: i32) -> (i32, i32) {
    let work = window::work_area(anchor);
    let tb = zx_core::win::rect_of(anchor);
    let gap = 8;
    let (mut x, mut y) = match tb {
        Some(r) => {
            let cx = (r.left + r.right) / 2 - w / 2;
            let bottom_taskbar = r.top >= work.bottom - 80;
            let top_taskbar = r.bottom <= work.top + 80;
            if bottom_taskbar { (cx, r.top - h - gap) }
            else if top_taskbar { (cx, r.bottom + gap) }
            else if r.left < work.left + 80 { (r.right + gap, (r.top + r.bottom) / 2 - h / 2) }
            else { (r.left - w - gap, (r.top + r.bottom) / 2 - h / 2) }
        }
        None => (work.right - w - 16, work.bottom - h - 16),
    };
    x = x.clamp(work.left, (work.right - w).max(work.left));
    y = y.clamp(work.top, (work.bottom - h).max(work.top));
    (x, y)
}

pub fn close_all() {
    let list: Vec<HWND> = MAP.with(|m| m.borrow().values().map(|p| unsafe { (**p).hwnd }).collect());
    for h in list {
        unsafe { let _ = SendMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)); }
    }
}

pub fn refresh_all() {
    MAP.with(|m| {
        for p in m.borrow().values() {
            unsafe {
                let d = &mut **p;
                d.snapshot = crate::snapshot::latest();
                d.draw();
            }
        }
    });
}

fn icon_for(path: &Option<String>) -> *mut GpImage {
    let Some(p) = path else { return std::ptr::null_mut() };
    ICONS.with(|cache| {
        let mut c = cache.borrow_mut();
        if let Some(s) = c.get(p) { return s.img; }
        let mut img: *mut GpImage = std::ptr::null_mut();
        unsafe {
            let wide = zx_core::win::wide(p);
            let mut large = HICON::default();
            let mut small = HICON::default();
            let n = ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut large), Some(&mut small), 1);
            let pick = if n > 0 && !small.is_invalid() { small }
                       else if n > 0 && !large.is_invalid() { large } else { HICON::default() };
            if !pick.is_invalid() {
                let mut bmp: *mut windows::Win32::Graphics::GdiPlus::GpBitmap = std::ptr::null_mut();
                GdipCreateBitmapFromHICON(pick, &mut bmp);
                if !bmp.is_null() { img = bmp as *mut _; }
            }
            // ExtractIconEx 会把大/小两套都创建出来，**两个都要销毁**，
            // 只销毁用到的那一个会每次泄漏一个 HICON。
            if !small.is_invalid() { let _ = DestroyIcon(small); }
            if !large.is_invalid() { let _ = DestroyIcon(large); }
        }
        // 缓存超限时先把旧的 GDI+ 位图释放掉再清空，否则每清一次泄漏 96 张
        if c.len() > 96 {
            for slot in c.values() {
                if !slot.img.is_null() {
                    unsafe { GdipDisposeImage(slot.img); }
                }
            }
            c.clear();
        }
        c.insert(p.clone(), IconSlot { img });
        img
    })
}

impl Detail {
    fn pal(&self) -> theme::Palette { theme::for_window(crate::ui_light()) }

    fn draw(&mut self) {
        let k = self.dpi as f32 / 96.0;
        let pal = self.pal();
        let w = self.w as f32;
        let h = self.h as f32;
                self.canvas.clear(0);
        self.canvas.fill_round(0.0, 0.0, w, h, 10.0 * k, pal.card);
        self.canvas.stroke_round(0.5, 0.5, w - 1.0, h - 1.0, 10.0 * k, 1.0, pal.card_border);

        let snap = self.snapshot.clone();
        let (title, big, sub) = self.headline(&snap);
        let (pin_rect, close_rect) = self.header_buttons(k);

        // 标题
        self.canvas.text(&title, 18.0 * k, 0.0, 200.0 * k, HEADER_H * k, 15.0 * k, pal.text,
               TextAlign::Near, VAlign::Center);
        // 钉住 / 关闭
        self.icon_button(pin_rect, self.pinned, self.hover == 1, &pal, k);
        self.close_button(close_rect, self.hover == 2, &pal, k);

        // 大数值 + 副标题（网络页的"↑ x ↓ y"可能很长，按宽度自选字号）
        let big_px = fit_px(&mut self.canvas, &big, w - 36.0 * k, 36.0 * k, 0.55);
        self.canvas.text_ex(&big, 18.0 * k, 50.0 * k, w - 36.0 * k, 52.0 * k,
                  big_px, pal.text, TextAlign::Near, VAlign::Center, true, false);
        self.canvas.text_ex(&sub, 18.0 * k, 104.0 * k, w - 36.0 * k, 22.0 * k,
                  12.0 * k, pal.subtle, TextAlign::Near, VAlign::Center, false, true);

        // 图表
        let chart = (18.0 * k, 130.0 * k, w - 18.0 * k, 208.0 * k);
        self.draw_chart(&snap, chart, k, &pal);

        // 设备标签（磁盘 / GPU：全部 + 每台一个）
        let devices = self.device_count(&snap);
        let mut y = 220.0 * k;
        if devices > 0 {
            self.draw_tabs(&snap, devices, k, &pal);
            y = (TAB_Y + TAB_H + 10.0) * k;
        }

        if devices > 0 && self.tab == 0 {
            // 总览：每台设备一行（这就是"没指定具体设备时看到所有状态"）
            self.draw_overview(&snap, devices, y, k, &pal);
            y += devices as f32 * ROW_H * k;
        } else {
            // 信息行
            let rows = self.info_rows(&snap);
            for (label, value) in rows {
                self.canvas.text(&label, 18.0 * k, y, 130.0 * k, ROW_H * k, 12.5 * k, pal.subtle,
                       TextAlign::Near, VAlign::Center);
                self.canvas.text_ex(&value, 140.0 * k, y, w - 158.0 * k, ROW_H * k, 12.5 * k, pal.text,
                          TextAlign::Far, VAlign::Center, false, true);
                y += ROW_H * k;
            }
        }

        // 分隔 + 进程列表标题
        y += 6.0 * k;
        self.canvas.fill_rect(18.0 * k, y, w - 36.0 * k, 1.0, pal.separator);
        y += 10.0 * k;
        if self.slot == 4 {
            self.canvas.text("进程流量", 18.0 * k, y, 120.0 * k, 20.0 * k, 12.5 * k, pal.subtle,
                   TextAlign::Near, VAlign::Center);
            // 实时 / 累计 切换
            for (i, (label, active)) in [("实时", !self.net_history), ("累计", self.net_history)]
                .into_iter().enumerate() {
                let r = self.net_toggle_rect(i, k);
                if active {
                    self.canvas.fill_round(r.0, r.1, r.2, r.3, 5.0 * k, pal.highlight);
                }
                self.canvas.text(label, r.0, r.1, r.2, r.3, 11.0 * k,
                                 if active { pal.accent } else { pal.subtle },
                                 TextAlign::Center, VAlign::Center);
            }
        } else {
            self.canvas.text("高占用进程", 18.0 * k, y, 200.0 * k, 20.0 * k, 12.5 * k, pal.subtle,
                   TextAlign::Near, VAlign::Center);
        }
        y += 26.0 * k;
        self.list_y0 = y / k;

        if self.slot == 4 {
            self.draw_net_rows(&snap, y, k, &pal);
            if let Some(r) = zx_core::win::rect_of(self.hwnd) {
                self.canvas.present(self.hwnd, r.left, r.top);
            }
            return;
        }

        let (rows_data, col) = self.proc_rows(&snap);
        if rows_data.is_empty() {
            let hint = if self.slot == 4 && !crate::srum_ok() && !crate::etw_ok() {
                "按进程网速不可用（SRUM/ETW 都没启动，通常是没有管理员权限）"
            } else if self.slot == 4 {
                "当前没有进程产生网络流量"
            } else if self.slot == 3 && !snap.gpu_ok {
                "未检测到 GPU"
            } else {
                "当前没有可显示的进程数据"
            };
            self.canvas.text(hint, 18.0 * k, y + 4.0 * k, w - 36.0 * k, 24.0 * k,
                             11.0 * k, pal.subtle, TextAlign::Near, VAlign::Center);
        }
        for (i, p) in rows_data.iter().take(8).enumerate() {
            self.proc_row(p, col, 18.0 * k, y + i as f32 * 25.0 * k, w - 36.0 * k, 25.0 * k, k, &pal);
        }

        // 分层窗口的 present 需要窗口坐标：改为读实际位置
        if let Some(r) = zx_core::win::rect_of(self.hwnd) {
            self.canvas.present(self.hwnd, r.left, r.top);
        }
    }

    /// "实时 / 累计"两个切换按钮的矩形（DIP 坐标）。
    fn net_toggle_rect(&self, i: usize, k: f32) -> (f32, f32, f32, f32) {
        let bw = 46.0 * k;
        let bh = 20.0 * k;
        let right = self.w as f32 - 18.0 * k;
        let x = right - (2 - i) as f32 * (bw + 4.0 * k);
        let y = self.list_y0 * k - 28.0 * k;
        (x, y, bw, bh)
    }

    /// 行尾封禁按钮的矩形。
    fn net_shield_rect(&self, i: usize, k: f32) -> (f32, f32, f32, f32) {
        let sz = 18.0 * k;
        let right = self.w as f32 - 18.0 * k;
        let y = self.list_y0 * k + i as f32 * 25.0 * k + (25.0 * k - sz) / 2.0;
        (right - sz, y, sz, sz)
    }

    /// 网络列表的内容（实时 ↔ 累计两种数据源，统一成同一形状）。
    fn net_items(&self, s: &Snapshot) -> Vec<(String, String, String, String, u32, bool)> {
        if self.net_history {
            s.net_flows.iter().take(8).map(|f| (
                f.name.clone(), f.path.clone(),
                fmt::bytes(f.up as i64), fmt::bytes(f.down as i64),
                1u32, f.blocked,
            )).collect()
        } else {
            s.top_net.iter().take(8).map(|r| {
                let path = r.path.clone().unwrap_or_default();
                (
                    r.name.clone(), path.clone(),
                    fmt::net_rate(r.up), fmt::net_rate(r.down),
                    r.count.max(1), crate::netstat::is_blocked(&path),
                )
            }).collect()
        }
    }

    fn draw_net_rows(&mut self, s: &Snapshot, y0: f32, k: f32, pal: &theme::Palette) {
        let w = self.w as f32;
        let items = self.net_items(s);
        if items.is_empty() {
            let hint = if !crate::srum_ok() && !crate::etw_ok() {
                "按进程网速不可用（SRUM / ETW 都没启动，需要管理员权限运行）"
            } else {
                "当前没有进程产生网络流量"
            };
            self.canvas.text(hint, 18.0 * k, y0 + 4.0 * k, w - 36.0 * k, 40.0 * k,
                             11.0 * k, pal.subtle, TextAlign::Near, VAlign::Top);
            return;
        }
        for (i, (name, path, up, down, count, blocked)) in items.iter().enumerate() {
            let ry = y0 + i as f32 * 25.0 * k;
            self.proc_icon(path, 18.0 * k, ry + 4.5 * k, 16.0 * k, k, pal);
            let name_x = 44.0 * k;
            let shield = self.net_shield_rect(i, k);
            let text_w = (shield.0 - 8.0 * k - name_x).max(40.0 * k);
            self.canvas.text(name, name_x, ry, text_w, 25.0 * k, 12.0 * k, pal.text,
                             TextAlign::Near, VAlign::Center);
            // 右侧：↑ 上行 ↓ 下行
            let rx = shield.0 - 8.0 * k;
            let rw = 150.0 * k;
            self.canvas.text(&format!("↑ {}", up), rx - rw, ry, rw * 0.5, 25.0 * k,
                             11.0 * k, pal.subtle, TextAlign::Far, VAlign::Center);
            self.canvas.text(&format!("↓ {}", down), rx - rw * 0.5, ry, rw * 0.5, 25.0 * k,
                             11.0 * k, pal.subtle, TextAlign::Far, VAlign::Center);
            if *count > 1 {
                self.canvas.text(&format!("×{count}"), name_x + text_w - 30.0 * k, ry,
                                 30.0 * k, 25.0 * k, 10.0 * k, pal.subtle,
                                 TextAlign::Far, VAlign::Center);
            }
            self.draw_shield(shield, *blocked, path.is_empty(), k, pal);
        }
        // 底部：全机累计
        let mut fy = y0 + 8.0 * 25.0 * k + 4.0 * k;
        if self.net_history {
            self.canvas.text(
                &format!("全机累计  ↑ {}   ↓ {}",
                         fmt::bytes(s.net_total_up as i64), fmt::bytes(s.net_total_down as i64)),
                18.0 * k, fy, w - 36.0 * k, 20.0 * k, 11.0 * k, pal.subtle, TextAlign::Near, VAlign::Center);
            fy += 18.0 * k;
        }
        self.canvas.text("点行尾盾牌：加入 / 解除防火墙出站阻止（需管理员）",
                         18.0 * k, fy, w - 36.0 * k, 20.0 * k, 10.5 * k, pal.subtle,
                         TextAlign::Near, VAlign::Center);
        fy += 17.0 * k;
        self.canvas.text(&format!("共 {} 个进程有流量记录 · 列表按{}排序",
                                  s.net_flow_count,
                                  if self.net_history { "历史累计" } else { "实时速率" }),
                         18.0 * k, fy, w - 36.0 * k, 20.0 * k, 10.5 * k, pal.separator,
                         TextAlign::Near, VAlign::Center);
    }

    /// 行尾那个小盾牌：实心红 = 已阻止出站，灰色轮廓 = 未阻止。
    fn draw_shield(&mut self, r: (f32, f32, f32, f32), blocked: bool, disabled: bool, k: f32, pal: &theme::Palette) {
        let (x, y, w, h) = r;
        let cx = x + w / 2.0;
        let cy = y + h / 2.0;
        let color = if disabled {
            pal.separator
        } else if blocked {
            pal.danger
        } else {
            pal.subtle
        };
        // 盾牌轮廓：上方矩形 + 下方三角
        let bw = 7.0 * k;
        let bh = 3.0 * k;
        self.canvas.fill_rect(cx - bw, cy - bh - 3.0 * k, bw * 2.0, bh + 1.0 * k, color);
        self.canvas.fill_polygon(&[
            (cx - bw, cy - 2.0 * k), (cx + bw, cy - 2.0 * k),
            (cx + bw, cy + 1.0 * k), (cx, cy + 6.0 * k), (cx - bw, cy + 1.0 * k),
        ], color);
        if blocked {
            // 斜杠：明确的"禁止"
            self.canvas.line(cx - 6.0 * k, cy + 5.0 * k, cx + 6.0 * k, cy - 5.0 * k,
                             1.6 * k, pal.card);
        }
    }

    /// 磁盘 / GPU 有几台设备（其它槽位没有"多设备"概念，返回 0 表示不画标签）。
    fn device_count(&self, s: &Snapshot) -> usize {
        match self.slot {
            2 => s.disks.len(),
            3 => s.gpus.len(),
            _ => 0,
        }
    }

    /// 标签几何：与命中检测共用，避免画的和点的不一致。
    fn tab_rects(&self, devices: usize, k: f32) -> Vec<(f32, f32, f32, f32)> {
        let n = devices + 1; // "全部" + 每台设备
        let x0 = 18.0 * k;
        let total = self.w as f32 - 36.0 * k;
        let gap = TAB_GAP * k;
        // 标签太多时压缩宽度（最少 46 DIP，保证"全部"和盘符能看全）
        let tw = ((total - gap * (n as f32 - 1.0)) / n as f32).max(46.0 * k);
        (0..n).map(|i| (x0 + i as f32 * (tw + gap), TAB_Y * k, tw, TAB_H * k)).collect()
    }

    fn draw_tabs(&mut self, s: &Snapshot, devices: usize, k: f32, pal: &theme::Palette) {
        let rects = self.tab_rects(devices, k);
        for (i, r) in rects.iter().enumerate() {
            let active = i == self.tab;
            let hover = self.hover == 3 + i as i32;
            if active {
                self.canvas.fill_round(r.0, r.1, r.2, r.3, 6.0 * k, pal.highlight);
            } else if hover {
                self.canvas.fill_round(r.0, r.1, r.2, r.3, 6.0 * k, pal.hover);
            }
            let label = if i == 0 {
                "全部".to_string()
            } else {
                match self.slot {
                    2 => s.disks.get(i - 1).map(disk_letters).unwrap_or_else(|| format!("磁盘 {i}")),
                    _ => s.gpus.get(i - 1)
                        .map(|g| short_gpu(&g.name))
                        .unwrap_or_else(|| format!("GPU {i}")),
                }
            };
            let color = if active { pal.accent } else { pal.text };
            self.canvas.text(&label, r.0, r.1, r.2, r.3, 11.5 * k, color, TextAlign::Center, VAlign::Center);
        }
    }

    /// 总览：每台设备一行 —— 左"标识 + 名称"，右"关键指标"。
    fn draw_overview(&mut self, s: &Snapshot, devices: usize, y0: f32, k: f32, pal: &theme::Palette) {
        let w = self.w as f32;
        for i in 0..devices {
            let y = y0 + i as f32 * ROW_H * k;
            let (left, right) = match self.slot {
                2 => {
                    let d = match s.disks.get(i) { Some(d) => d, None => continue };
                    (format!("{}  {}", disk_letters(d), d.name),
                     // 总览只放"类型 · 容量 · 利用率"，读写留给具体盘页（一行塞不下会缩成一团）
                     format!("{} · {} · {}",
                             disk_kind_line(d),
                             if d.total_bytes > 0 { fmt::bytes(d.total_bytes as i64) } else { "—".into() },
                             fmt::pct(d.util)))
                }
                _ => {
                    let g = match s.gpus.get(i) { Some(g) => g, None => continue };
                    let temp = fmt::temp(if g.temp_valid { g.temp } else { f64::NAN },
                        zx_core::cfg::cfg().gpu_temp_unit() == zx_core::GpuTempUnit::Fahrenheit);
                    let mem = if g.dedicated_total > 0 || g.dedicated > 0 {
                        mem_pair(g.dedicated as u64, g.dedicated_total)
                    } else { "—".into() };
                    (g.name.clone(),
                     format!("{} · {} · {}", if s.gpu_ok { fmt::pct(g.util) } else { "--".into() }, temp, mem))
                }
            };
            // 左半：标识 + 名称（截断）
            let half = (w - 36.0 * k) * 0.46;
            self.canvas.text_ex(&left, 18.0 * k, y, half, ROW_H * k, 12.0 * k, pal.text,
                                TextAlign::Near, VAlign::Center, false, true);
            // 右半：指标（右对齐）
            self.canvas.text_ex(&right, 18.0 * k + half + 6.0 * k, y,
                                w - 36.0 * k - half - 6.0 * k, ROW_H * k, 12.0 * k, pal.subtle,
                                TextAlign::Far, VAlign::Center, false, true);
        }
    }

    fn headline(&self, s: &Snapshot) -> (String, String, String) {
        match self.slot {
            0 => ("CPU".to_string(), fmt::pct(s.cpu),
                  format!("{} · {}", if s.cpu_name.is_empty() { "—" } else { &s.cpu_name }, fmt::mhz(s.cpu_cur_mhz))),
            1 => ("内存".to_string(), fmt::pct(s.ram),
                  format!("已用 {} / 共 {}", fmt::gb(s.ram_used_gb), fmt::gb(s.ram_total_gb))),
            2 => {
                if s.disks.is_empty() {
                    return ("磁盘".into(), fmt::pct(s.disk_pct), "未检测到磁盘".into());
                }
                match self.tab {
                    0 => ("磁盘".into(), fmt::pct(s.disk_pct),
                          format!("全部 {} 块磁盘 · {}", s.disks.len(), disk_mode_text())),
                    n => match s.disks.get(n - 1) {
                        Some(d) => ("磁盘".into(), fmt::pct(d.util),
                                    format!("{} · {}", disk_letters(d), d.name)),
                        None => ("磁盘".into(), fmt::pct(s.disk_pct), "未检测到磁盘".into()),
                    },
                }
            }
            3 => {
                if s.gpus.is_empty() {
                    return ("GPU".into(), if s.gpu_ok { fmt::pct(s.gpu_pct) } else { "--".into() },
                            "未检测到 GPU".into());
                }
                match self.tab {
                    0 => ("GPU".into(), if s.gpu_ok { fmt::pct(s.gpu_pct) } else { "--".into() },
                          format!("全部 {} 个适配器 · {}", s.gpus.len(), gpu_mode_text())),
                    n => match s.gpus.get(n - 1) {
                        Some(g) => ("GPU".into(), if s.gpu_ok { fmt::pct(g.util) } else { "--".into() },
                                    g.name.clone()),
                        None => ("GPU".into(), if s.gpu_ok { fmt::pct(s.gpu_pct) } else { "--".into() },
                                 "未检测到 GPU".into()),
                    },
                }
            }
            _ => ("网络".to_string(),
                  format!("↑ {}  ↓ {}", fmt::net_rate(s.net_up), fmt::net_rate(s.net_down)),
                  if s.net_name.is_empty() { "未连接".into() } else { s.net_name.clone() }),
        }
    }

    fn header_buttons(&self, k: f32) -> (Rect, Rect) {
        let size = 28.0 * k;
        let top = ((HEADER_H * k) - size) / 2.0;
        let close = (self.w as f32 - 14.0 * k - size, top, size, size);
        let pin = (close.0 - 6.0 * k - size, top, size, size);
        (pin, close)
    }

    fn icon_button(&mut self, r: (f32, f32, f32, f32), active: bool, hover: bool, pal: &theme::Palette, k: f32) {
        let c = &mut self.canvas;
        if hover || active {
            c.fill_round(r.0, r.1, r.2, r.3, 6.0 * k, if active { pal.highlight } else { pal.hover });
        }
        // 图钉：一个简单的圆点 + 斜杆
        let cx = r.0 + r.2 / 2.0;
        let cy = r.1 + r.3 / 2.0;
        let color = if active { pal.accent } else { pal.subtle };
        c.fill_round(cx - 1.0 * k, cy - 7.0 * k, 2.0 * k, 8.5 * k, 1.0 * k, color);
        c.fill_round(cx - 4.5 * k, cy - 7.5 * k, 9.0 * k, 5.0 * k, 2.0 * k, color);
        c.fill_round(cx - 1.0 * k, cy, 2.0 * k, 7.0 * k, 1.0 * k, color);
    }

    fn close_button(&mut self, r: (f32, f32, f32, f32), hover: bool, pal: &theme::Palette, k: f32) {
        let c = &mut self.canvas;
        if hover {
            c.fill_round(r.0, r.1, r.2, r.3, 6.0 * k, 0x33C42B1C);
        }
        let cx = r.0 + r.2 / 2.0;
        let cy = r.1 + r.3 / 2.0;
        let color = if hover { pal.danger } else { pal.subtle };
        c.line(cx - 4.5 * k, cy - 4.5 * k, cx + 4.5 * k, cy + 4.5 * k, 1.4 * k, color);
        c.line(cx + 4.5 * k, cy - 4.5 * k, cx - 4.5 * k, cy + 4.5 * k, 1.4 * k, color);
    }

    fn draw_chart(&mut self, s: &Snapshot, r: (f32, f32, f32, f32), k: f32, pal: &theme::Palette) {
        let (x0, y0, x1, y1) = r;
        let c = &mut self.canvas;
        // 底纹
        c.fill_round(x0, y0, x1 - x0, y1 - y0, 6.0 * k, pal.track);
        for i in 1..4 {
            let y = y0 + (y1 - y0) * i as f32 / 4.0;
            c.fill_rect(x0 + 4.0 * k, y, x1 - x0 - 8.0 * k, 1.0, pal.separator);
        }
        let (a, b, max) = match self.slot {
            0 => (s.cpu_hist.clone(), Vec::new(), 100.0),
            1 => (s.ram_hist.clone(), Vec::new(), 100.0),
            2 => (s.disk_hist.clone(), Vec::new(), 100.0),
            3 => (s.gpu_hist.clone(), Vec::new(), 100.0),
            _ => (s.net_up_hist.iter().map(|v| *v as f64).collect(),
                  s.net_down_hist.iter().map(|v| *v as f64).collect(), -1.0),
        };
        let inner_w = x1 - x0 - 8.0 * k;
        let inner_h = y1 - y0 - 8.0 * k;

        if max < 0.0 {
            // 网络：上行在中线之上、下行在中线之下，两条对称的面积曲线
            let mid = y0 + (y1 - y0) / 2.0;
            let peak = a.iter().chain(b.iter()).copied().fold(1.0f64, f64::max).max(1.0);
            let area = |vals: &[f64]| -> Vec<(f32, f32)> {
                let cnt = vals.len().max(1);
                let step = inner_w / (cnt.max(2) - 1).max(1) as f32;
                vals.iter().enumerate().map(|(i, v)| {
                    let hh = (v / peak) as f32 * (inner_h / 2.0);
                    (x0 + 4.0 * k + step * i as f32, mid - hh)
                }).collect()
            };
            let up = area(&a);
            if up.len() >= 2 {
                let mut poly = up.clone();
                poly.push((up[up.len() - 1].0, mid));
                poly.push((up[0].0, mid));
                c.fill_polygon_stroked(&poly, 0x3045C8FF, 0xE645C8FF, 1.4 * k);
            }
            let down = area(&b);
            if down.len() >= 2 {
                let mut poly: Vec<(f32, f32)> = down.iter().map(|(x, y)| (*x, mid + (mid - y))).collect();
                poly.push((down[down.len() - 1].0, mid));
                poly.push((down[0].0, mid));
                c.fill_polygon_stroked(&poly, 0x30A78BFA, 0xE6A78BFA, 1.4 * k);
            }
        } else {
            // 单值：面积图（上半实心渐变感 + 顶边亮线），与上游手绘的面积图一致
            if a.len() >= 2 {
                let cnt = a.len();
                let step = inner_w / (cnt - 1) as f32;
                let base = y1 - 4.0 * k;
                let pts: Vec<(f32, f32)> = a.iter().enumerate().map(|(i, v)| {
                    let hh = (v / max).clamp(0.0, 1.0) as f32 * inner_h;
                    (x0 + 4.0 * k + step * i as f32, base - hh)
                }).collect();
                let mut poly = pts.clone();
                poly.push((pts[pts.len() - 1].0, base));
                poly.push((pts[0].0, base));
                c.fill_polygon(&poly, 0x3345C8FF);
                c.fill_polygon_stroked(&pts, 0x00000000, 0xF245C8FF, 1.6 * k);
            }
        }
    }

    fn info_rows(&self, s: &Snapshot) -> Vec<(String, String)> {
        match self.slot {
            0 => vec![
                ("温度".into(), if s.cpu_temp_valid {
                    fmt::temp(s.cpu_temp, false)
                } else {
                    "--".into()
                }),
                ("速度".into(), format!("{:.2} GHz", s.cpu_cur_mhz as f64 / 1000.0)),
                ("进程".into(), s.process_count.to_string()),
                ("线程".into(), s.thread_count.to_string()),
                ("句柄".into(), s.handle_count.to_string()),
                ("运行时间".into(), fmt::uptime(s.uptime_ms)),
            ],
            1 => vec![
                ("使用中(已压缩)".into(), fmt::bytes(s.mem.in_use)),
                ("可用".into(), fmt::bytes(s.mem.available)),
                ("已提交".into(), format!("{} / {}", fmt::bytes(s.mem.committed), fmt::bytes(s.mem.commit_limit))),
                ("已缓存".into(), fmt::bytes(s.mem.cached)),
                ("分页池".into(), fmt::bytes(s.mem.paged_pool)),
                ("非分页池".into(), fmt::bytes(s.mem.nonpaged_pool)),
            ],
            2 => {
                let d = s.disks.get(self.tab.saturating_sub(1)).cloned().unwrap_or_default();
                vec![
                    ("名称".into(), if d.name.is_empty() { "—".into() } else { d.name.clone() }),
                    ("类型".into(), disk_kind_line(&d)),
                    ("容量".into(), if d.total_bytes > 0 { fmt::bytes(d.total_bytes as i64) } else { "—".into() }),
                    ("利用率".into(), fmt::pct(d.util)),
                    ("读取速度".into(), fmt::net_rate(d.read)),
                    ("写入速度".into(), fmt::net_rate(d.write)),
                    ("平均响应时间".into(), format!("{:.1} ms", d.response_ms)),
                ]
            }
            3 => {
                let g = s.gpus.get(self.tab.saturating_sub(1)).cloned().unwrap_or_default();
                vec![
                    ("名称".into(), if g.name.is_empty() { "—".into() } else { g.name.clone() }),
                    ("利用率".into(), if s.gpu_ok { fmt::pct(g.util) } else { "--".into() }),
                    ("温度".into(), fmt::temp(if g.temp_valid { g.temp } else { f64::NAN },
                        zx_core::cfg::cfg().gpu_temp_unit() == zx_core::GpuTempUnit::Fahrenheit)),
                    ("负载类型".into(), if g.engine.is_empty() { "—".into() } else { g.engine.clone() }),
                    ("专用 GPU 内存".into(), mem_pair(g.dedicated as u64, g.dedicated_total)),
                    ("共享 GPU 内存".into(), mem_pair(g.shared as u64, g.shared_total)),
                ]
            }
            _ => {
                let n = &s.net_info;
                vec![
                    ("连接".into(), or_dash(&n.connection_type)),
                    ("速率".into(), fmt::link_rate(n.link_rx_bps)),
                    ("标准".into(), n.standard.clone().unwrap_or_else(|| "—".into())),
                    ("频段".into(), n.band.clone().unwrap_or_else(|| "—".into())),
                    ("本机 IPv4".into(), n.local_ip.clone().unwrap_or_else(|| "—".into())),
                    ("公网 IPv4".into(), n.public_ip.clone().unwrap_or_else(|| "—".into())),
                    ("公网 IPv6".into(), n.public_ip_v6.clone().unwrap_or_else(|| "—".into())),
                    ("本地延迟".into(), if n.gateway_rtt_ms >= 0 { format!("{} ms", n.gateway_rtt_ms) } else { "—".into() }),
                    ("公网延迟".into(), if n.public_rtt_ms >= 0 { format!("{} ms", n.public_rtt_ms) } else { "—".into() }),
                ]
            }
        }
    }

    fn proc_rows<'a>(&self, s: &'a Snapshot) -> (&'a [ProcRow], usize) {
        match self.slot {
            0 => (&s.top_cpu, 0),
            1 => (&s.top_mem, 1),
            2 => (&s.top_disk, 2),
            3 => (&s.top_gpu, 3),
            _ => (&s.top_net, 4),
        }
    }

    /// 只画进程图标（网络行单独排版，不共用 proc_row）。
    fn proc_icon(&mut self, path: &str, x: f32, y: f32, sz: f32, k: f32, pal: &theme::Palette) {
        let owned = if path.is_empty() { None } else { Some(path.to_string()) };
        let img = icon_for(&owned);
        if !img.is_null() {
            self.canvas.draw_image(img, x, y, sz, sz);
        } else {
            self.canvas.fill_round(x, y, sz, sz, 4.0 * k, pal.track);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn proc_row(&mut self, p: &ProcRow, col: usize, x: f32, y: f32, w: f32, h: f32, k: f32, pal: &theme::Palette) {
        let c = &mut self.canvas;
        let img = icon_for(&p.path);
        if !img.is_null() {
            c.draw_image(img, x, y + (h - 16.0 * k) / 2.0, 16.0 * k, 16.0 * k);
        } else {
            c.fill_round(x, y + (h - 16.0 * k) / 2.0, 16.0 * k, 16.0 * k, 4.0 * k, pal.track);
        }
        let name_x = x + 22.0 * k;
        let value_w = if self.slot == 2 || self.slot == 4 { 150.0 * k } else { 96.0 * k };
        c.text_ex(&p.name, name_x, y, w - 22.0 * k - value_w - 8.0 * k, h, 12.5 * k, pal.text,
                  TextAlign::Near, VAlign::Center, false, true);
        if let Some(tag) = &p.tag {
            let tw = c.measure(tag, 10.0 * k, false) + 8.0 * k;
            c.fill_round(x + w - value_w - tw - 6.0 * k, y + (h - 15.0 * k) / 2.0, tw, 15.0 * k, 7.0 * k, pal.track);
            c.text(tag, x + w - value_w - tw - 6.0 * k, y, tw, h, 10.0 * k, pal.subtle,
                   TextAlign::Center, VAlign::Center);
        }
        let value = match col {
            0 => fmt::pct(p.cpu),
            1 => fmt::bytes(p.working_set),
            2 => format!("R {}  W {}", fmt::net_rate_compact(p.disk_read), fmt::net_rate_compact(p.disk_write)),
            3 => format!("{} {}", fmt::pct(p.gpu), p.gpu_engine),
            _ => format!("↑{} ↓{}", fmt::net_rate_compact(p.up), fmt::net_rate_compact(p.down)),
        };
        c.text_ex(&value, x + w - value_w, y, value_w, h, 12.5 * k, pal.text,
                  TextAlign::Far, VAlign::Center, false, true);
    }
}

/// 把文本缩到能放进 `avail` 宽度（最小 `min_ratio` 倍字号）。
fn fit_px(c: &mut Canvas, s: &str, avail: f32, px: f32, min_ratio: f32) -> f32 {
    if avail <= 0.0 { return px * min_ratio; }
    let mut f = px;
    while f > px * min_ratio && c.measure(s, f, true) > avail {
        f -= 1.0;
    }
    f
}

fn or_dash(s: &str) -> String { if s.is_empty() { "—".into() } else { s.into() } }

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    match msg {
        WM_NCCREATE => LRESULT(1),
        WM_SNAPSHOT => {
            if let Some(d) = get(hwnd) {
                d.snapshot = crate::snapshot::latest();
                // 设备清单要等采样线程真出过一帧才有；在那之前一直挂起
                if d.tab_pending {
                    let n = d.device_count(&d.snapshot.clone());
                    if n > 0 {
                        d.tab_pending = false;
                        let cfg = zx_core::cfg::cfg();
                        let idx = if d.slot == 2 { cfg.disk_display_index } else { cfg.gpu_display_index };
                        d.tab = (idx.unwrap_or(0) + 1).min(n);
                    }
                }
                d.draw();
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let active = (w.0 & 0xFFFF) != 0;
            if let Some(d) = get(hwnd) {
                if active {
                    d.activated = true;
                } else if d.activated && !d.pinned {
                    // 只有"曾经激活过再失焦"才关——SetForegroundWindow 被系统拒绝时
                    // 会立刻收到一次 WA_INACTIVE，那不该当成用户点走了。
                    unsafe { let _ = DestroyWindow(hwnd); }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(d) = get(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                let k = d.dpi as f32 / 96.0;
                let (pin, close) = d.header_buttons(k);
                let hit = if in_rect(x, y, pin) { 1 }
                    else if in_rect(x, y, close) { 2 } else { 0 };
                if hit != d.hover { d.hover = hit; d.draw(); }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(d) = get(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                let k = d.dpi as f32 / 96.0;
                let (pin, close) = d.header_buttons(k);
                if in_rect(x, y, close) {
                    unsafe { let _ = DestroyWindow(hwnd); }
                    return LRESULT(0);
                }
                if in_rect(x, y, pin) {
                    d.pinned = !d.pinned;
                    d.draw();
                    return LRESULT(0);
                }
                // 切标签（磁盘 / GPU：全部 + 每台设备）
                let devices = d.device_count(&d.snapshot.clone());
                if devices > 0 {
                    for (i, r) in d.tab_rects(devices, k).iter().enumerate() {
                        if (x as f32) >= r.0 && (x as f32) < r.0 + r.2
                            && (y as f32) >= r.1 && (y as f32) < r.1 + r.3 {
                            if d.tab != i { d.tab = i; d.draw(); }
                            break;
                        }
                    }
                }
                // 网络页：实时 / 累计切换 + 行尾封禁按钮
                if d.slot == 4 {
                    let mut redraw = false;
                    for i in 0..2usize {
                        let r = d.net_toggle_rect(i, k);
                        if in_rect(x, y, r) {
                            let want = i == 1;
                            if d.net_history != want { d.net_history = want; redraw = true; }
                        }
                    }
                    if !redraw {
                        let snap = d.snapshot.clone();
                        let items = d.net_items(&snap);
                        for (i, (name, path, ..)) in items.iter().enumerate() {
                            let r = d.net_shield_rect(i, k);
                            if !in_rect(x, y, r) { continue; }
                            if path.is_empty() {
                                zx_core::log::warn(format!("无法读取 {name} 的可执行文件路径，不能加防火墙规则"));
                            } else {
                                // netsh 要 300–600ms，绝不能占着 UI 线程；
                                // 后台改完再让窗口重绘一次（盾牌颜色从 netstat 实时读）。
                                let want = !crate::netstat::is_blocked(path);
                                let path = path.clone();
                                let name = name.clone();
                                let h = hwnd.0 as isize;
                                std::thread::spawn(move || {
                                    if crate::netstat::set_blocked(&path, want) {
                                        zx_core::log::info(format!("{} 出站联网：{}",
                                            if want { "阻止" } else { "恢复" }, name));
                                    }
                                    unsafe {
                                        let _ = PostMessageW(HWND(h as *mut _), WM_SNAPSHOT,
                                                             WPARAM(0), LPARAM(0));
                                    }
                                });
                                // 先重绘一次让点击有即时反馈（盾牌此时还是旧状态）
                                redraw = true;
                                break;
                            }
                        }
                    }
                    if redraw { d.draw(); }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let p = MAP.with(|m| m.borrow_mut().remove(&(hwnd.0 as isize)));
            if let Some(p) = p {
                crate::runtime::unregister_snapshot_window(hwnd);
                if unsafe { (*p).slot } == 4 { crate::sampling::net::set_wifi_wanted(false); }
                crate::app::set_selected(-1);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn in_rect(x: i32, y: i32, r: (f32, f32, f32, f32)) -> bool {
    (x as f32) >= r.0 && (x as f32) <= r.0 + r.2 && (y as f32) >= r.1 && (y as f32) <= r.1 + r.3
}

fn get(hwnd: HWND) -> Option<&'static mut Detail> {
    MAP.with(|m| m.borrow().get(&(hwnd.0 as isize)).map(|p| unsafe { &mut **p }))
}

pub fn register() {
    window::register_class(CLASS, wndproc, None);
}

// ---------------- 设备标签用的小工具 ----------------

/// 磁盘的盘符（取第一个）：`"C: D:"` → `"C:"`；没有盘符时给个短标识。
fn disk_letters(d: &crate::snapshot::DiskInfo) -> String {
    d.letters.split_whitespace().next()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("磁盘 {}", d.index + 1))
}

/// 显存"已用 / 总量"。总量拿不到（驱动不报）时只显示已用。
fn mem_pair(used: u64, total: u64) -> String {
    if total > 0 {
        format!("{} / {}", fmt::bytes(used as i64), fmt::bytes(total as i64))
    } else {
        fmt::bytes(used as i64)
    }
}

/// "HDD · GPT · NTFS" —— 介质类型 / 分区表 / 文件系统
fn disk_kind_line(d: &crate::snapshot::DiskInfo) -> String {
    let mut parts = vec![fmt::disk_kind(d.kind).to_string()];
    match d.part_style {
        2 => parts.push("GPT".into()),
        1 => parts.push("MBR".into()),
        _ => {}
    }
    if !d.fs.is_empty() { parts.push(d.fs.clone()); }
    parts.join(" · ")
}

fn disk_mode_text() -> &'static str {
    match zx_core::cfg::cfg().disk_display() {
        zx_core::MetricDisplayMode::Max => "最大值",
        zx_core::MetricDisplayMode::Specific => "指定磁盘",
        zx_core::MetricDisplayMode::Average => "均值",
    }
}

fn gpu_mode_text() -> &'static str {
    match zx_core::cfg::cfg().gpu_display() {
        zx_core::MetricDisplayMode::Max => "最大值",
        zx_core::MetricDisplayMode::Specific => "指定 GPU",
        zx_core::MetricDisplayMode::Average => "均值",
    }
}

/// 标签里的短名：去掉厂商前缀，太长就截断（标签只有 60–90 DIP 宽）。
fn short_gpu(name: &str) -> String {
    let mut s = name;
    for p in ["NVIDIA GeForce ", "NVIDIA ", "AMD Radeon(TM) ", "AMD Radeon ", "AMD ", "Intel(R) ", "Microsoft "] {
        if let Some(r) = s.strip_prefix(p) { s = r; break; }
    }
    if s.chars().count() > 12 {
        let t: String = s.chars().take(11).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}
