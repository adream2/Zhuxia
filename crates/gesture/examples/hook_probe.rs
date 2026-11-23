//! 钩子/引擎事件探针：安装真实 WH_MOUSE_LL，注入一条不会命中任何规则的
//! 右下斜线，验证 Start / Move / End 能走通。
//!
//! 只用于开发验证；会短暂移动鼠标并在结束后复位。

use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use gesture::{apply_config, cfg, engine, hook};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

fn mouse(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) {
    send(0, 0, flags);
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
    let (hook_tx, hook_rx) = channel();
    let (_engine_tx, engine_rx) = channel();
    engine::start(engine_rx);
    let relay = std::thread::spawn(move || {
        let mut count = 0usize;
        loop {
            match hook_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(ev) => {
                    count += 1;
                    println!("event {count}: {ev:?}");
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        count
    });

    apply_config(&cfg());
    if !hook::install(hook_tx) {
        eprintln!("钩子安装失败");
        std::process::exit(1);
    }
    std::thread::sleep(Duration::from_millis(400));

    let mut old = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut old);
        let _ = SetCursorPos(600, 400);
    }
    std::thread::sleep(Duration::from_millis(80));

    mouse(MOUSEEVENTF_RIGHTDOWN);
    for i in 1..=12 {
        std::thread::sleep(Duration::from_millis(8));
        let _ = i;
        send(8, 8, MOUSEEVENTF_MOVE);
    }
    mouse(MOUSEEVENTF_RIGHTUP);

    std::thread::sleep(Duration::from_millis(500));
    unsafe {
        let _ = SetCursorPos(old.x, old.y);
    }
    hook::uninstall();
    let count = relay.join().unwrap_or(0);
    println!("events={count} hook_active={}", hook::active());
    if count < 3 {
        std::process::exit(1);
    }
}
