//! 文字识别：优先本地 PaddleOCR 模型，缺失/失败时回退 Windows 自带 OCR。
//!
//! 系统 OCR 不联网、不随包携带模型；代价是依赖系统语言包。
//! 链路：画布 → PNG → 内存流 → BitmapDecoder → SoftwareBitmap → OcrEngine。

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, GetDC, InvalidateRect,
    ReleaseDC, SetBkColor, SetBkMode, SetTextColor, UpdateWindow, CLIP_DEFAULT_PRECIS,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_PITCH, HDC, HGDIOBJ, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_ESCAPE,
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
//
// 识别结果只是"草稿"：错字、漏行、串行都很常见。给一块只读文本等于把改错的机会
// 收回去了，所以正文用原生多行 EDIT 承载——能改、能框选、能只复制其中一段，
// Ctrl+A / Ctrl+C 走系统行为，不用自己实现。
//
// 这也决定了窗口不能再是分层的：`UpdateLayeredWindow` 的窗口里塞子控件，
// 子控件不参与合成。改成普通窗口 + 圆角区域，绘制走 WM_PAINT 的 BitBlt。

const ID_EDIT: i32 = 1;
/// `EM_SETSEL`
const EM_SETSEL: u32 = 0x00B1;

struct ResultWin {
    hwnd: HWND,
    canvas: Canvas,
    /// 当前正文（跟着 EDIT 同步，用来统计行数）
    text: String,
    light: bool,
    dpi: u32,
    w: i32,
    h: i32,
    drag: Option<(POINT, RECT)>,
    /// 0 = 无，1 = 复制全部，2 = 关闭
    hover: u8,
    edit: HWND,
    font: isize,
    brush: isize,
}

thread_local! {
    static WIN: Cell<*mut ResultWin> = const { Cell::new(std::ptr::null_mut()) };
}

static OLD_EDIT: AtomicIsize = AtomicIsize::new(0);

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
    let dpi = unsafe { GetDpiForSystem() }.max(96);
    let s = dpi as f32 / 96.0;

    let w = (560.0 * s).round() as i32;
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    let h = fit_height(&text, w, s).clamp((250.0 * s) as i32, (vh as f32 * 0.72) as i32);
    let Some(hwnd) = create_window(
        CLASS,
        "驻匣 · 文字识别",
        WS_POPUP | WS_CLIPCHILDREN,
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        w,
        h,
    ) else {
        return;
    };
    zx_ui::painter::set_round_region(hwnd, w, h, (10.0 * s) as i32);

    let mut win = Box::new(ResultWin {
        hwnd,
        canvas: Canvas::new(w, h, dpi),
        text,
        light,
        dpi,
        w,
        h,
        drag: None,
        hover: 0,
        edit: HWND::default(),
        font: 0,
        brush: 0,
    });

    let (ex, ey, ew, eh) = win.body_rect_i();
    let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL
        | WINDOW_STYLE(
            ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32
                | ES_NOHIDESEL as u32 | ES_LEFT as u32,
        );
    let edit = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("EDIT"),
            PCWSTR::null(),
            style,
            ex, ey, ew, eh,
            hwnd,
            HMENU(ID_EDIT as isize as *mut core::ffi::c_void),
            HINSTANCE::default(),
            None,
        )
    };
    let Ok(edit) = edit else {
        unsafe { let _ = DestroyWindow(hwnd); }
        return;
    };
    win.edit = edit;
    win.font = make_font((13.5 * s).round() as i32);
    win.brush = unsafe { CreateSolidBrush(win.brush_color()).0 as isize };
    let body = zx_core::win::wide(&win.text);
    unsafe {
        SendMessageW(edit, WM_SETFONT, WPARAM(win.font as usize), LPARAM(1));
        let _ = SetWindowTextW(edit, PCWSTR(body.as_ptr()));
        // 默认边距贴着控件边缘，字形会撞到圆角：左右各留 8px。
        SendMessageW(
            edit,
            0x00D3, // EM_SETMARGINS
            WPARAM(0x0001 | 0x0002),
            LPARAM((8.0 * s) as isize | (((8.0 * s) as isize) << 16)),
        );
        subclass_edit(edit);
    }

    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let x = near.x.clamp(vx + 8, (vx + vw - w - 8).max(vx + 8));
    let y = (near.y + 16).clamp(vy + 8, (vy + vh - h - 8).max(vy + 8));

    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW);
    }
    win.draw();
    let ptr = Box::into_raw(win);
    WIN.with(|c| c.set(ptr));
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        // 光标落在正文末尾：用户下一步多半是改字或全选复制
        let _ = SetFocus(edit);
        let n = GetWindowTextLengthW(edit).max(0) as usize;
        SendMessageW(edit, EM_SETSEL, WPARAM(n), LPARAM(n as isize));
    }
}

pub fn close() {
    let p = WIN.with(|c| c.replace(std::ptr::null_mut()));
    if p.is_null() {
        return;
    }
    let win = unsafe { Box::from_raw(p) };
    unsafe {
        // 子控件随父窗口一起销毁，字体和刷子要自己还
        let _ = DestroyWindow(win.hwnd);
        if win.font != 0 {
            let _ = DeleteObject(HGDIOBJ(win.font as *mut core::ffi::c_void));
        }
        if win.brush != 0 {
            let _ = DeleteObject(HGDIOBJ(win.brush as *mut core::ffi::c_void));
        }
    }
}

/// 按字符数估高度：一行结果不该顶着一个 420px 的空窗口。
fn fit_height(text: &str, w: i32, s: f32) -> i32 {
    let text_w = (w as f32 - 36.0 * s - 18.0 * s).max(80.0);
    let per_line = (text_w / (13.5 * s)).max(8.0);
    let rows: f32 = text
        .lines()
        .map(|l| (l.chars().count() as f32 / per_line).ceil().max(1.0))
        .sum::<f32>()
        .max(1.0);
    (58.0 * s + rows.clamp(1.0, 40.0) * 21.0 * s + 34.0 * s).round() as i32
}

fn make_font(px: i32) -> isize {
    unsafe {
        let face = zx_core::win::wide("Microsoft YaHei UI");
        CreateFontW(
            -px.max(8),
            0, 0, 0,
            400,
            0, 0, 0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            PCWSTR(face.as_ptr()),
        )
        .0 as isize
    }
}

impl ResultWin {
    fn sc(&self, v: f32) -> f32 { v * self.dpi as f32 / 96.0 }

    /// (底色, 文字色, 描边色, 次要文字色)
    fn palette(&self) -> (u32, u32, u32, u32) {
        if self.light {
            (0xFFF7F7F7, 0xFF1A1A1A, 0x22000000, 0x991A1A1A)
        } else {
            (0xFF1F1F1F, 0xFFF2F2F2, 0x33FFFFFF, 0x99F2F2F2)
        }
    }

    /// EDIT 的底色必须和面板一致，否则正文后面会露出一块别的颜色。
    fn brush_color(&self) -> COLORREF {
        rgb(self.palette().0)
    }

    fn copy_rect(&self) -> RECT {
        let w = self.sc(96.0) as i32;
        let h = self.sc(26.0) as i32;
        let right = self.w - self.sc(18.0) as i32 - self.sc(26.0) as i32 - self.sc(8.0) as i32;
        let top = self.sc(14.0) as i32;
        RECT { left: right - w, top, right, bottom: top + h }
    }

    fn close_rect(&self) -> RECT {
        let d = self.sc(26.0) as i32;
        let right = self.w - self.sc(18.0) as i32;
        let top = self.sc(14.0) as i32;
        RECT { left: right - d, top, right, bottom: top + d }
    }

    /// 正文区（EDIT 的位置，物理像素）
    fn body_rect_i(&self) -> (i32, i32, i32, i32) {
        let pad = self.sc(18.0) as i32;
        let top = self.sc(58.0) as i32;
        let bottom = self.h - self.sc(32.0) as i32;
        (pad, top, (self.w - pad * 2).max(60), (bottom - top).max(40))
    }

    /// 正文区外框（画在 EDIT 外面，否则 EDIT 自己的底色会把描边盖掉）
    fn body_frame(&self) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = self.body_rect_i();
        let m = self.sc(4.0);
        (x as f32 - m, y as f32 - m, w as f32 + m * 2.0, h as f32 + m * 2.0)
    }

    fn draw(&mut self) {
        let hdc = unsafe { GetDC(self.hwnd) };
        self.draw_to(hdc);
        unsafe {
            ReleaseDC(self.hwnd, hdc);
            // 子窗口（EDIT）在父窗口 BitBlt 时会被盖住，画完让它自己补一遍
            let _ = InvalidateRect(self.edit, None, false);
            let _ = UpdateWindow(self.edit);
        }
    }

    fn draw_to(&mut self, hdc: HDC) {
        let (bg, fg, line, dim) = self.palette();
        let s = self.sc(1.0);
        let (w, h) = (self.w as f32, self.h as f32);
        // 先把布局算完再借画布：`&mut self.canvas` 之后不能再读 self 的其他字段
        let cr = self.copy_rect();
        let xr = self.close_rect();
        let bf = self.body_frame();
        let lines = self.text.lines().count().max(1);
        let empty = self.text.trim().is_empty();
        let sub = if empty {
            "没有识别到文字，可以框小一点或放大后再试".to_string()
        } else {
            format!("{lines} 行 · 可直接修改后再复制")
        };
        let c = &mut self.canvas;

        c.clear(0);
        c.fill_rect(0.0, 0.0, w, h, bg);
        c.stroke_round(0.5, 0.5, w - 1.0, h - 1.0, 10.0 * s, 1.0, line);
        // 正文外框：不给个框的话，白底窗口里的可编辑区跟面板分不出来
        c.stroke_round(bf.0, bf.1, bf.2, bf.3, 6.0 * s, 1.0, line);

        c.text_ex(
            &format!("文字识别 · {}", engine_name()),
            18.0 * s, 8.0 * s, 360.0 * s, 26.0 * s, 15.0 * s, fg,
            zx_ui::TextAlign::Near, zx_ui::VAlign::Center, true, false,
        );
        c.text_ex(
            &sub,
            18.0 * s, 32.0 * s, 360.0 * s, 18.0 * s, 11.0 * s, dim,
            zx_ui::TextAlign::Near, zx_ui::VAlign::Center, false, false,
        );

        // 复制全部
        let (cw, chh) = ((cr.right - cr.left) as f32, (cr.bottom - cr.top) as f32);
        let hot = self.hover == 1;
        c.fill_round(
            cr.left as f32, cr.top as f32, cw, chh, 6.0 * s,
            if hot { 0xFF2E90FA } else { 0x332E90FA },
        );
        c.text_ex(
            "复制全部", cr.left as f32, cr.top as f32, cw, chh, 12.0 * s,
            if hot { 0xFFFFFFFF } else { fg },
            zx_ui::TextAlign::Center, zx_ui::VAlign::Center, false, false,
        );

        // 关闭
        let (dw, dh) = ((xr.right - xr.left) as f32, (xr.bottom - xr.top) as f32);
        if self.hover == 2 {
            c.fill_round(xr.left as f32, xr.top as f32, dw, dh, 6.0 * s, 0x22FFFFFF);
        }
        c.text_ex(
            "×", xr.left as f32, xr.top as f32, dw, dh, 16.0 * s,
            if self.hover == 2 { fg } else { dim },
            zx_ui::TextAlign::Center, zx_ui::VAlign::Center, false, false,
        );

        c.text_ex(
            "选中一段可单独复制 · Ctrl+A 全选 · Ctrl+C 复制 · Esc 关闭",
            18.0 * s, h - 30.0 * s, (w - 36.0 * s).max(60.0), 20.0 * s, 11.0 * s, dim,
            zx_ui::TextAlign::Near, zx_ui::VAlign::Center, false, false,
        );

        c.blit(hdc, 0, 0);
    }

    /// 输入框里的当前内容（用户可能已经改过，复制要以它为准）
    fn current_text(&self) -> String {
        edit_text(self.edit)
    }

    fn hit(&self, x: i32, y: i32) -> u8 {
        let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if inside(&self.copy_rect()) {
            1
        } else if inside(&self.close_rect()) {
            2
        } else {
            0
        }
    }
}

fn edit_text(edit: HWND) -> String {
    if edit.is_invalid() {
        return String::new();
    }
    let n = unsafe { GetWindowTextLengthW(edit) };
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; n as usize + 1];
    let got = unsafe { GetWindowTextW(edit, &mut buf) };
    String::from_utf16_lossy(&buf[..got.max(0) as usize])
}

fn rgb(argb: u32) -> COLORREF {
    let r = (argb >> 16) & 0xFF;
    let g = (argb >> 8) & 0xFF;
    let b = argb & 0xFF;
    COLORREF(b << 16 | g << 8 | r)
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

/// EDIT 的子类化：Esc 关窗、Ctrl+A 全选（原生 EDIT 两个都不管）。
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_KEYDOWN {
        let key = w.0 as u16;
        let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16) & 0x8000 != 0;
        if key == VK_ESCAPE.0 {
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(parent, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        if ctrl && key == b'A' as u16 {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            return LRESULT(0);
        }
    }
    let old = OLD_EDIT.load(Ordering::SeqCst);
    if old == 0 {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    type Proc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    let old: Proc = std::mem::transmute(old);
    CallWindowProcW(Some(old), hwnd, msg, w, l)
}

fn subclass_edit(edit: HWND) {
    unsafe {
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as usize as isize);
        OLD_EDIT.store(old, Ordering::SeqCst);
    }
}

fn window_left_top(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
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
                        hwnd, HWND_TOPMOST, rect.left + dx, rect.top + dy, 0, 0,
                        SWP_NOACTIVATE | SWP_NOSIZE,
                    );
                }
                let (x, y) = window_left_top(hwnd);
                let hot = win.hit(pt.x - x, pt.y - y);
                if hot != win.hover {
                    win.hover = hot;
                    win.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let (x0, y0) = window_left_top(hwnd);
            let (lx, ly) = (pt.x - x0, pt.y - y0);
            with_win(|win| match win.hit(lx, ly) {
                1 => {
                    let text = win.current_text();
                    if !text.trim().is_empty() {
                        copy_text(&text);
                    }
                }
                2 => {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                _ => {
                    // 只有标题栏能拖：正文区留给 EDIT 做框选
                    let head = (52.0 * win.dpi as f32 / 96.0) as i32;
                    if ly >= 0 && ly < head {
                        win.drag = Some((pt, RECT { left: x0, top: y0, right: 0, bottom: 0 }));
                        let _ = SetCapture(hwnd);
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with_win(|win| win.drag = None);
            LRESULT(0)
        }
        WM_COMMAND => {
            // EN_CHANGE：用户改了正文，行数提示要跟着变
            let code = ((w.0 >> 16) & 0xFFFF) as u32;
            if code == 0x0300 {
                with_win(|win| {
                    win.text = edit_text(win.edit);
                    win.draw();
                });
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16) & 0x8000 != 0;
            let key = w.0 as u16;
            if key == VK_ESCAPE.0 {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            } else if ctrl && key == b'C' as u16 {
                with_win(|win| {
                    let text = win.current_text();
                    if !text.trim().is_empty() {
                        copy_text(&text);
                    }
                });
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let (x, y) = window_left_top(hwnd);
            let hand =
                with_win(|win| win.hit(pt.x - x, pt.y - y) != 0).unwrap_or(false);
            let cur = if hand { IDC_HAND } else { IDC_ARROW };
            let _ = SetCursor(LoadCursorW(None, cur).unwrap_or_default());
            LRESULT(1)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let hdc = HDC(w.0 as *mut core::ffi::c_void);
            let mut brush = 0isize;
            let mut fg = COLORREF(0);
            let mut bg = 0xFF1F1F1F;
            with_win(|win| {
                let (b, t, _, _) = win.palette();
                bg = b;
                fg = rgb(t);
                brush = win.brush;
            });
            if brush != 0 {
                SetTextColor(hdc, fg);
                SetBkColor(hdc, rgb(bg));
                SetBkMode(hdc, TRANSPARENT);
                return LRESULT(brush);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with_win(|win| win.draw_to(hdc));
            let _ = EndPaint(hwnd, &ps);
            // 父窗口重绘会把子控件裁掉，EndPaint 之后单独补一遍
            with_win(|win| {
                let _ = InvalidateRect(win.edit, None, false);
                let _ = UpdateWindow(win.edit);
            });
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
