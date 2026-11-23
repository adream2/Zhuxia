//! 面板截图探针：按窗口标题找窗口 → 拉到前台 → 抓它的矩形存 PNG。
//!
//! 用途：验证设置页/对话框的排版（不依赖系统截图工具，也不用 Add-Type）。
//!
//! ```bash
//! cargo run -p capture --example panel_shot -- "驻匣 · 设置" out.png
//! ```

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SetCursorPos, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_NOTOPMOST,
    HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let title = args.next().unwrap_or_else(|| "驻匣 · 设置".to_string());
    let out = args.next().unwrap_or_else(|| "panel.png".to_string());
    if title == "--screen" {
        let Some(frame) = capture::screen::Frame::grab() else {
            eprintln!("抓屏失败");
            std::process::exit(1);
        };
        let b = frame.bounds();
        let (w, h) = (b.right - b.left, b.bottom - b.top);
        let mut canvas = zx_ui::Canvas::new(w, h, 96);
        frame.blit_into(&mut canvas);
        let Some(png) = canvas.png_bytes() else { std::process::exit(1) };
        std::fs::write(&out, png).ok();
        println!("saved {out}  {w}x{h}");
        return;
    }
    // 第三个参数：向上滚几格（验证长页面的下半部分）
    let scroll: i32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);

    let title_w = zx_core::win::wide(&title);
    let hwnd = unsafe {
        windows::Win32::UI::WindowsAndMessaging::FindWindowW(
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR(title_w.as_ptr()),
        )
    };
    let Ok(hwnd) = hwnd else {
        eprintln!("没找到标题为「{title}」的窗口");
        std::process::exit(1);
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        // 前台权限可能被别的窗口抢走：临时提到 topmost，抓完再还原
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
    }
    std::thread::sleep(std::time::Duration::from_millis(500));

    if scroll != 0 {
        let mut r0 = RECT::default();
        unsafe { let _ = GetWindowRect(hwnd, &mut r0 as *mut _); }
        unsafe { let _ = SetCursorPos((r0.left + r0.right) / 2, (r0.top + r0.bottom) / 2); }
        unsafe {
            for _ in 0..scroll.abs() {
                let input = INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: 0,
                            dy: 0,
                            mouseData: if scroll > 0 { -120i32 } else { 120 } as u32,
                            dwFlags: MOUSEEVENTF_WHEEL,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    let mut r = RECT::default();
    unsafe { let _ = GetWindowRect(hwnd, &mut r as *mut _); }
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    if w <= 0 || h <= 0 {
        eprintln!("窗口矩形不合法：{w}x{h}");
        std::process::exit(1);
    }
    let Some(frame) = capture::screen::Frame::grab_rect(r) else {
        eprintln!("抓屏失败（安全桌面/被遮挡？）");
        std::process::exit(1);
    };
    let mut canvas = zx_ui::Canvas::new(w, h, 96);
    frame.blit_into(&mut canvas);
    let Some(png) = canvas.png_bytes() else {
        eprintln!("PNG 编码失败");
        std::process::exit(1);
    };
    unsafe { let _ = SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE); }
    if let Err(e) = std::fs::write(&out, png) {
        eprintln!("写文件失败：{e}");
        std::process::exit(1);
    }
    println!("saved {out}  {w}x{h}  hwnd={:?}", HWND(hwnd.0));
}
