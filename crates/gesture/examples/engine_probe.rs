//! 端到端探针：真实钩子 → 引擎 → 匹配 → 动作钩子。
//!
//! 注入一条向右的右键拖拽，临时把通用规则改成「→ = internal:capture」，
//! 并用宿主动作钩子计数。验证的是整条链路，不会真的截图。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use gesture::model::{Action, Direction, MouseButton, Rule, Scope};
use gesture::{engine, hook, matcher, model, set_action_hook};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

static HITS: AtomicUsize = AtomicUsize::new(0);

fn hit(_op: &str) {
    HITS.fetch_add(1, Ordering::SeqCst);
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
    matcher::set_config(
        vec![Rule {
            pattern: vec![Direction::Right],
            scope: Scope::Global,
            button: MouseButton::Right,
            wheel: None,
            on_wheel: false,
            name: "probe".into(),
            action: Action::Internal("capture".into()),
        }],
        vec![],
    );
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
        let _ = SetCursorPos(700, 500);
    }
    std::thread::sleep(Duration::from_millis(80));

    send(0, 0, MOUSEEVENTF_RIGHTDOWN);
    for _ in 0..12 {
        std::thread::sleep(Duration::from_millis(8));
        send(10, 0, MOUSEEVENTF_MOVE);
    }
    send(0, 0, MOUSEEVENTF_RIGHTUP);

    std::thread::sleep(Duration::from_millis(700));
    unsafe {
        let _ = SetCursorPos(old.x, old.y);
    }
    hook::uninstall();

    let hits = HITS.load(Ordering::SeqCst);
    println!("action_hits={hits} rules={}", model::defaults().len());
    std::process::exit(if hits > 0 { 0 } else { 1 });
}
