//! 卡片式对话框：提权同意、二次启动提示、首次启动引导、更新提醒。
//!
//! 一个类、一张画布、若干按钮——比引入对话框框架便宜得多，也比 MessageBox 好看。

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use zx_ui::painter::Canvas;
use zx_ui::theme;
use zx_ui::window;
use zx_ui::{TextAlign, VAlign};

const CLASS: &str = "ZhuxiaDialog";

struct Dlg {
    hwnd: HWND,
    canvas: Canvas,
    dpi: u32,
    w: i32,
    h: i32,
    title: String,
    body: Vec<String>,
    buttons: Vec<String>,
    hover: usize,
    result: usize,
    done: bool,
}

thread_local! {
    static D: RefCell<Option<*mut Dlg>> = const { RefCell::new(None) };
}

/// 阻塞式卡片对话框，返回被点中的按钮下标（Esc/关闭 = 最后一个按钮）。
pub fn card(title: &str, body: &[&str], buttons: &[&str]) -> usize {
    window::register_class(CLASS, wndproc, None);
    let body_lines: Vec<String> = body.iter().map(|s| s.to_string()).collect();
    let h = 150 + body_lines.len() as i32 * 22 + 60;
    let dpi = window::dpi_of(unsafe { GetForegroundWindow() });
    let w = zx_core::win::dpi_scale_i(430.0, dpi);
    let h = zx_core::win::dpi_scale_i(h as f32, dpi);
    let Some(hwnd) = window::create_window(CLASS, title, WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w, h) else { return 0 };

    let d = Box::new(Dlg {
        hwnd,
        canvas: Canvas::new(w, h, dpi),
        dpi, w, h,
        title: title.to_string(),
        body: body_lines,
        buttons: buttons.iter().map(|s| s.to_string()).collect(),
        hover: usize::MAX, result: buttons.len().saturating_sub(1), done: false,
    });
    window::set_window_icon(hwnd);
    D.with(|s| *s.borrow_mut() = Some(Box::into_raw(d)));
    zx_ui::painter::set_round_region(hwnd, w, h, zx_core::win::dpi_scale_i(12.0, dpi));
    if let Some(d) = get(hwnd) { d.draw(); }
    unsafe {
        let work = window::work_area(hwnd);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, (work.left + work.right - w) / 2, (work.top + work.bottom - h) / 2,
                             w, h, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
    }
    // 局部消息循环，直到本对话框关掉
    unsafe {
        let mut msg = MSG::default();
        while !done(hwnd) {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if !r.as_bool() { break; }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let result = get(hwnd).map(|d| d.result).unwrap_or(0);
    let _ = get(hwnd);
    D.with(|s| {
        if let Some(p) = s.borrow_mut().take() {
            unsafe { drop(Box::from_raw(p)); }
        }
    });
    result
}

fn done(hwnd: HWND) -> bool { get(hwnd).map(|d| d.done).unwrap_or(true) }

fn get(hwnd: HWND) -> Option<&'static mut Dlg> {
    D.with(|s| match *s.borrow() { Some(p) if unsafe { (*p).hwnd } == hwnd => Some(unsafe { &mut *p }), _ => None })
}

impl Dlg {
    fn draw(&mut self) {
        let k = self.dpi as f32 / 96.0;
        let pal = theme::for_window(crate::ui_light());
        let (w, h) = (self.w as f32, self.h as f32);
        let c = &mut self.canvas;
        c.clear(0);
        c.fill_round(0.0, 0.0, w, h, 12.0 * k, pal.card);
        c.stroke_round(0.5, 0.5, w - 1.0, h - 1.0, 12.0 * k, 1.0, pal.card_border);
        c.text_ex(&self.title, 22.0 * k, 22.0 * k, w - 44.0 * k, 30.0 * k, 16.0 * k, pal.text,
                  TextAlign::Near, VAlign::Center, true, false);
        let mut y = 66.0 * k;
        for line in &self.body {
            c.text(line, 22.0 * k, y, w - 44.0 * k, 22.0 * k, 12.0 * k, pal.subtle,
                   TextAlign::Near, VAlign::Center);
            y += 22.0 * k;
        }
        // 按钮（右下）
        let mut bx = w - 22.0 * k;
        for (i, b) in self.buttons.iter().enumerate().rev() {
            let bw = (c.measure(b, 12.0 * k, false) + 34.0 * k).max(72.0 * k);
            bx -= bw;
            let r = (bx, h - 22.0 * k - 32.0 * k, bw, 32.0 * k);
            let primary = i == self.buttons.len() - 1;
            let bg = if self.hover == i {
                if primary { pal.accent } else { pal.highlight }
            } else if primary { pal.accent } else { pal.track };
            c.fill_round(r.0, r.1, r.2, r.3, 6.0 * k, bg);
            c.text(b, r.0, r.1, r.2, r.3, 12.0 * k,
                   if primary { 0xFFFFFFFF } else { pal.text }, TextAlign::Center, VAlign::Center);
            bx -= 10.0 * k;
        }
        if let Some(r) = zx_core::win::rect_of(self.hwnd) {
            c.present(self.hwnd, r.left, r.top);
        }
    }

    fn button_at(&self, x: i32, y: i32) -> Option<usize> {
        let k = self.dpi as f32 / 96.0;
        let w = self.w as f32;
        let h = self.h as f32;
        let mut bx = w - 22.0 * k;
        for (i, b) in self.buttons.iter().enumerate().rev() {
            let bw = (b.chars().count() as f32 * 12.0 * k + 34.0 * k).max(72.0 * k);
            bx -= bw;
            let r = (bx, h - 54.0 * k, bw, 32.0 * k);
            if (x as f32) >= r.0 && (x as f32) <= r.0 + r.2 && (y as f32) >= r.1 && (y as f32) <= r.1 + r.3 {
                return Some(i);
            }
            bx -= 10.0 * k;
        }
        None
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => LRESULT(1),
        WM_MOUSEMOVE => {
            if let Some(d) = get(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                let hit = d.button_at(x, y).unwrap_or(usize::MAX);
                if hit != d.hover { d.hover = hit; d.draw(); }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(d) = get(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
                if let Some(i) = d.button_at(x, y) { d.result = i; d.done = true; }
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if w.0 == 0x1B { // Esc
                if let Some(d) = get(hwnd) { d.done = true; let _ = DestroyWindow(hwnd); }
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// 提权同意：返回是否允许。
pub fn consent() -> bool {
    let i = card("驻匣需要管理员权限",
        &["部分指标（按进程网络、部分磁盘与 GPU 计数）需要提权才能读取。",
          "允许后，驻匣每次启动都会请求管理员权限；", "程序本身不做任何联网写操作（公网 IP 查询可在设置中关闭）。"],
        &["不允许", "允许"]);
    i == 1
}

/// 二次启动提示。
pub fn already_running() {
    let _ = card("驻匣已在运行", &["任务栏上的监视条已经在工作。", "如需打开设置，请右键点击它。"], &["知道了"]);
}

/// 首次启动引导：0 = 任务栏窗口，1 = 悬浮窗。
pub fn guide() -> usize {
    card("选择显示方式",
        &["任务栏窗口：贴着任务栏常驻，最省地方（推荐）。", "悬浮窗：可拖动的桌面卡片，适合隐藏任务栏的场景。"],
        &["任务栏窗口", "悬浮窗"])
}

/// 首次启动引导第二步：是否用驻匣接管 Win+V。
///
/// 接管 = 用低级键盘钩子吞掉系统快捷键，改成弹出驻匣的剪贴板面板。这属于
/// "改了系统的默认行为"，所以不在默认值里替用户决定，第一次就问清楚；
/// 之后随时能在「剪贴板 → 接管 Win+V」里改。返回是否接管。
pub fn guide_win_v() -> bool {
    let i = card("要接管 Win+V 吗",
        &["Windows 自带的剪贴板历史只有 25 条、不能搜索、不能收藏。",
          "接管后按 Win+V 弹出驻匣的剪贴板面板，系统面板不再出现。",
          "不接管也照用：驻匣的剪贴板用 Alt+C 打开。"],
        &["不接管", "接管 Win+V"]);
    i == 1
}

/// 更新提醒：返回 0 = 立即更新，1 = 稍后，2 = 不再提醒。
pub fn update(version: &str) -> usize {
    card("发现新版本",
        &[&format!("最新版本：{version}"), "是否打开下载页面？"],
        &["不再提醒", "稍后", "立即更新"])
}
