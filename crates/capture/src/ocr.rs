//! 文字识别：优先本地 PaddleOCR 模型，缺失/失败时回退 Windows 自带 OCR。
//!
//! 系统 OCR 不联网、不随包携带模型；代价是依赖系统语言包。
//! 链路：画布 → PNG → 内存流 → BitmapDecoder → SoftwareBitmap → OcrEngine。

use std::cell::Cell;
use std::sync::OnceLock;

use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::window::create_window;
use zx_ui::Canvas;

use crate::register_class_dbl;

const CLASS: &str = "ZhuxiaCaptureOcr";

pub fn engine_name() -> &'static str {
    if crate::ppocr::available() { "PaddleOCR" } else { "系统 OCR" }
}

/// 识别一张图，返回按行拼接的文本。
pub fn recognize(canvas: &Canvas) -> Result<String, String> {
    if crate::ppocr::available() {
        match crate::ppocr::recognize(canvas) {
            Ok(t) if !t.trim().is_empty() => return Ok(t),
            Ok(_) => zx_core::log::warn("PaddleOCR 没识别到文字，回退系统 OCR"),
            Err(e) => zx_core::log::warn(format!("PaddleOCR 失败（{e}），回退系统 OCR")),
        }
    }
    recognize_system(canvas)
}

fn recognize_system(canvas: &Canvas) -> Result<String, String> {
    let png = ocr_png(canvas).ok_or_else(|| "PNG 编码失败".to_string())?;
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }
    run_ocr(&png).map_err(describe)
}

fn describe(e: windows::core::Error) -> String {
    let msg = e.message().to_string();
    if msg.to_ascii_lowercase().contains("language") {
        return "系统里没有可用的 OCR 语言包：设置 → 时间和语言 → 语言和区域 → \
                中文(简体) → 语言选项 → 可选功能，安装「光学字符识别」。"
            .to_string();
    }
    format!("识别失败：{msg}（0x{:08X}）", e.code().0 as u32)
}

fn run_ocr(png: &[u8]) -> windows::core::Result<String> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(png)?;
    writer.StoreAsync()?.get()?;
    writer.FlushAsync()?.get()?;
    let _ = writer.DetachStream()?;
    stream.Seek(0)?;

    let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let bitmap = decoder.GetSoftwareBitmapAsync()?.get()?;
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
    let result = engine.RecognizeAsync(&bitmap)?.get()?;
    Ok(result.Text()?.to_string())
}

/// 超大图先缩到 4000，系统 OCR 对超长图更稳。
fn ocr_png(canvas: &Canvas) -> Option<Vec<u8>> {
    if canvas.w <= 4000 && canvas.h <= 4000 {
        return canvas.png_bytes();
    }
    let k = (4000.0 / canvas.w as f32).min(4000.0 / canvas.h as f32);
    let w = ((canvas.w as f32 * k) as i32).max(1);
    let h = ((canvas.h as f32 * k) as i32).max(1);
    let mut small = Canvas::new(w, h, 96);
    small.draw_canvas(canvas, 0.0, 0.0, w as f32, h as f32);
    small.png_bytes()
}

// ---------------------------------------------------------------- 结果窗口

struct ResultWin {
    hwnd: HWND,
    canvas: Canvas,
    text: String,
    light: bool,
    drag: Option<(POINT, RECT)>,
    hover_copy: bool,
}

thread_local! {
    static WIN: Cell<*mut ResultWin> = const { Cell::new(std::ptr::null_mut()) };
}

fn with_win<R>(f: impl FnOnce(&mut ResultWin) -> R) -> Option<R> {
    WIN.with(|c| {
        let p = c.get();
        if p.is_null() { None } else { Some(unsafe { f(&mut *p) }) }
    })
}

pub fn is_open() -> bool {
    WIN.with(|c| !c.get().is_null())
}

pub fn show(text: String, near: POINT) {
    register();
    if is_open() {
        close();
    }
    let light = zx_core::win::apps_light();
    let (w, h) = (560, 420);
    let Some(hwnd) = create_window(
        CLASS,
        "驻匣 · 文字识别",
        WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        w,
        h,
    ) else {
        return;
    };

    let mut win = Box::new(ResultWin {
        hwnd,
        canvas: Canvas::new(w, h, 96),
        text,
        light,
        drag: None,
        hover_copy: false,
    });

    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    let x = near.x.clamp(vx + 8, vx + vw - w - 8);
    let y = (near.y + 16).clamp(vy + 8, vy + vh - h - 8);

    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW);
    }
    win.draw();
    let ptr = Box::into_raw(win);
    WIN.with(|c| c.set(ptr));
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn close() {
    let p = WIN.with(|c| c.replace(std::ptr::null_mut()));
    if p.is_null() {
        return;
    }
    let win = unsafe { Box::from_raw(p) };
    unsafe {
        let _ = DestroyWindow(win.hwnd);
    }
}

impl ResultWin {
    fn palette(&self) -> (u32, u32, u32, u32) {
        if self.light {
            (0xFFF7F7F7, 0xFF1A1A1A, 0x33000000, 0x33000000)
        } else {
            (0xFF1F1F1F, 0xFFF2F2F2, 0x33FFFFFF, 0x33FFFFFF)
        }
    }

    fn copy_rect(&self) -> RECT {
        let w = 92;
        RECT {
            left: self.canvas.w - w - 14,
            top: 52,
            right: self.canvas.w - 14,
            bottom: 82,
        }
    }

    fn close_rect(&self) -> RECT {
        RECT {
            left: self.canvas.w - 40,
            top: 14,
            right: self.canvas.w - 14,
            bottom: 40,
        }
    }

    fn draw(&mut self) {
        let (bg, fg, line, _) = self.palette();
        let xr = self.close_rect();
        let cr = self.copy_rect();
        let hot = self.hover_copy;
        let c = &mut self.canvas;

        c.clear(0x00000000);
        c.fill_round(0.0, 0.0, c.w as f32, c.h as f32, 10.0, bg);
        c.stroke_round(0.5, 0.5, c.w as f32 - 1.0, c.h as f32 - 1.0, 10.0, 1.0, line);

        let title = format!("文字识别 · {}", engine_name());
        c.text_ex(
            &title,
            18.0,
            12.0,
            300.0,
            28.0,
            15.0,
            fg,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            true,
            false,
        );
        let dim = if self.light { 0x991A1A1A } else { 0x99F2F2F2 };
        c.text_ex(
            &format!("{} 行", self.text.lines().count()),
            150.0,
            12.0,
            200.0,
            28.0,
            12.0,
            dim,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            false,
            false,
        );

        c.text_ex(
            "×",
            xr.left as f32,
            xr.top as f32,
            (xr.right - xr.left) as f32,
            (xr.bottom - xr.top) as f32,
            16.0,
            dim,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            false,
            false,
        );

        c.fill_round(
            cr.left as f32,
            cr.top as f32,
            (cr.right - cr.left) as f32,
            (cr.bottom - cr.top) as f32,
            6.0,
            if hot { 0xEE2E90FA } else { 0x552E90FA },
        );
        c.text_ex(
            "复制全部",
            cr.left as f32,
            cr.top as f32,
            (cr.right - cr.left) as f32,
            (cr.bottom - cr.top) as f32,
            12.0,
            0xFFFFFFFF,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            false,
            false,
        );

        let body = self.text.trim();
        if body.is_empty() {
            c.text_ex(
                "没有识别到文字。可以框小一点、或放大后再试。",
                18.0,
                96.0,
                500.0,
                40.0,
                13.0,
                dim,
                zx_ui::TextAlign::Near,
                zx_ui::VAlign::Top,
                false,
                false,
            );
        } else {
            c.text_wrapped(
                body,
                18.0,
                92.0,
                (c.w - 36) as f32,
                (c.h - 140) as f32,
                13.0,
                fg,
                40,
            );
        }

        c.text_ex(
            "Ctrl+C 复制全部 · Esc 关闭 · 拖动可移动",
            18.0,
            (c.h - 30) as f32,
            (c.w - 36) as f32,
            20.0,
            11.0,
            dim,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            false,
            false,
        );

        let (x, y) = window_pos(self.hwnd);
        self.canvas.present(self.hwnd, x, y);
    }
}

fn window_pos(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
}

fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        register_class_dbl(CLASS, wndproc);
    });
}

fn copy_text(text: &str) {
    let mut bytes = Vec::new();
    for u in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) {
            let p = GlobalLock(hmem) as *mut u8;
            if p.is_null() {
                let _ = windows::Win32::Foundation::GlobalFree(hmem);
            } else {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                let _ = GlobalUnlock(hmem);
                if SetClipboardData(13, windows::Win32::Foundation::HANDLE(hmem.0)).is_err() {
                    let _ = windows::Win32::Foundation::GlobalFree(hmem);
                }
            }
        }
        let _ = CloseClipboard();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_win(|win| {
                if let Some((start, rect)) = win.drag {
                    let (dx, dy) = (pt.x - start.x, pt.y - start.y);
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        rect.left + dx,
                        rect.top + dy,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOSIZE,
                    );
                }
                let (x, y) = window_pos(hwnd);
                let cr = win.copy_rect();
                let inside = pt.x - x >= cr.left
                    && pt.x - x < cr.right
                    && pt.y - y >= cr.top
                    && pt.y - y < cr.bottom;
                if inside != win.hover_copy {
                    win.hover_copy = inside;
                    win.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let mut rect = RECT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = GetWindowRect(hwnd, &mut rect);
            let (lx, ly) = (pt.x - rect.left, pt.y - rect.top);
            with_win(|win| {
                let cr = win.copy_rect();
                let xr = win.close_rect();
                if lx >= cr.left && lx < cr.right && ly >= cr.top && ly < cr.bottom {
                    copy_text(&win.text);
                } else if lx >= xr.left && lx < xr.right && ly >= xr.top && ly < xr.bottom {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                } else {
                    win.drag = Some((pt, rect));
                    let _ = SetCapture(hwnd);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with_win(|win| win.drag = None);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16) & 0x8000 != 0;
            if w.0 as u16 == b'C' as u16 && ctrl {
                with_win(|win| copy_text(&win.text));
            } else if w.0 as u16 == 0x1B {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let cur = if with_win(|w| w.hover_copy).unwrap_or(false) {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            let _ = SetCursor(LoadCursorW(None, cur).unwrap_or_default());
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
