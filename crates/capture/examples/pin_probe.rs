//! 贴图探针：创建一张贴图并保持消息循环，供截图核对。
use capture::pin;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
use zx_ui::{Canvas, TextAlign, VAlign};

fn main() {
    pin::register();
    let mut c = Canvas::new(360, 220, 96);
    c.clear(0xFF1F2937);
    c.text_ex("驻匣 · 贴图", 24.0, 40.0, 300.0, 50.0, 28.0, 0xFFFFFFFF,
              TextAlign::Near, VAlign::Top, true, false);
    c.text_ex("拖动 / 滚轮缩放 / 双击关闭", 24.0, 110.0, 320.0, 30.0, 14.0, 0xFFC7D2FE,
              TextAlign::Near, VAlign::Top, false, false);
    if pin::create(&mut c, POINT { x: 600, y: 420 }).is_none() {
        std::process::exit(1);
    }
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
