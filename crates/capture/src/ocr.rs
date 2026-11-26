//! 文字识别：优先本地 PaddleOCR 模型，缺失/失败时回退 Windows 自带 OCR。
//!
//! 系统 OCR 不联网、不随包携带模型；代价是依赖系统语言包。
//! 链路：画布 → PNG → 内存流 → BitmapDecoder → SoftwareBitmap → OcrEngine。

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
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
/// 异步动作（翻译/AI）完成后的回投消息。
///
/// `WPARAM` 带请求序号、`LPARAM` 带窗口代号——消息本身必须能自证"我是哪一次、
/// 哪个窗口的"，只靠一个共享槽位是分不清的。
const WM_ACT_DONE: u32 = 0x8000 + 7;
/// 远程动作（翻译/AI）的返回：译文 + 可选警告（例如"3 段未翻译，保留原文"）。
type RemoteResult = Result<(String, Option<String>), String>;
/// 一条待取的动作结果：`(序号, 动作号, 结果)`。
type ActItem = (u64, u8, RemoteResult);
/// 待取的动作结果队列。
///
/// 用队列而不是单槽：连点「翻译」和「AI 整理」时两条结果都要留住。以前后写的
/// 直接覆盖先写的，先完成的那条就被静默丢掉，界面上表现为"点了没反应"。
/// 上限只是防御——正常情况下 UI 线程随到随取，队列长度不超过 2。
static ACT_DONE: std::sync::Mutex<Vec<ActItem>> = std::sync::Mutex::new(Vec::new());
/// 请求序号。每次发起动作自增，回投时用它把结果对上号。
static ACT_SEQ: AtomicU64 = AtomicU64::new(1);
/// 结果窗口代号。每次 `show()` 自增；回投时对不上说明窗口已经换了一个，
/// 这条结果属于上一个窗口（用户已经关了或重新识别了），必须丢掉，
/// 否则会把旧翻译写进新窗口的正文。
static ACT_GEN: AtomicU64 = AtomicU64::new(0);
/// 结果队列的上限（异常情况下的兜底，别让它无限长）。
const ACT_BACKLOG: usize = 8;
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
    /// 原图像素（i32 w/h + RGBA）。表格识别要重新跑 det，不能只留文本。
    doc_px: Option<(i32, i32, Vec<u8>)>,
    /// 动作结果/错误，显示在副标题那一行
    status: Option<String>,
    /// 翻译目标语言（结果窗口里选，初值取设置的默认）
    lang: String,
    /// 等待期间正文被用户改过时，结果先挂在这里，不直接覆盖。
    /// `(结果文本, 成功提示)`；点底栏那行提示才写进正文。
    pending: Option<(String, String)>,
    /// 当前这次动作发起时的正文快照。结果回来拿它和当前正文比，
    /// 不一致说明用户中途改过字，不能覆盖。
    remote_snapshot: String,
}

thread_local! {
    static WIN: Cell<*mut ResultWin> = const { Cell::new(std::ptr::null_mut()) };
    /// `with_win` 的重入计数。嵌套调用 = 同时持有两个 `&mut ResultWin`，是 UB。
    static WIN_DEPTH: Cell<u32> = const { Cell::new(0) };
}

static OLD_EDIT: AtomicIsize = AtomicIsize::new(0);

/// 统一成 CRLF。已经是 CRLF 的不会变成 CRCRLF。
fn crlf(s: &str) -> String {
    let lf = String::from("\u{000A}");
    let crlf = String::from("\u{000D}\u{000A}");
    s.replace(&crlf, &lf).replace(&lf, &crlf)
}

/// 借到窗口状态的唯一入口。
///
/// 用裸指针而不是 `RefCell`：窗口过程里到处是 `&mut`，`RefCell` 的借用检查会在
/// 编译期之外炸成运行时 panic，而这里所有调用点都是"短借用、不嵌套"。
/// 代价是编译器管不了重入——所以用一个深度计数在 debug 下把它变成一条明确的断言，
/// 而不是让两个 `&mut` 悄悄同时存在（那是 UB，表现为偶发错绘/崩溃）。
fn with_win<R>(f: impl FnOnce(&mut ResultWin) -> R) -> Option<R> {
    let p = WIN.with(|c| c.get());
    if p.is_null() {
        return None;
    }
    WIN_DEPTH.with(|d| {
        let n = d.get() + 1;
        d.set(n);
        debug_assert!(n == 1, "with_win 不能嵌套：同一时刻只能有一个 &mut ResultWin");
        let r = unsafe { f(&mut *p) };
        d.set(n - 1);
        Some(r)
    })
}

pub fn is_open() -> bool {
    WIN.with(|c| !c.get().is_null())
}

pub fn show(text: String, doc_px: Option<(i32, i32, Vec<u8>)>, near: POINT) {
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
        doc_px,
        status: None,
        // 初值 = 设置里的默认（默认 auto）。用户在窗口里改了只影响这一次。
        lang: zx_core::cfg::cfg().capture().translate_to,
        pending: None,
        remote_snapshot: String::new(),
    });
    // 新窗口 = 新代号：上一个窗口还没回来的翻译/AI 结果一律作废。
    // 只递增全局代号就够了——回投消息里带着发起时的代号，对不上就说明窗口换了。
    ACT_GEN.fetch_add(1, Ordering::SeqCst);

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
    // Win32 的 EDIT **只认 CRLF**：裸 LF 会被当成不可见字符吃掉，多行文本于是
    // 全挤成一行显示（用户反馈"识别结果没有换行"就是这个）。写进去之前统一成 CRLF。
    let body = zx_core::win::wide(&crlf(&win.text));
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
        let acts: [RECT; 4] = [
            self.act_rect(0),
            self.act_rect(1),
            self.act_rect(2),
            self.act_rect(3),
        ];
        let act_labels: [String; 4] = [
            self.act_label(0),
            self.act_label(1),
            self.act_label(2),
            self.act_label(3),
        ];
        let lines = self.text.lines().count().max(1);
        let empty = self.text.trim().is_empty();
        let sub = match &self.status {
            Some(st) => st.clone(),
            None if empty => "没有识别到文字，可以框小一点或放大后再试".to_string(),
            None => format!("{lines} 行 · 可直接修改后再复制"),
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
            18.0 * s, 32.0 * s, (cr.left as f32 - 30.0 * s).max(120.0), 18.0 * s, 11.0 * s, dim,
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

        // 底部动作条。表格是本地重识别，翻译/AI 走外壳注入的接口。
        for (i, r) in acts.iter().enumerate() {
            let r = *r;
            let hot = match i {
                0 => self.hover == 3,
                1 => self.hover == 4,
                2 => self.hover == 6,
                _ => self.hover == 5,
            };
            let (bw, bh) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
            c.fill_round(
                r.left as f32, r.top as f32, bw, bh, 6.0 * s,
                if hot { 0x332E90FA } else { 0x14000000 },
            );
            c.text_ex(
                &act_labels[i],
                r.left as f32, r.top as f32, bw, bh, 11.5 * s, fg,
                zx_ui::TextAlign::Center, zx_ui::VAlign::Center, false, false,
            );
        }
        let hint_x = acts[3].right as f32 + 10.0 * s;
        // 有待应用的结果时，这一行变成可点的入口；平时就是快捷键提示。
        // 位置必须与 `hint_rect()` 一致——那边负责命中，这里负责画，差一个像素点击就落空。
        let (hint, hint_color) = match &self.pending {
            Some(_) => ("结果已就绪 · 点这行替换".to_string(), 0xFF2E90FAu32),
            None => ("Ctrl+A 全选 · Ctrl+C 复制 · Esc 关闭".to_string(), dim),
        };
        c.text_ex(
            &hint,
            hint_x, h - 32.0 * s, (w - hint_x - 18.0 * s).max(40.0), 22.0 * s, 10.5 * s,
            hint_color,
            zx_ui::TextAlign::Near, zx_ui::VAlign::Center, false, false,
        );

        c.blit(hdc, 0, 0);
    }

    /// 输入框里的当前内容（用户可能已经改过，复制要以它为准）
    fn current_text(&self) -> String {
        edit_text(self.edit)
    }

    /// 底部动作按钮：0=表格 1=翻译 2=语言下拉 3=AI 整理。
    /// 翻译和语言下拉贴在一起（中间不留缝），看起来是一个整体的分体按钮。
    fn act_rect(&self, i: usize) -> RECT {
        let pad = self.sc(18.0) as i32;
        let bh = self.sc(22.0) as i32;
        let top = self.h - self.sc(32.0) as i32;
        let (left, w) = match i {
            0 => (pad, self.sc(64.0) as i32),
            1 => (pad + self.sc(70.0) as i32, self.sc(56.0) as i32),
            2 => (pad + self.sc(126.0) as i32, self.sc(20.0) as i32),
            _ => (pad + self.sc(150.0) as i32, self.sc(64.0) as i32),
        };
        RECT { left, top, right: left + w, bottom: top + bh }
    }

    fn act_label(&self, i: usize) -> String {
        match i {
            0 => "表格".into(),
            1 => {
                let l = crate::ocr_ai::lang_label(&self.lang);
                if self.lang == "auto" {
                    "翻译".into()
                } else {
                    format!("翻译·{l}")
                }
            }
            2 => "▾".into(),
            _ => "AI 整理".into(),
        }
    }

    /// 把动作结果写回正文。`note` 是成功时显示的一句话。
    fn apply(&mut self, res: Result<String, String>, note: &str) {
        match res {
            Ok(t) if !t.trim().is_empty() => self.apply_text(t, note),
            Ok(_) => {
                self.status = Some("这一块没有可用内容".into());
                self.draw();
            }
            Err(e) => {
                self.status = Some(e);
                self.draw();
            }
        }
    }

    /// 真正落文本的那一步（同步结果与"用户确认覆盖"都走它）。
    fn apply_text(&mut self, t: String, note: &str) {
        self.text = t;
        let body = zx_core::win::wide(&crlf(&self.text));
        unsafe {
            let _ = SetWindowTextW(self.edit, PCWSTR(body.as_ptr()));
        }
        self.status = Some(note.to_string());
        self.pending = None;
        self.draw();
    }

    /// 异步动作（翻译 / AI）的结果落地。
    ///
    /// 和 [`Self::apply`] 的区别只有一个但很关键：**正文在等待期间被改过就不覆盖**。
    /// 用户点完翻译接着改错字是常见操作，一秒钟后结果回来把他的修改冲掉，
    /// 那是实打实的数据丢失，而且无法撤销（`SetWindowTextW` 会清空 EDIT 的撤销栈）。
    /// 这种情况把结果挂到 `pending`，底栏换成「点这行替换」，由用户决定。
    fn apply_remote(&mut self, res: RemoteResult, note: &str, snapshot: &str) {
        match res {
            Ok((t, warn)) if !t.trim().is_empty() => {
                // 部分块没翻成功时把警告挂进标题行：否则用户看到中英混杂
                // 只会以为是 bug，不知道自己那句话被原样保留了
                let note = match &warn {
                    Some(w) => format!("{note} · {w}"),
                    None => note.to_string(),
                };
                if self.current_text() == snapshot {
                    self.apply_text(t, &note);
                } else {
                    self.status = Some(format!("{note} · 正文已改动，结果未覆盖"));
                    self.pending = Some((t, note));
                    self.draw();
                }
            }
            Ok(_) => {
                self.status = Some("这一块没有可用内容".into());
                self.draw();
            }
            Err(e) => {
                self.status = Some(e);
                self.draw();
            }
        }
    }

    /// 底栏那行提示的位置。有待应用结果时它是可点的，所以矩形要和绘制严格一致。
    fn hint_rect(&self) -> RECT {
        let left = self.act_rect(3).right + self.sc(10.0) as i32;
        let top = self.h - self.sc(32.0) as i32;
        RECT {
            left,
            top,
            right: self.w - self.sc(18.0) as i32,
            bottom: top + self.sc(22.0) as i32,
        }
    }

    fn hit(&self, x: i32, y: i32) -> u8 {
        let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if inside(&self.copy_rect()) {
            return 1;
        }
        if inside(&self.close_rect()) {
            return 2;
        }
        // 7 = 应用挂起的结果。只在真的挂着一份时才认，平时这一行是死区
        if self.pending.is_some() && inside(&self.hint_rect()) {
            return 7;
        }
        for i in 0..4 {
            if inside(&self.act_rect(i)) {
                return match i {
                    0 => 3, // 表格
                    1 => 4, // 翻译
                    2 => 6, // 语言下拉
                    _ => 5, // AI 整理
                };
            }
        }
        0
    }
}

/// 表格识别：拿原图像素重建画布再跑 det。
///
/// 为什么在 UI 线程跑：`Canvas` 里握着 HDC/GDI+ 句柄，不是 `Send`；而 WinML 的会话
/// 也在 `thread_local` 里，换线程要重新加载模型。所以不挪线程，改成在 ppocr 里
/// 加软截止（见 `ppocr::TABLE_DEADLINE_MS`），把最坏耗时压在"未响应"阈值之下。
fn run_table(_hwnd: HWND) {
    let Some((w, h, px)) = with_win(|win| {
        win.status = Some("表格识别中…".into());
        win.draw();
        win.doc_px.clone()
    }).flatten() else {
        with_win(|win| {
            win.status = Some("没有原图，无法重新识别（这次是从文本打开的）".into());
            win.draw();
        });
        return;
    };
    let dpi = with_win(|win| win.dpi).unwrap_or(96);
    let mut c = Canvas::new(w, h, dpi);
    {
        let dst = c.pixels_mut();
        let n = (w as usize * h as usize * 4).min(px.len()).min(dst.len());
        dst[..n].copy_from_slice(&px[..n]);
    }
    let res = crate::ppocr::table_markdown(&c);
    // 表格失败时别把正文清空：错误放副标题，原文留着
    with_win(|win| match res {
        Ok(md) if !md.trim().is_empty() => win.apply(Ok(md), "已转成表格"),
        Ok(_) => {
            win.status = Some("这块内容不像表格（至少要两行两列）".into());
            win.draw();
        }
        Err(e) => {
            win.status = Some(e);
            win.draw();
        }
    });
}

/// 选目标语言。用系统菜单（`TrackPopupMenu`），省得自己画一套下拉。
///
/// 选完**立刻翻译**：点这个按钮的人意图就是"换个语言再翻一次"，让他再点一次
/// 「翻译」是多余的。
fn pick_lang(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let cur = with_win(|win| win.lang.clone()).unwrap_or_default();
        for (i, (code, name)) in crate::ocr_ai::LANGS.iter().enumerate() {
            let mut flags = MF_STRING;
            if *code == cur {
                flags |= MF_CHECKED;
            }
            // 绑定到局部量：`wide()` 返回的是临时 Vec，直接 `.as_ptr()` 传进去
            // 靠的是"Win32 在调用内复制字符串"这条隐含约定，改成延后使用就是 UAF
            let name_w = zx_core::win::wide(name);
            let _ = AppendMenuW(menu, flags, i + 1, PCWSTR(name_w.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // 菜单要在自己窗口是前台时才收得干净（点别处能关掉），这是 Win32 的老规矩
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let idx = cmd.0 as usize;
        if idx >= 1 && idx <= crate::ocr_ai::LANGS.len() {
            let code = crate::ocr_ai::LANGS[idx - 1].0.to_string();
            with_win(|win| win.lang = code);
            start_remote(hwnd, 4);
        }
    }
}

/// 翻译前的一次性同意。
///
/// 内置翻译会把**整段正文**发给第三方免费接口（有道 / Google）。OCR 的内容经常是
/// 密码、验证码、私聊、内部文档——默认静默外发是不可接受的，所以第一次必须问。
/// 配了自建 `translateUrl` 时不问：那是用户自己的服务。
///
/// 返回 `false` 表示用户拒绝，调用方不该发起任何请求。
fn ensure_translate_consent(hwnd: HWND) -> bool {
    let c = zx_core::cfg::cfg().capture();
    if !c.translate_url.trim().is_empty() || c.translate_consent {
        return true;
    }
    let body = "内置翻译会把这段文字发送到第三方免费接口（有道 / Google）。\n\n\
                如果内容里有密码、验证码或私人信息，建议先取消，\n\
                改为在设置里填一个自建翻译接口（只多发给你自己的服务）。\n\n\
                现在翻译吗？只问这一次，之后可在设置里改。";
    let title = "翻译需要联网";
    let bw = zx_core::win::wide(body);
    let tw = zx_core::win::wide(title);
    let r = unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR(bw.as_ptr()),
            PCWSTR(tw.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if r != IDYES {
        return false;
    }
    zx_core::cfg::update_cfg(|s| {
        s.capture.get_or_insert_with(Default::default).translate_consent = Some(true);
    });
    true
}

/// 翻译 / AI 整理：纯文本进出，扔到工作线程，完成后回投消息。
///
/// 回投走 `WM_ACT_DONE(wparam = 序号, lparam = 窗口代号)`，结果体放 [`ACT_DONE`] 队列。
/// 序号把"哪一次请求"对上号，代号把"哪个窗口"对上号——两者缺一个，迟到的结果
/// 就会写进错的地方（见 `ACT_DONE` 的注释）。
fn start_remote(hwnd: HWND, act: u8) {
    // 同意的闸门放在这里：翻译有两条入口（「翻译」按钮和语言下拉选完自动翻），
    // 各写一遍迟早会漏一个。
    if act == 4 && !ensure_translate_consent(hwnd) {
        with_win(|win| {
            win.status = Some("已取消翻译 · 可在设置里允许内置接口".into());
            win.draw();
        });
        return;
    }
    let text = with_win(|win| win.current_text()).unwrap_or_default();
    let lang = with_win(|win| win.lang.clone()).unwrap_or_else(|| "auto".into());
    if text.trim().is_empty() {
        with_win(|win| {
            win.status = Some("正文是空的".into());
            win.draw();
        });
        return;
    }
    // 新的请求作废旧结果：上一次挂着的"点这行替换"已经没意义了
    let gen = ACT_GEN.load(Ordering::SeqCst);
    let seq = ACT_SEQ.fetch_add(1, Ordering::SeqCst);
    with_win(|win| {
        win.pending = None;
        win.remote_snapshot = text.clone();
        win.status = Some(if act == 4 {
            format!("翻译中…（{}）", crate::ocr_ai::lang_label(&lang))
        } else {
            "AI 整理中…".into()
        });
        win.draw();
    });
    let raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        let res: RemoteResult = if act == 4 {
            crate::ocr_ai::translate(&text, &lang)
        } else {
            // AI 整理不分块，没有"部分失败"这回事
            crate::ocr_ai::polish(&text).map(|t| (t, None))
        };
        if let Ok(mut g) = ACT_DONE.lock() {
            // 异常情况下（UI 线程长时间没泵消息）别让队列无限长
            while g.len() >= ACT_BACKLOG {
                let _ = g.remove(0);
            }
            g.push((seq, act, res));
        }
        unsafe {
            let _ = PostMessageW(
                HWND(raw as *mut core::ffi::c_void),
                WM_ACT_DONE,
                WPARAM(seq as usize),
                LPARAM(gen as isize),
            );
        }
    });
}

/// 从结果队列里按序号取一条，取到就摘掉。
///
/// 代号（`gen`）对不上说明这条结果属于上一个窗口——用户已经关掉重开或重新识别了，
/// 直接丢弃。没有这道检查，迟到的旧译文会写进新窗口的正文，比丢结果更糟。
///
/// 抽成独立函数是为了能单测：窗口过程本身没法在 CI 里跑。
fn take_act(
    queue: &mut Vec<ActItem>,
    seq: u64,
    gen: u64,
    cur_gen: u64,
) -> Option<(u8, RemoteResult)> {
    if gen != cur_gen {
        queue.retain(|(s, _, _)| *s != seq);
        return None;
    }
    let i = queue.iter().position(|(s, _, _)| *s == seq)?;
    let (_, act, res) = queue.remove(i);
    Some((act, res))
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
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { edit_proc_inner(hwnd, msg, w, l) })
}

unsafe fn edit_proc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

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
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

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
            let hit = with_win(|win| win.hit(lx, ly)).unwrap_or(0);
            match hit {
                1 => {
                    let text = with_win(|win| win.current_text()).unwrap_or_default();
                    if !text.trim().is_empty() {
                        copy_text(&text);
                    }
                }
                2 => {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                3 => run_table(hwnd),
                4 => start_remote(hwnd, 4),
                5 => start_remote(hwnd, 5),
                6 => pick_lang(hwnd),
                7 => {
                    // 应用挂起的翻译/AI 结果（用户确认覆盖自己的改动）
                    with_win(|win| {
                        if let Some((t, note)) = win.pending.take() {
                            win.apply(Ok(t), &note);
                        }
                    });
                }
                _ => {
                    // 只有标题栏能拖：正文区留给 EDIT 做框选
                    with_win(|win| {
                        let head = (52.0 * win.dpi as f32 / 96.0) as i32;
                        if ly >= 0 && ly < head {
                            win.drag = Some((pt, RECT { left: x0, top: y0, right: 0, bottom: 0 }));
                            let _ = SetCapture(win.hwnd);
                        }
                    });
                }
            }
            LRESULT(0)
        }
        WM_ACT_DONE => {
            let seq = w.0 as u64;
            let gen = l.0 as u64;
            let payload = ACT_DONE
                .lock()
                .ok()
                .and_then(|mut g| take_act(&mut g, seq, gen, ACT_GEN.load(Ordering::SeqCst)));
            if let Some((act, res)) = payload {
                let note = if act == 4 { "已翻译" } else { "已由 AI 整理" };
                with_win(|win| {
                    let snapshot = win.remote_snapshot.clone();
                    win.apply_remote(res, note, &snapshot);
                });
            }
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

#[cfg(test)]
mod tests {
    use super::take_act;

    type Q = Vec<super::ActItem>;

    fn q(items: &[(u64, u8, &str)]) -> Q {
        items
            .iter()
            .map(|(s, a, t)| (*s, *a, Ok::<_, String>(((*t).to_string(), None))))
            .collect()
    }

    #[test]
    fn takes_the_matching_seq_and_leaves_the_rest() {
        // 连点「翻译」(seq 1) 和「AI 整理」(seq 2)：两条都得留住，各取各的
        let mut queue = q(&[(1, 4, "译文"), (2, 5, "整理后")]);
        assert_eq!(take_act(&mut queue, 2, 7, 7).map(|(a, _)| a), Some(5));
        assert_eq!(queue.len(), 1, "另一条不能被顺带吃掉");
        assert_eq!(take_act(&mut queue, 1, 7, 7).map(|(a, _)| a), Some(4));
        assert!(queue.is_empty());
    }

    #[test]
    fn unknown_seq_is_a_noop() {
        let mut queue = q(&[(5, 4, "x")]);
        assert!(take_act(&mut queue, 99, 7, 7).is_none());
        assert_eq!(queue.len(), 1, "取不到就不能动别人的结果");
    }

    #[test]
    fn result_from_a_dead_window_is_dropped() {
        // 用户关了窗口重开（gen 7 → 8），上一个窗口的译文到达时必须是空操作，
        // 而且要从队列里清掉，否则会一直占着位置等到被上限挤掉
        let mut queue = q(&[(1, 4, "旧窗口的译文")]);
        assert!(take_act(&mut queue, 1, 7, 8).is_none());
        assert!(queue.is_empty(), "属于旧窗口的结果应当被摘掉");
    }

    #[test]
    fn each_result_is_delivered_exactly_once() {
        let mut queue = q(&[(1, 4, "a")]);
        assert!(take_act(&mut queue, 1, 7, 7).is_some());
        // 同一条消息重复投递（PostMessage 之后又被转发之类）不能应用两次
        assert!(take_act(&mut queue, 1, 7, 7).is_none());
    }
}
