//! 斜向 W 手势端到端探针：真实钩子注入 ↙↘↙，应命中默认「截图」规则。
//! 动作钩子只计数，不会真的打开截图。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use gesture::{engine, hook, matcher, model, set_action_hook};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

static HITS: AtomicUsize = AtomicUsize::new(0);

fn hit(op: &str) {
    if op == "capture" {
        HITS.fetch_add(1, Ordering::SeqCst);
    }
}

fn send(dx: i32, dy: i32, flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn main() {
    set_action_hook(hit);
    matcher::set_config(model::defaults(), model::default_groups());
    engine::set_sensitivity(100);

    let (tx, rx) = channel();
    if !hook::install(tx) {
        eprintln!("钩子安装失败");
        std::process::exit(1);
    }
    engine::start(rx);
    std::thread::sleep(Duration::from_millis(400));

    let mut old = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut old);
        let _ = SetCursorPos(700, 400);
    }
    std::thread::sleep(Duration::from_millis(80));

    send(0, 0, MOUSEEVENTF_RIGHTDOWN);
    for _ in 0..10 {
        send(-6, 12, MOUSEEVENTF_MOVE);
        std::thread::sleep(Duration::from_millis(8));
    }
    for _ in 0..20 {
        send(12, 12, MOUSEEVENTF_MOVE);
        std::thread::sleep(Duration::from_millis(8));
    }
    for _ in 0..10 {
        send(-12, 12, MOUSEEVENTF_MOVE);
        std::thread::sleep(Duration::from_millis(8));
    }
    send(0, 0, MOUSEEVENTF_RIGHTUP);

    std::thread::sleep(Duration::from_millis(700));
    unsafe {
        let _ = SetCursorPos(old.x, old.y);
    }
    hook::uninstall();

    let hits = HITS.load(Ordering::SeqCst);
    println!("capture_hits={hits}");
    std::process::exit(if hits > 0 { 0 } else { 1 });
}
