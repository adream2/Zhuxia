//! 边缘滚动自检：把光标贴到屏幕左边，注入一格滚轮，验证 钩子→边缘判定→亮度动作 全链路。
//!
//! `ZHUXIA_GESTURE_TRACE=1 cargo run --release --example edge_probe`
//! 动作选亮度是因为它可逆：做完会再反向滚一格还原。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
use zx_core::cfg::{EdgeAction, EdgeSettings};

fn wheel(notches: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: (120 * notches) as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn main() {
    let _ = zx_core::cfg::load();
    zx_ui::window::init_dpi();
    // 必须先 start 再 apply：start 会用配置文件里的值覆盖一次
    gesture::start();
    gesture::edges::apply(&EdgeSettings {
        left: Some(EdgeAction::Brightness),
        right: None,
        top: None,
        bottom: None,
        margin: 2,
        repeat_ms: 40,
    });
    std::thread::sleep(std::time::Duration::from_millis(600));

    let (vx, vy, _vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let (px, py) = (vx, vy + vh / 2);
    println!("光标移到左边 ({px},{py})，注入 +1 / -1 格滚轮");
    unsafe { let _ = SetCursorPos(px, py); }
    std::thread::sleep(std::time::Duration::from_millis(250));
    wheel(1);
    std::thread::sleep(std::time::Duration::from_millis(500));
    wheel(-1);
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("完成（亮度 +5% 再 -5%，日志里能看到边缘滚动记录）");
}
