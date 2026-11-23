//! 长截图：选定区域后，用户自己滚动页面，程序按帧内容找重叠、边滚边拼。
//!
//! **不注入滚动、不接管滚轮**，只按固定间隔抓同一块区域，用行指纹找竖直位移，
//! 把新露出来的那几行追加到累积图。好处是对任何程序都成立（浏览器、聊天记录、编辑器），
//! 坏处是“页面必须真的在滚”，以及吸顶/吸底元素会干扰匹配。
//!
//! 行指纹：每行在水平方向等距采样 16 个点做 FNV-1a。找 dy 时要求
//! `new[0..h-dy]` 与 `prev[dy..h]` 逐行相等（允许 2 行噪声，光标闪烁/动画），
//! 并且重叠不少于 1/4，取"匹配上的最大滚动量"。
//!
//! 关键点：长截图**不能**让全屏覆盖层留在屏幕上——那会把用户的滚轮事件全吃掉，
//! 页面根本滚不动。选区结束就关掉覆盖层，只留一条小控制条；控制条不滚页面，
//! 只负责定时抓图、显示进度、收“完成/取消”。

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::Canvas;

use crate::register_class_dbl;
use crate::screen::Frame;

const MAX_HEIGHT: i32 = 20000;
const CLASS: &str = "ZhuxiaCaptureLong";
const WM_LONG_DONE: u32 = WM_APP + 102;

struct Session {
    rect: RECT,
    w: i32,
    h: i32,
    /// 累积图：BGRA，行序自上而下，stride = w*4。
    pixels: Vec<u8>,
    total_h: i32,
    /// 上一次抓到的整帧的行指纹（长度 = h）。
    hashes: Vec<RowSig>,
    /// 连续多少帧没找到位移（用来在控制条上提示"没滚"）。
    idle: u32,
}

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
/// 是否在采集：`暂停` 置 false，`开始` 置 true。暂停期间定时器照跑但不抓帧。
static RUNNING: AtomicBool = AtomicBool::new(true);

fn lock() -> std::sync::MutexGuard<'static, Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|p| p.into_inner())
}

/// 连续多少帧没检测到滚动（控制条据此提示用户）。
pub fn idle_frames() -> u32 {
    lock().as_ref().map(|s| s.idle).unwrap_or(0)
}

pub fn height() -> i32 {
    lock().as_ref().map(|s| s.total_h).unwrap_or(0)
}

/// 开始：以选区当前内容作为第一屏。
pub fn start(rect: RECT) -> bool {
    if rect.right - rect.left < 8 || rect.bottom - rect.top < 8 {
        return false;
    }
    let Some(frame) = Frame::grab_rect(rect) else { return false };
    let (w, h) = (frame.w, frame.h);
    let mut pixels = Vec::with_capacity(w as usize * 4 * h as usize);
    unsafe {
        let src = frame.pixels_ptr();
        let len = frame.pixels_len();
        pixels.extend_from_slice(std::slice::from_raw_parts(src, len));
    }
    let hashes = row_hashes(&frame);
    let mut g = lock();
    *g = Some(Session { rect, w, h, pixels, total_h: h, hashes, idle: 0 });
    zx_core::log::info(format!("长截图开始：{}x{} at ({},{})", w, h, rect.left, rect.top));
    true
}

/// 抓一帧并尝试追加。返回当前总高度（没在进行中返回 0）。
pub fn tick() -> i32 {
    let mut g = lock();
    let Some(s) = g.as_mut() else { return 0 };
    if s.total_h >= MAX_HEIGHT {
        return s.total_h;
    }
    let Some(frame) = Frame::grab_rect(s.rect) else { return s.total_h };
    if frame.w != s.w || frame.h != s.h {
        return s.total_h;
    }
    let hashes = row_hashes(&frame);
    let trace = std::env::var("ZHUXIA_LONG_TRACE").is_ok();
    let dy = match find_shift(&hashes, &s.hashes, s.h) {
        Some(d) if d > 0 && d < s.h => d,
        _ => {
            if trace {
                zx_core::log::info(format!("长截图：这一帧没找到滚动位移（{}x{}）", s.w, s.h));
            }
            s.idle = s.idle.saturating_add(1);
            s.hashes = hashes;
            return s.total_h;
        }
    };
    if trace {
        zx_core::log::info(format!("长截图：找到位移 dy={dy}，累计 {}", s.total_h + dy));
    }
    // 追加新露出的底部 dy 行
    let stride = s.w as usize * 4;
    let start_row = (s.h - dy) as usize;
    unsafe {
        let base = frame.pixels_ptr().add(start_row * stride);
        let bytes = std::slice::from_raw_parts(base, dy as usize * stride);
        s.pixels.extend_from_slice(bytes);
    }
    s.total_h += dy;
    s.hashes = hashes;
    s.idle = 0;
    s.total_h
}

/// 结束并交出累积图。
pub fn finish() -> Option<Canvas> {
    let mut g = lock();
    let s = g.take()?;
    let take = s.total_h.max(s.h) as usize;
    let mut out = Canvas::new(s.w, take as i32, 96);
    let need = s.w as usize * 4 * take;
    let have = s.pixels.len().min(need);
    out.pixels_mut()[..have].copy_from_slice(&s.pixels[..have]);
    zx_core::log::info(format!("长截图完成：{}x{}", s.w, take));
    Some(out)
}

pub fn cancel() {
    if lock().take().is_some() {
        zx_core::log::info("长截图已取消");
    }
}

/// 缩略预览：把累积图等距采样成 `pw × ph`。
pub fn preview(pw: i32, ph: i32) -> Option<Canvas> {
    let g = lock();
    let s = g.as_ref()?;
    let (pw, ph) = (pw.max(4), ph.max(4));
    let mut out = Canvas::new(pw, ph, 96);
    let dst_stride = out.stride();
    let src_stride = s.w as usize * 4;
    let dst = out.pixels_mut();
    for y in 0..ph as usize {
        let sy = (y * s.total_h as usize / ph as usize).min(s.total_h as usize - 1);
        for x in 0..pw as usize {
            let sx = (x * s.w as usize / pw as usize).min(s.w as usize - 1);
            let so = sy * src_stride + sx * 4;
            let dof = y * dst_stride + x * 4;
            dst[dof..dof + 4].copy_from_slice(&s.pixels[so..so + 4]);
            dst[dof + 3] = 255;
        }
    }
    Some(out)
}

/// 一行的「列带亮度签名」。
///
/// **不能用整行哈希**：Windows 11 的滚动是亚像素的，同一行文字滚过之后 ClearType
/// 取样相位变了，逐字节比永远不相等（实测记事本滚一次只有 358/841 行相等）。
/// 折不下来就得比「差不多」，但纯色段会把差异稀释掉，所以只比**有内容的那几段**。
type RowSig = [u8; BANDS];
const BANDS: usize = 24;
/// 段亮度偏离本行最亮值多少，才算「这段有内容」。
const BAND_INK: i32 = 5;
/// 有内容的段允许的亮度差（亚像素抗锯齿通常 < 8）。
const BAND_TOL: i32 = 8;
/// 一帧至少要有这么多「有内容」的行参与投票（太少等于没证据）。
const MIN_ROWS: usize = 32;
/// 有内容的行里，能对上的比例。
const MATCH_RATIO: f32 = 0.85;

fn row_hashes(f: &Frame) -> Vec<RowSig> {
    let w = f.w as usize;
    let stride = w * 4;
    let mut out = Vec::with_capacity(f.h as usize);
    unsafe {
        let base = f.pixels_ptr();
        for y in 0..f.h as usize {
            let row = base.add(y * stride);
            let mut sig = [0u8; BANDS];
            for (b, slot) in sig.iter_mut().enumerate() {
                let x0 = w * b / BANDS;
                let x1 = (w * (b + 1) / BANDS).max(x0 + 1).min(w);
                let mut acc = 0u32;
                for k in 0..8 {
                    let x = x0 + (x1 - x0) * k / 8;
                    let p = row.add(x.min(w - 1) * 4);
                    let (r, g, bl) = (*p as u32, *p.add(1) as u32, *p.add(2) as u32);
                    acc += (r * 299 + g * 587 + bl * 114) / 1000;
                }
                *slot = (acc / 8) as u8;
            }
            out.push(sig);
        }
    }
    out
}

/// 这一行有没有内容：至少两段明显偏离本行最亮值（纯白行对位移没有信息量）。
fn row_ink(sig: &RowSig) -> usize {
    let max = sig.iter().copied().max().unwrap_or(0) as i32;
    sig.iter().filter(|v| (max - **v as i32).abs() > BAND_INK).count()
}

/// 两行是不是「同一行内容」：只比双方都有内容的那几段，要求逐段都接近。
///
/// 这是能区分「第 100 行」和「第 101 行」的关键——整行求差会被大片留白稀释，
/// 只比有墨的段，一个数字变了就会露馅。
fn rows_match(a: &RowSig, b: &RowSig) -> bool {
    let max_a = a.iter().copied().max().unwrap_or(0) as i32;
    let max_b = b.iter().copied().max().unwrap_or(0) as i32;
    let mut compared = 0usize;
    for i in 0..BANDS {
        let ink = (max_b - b[i] as i32).abs() > BAND_INK && (max_a - a[i] as i32).abs() > BAND_INK;
        if !ink {
            continue;
        }
        compared += 1;
        if (a[i] as i32 - b[i] as i32).abs() > BAND_TOL {
            return false;
        }
    }
    // 只有一两段墨的对比不算证据：可能只是列宽不同撞上的
    compared >= 2
}

/// 在 `prev` 里找 `cur` 的起始偏移：`cur[0..n-dy]` 和图 `prev[dy..n]` 是同一段内容。
///
/// 从大到小找：内容自相似时（表格、重复行、等宽数字）小的 dy 也能对上，
/// 而真实位移是**最大的**那个合法重叠——比它更大的位移必然对不上。
/// 只拿有内容的行投票，纯白/纯色行对任何 dy 都"相等"，会把结果带偏。
/// 追加区（底部 `dy` 行）到底是不是**新内容**？
///
/// 静态画面上去比也能"对上"（自相似的行太多），但底部那几行和上一帧同一位置是
/// 一模一样的——真滚动时它们是刚从下面露出来的新内容。这条是防假命中的最后一道闸，
/// 没有它，静态桌面会一直"拼"到 9000+ px。
fn bottom_is_new(cur: &[RowSig], prev: &[RowSig], n: usize, dy: usize) -> bool {
    let start = n - dy;
    let mut total = 0usize;
    let mut changed = 0usize;
    for i in start..n {
        if row_ink(&cur[i]) < 2 {
            continue;
        }
        total += 1;
        if !rows_match(&cur[i], &prev[i]) {
            changed += 1;
        }
    }
    // 底部本身是空白（没什么可追加的）也算合理
    total < 3 || changed as f32 / total as f32 >= 0.5
}

fn find_shift(cur: &[RowSig], prev: &[RowSig], h: i32) -> Option<i32> {
    let n = h as usize;
    if cur.len() < n || prev.len() < n {
        return None;
    }
    let min_overlap = (n / 4).max(8);
    let mut best: Option<(f32, i32)> = None;
    // 从大到小扫，比例相同就保留更大的 dy（内容自相似时，真实位移是最大的那个）。
    for dy in (1..=n - min_overlap).rev() {
        let overlap = n - dy;
        let mut total = 0usize;
        let mut good = 0usize;
        for i in 0..overlap {
            let target = &prev[i + dy];
            if row_ink(target) < 2 {
                continue;
            }
            total += 1;
            if rows_match(&cur[i], target) {
                good += 1;
            }
        }
        if total < MIN_ROWS {
            continue;
        }
        let ratio = good as f32 / total as f32;
        if ratio >= 1.0 && bottom_is_new(cur, prev, n, dy) {
            return Some(dy as i32);
        }
        // 取**最佳比例**而不是"第一个过线的"：吸顶标题栏会让固定几行永远对不上，
        // 但那几行对所有 dy 都一样，不影响比例峰值落在真实位移上。
        if best.map(|(r, _)| ratio > r + 0.01).unwrap_or(true) {
            best = Some((ratio, dy as i32));
        }
    }
    match best {
        Some((r, dy)) if r >= MATCH_RATIO => {
            if bottom_is_new(cur, prev, n, dy as usize) {
                Some(dy)
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- 控制条窗口

static DONE: AtomicBool = AtomicBool::new(false);

struct Bar {
    hwnd: HWND,
    canvas: Canvas,
    hover: i32,
}

thread_local! {
    static BAR: Cell<*mut Bar> = const { Cell::new(std::ptr::null_mut()) };
}

fn with_bar<R>(f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    BAR.with(|c| {
        let p = c.get();
        if p.is_null() { None } else { Some(unsafe { f(&mut *p) }) }
    })
}

/// 跑一次长截图：`rect` 是选区。返回拼好的图（取消返回 None）。
pub fn run(rect: RECT) -> Option<Canvas> {
    if !start(rect) {
        return None;
    }
    register_class_dbl(CLASS, bar_proc);
    let (w, h) = (BAR_W, BAR_H);
    RUNNING.store(true, Ordering::SeqCst);
    // 控制条要避开选区，否则会被自己抓进去（CAPTUREBLT 会拍分层窗口）
    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    let x = if rect.right + w + 16 < vx + vw { rect.right + 16 } else { (rect.left - w - 16).max(vx + 8) };
    let y = rect.top.clamp(vy + 8, (vy + vh - h - 8).max(vy + 8));
    let Some(hwnd) = zx_ui::window::create_window(CLASS, "驻匣 · 长截图", WS_POPUP,
                                                  WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, w, h) else {
        cancel();
        return None;
    };
    let mut bar = Box::new(Bar { hwnd, canvas: Canvas::new(w, h, 96), hover: 0 });
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x.max(vx + 8), y, w, h, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
    }
    bar.draw();
    let ptr = Box::into_raw(bar);
    BAR.with(|c| c.set(ptr));
    DONE.store(false, Ordering::SeqCst);
    unsafe { let _ = SetTimer(hwnd, 1, 140, None); }
    // 自检：`ZHUXIA_CAPTURE_LONG_AUTO=1` 时 2.5 秒后自动完成（验证控制条/抓取/收尾）
    if std::env::var("ZHUXIA_CAPTURE_LONG_AUTO").ok().as_deref() == Some("1") {
        unsafe { let _ = SetTimer(hwnd, 2, 2500, None); }
    }

    // 局部消息循环：由 `bar_proc` 处理完 WM_LONG_DONE 后 `PostQuitMessage` 退出。
    //
    // 这里**不能**在取到消息时就 break——那样 bar_proc 收不到 WM_LONG_DONE，
    // 控制条窗口和它那份 Box 就永远留在屏幕上（之前的实现就是这样泄漏的）。
    unsafe {
        // 上一次如果"完成"和定时器同时触发，队列里可能留一条 WM_LONG_DONE；
        // 不清掉的话，下一次长截图的循环会立刻 dispatch 到它，然后 PostQuitMessage
        // —— 表现为"刚点 Ctrl+L 就结束了"。
        let mut stale = MSG::default();
        while PeekMessageW(&mut stale, None, WM_LONG_DONE, WM_LONG_DONE, PM_REMOVE).as_bool() {}

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == -1 || r.0 == 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if DONE.load(Ordering::SeqCst) { finish() } else { cancel(); None }
}

impl Bar {
    fn draw(&mut self) {
        use crate::design::{self, Tokens};
        let t: Tokens = design::tokens();
        let total = height();
        let running = RUNNING.load(Ordering::SeqCst);
        let c = &mut self.canvas;
        c.clear(0x00000000);
        c.fill_round(0.0, 0.0, c.w as f32, c.h as f32, 12.0, t.panel);
        c.stroke_round(0.5, 0.5, c.w as f32 - 1.0, c.h as f32 - 1.0, 12.0, 1.0, t.border_strong);

        design::text(c, "长截图", 14.0, 8.0, 80.0, 22.0, 13.0, t.text,
                     zx_ui::TextAlign::Near, zx_ui::VAlign::Center, 600);
        let state = if running { "采集中" } else { "已暂停" };
        design::text(c, &format!("已拼接 {total} px · {state}"), 68.0, 8.0, 240.0, 22.0,
                     11.5, if running { t.text3 } else { t.danger },
                     zx_ui::TextAlign::Near, zx_ui::VAlign::Center, 500);

        let pw = (c.w as f32) - 28.0;
        if let Some(p) = preview(pw as i32, 34) {
            c.fill_round(14.0, 34.0, pw, 34.0, 6.0, t.field);
            c.clip_round(14.0, 34.0, pw, 34.0, 6.0);
            c.draw_canvas(&p, 14.0, 34.0, pw, 34.0);
            c.reset_clip();
        }

        // 四个按钮：开始 / 暂停 / 完成 / 取消。运行中的那个是"禁用态"（浅字），
        // 免得用户以为点了没反应。
        let labels = ["开始", "暂停", "完成", "取消"];
        for (i, label) in labels.iter().enumerate() {
            let (bx, by, bw, bh) = button_rect(i);
            let hot = self.hover == i as i32 + 1;
            let actionable = if i == 0 { !running } else if i == 1 { running } else { true };
            if i == 2 {
                let bg = if hot { t.primary_hover } else { t.primary };
                c.fill_round(bx, by, bw, bh, 9.0, bg);
                design::text(c, label, bx, by, bw, bh, 12.5, t.primary_text,
                             zx_ui::TextAlign::Center, zx_ui::VAlign::Center, 600);
            } else {
                // 不可点的那个也给一块底色（只是字更浅）：整行看着才像一组按钮，
                // 不然「开始」会像一句孤立文字
                let bg = if hot && actionable { t.hover } else { t.field };
                c.fill_round(bx, by, bw, bh, 9.0, bg);
                let color = if i == 3 {
                    if actionable { t.danger } else { t.text3 }
                } else if actionable {
                    t.text2
                } else {
                    t.text3
                };
                design::text(c, label, bx, by, bw, bh, 12.0, color,
                             zx_ui::TextAlign::Center, zx_ui::VAlign::Center, 500);
            }
        }
        let idle = idle_frames();
        let (hint, hcolor) = if !running {
            ("已暂停：点「开始」继续采集", t.text3)
        } else if idle >= 6 {
            ("没检测到滚动：把鼠标移到框内，滚一下页面", t.accent)
        } else {
            ("滚动页面，内容会自动接上；点「暂停」可以先停下", t.text3)
        };
        design::text(c, hint, 14.0, 104.0, (c.w as f32) - 28.0, 14.0, 10.5, hcolor,
                     zx_ui::TextAlign::Near, zx_ui::VAlign::Center, 400);
        let mut r = RECT::default();
        unsafe { let _ = GetWindowRect(self.hwnd, &mut r); }
        self.canvas.present(self.hwnd, r.left, r.top);
    }

    fn hit(&self, x: i32, y: i32) -> i32 {
        for i in 0..4 {
            let (l, t, w, h) = button_rect(i);
            if x >= l as i32 && x < (l + w) as i32 && y >= t as i32 && y < (t + h) as i32 {
                return i as i32 + 1;
            }
        }
        0
    }
}

/// 控制条尺寸与按钮格（4 个等宽按钮，28 + 4 间距铺满）。
const BAR_W: i32 = 404;
const BAR_H: i32 = 122;

fn button_rect(i: usize) -> (f32, f32, f32, f32) {
    let bw = 88.0;
    let gap = 8.0;
    (14.0 + i as f32 * (bw + gap), 76.0, bw, 26.0)
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            if w.0 == 2 {
                let _ = KillTimer(hwnd, 2);
                let _ = PostMessageW(hwnd, WM_LONG_DONE, WPARAM(1), LPARAM(0));
                return LRESULT(0);
            }
            if RUNNING.load(Ordering::SeqCst) {
                let _ = tick();
            }
            with_bar(|b| b.draw());
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_bar(|b| {
                let mut r = RECT::default();
                let _ = GetWindowRect(b.hwnd, &mut r);
                let hit = b.hit(pt.x - r.left, pt.y - r.top);
                if hit != b.hover {
                    b.hover = hit;
                    b.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let hit = with_bar(|b| {
                let mut r = RECT::default();
                let _ = GetWindowRect(b.hwnd, &mut r);
                b.hit(pt.x - r.left, pt.y - r.top)
            }).unwrap_or(0);
            match hit {
                1 => {
                    RUNNING.store(true, Ordering::SeqCst);
                    with_bar(|b| b.draw());
                }
                2 => {
                    RUNNING.store(false, Ordering::SeqCst);
                    with_bar(|b| b.draw());
                }
                3 => {
                    let _ = PostMessageW(hwnd, WM_LONG_DONE, WPARAM(1), LPARAM(0));
                }
                4 => {
                    let _ = PostMessageW(hwnd, WM_LONG_DONE, WPARAM(0), LPARAM(0));
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = w.0 as u16;
            if key == 0x1B {
                let _ = PostMessageW(hwnd, WM_LONG_DONE, WPARAM(0), LPARAM(0));
            } else if key == 0x0D {
                let _ = PostMessageW(hwnd, WM_LONG_DONE, WPARAM(1), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let _ = SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LONG_DONE => {
            let _ = KillTimer(hwnd, 1);
            let _ = KillTimer(hwnd, 2);
            // 1 = 完成，0 = 取消
            DONE.store(w.0 & 0xFFFF != 0, Ordering::SeqCst);
            let p = BAR.with(|c| c.replace(std::ptr::null_mut()));
            if !p.is_null() {
                let bar = Box::from_raw(p);
                let hwnd = bar.hwnd;
                drop(bar);
                let _ = DestroyWindow(hwnd);
            }
            // 让 run() 的局部循环退出（WM_QUIT 会被这个嵌套循环自己吃掉）
            unsafe { PostQuitMessage(0); }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条"有墨"的行：前 6 段是内容、其余留白。用哈希把行号打散，
    /// 相邻行差别很大——否则"差一行"也落在容差里，测出来的 dy 会偏小。
    fn row(i: usize) -> RowSig {
        let mut s = [250u8; BANDS];
        let mut h = (i as u32).wrapping_mul(2_654_435_761);
        for band in s.iter_mut().take(6) {
            h ^= h >> 13;
            h = h.wrapping_mul(1_274_126_177);
            *band = 20 + ((h >> 24) % 200) as u8;
        }
        s
    }

    fn rows(from: usize, to: usize) -> Vec<RowSig> {
        (from..to).map(row).collect()
    }

    #[test]
    fn finds_scroll_offset() {
        // 旧帧 0..100，新帧是 12..100 加上 12 行新内容
        let prev = rows(0, 100);
        let mut cur = rows(12, 100);
        cur.extend(rows(140, 152));
        assert_eq!(find_shift(&cur, &prev, 100), Some(12));
    }

    #[test]
    fn static_frame_has_no_shift() {
        let prev = rows(0, 60);
        let cur = prev.clone();
        assert_eq!(find_shift(&cur, &prev, 60), None);
    }

    #[test]
    fn tolerates_antialias_noise() {
        let prev = rows(0, 80);
        let mut cur = rows(10, 80);
        cur.extend(rows(150, 160));
        // 亚像素滚动会让少数行的亮度整体偏几级，不该影响判定
        for (i, sig) in cur.iter_mut().enumerate() {
            if i % 9 == 0 {
                for band in sig.iter_mut().take(4) {
                    *band = band.saturating_add(3);
                }
            }
        }
        assert_eq!(find_shift(&cur, &prev, 80), Some(10));
    }

    #[test]
    fn identical_bottom_rows_are_rejected() {
        // 静态画面：内容能"对上"，但底部没有新内容 → 必须拒绝（否则会一直假拼接）
        let prev = rows(0, 60);
        let cur = prev.clone();
        assert!(!bottom_is_new(&cur, &prev, 60, 12));
    }
}
