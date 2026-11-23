//! 界面探针：把截图工具条 / OCR 结果窗口真正显示出来，抓屏存 PNG，用来肉眼核对。
//!
//! `cargo run --release --example ui_probe -- toolbar out.png`
//! `cargo run --release --example ui_probe -- ocr out.png`
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_CLOSE,
};
use zx_ui::Canvas;

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut msg = MSG::default();
    while Instant::now() < end {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn grab_to(path: &str) {
    let f = capture::screen::Frame::grab().expect("抓屏失败");
    let mut c = Canvas::new(f.w, f.h, 96);
    f.blit_into(&mut c);
    let png = c.png_bytes().expect("png");
    std::fs::write(path, png).expect("写文件失败");
    println!("{path}  {}x{}", f.w, f.h);
}

fn main() {
    let _ = zx_core::cfg::load();
    let mode = std::env::args().nth(1).unwrap_or_else(|| "toolbar".into());
    let out = std::env::args().nth(2).unwrap_or_else(|| "ui_probe.png".into());
    let sel = std::env::var("SEL").is_ok();

    match mode.as_str() {
        "ocr" => {
            let text = std::env::args().nth(3).unwrap_or_else(|| {
                "驻匣 · 文字识别结果（可直接编辑）\n\
                 Ctrl+A 全选后可整段复制，也可以只选中其中一行。\n\
                 OCR 常见错字：巳 → 已，木 → 术。"
                    .into()
            });
            capture::ocr::show(text, POINT { x: 400, y: 200 });
            pump(700);
            grab_to(&out);
            // 端到端确认正文真的是「可编辑」的：往输入框里打一个字，再读回来
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    FindWindowExW, FindWindowW, GetWindowTextLengthW, GetWindowTextW, SendMessageW,
                    WM_CHAR,
                };
                let cls = windows::core::w!("ZhuxiaCaptureOcr");
                if let Ok(root) = FindWindowW(cls, windows::core::PCWSTR::null()) {
                    let edit_cls = windows::core::w!("EDIT");
                    if let Ok(edit) =
                        FindWindowExW(root, None, edit_cls, windows::core::PCWSTR::null())
                    {
                        SendMessageW(
                            edit,
                            WM_CHAR,
                            windows::Win32::Foundation::WPARAM('改' as usize),
                            windows::Win32::Foundation::LPARAM(0),
                        );
                        let n = GetWindowTextLengthW(edit);
                        let mut buf = vec![0u16; n as usize + 1];
                        let got = GetWindowTextW(edit, &mut buf);
                        let s = String::from_utf16_lossy(&buf[..got as usize]);
                        println!("输入框长度 {n}，末尾：…{}", s.chars().rev().take(6).collect::<String>());
                    }
                }
            }
            capture::ocr::close();
        }
        _ => {
            capture::overlay::register();
            std::env::set_var("ZHUXIA_CAPTURE_SELFTEST_EDIT", "1");
            std::env::set_var("ZHUXIA_CAPTURE_SELFTEST_UI", "60000");
            if sel {
                std::env::set_var("ZHUXIA_CAPTURE_SELFTEST_SELECT", "1");
            }
            if !capture::overlay::begin_with(capture::overlay::Mode::Select) {
                println!("覆盖层没起来");
                return;
            }
            pump(500);
            if mode == "resize" {
                // 端到端模拟：选中自检矩形（doc 40,40-320,180）的右下控制点，拖到 +100,+100。
                // 拖动能不能改尺寸，看抓出来的图就知道。
                let h = capture::overlay::hwnd().expect("没有窗口");
                let vw = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                        windows::Win32::UI::WindowsAndMessaging::SM_CXVIRTUALSCREEN,
                    )
                };
                let vh = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                        windows::Win32::UI::WindowsAndMessaging::SM_CYVIRTUALSCREEN,
                    )
                };
                let sel_w = (vw as f32 * 0.55) as i32;
                let sel_h = (vh as f32 * 0.5) as i32;
                let left = (vw - sel_w) / 2;
                let top = (vh - sel_h) / 2;
                let edge = std::env::var("EDGE").is_ok();
                // 角：右下控制点 (320,180)；边：底边中点 (180,180)
                let (x0, y0) = if edge {
                    (left + 180, top + 180)
                } else {
                    (left + 320, top + 180)
                };
                let (dx, dy) = if edge { (0, 80) } else { (100, 100) };
                unsafe {
                    use windows::Win32::Foundation::{LPARAM, WPARAM};
                    use windows::Win32::UI::WindowsAndMessaging::*;
                    let lp = |x: i32, y: i32| {
                        LPARAM(((y as u16 as u32) << 16 | (x as u16 as u32)) as isize)
                    };
                    let _ = SendMessageW(h, WM_MOUSEMOVE, WPARAM(0), lp(left + 200, top + 100));
                    let _ = SendMessageW(h, WM_LBUTTONDOWN, WPARAM(1), lp(x0, y0));
                    for i in 1..=10 {
                        let _ = SendMessageW(
                            h,
                            WM_MOUSEMOVE,
                            WPARAM(1),
                            lp(x0 + i * dx / 10, y0 + i * dy / 10),
                        );
                    }
                    let _ = SendMessageW(h, WM_LBUTTONUP, WPARAM(0), lp(x0 + dx, y0 + dy));
                }
                pump(200);
            }
            if mode == "text" {
                // 切到文字工具，在指定点按一下，让输入框浮出来
                let x: i32 = std::env::var("TX").ok().and_then(|v| v.parse().ok()).unwrap_or(900);
                let y: i32 = std::env::var("TY").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
                let h = capture::overlay::hwnd().expect("没有窗口");
                unsafe {
                    use windows::Win32::Foundation::{LPARAM, WPARAM};
                    use windows::Win32::UI::WindowsAndMessaging::*;
                    let lp = |x: i32, y: i32| {
                        LPARAM(((y as u16 as u32) << 16 | (x as u16 as u32)) as isize)
                    };
                    let _ = SendMessageW(h, WM_KEYDOWN, WPARAM(b'T' as usize), LPARAM(0));
                    let _ = SendMessageW(h, WM_MOUSEMOVE, WPARAM(0), lp(x, y));
                    let _ = SendMessageW(h, WM_LBUTTONDOWN, WPARAM(1), lp(x, y));
                    let _ = SendMessageW(h, WM_LBUTTONUP, WPARAM(0), lp(x, y));
                }
                pump(400);
                if std::env::var("COMMIT").is_ok() {
                    // 敲几个字再回车：确认提交后文字落点仍然在点击处
                    unsafe {
                        use windows::Win32::UI::WindowsAndMessaging::*;
                        let cls = windows::core::w!("ZhuxiaCaptureText");
                        if let Ok(win) = FindWindowW(cls, windows::core::PCWSTR::null()) {
                            let edit_cls = windows::core::w!("EDIT");
                            if let Ok(edit) = FindWindowExW(
                                win, None, edit_cls, windows::core::PCWSTR::null(),
                            ) {
                                for ch in "测试文字".encode_utf16() {
                                    SendMessageW(edit, WM_CHAR,
                                        windows::Win32::Foundation::WPARAM(ch as usize),
                                        windows::Win32::Foundation::LPARAM(0));
                                }
                                SendMessageW(edit, WM_KEYDOWN,
                                    windows::Win32::Foundation::WPARAM(0x0D),
                                    windows::Win32::Foundation::LPARAM(0));
                            }
                        }
                    }
                    pump(300);
                }
            }
            grab_to(&out);
            if let Some(h) = capture::overlay::hwnd() {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        h,
                        WM_CLOSE,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
            }
            pump(200);
        }
    }
}
