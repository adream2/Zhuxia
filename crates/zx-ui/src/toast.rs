//! 结果提示卡：右下角一张不抢焦点的卡片，几秒后自动消失，点一下立刻关。
//!
//! 为什么不用 MessageBox：批量处理 20 个文件，弹 20 个模态框没法用；而"做完了"
//! 如果一点反馈都没有，用户会怀疑右键没生效。卡片是这两者之间的唯一合理形态。
//!
//! 这份实现原来在 imgconv / appicon / imgbed 里各抄了一份（148 行 × 3），
//! 连"几秒消失"都开始漂（4.0 / 4.2）。现在只留这一份，各模块用 `Style` 描述外观差异。

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, LOGPIXELSY, GetDeviceCaps};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::painter::Canvas;
use crate::theme;
use crate::window;
use crate::{TextAlign, VAlign};

/// 一张卡片的身份：窗口类名、线程名、默认标题、停留时长。
///
/// 各模块自带 `BUSY`：同一模块的卡片不叠，但"转换完成"和"上传完成"互不干扰
/// （与拆分前行为一致）。
pub struct Style {
    pub class: &'static str,
    pub thread: &'static str,
    pub title: &'static str,
    pub timeout_ms: u32,
    /// 卡片宽度（逻辑像素，按 DPI 缩放）。图床那张更宽，因为要放长 URL。
    pub width: f32,
    /// 标题区高度（逻辑像素）；正文每行再叠加 21。
    pub height_base: f32,
    pub busy: &'static AtomicBool,
}

const WM_DISMISS: u32 = WM_APP + 77;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    crate::guard_proc!(DefWindowProcW(hwnd, msg, wp, lp), { wndproc_inner(hwnd, msg, wp, lp) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER | WM_LBUTTONUP | WM_DISMISS => {
            unsafe {
                let _ = KillTimer(hwnd, 1);
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0); }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// 弹一张卡片并在当前线程等它消失（命令行兜底路径用：进程不能先退出）。
pub fn show_blocking(style: &'static Style, title: &str, lines: &[String], error: bool) {
    style.busy.store(true, Ordering::Release);
    run(style, title, lines, error);
    style.busy.store(false, Ordering::Release);
}

/// 弹一张卡片。`error = true` 时左侧竖条用警示色。
pub fn show(style: &'static Style, title: &str, lines: &[String], error: bool) {
    if style.busy.swap(true, Ordering::AcqRel) {
        // 上一张还在：直接丢掉，避免卡片叠成一摞
        return;
    }
    let title = title.to_string();
    let lines: Vec<String> = lines.to_vec();
    let spawned = std::thread::Builder::new()
        .name(style.thread.to_string())
        .spawn(move || {
            run(style, &title, &lines, error);
            style.busy.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        style.busy.store(false, Ordering::Release);
    }
}

fn run(style: &'static Style, title: &str, lines: &[String], error: bool) {
    window::register_class(style.class, wndproc, None);

    let dpi = unsafe {
        let dc = GetDC(None);
        let d = GetDeviceCaps(dc, LOGPIXELSY) as u32;
        ReleaseDC(None, dc);
        d.clamp(96, 288)
    };
    let scale = dpi as f32 / 96.0;
    let w = (style.width * scale) as i32;
    // 标题 + 每行 20px + 上下 padding
    let h = (style.height_base * scale) as i32 + lines.len() as i32 * (21.0 * scale) as i32;

    let Some(hwnd) = window::create_window(
        style.class,
        style.title,
        WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        w,
        h,
    ) else {
        return;
    };

    let work = window::work_area(hwnd);
    let x = work.right - w - (18.0 * scale) as i32;
    let y = work.bottom - h - (18.0 * scale) as i32;

    let light = zx_core::win::apps_light();
    let p = theme::for_window(light);
    let mut c = Canvas::new(w, h, dpi);
    c.clear(0);
    // 卡片底 + 细边 + 左侧 3px 状态条
    c.fill_round(0.0, 0.0, w as f32, h as f32, 10.0 * scale, p.card_solid);
    c.stroke_round(0.5, 0.5, w as f32 - 1.0, h as f32 - 1.0, 10.0 * scale, 1.0, p.card_border);
    let bar = if error { p.danger } else { p.accent };
    c.fill_round(1.0, 1.0, 3.0 * scale, h as f32 - 2.0, 2.0 * scale, bar);

    let pad = 16.0 * scale;
    c.text(
        title,
        pad + 4.0 * scale,
        11.0 * scale,
        w as f32 - pad * 2.0,
        24.0 * scale,
        (14.0 * scale).round(),
        p.text,
        TextAlign::Near,
        VAlign::Top,
    );
    for (i, line) in lines.iter().enumerate() {
        c.text(
            line,
            pad + 4.0 * scale,
            34.0 * scale + i as f32 * 21.0 * scale,
            w as f32 - pad * 2.0,
            20.0 * scale,
            (12.5 * scale).round(),
            p.label,
            TextAlign::Near,
            VAlign::Top,
        );
    }
    c.present(hwnd, x, y);

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, 1, style.timeout_ms, None);
        // 卡片必须在自己的线程里跑消息循环（CreateWindow 在哪条线程，循环就得在哪条）
        window::run_loop();
    }
}

/// 让外部能主动收掉（退出时用）。
pub fn dismiss(class: &'static str) {
    let cls = zx_core::win::wide(class);
    unsafe {
        if let Ok(h) = FindWindowW(windows::core::PCWSTR(cls.as_ptr()), windows::core::PCWSTR::null()) {
            let _ = PostMessageW(h, WM_DISMISS, WPARAM(0), LPARAM(0));
        }
    }
}

/// 截断到适合卡片宽度的长度（不是精确排版，够用就行）。
pub fn ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{head}…")
}
