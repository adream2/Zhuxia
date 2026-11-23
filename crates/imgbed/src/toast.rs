//! 上传结果的右下角卡片。
//!
//! 形态与图片转换/图标提取一致（同一套"过一眼就走"的语言）：不抢焦点、4.5 秒消失、
//! 点一下立刻关。区别只有一处：链接可能很长，所以第一行按宽度截断，
//! 完整内容已经在剪贴板里了——卡片的任务是告诉用户"成了"，不是给他复制。

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSY, ReleaseDC};
use windows::Win32::UI::WindowsAndMessaging::*;

use zx_ui::painter::Canvas;
use zx_ui::theme;
use zx_ui::window;
use zx_ui::{TextAlign, VAlign};

const CLASS: &str = "ZhuxiaImgBedToast";
const WM_DISMISS: u32 = WM_APP + 78;
static BUSY: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
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

/// 弹一张卡片并在当前线程等它消失（命令行兜底路径用）。
pub fn show_blocking(title: &str, lines: &[String], error: bool) {
    BUSY.store(true, Ordering::Release);
    run(title, lines, error);
    BUSY.store(false, Ordering::Release);
}

pub fn show(title: &str, lines: &[String], error: bool) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let title = title.to_string();
    let lines: Vec<String> = lines.to_vec();
    let spawned = std::thread::Builder::new()
        .name("zx-imgbed-toast".into())
        .spawn(move || {
            run(&title, &lines, error);
            BUSY.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::Release);
    }
}

fn run(title: &str, lines: &[String], error: bool) {
    window::register_class(CLASS, wndproc, None);

    let dpi = unsafe {
        let dc = GetDC(None);
        let d = GetDeviceCaps(dc, LOGPIXELSY) as u32;
        ReleaseDC(None, dc);
        d.clamp(96, 288)
    };
    let scale = dpi as f32 / 96.0;
    let w = (468.0 * scale) as i32;
    let h = (60.0 * scale) as i32 + lines.len() as i32 * (21.0 * scale) as i32;

    let Some(hwnd) = window::create_window(
        CLASS,
        "驻匣 · 图床",
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
        SetTimer(hwnd, 1, 4500, None);
        window::run_loop();
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
