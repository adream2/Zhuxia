//! 按键回显自检：装钩子 → 注入 Ctrl+A → 停 6 秒，供外部截屏核对。
//!
//! `ZHUXIA_KEYECHO_INJECT_TEST=1 cargo run --release --example keyecho_probe`
//!
//! 注入的键本来会被过滤（`LLKHF_INJECTED`），自检模式下放行，这样能验证
//! 「钩子收到 → 队列 → 窗口 → 绘制」整条链路，而不用人手按键盘。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, VIRTUAL_KEY,
};

fn key(vk: u16, up: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYBD_EVENT_FLAGS(2) } else { KEYBD_EVENT_FLAGS(0) },
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
    std::env::set_var("ZHUXIA_KEYECHO_INJECT_TEST", "1");
    let _ = zx_core::cfg::load();
    gesture::keyecho::start();
    // 位置可以由命令行指定：`keyecho_probe top-left`
    let pos = std::env::args().nth(1).unwrap_or_else(|| "bottom-center".into());
    let pos: &'static str = match pos.as_str() {
        "top-center" => "top-center",
        "top-left" => "top-left",
        "top-right" => "top-right",
        "bottom-left" => "bottom-left",
        "bottom-right" => "bottom-right",
        _ => "bottom-center",
    };
    gesture::keyecho::apply(&zx_core::cfg::KeyEchoSettings {
        enabled: true,
        position: pos,
        duration_ms: 5000,
        font_size: 16,
    });
    println!("位置 = {pos}");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Ctrl 按下不松，再按 A / C / V：键帽应该是"Ctrl A C V"，Ctrl 高亮
    key(0x11, false);
    for vk in [0x41u16, 0x43, 0x56] {
        key(vk, false);
        std::thread::sleep(std::time::Duration::from_millis(120));
        key(vk, true);
    }
    println!("已注入 Ctrl+A/C/V，保持 6 秒等外部截屏");
    std::thread::sleep(std::time::Duration::from_secs(6));
    key(0x11, true);
}
