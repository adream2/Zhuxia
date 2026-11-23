//! 美化面板探针：打开面板并保持窗口，供截图核对。
use capture::stylepanel;
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
use zx_ui::{Canvas, TextAlign, VAlign};

fn main() {
    let mut doc = Canvas::new(720, 420, 96);
    doc.clear(0xFFF7F7FA);
    doc.text_ex("驻匣 · 美化面板自检", 40.0, 40.0, 620.0, 60.0, 34.0, 0xFF1D1D1F,
                TextAlign::Near, VAlign::Top, true, false);
    doc.text_ex("背景 / 边框 / 水印", 40.0, 120.0, 620.0, 40.0, 20.0, 0xFF66666E,
                TextAlign::Near, VAlign::Top, false, false);

    let tab = std::env::args().nth(1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    std::env::set_var("ZHUXIA_CAPTURE_SELFTEST_STYLEPANEL_TAB", tab.to_string());
    stylepanel::open(
        &doc,
        &[],
        RECT { left: 0, top: 0, right: 0, bottom: 0 },
        POINT { x: 500, y: 300 },
    );
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
