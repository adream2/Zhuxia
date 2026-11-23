//! 覆盖层状态机探针。
//!
//! 往真实覆盖层窗口注入「按下-拖动-松手」再补两次 Esc，配合
//! `ZHUXIA_CAPTURE_TRACE=1` 可以确认阶段迁移确实走到了编辑态、并能退回选区。
//! 单独跑没有意义：它只发输入，判断要看覆盖层进程的日志。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
    MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, PostMessageW, SM_CXSCREEN, SM_CYSCREEN, WM_KEYDOWN, WM_KEYUP,
};

fn abs(x: i32, y: i32) -> (i32, i32) {
    let w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(2);
    let h = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(2);
    (x * 65535 / (w - 1), y * 65535 / (h - 1))
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn key(vk: u16) {
    let mk = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let down = mk(Default::default());
    let up = mk(KEYEVENTF_KEYUP);
    unsafe {
        SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32 * 2);
    }
}

fn wide(v: &str) -> Vec<u16> {
    v.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 注入的键盘消息会发给前台窗口，脚本跑在终端里时覆盖层拿不到焦点，
/// 所以再直接往窗口投递一次按键，专测状态机对按键的处理。
fn post_key(vk: u16) {
    let cls = wide("ZhuxiaCaptureOverlay");
    let Ok(h) = (unsafe { FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) }) else {
        println!("overlay window not found");
        return;
    };
    unsafe {
        let _ = PostMessageW(h, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(0));
        let _ = PostMessageW(h, WM_KEYUP, WPARAM(vk as usize), LPARAM(0));
    }
}

fn drag(x0: i32, y0: i32, x1: i32, y1: i32) {
    let (ax, ay) = abs(x0, y0);
    mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay);
    std::thread::sleep(std::time::Duration::from_millis(80));
    mouse(MOUSEEVENTF_LEFTDOWN, 0, 0);
    for i in 1..=15 {
        let x = x0 + (x1 - x0) * i / 15;
        let y = y0 + (y1 - y0) * i / 15;
        let (ax, ay) = abs(x, y);
        mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay);
        std::thread::sleep(std::time::Duration::from_millis(18));
    }
    mouse(MOUSEEVENTF_LEFTUP, 0, 0);
}

fn main() {
    // 画标注模式：先框选，再在编辑态里拖一条矩形。
    if std::env::args().any(|a| a == "draw") {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        drag(400, 300, 1100, 760);
        std::thread::sleep(std::time::Duration::from_millis(600));
        drag(1000, 550, 1350, 780);
        std::thread::sleep(std::time::Duration::from_millis(400));
        println!("drew a rect inside the doc");
        return;
    }

    // 只发按键的模式：用来验证「选区 → 关闭」这一段。
    if std::env::args().any(|a| a == "esc") {
        post_key(0x1B);
        std::thread::sleep(std::time::Duration::from_millis(600));
        post_key(0x1B);
        println!("posted Esc x2");
        return;
    }

    std::thread::sleep(std::time::Duration::from_millis(2000));

    let (x0, y0) = (360, 260);
    let (x1, y1) = (1000, 720);
    let (ax, ay) = abs(x0, y0);
    mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay);
    std::thread::sleep(std::time::Duration::from_millis(80));
    mouse(MOUSEEVENTF_LEFTDOWN, 0, 0);

    for i in 1..=15 {
        let x = x0 + (x1 - x0) * i / 15;
        let y = y0 + (y1 - y0) * i / 15;
        let (ax, ay) = abs(x, y);
        mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay);
        std::thread::sleep(std::time::Duration::from_millis(18));
    }

    mouse(MOUSEEVENTF_LEFTUP, 0, 0);
    std::thread::sleep(std::time::Duration::from_millis(500));

    key(0x1B); // Esc：编辑态 → 选区（需要覆盖层有焦点才生效）
    std::thread::sleep(std::time::Duration::from_millis(400));
    key(0x1B); // Esc：选区 → 关闭

    post_key(0x1B); // 直接投递：编辑态 → 选区
    std::thread::sleep(std::time::Duration::from_millis(500));
    post_key(0x1B); // 直接投递：选区 → 关闭

    println!("flow injected: drag ({x0},{y0}) -> ({x1},{y1}) + Esc x2 (input) + Esc x2 (post)");
}
