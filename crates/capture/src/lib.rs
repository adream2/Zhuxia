//! 驻匣 · 截图模块入口。
//!
//! 常驻时只注册热键与窗口类，不持有画布；一次截图的内存峰值约为「冻结帧 + 合成画布」，
//! 结束即释放。
//!
//! ```text
//! RegisterHotKey → 隐藏贴图 → BitBlt 冻结虚拟桌面
//!                                   │
//!                     全屏覆盖层（选区 → 标注）→ 合成 → 剪贴板 / PNG / 贴图
//! ```

pub mod annotate;
pub mod design;
pub mod hotkeys;
pub mod longshot;
pub mod ocr;
pub mod ocr_model;
pub mod output;
pub mod overlay;
pub mod picker;
pub mod pin;
pub mod ppocr;
pub mod prompt;
pub mod record;
pub mod screen;
pub mod stitch;
pub mod style;
pub mod stylepanel;
pub mod target;

use std::sync::OnceLock;

use zx_core::CaptureSettings;

struct UploadHook {
    available: Box<dyn Fn() -> bool + Send + Sync>,
    send: Box<dyn Fn(Vec<u8>) + Send + Sync>,
}

static UPLOAD_HOOK: OnceLock<UploadHook> = OnceLock::new();

/// 注册「上传到图床」的宿主钩子；capture 不直接依赖 imgbed。
pub fn set_upload_hook(
    available: impl Fn() -> bool + Send + Sync + 'static,
    send: impl Fn(Vec<u8>) + Send + Sync + 'static,
) {
    let _ = UPLOAD_HOOK.set(UploadHook {
        available: Box::new(available),
        send: Box::new(send),
    });
}

pub fn upload_available() -> bool {
    UPLOAD_HOOK.get().map(|h| (h.available)()).unwrap_or(false)
}

pub fn send_upload(png: Vec<u8>) -> bool {
    match UPLOAD_HOOK.get() {
        Some(h) => {
            (h.send)(png);
            true
        }
        None => false,
    }
}

pub fn cfg() -> CaptureSettings {
    zx_core::cfg::cfg().capture()
}

/// 模块入口，宿主只调一次。
pub fn start() {
    let c = cfg();
    if !c.enabled {
        zx_core::log::info("截图模块已禁用（settings.yaml → capture.enabled）");
        return;
    }
    overlay::register();
    pin::register();
    hotkeys::start();
    maintenance();
    zx_core::log::info(format!(
        "截图模块已启动：热键配置 {}（实际注册结果见 capture-hotkey.txt），保存目录 {}，自动存盘={}",
        c.hotkey,
        c.save_dir.display(),
        c.auto_save
    ));
}

pub fn trigger() {
    hotkeys::trigger();
}

pub fn trigger_record() {
    hotkeys::trigger_record();
}

/// 配置变更轮询；设置面板跨进程，mtime 是最可靠的信号。
fn maintenance() {
    std::thread::Builder::new()
        .name("zx-capture-maint".into())
        .spawn(|| {
            let mut last = settings_mtime();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let mtime = settings_mtime();
                if mtime != last {
                    last = mtime;
                    zx_core::cfg::reload();
                    style::reload();
                    hotkeys::reload();
                    zx_core::log::info("截图配置已重载");
                }
            }
        })
        .ok();
}

/// 窗口类需要 `CS_DBLCLKS`，贴图与文本覆盖层都依赖双击消息。
pub(crate) fn register_class_dbl(
    name: &str,
    proc: unsafe extern "system" fn(
        windows::Win32::Foundation::HWND,
        u32,
        windows::Win32::Foundation::WPARAM,
        windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT,
) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    unsafe {
        let wide = zx_core::win::wide(name);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(proc),
            hInstance: HINSTANCE(hinst.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(wide.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }
}

fn settings_mtime() -> Option<std::time::SystemTime> {
    zx_core::cfg::settings_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

fn save_png(name: &str, canvas: &zx_ui::Canvas) -> Option<std::path::PathBuf> {
    let png = canvas.png_bytes()?;
    let path = zx_core::cfg::run_dir().join(name);
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// 自检：抓屏 → 裁剪 → 标注 → 合成 → PNG/剪贴板。
pub fn selftest() -> bool {
    let Some(frame) = screen::Frame::grab() else {
        zx_core::log::warn("自检失败：抓不到屏幕");
        return false;
    };

    let w = frame.w.min(520);
    let h = frame.h.min(320);
    let left = frame.x + (frame.w - w) / 2;
    let top = frame.y + (frame.h - h) / 2;
    let sel = windows::Win32::Foundation::RECT {
        left,
        top,
        right: left + w,
        bottom: top + h,
    };

    let annos = sample_annos();
    let Some(doc) = output::crop(&frame, sel) else {
        zx_core::log::warn("自检失败：裁剪失败");
        return false;
    };
    let canvas = output::compose_doc(&doc, &annos);
    let Some(path) = save_png("capture-selftest.png", &canvas) else {
        zx_core::log::warn("自检失败：PNG 编码/写盘失败");
        return false;
    };

    if env_on("ZHUXIA_CAPTURE_FULL") {
        let full = windows::Win32::Foundation::RECT {
            left: frame.x,
            top: frame.y,
            right: frame.x + frame.w,
            bottom: frame.y + frame.h,
        };
        if let Some(doc) = output::crop(&frame, full) {
            let _ = save_png("capture-selftest-full.png", &output::compose_doc(&doc, &[]));
        }
    }

    if env_on("ZHUXIA_CAPTURE_STYLE") {
        let st = style::Style { enabled: true, ..style::Style::default() };
        let styled = style::render(&doc, &annos, &st, 1.0);
        let _ = save_png("capture-selftest-styled.png", &styled);
        zx_core::log::info(format!("美化自检：{}x{}", styled.w, styled.h));
    }

    if env_on("ZHUXIA_CAPTURE_PPOCR") {
        let mut probe = zx_ui::Canvas::new(760, 220, 96);
        probe.clear(0xFFFFFFFF);
        probe.text_ex(
            "驻匣 PaddleOCR 测试",
            24.0,
            26.0,
            700.0,
            56.0,
            40.0,
            0xFF111111,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Top,
            true,
            false,
        );
        probe.text_ex(
            "Hello PP-OCRv4 2026",
            24.0,
            110.0,
            700.0,
            46.0,
            30.0,
            0xFF333333,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Top,
            false,
            false,
        );
        match crate::ppocr::recognize(&probe) {
            Ok(t) => {
                let clean: String = t.lines().collect::<Vec<_>>().join(" | ");
                zx_core::log::info(format!("PPOCR 自检：{clean}"));
                let _ = save_png("capture-selftest-ppocr-src.png", &probe);
            }
            Err(e) => zx_core::log::warn(format!("PPOCR 自检失败：{e}")),
        }
    }

    if env_on("ZHUXIA_CAPTURE_OCR") {
        let mut probe = zx_ui::Canvas::new(700, 160, 96);
        probe.clear(0xFFFFFFFF);
        probe.text_ex(
            "驻匣 OCR 123",
            20.0,
            30.0,
            660.0,
            60.0,
            40.0,
            0xFF111111,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Top,
            true,
            false,
        );
        match ocr::recognize(&probe) {
            Ok(t) => zx_core::log::info(format!(
                "OCR 自检：{} 字，前 40 字「{}」",
                t.chars().count(),
                t.chars().take(40).collect::<String>()
            )),
            Err(e) => zx_core::log::warn(format!("OCR 自检失败：{e}")),
        }
    }

    if env_on("ZHUXIA_CAPTURE_STITCH") {
        stitch_selftest();
    }

    if env_on("ZHUXIA_CAPTURE_LONG") {
        std::env::set_var("ZHUXIA_CAPTURE_LONG_AUTO", "1");
        let rect = windows::Win32::Foundation::RECT {
            left: frame.x + frame.w / 4,
            top: frame.y + frame.h / 4,
            right: frame.x + frame.w * 3 / 4,
            bottom: frame.y + frame.h * 3 / 4,
        };
        match crate::longshot::run(rect) {
            Some(c) => {
                let dims = (c.w, c.h);
                let _ = save_png("capture-selftest-long.png", &c);
                zx_core::log::info(format!("长截图自检：{}x{}", dims.0, dims.1));
            }
            None => zx_core::log::warn("长截图自检失败：没有结果"),
        }
    }

    let saved = clipboard_text();
    let copied = output::copy_to_clipboard(&canvas);
    let verified = clipboard_has_image();
    restore_text(saved);

    zx_core::log::info(format!(
        "截图自检通过：屏幕 {}x{}，裁剪 {}x{}，PNG {} 字节 → {}；剪贴板写入={} 校验={}",
        frame.w,
        frame.h,
        w,
        h,
        canvas.png_bytes().map(|b| b.len()).unwrap_or(0),
        path.display(),
        copied,
        verified
    ));
    copied && verified
}

fn env_on(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}

fn sample_annos() -> Vec<annotate::Anno> {
    let mut annos = Vec::new();

    let mut rect =
        annotate::Anno::new(annotate::Tool::Rect, (20, 20), annotate::PALETTE[0], 0);
    rect.set_end((160, 90));
    annos.push(rect);

    let mut arrow =
        annotate::Anno::new(annotate::Tool::Arrow, (200, 200), annotate::PALETTE[3], 0);
    arrow.set_end((330, 110));
    annos.push(arrow);

    let mut counter =
        annotate::Anno::new(annotate::Tool::Counter, (60, 220), annotate::PALETTE[4], 1);
    counter.width = 9.0;
    annos.push(counter);

    let mut text =
        annotate::Anno::new(annotate::Tool::Text, (24, 260), annotate::PALETTE[6], 0);
    text.text = "驻匣截图自检".into();
    annos.push(text);

    annos
}

fn stitch_selftest() {
    for (idx, dir) in [
        crate::stitch::Dir::Vertical,
        crate::stitch::Dir::Horizontal,
        crate::stitch::Dir::Grid,
    ]
    .iter()
    .enumerate()
    {
        crate::stitch::cancel();
        let mut a = zx_ui::Canvas::new(220, 120, 96);
        a.clear(0xFFFF6B6B);
        a.text_ex(
            "A",
            16.0,
            40.0,
            120.0,
            40.0,
            30.0,
            0xFFFFFFFF,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Top,
            true,
            false,
        );
        let mut b = zx_ui::Canvas::new(160, 90, 96);
        b.clear(0xFF4ECDC4);
        b.text_ex(
            "B",
            16.0,
            28.0,
            100.0,
            34.0,
            26.0,
            0xFFFFFFFF,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Top,
            true,
            false,
        );
        crate::stitch::begin(a, *dir);
        crate::stitch::push(b);
        if let Some(out) = crate::stitch::render() {
            let _ = save_png(&format!("capture-selftest-stitch-{idx}.png"), &out);
        }
        crate::stitch::cancel();
    }
    zx_core::log::info("拼接自检：已生成 3 张对照图");
}

fn clipboard_text() -> Option<String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let mut out = None;
        if IsClipboardFormatAvailable(13).is_ok() {
            if let Ok(h) = GetClipboardData(13) {
                let p = windows::Win32::System::Memory::GlobalLock(
                    windows::Win32::Foundation::HGLOBAL(h.0),
                );
                if !p.is_null() {
                    let wide = p as *const u16;
                    let mut len = 0usize;
                    while *wide.add(len) != 0 {
                        len += 1;
                    }
                    out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(wide, len)));
                    let _ = windows::Win32::System::Memory::GlobalUnlock(
                        windows::Win32::Foundation::HGLOBAL(h.0),
                    );
                }
            }
        }
        let _ = CloseClipboard();
        let _ = HANDLE::default();
        out
    }
}

fn clipboard_has_image() -> bool {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    };
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let ok = IsClipboardFormatAvailable(8).is_ok()
            || IsClipboardFormatAvailable(2).is_ok()
            || IsClipboardFormatAvailable(RegisterClipboardFormatW(windows::core::w!("PNG")))
                .is_ok();
        let _ = CloseClipboard();
        ok
    }
}

fn restore_text(text: Option<String>) {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    let Some(text) = text else { return };
    let mut bytes = Vec::new();
    for u in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(hmem) = windows::Win32::System::Memory::GlobalAlloc(
            windows::Win32::System::Memory::GMEM_MOVEABLE,
            bytes.len(),
        ) {
            let p = windows::Win32::System::Memory::GlobalLock(hmem) as *mut u8;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                let _ = windows::Win32::System::Memory::GlobalUnlock(hmem);
                let _ = SetClipboardData(13, windows::Win32::Foundation::HANDLE(hmem.0));
            }
        }
        let _ = CloseClipboard();
    }
}
