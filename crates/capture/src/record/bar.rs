//! 录制控制栏。设计见 `docs/RECORD.md`。
//!
//! 三条硬约束（都不是审美问题）：
//!
//! 1. **不能大**。v1 是 420×154 常驻，占 1440p 屏幕 1.7%；现在常态 180×32、
//!    3 秒无操作缩到 56×24（0.036%）。录制时用户只需要「暂停」和「停止」。
//! 2. **不能入镜**。抓屏拍的是屏幕像素，任何可见窗口都会进画面。所以：
//!    区域录制时贴选区外侧；**全屏录制时只显示 3 秒提示就销毁**，之后靠热键控制。
//! 3. **不能抢焦点**。`WS_EX_NOACTIVATE`——点控制栏不能让被录的程序失活，
//!    否则录「某个程序的操作」时，一点它就看不见效果了。

use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::Canvas;

use super::source::Rect;
use super::{Outcome, UiState};

const CLASS: &str = "ZhuxiaRecordBar";
/// 常态：够放「红点 + 计时 + 参数 + 两个按钮」
const W: i32 = 180;
/// 录麦克风时多一个静音按钮（+24px）。没开麦克风就不占这点地方。
const W_MIC: i32 = 204;
const H: i32 = 32;
/// 胶囊：只留红点与计时
const MINI_W: i32 = 58;
const MINI_H: i32 = 24;
/// 无操作多久收缩成胶囊
const IDLE_MINI_MS: u64 = 3000;
/// 全屏录制时，提示胶囊存在多久（之后销毁，避免入镜）
const FULLSCREEN_HINT_MS: u64 = 3000;

const WM_REC_DONE: u32 = WM_APP + 121;
const WM_REC_CMD: u32 = WM_APP + 122;

const CMD_PAUSE: usize = 1;
const CMD_RESUME: usize = 2;
const CMD_FINISH: usize = 3;
const CMD_DISCARD: usize = 4;
const CMD_MIC: usize = 5;

struct Bar {
    hwnd: HWND,
    canvas: Canvas,
    /// 0 = 无悬停；1..=4 = 按钮
    hover: i32,
    mini: bool,
    /// 最后一次鼠标活动（用于自动收缩）
    last_move: std::time::Instant,
    /// 拖动起点（窗口 + 鼠标）
    drag: Option<(i32, i32, i32, i32)>,
    /// 全屏模式：到点自动销毁窗口
    kill_at: Option<std::time::Instant>,
    /// 这次录制是否含麦克风（有才显示静音按钮）
    mic: bool,
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

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn fmt_bytes(b: u64) -> String {
    if b >= 1024 * 1024 * 1024 {
        format!("{:.1}G", b as f64 / 1073741824.0)
    } else if b >= 1024 * 1024 {
        format!("{:.0}M", b as f64 / 1048576.0)
    } else {
        format!("{}K", b / 1024)
    }
}

/// 选区是否（近似）铺满整个虚拟桌面。
///
/// 全屏时任何窗口都会入镜，所以这里不是"放哪"的问题，是"不能有窗口"。
fn is_fullscreen(sel: Rect, bounds: Rect) -> bool {
    let tol = 8;
    sel.x <= bounds.x + tol
        && sel.y <= bounds.y + tol
        && sel.right() >= bounds.right() - tol
        && sel.bottom() >= bounds.bottom() - tol
}

/// 开控制栏并跑消息循环，直到用户停止/丢弃。
pub fn run(state: Arc<UiState>, sel: Rect, bounds: Rect) -> Outcome {
    crate::register_class_dbl(CLASS, wndproc);

    // 只有真的开了麦克风才加这个按钮——没麦克风时不该多占 24px 屏幕
    let mic = state.audio_label.lock().map(|g| g.contains("麦克风")).unwrap_or(false);
    let full = is_fullscreen(sel, bounds);
    let (w, h) = if full { (MINI_W, MINI_H) } else if mic { (W_MIC, H) } else { (W, H) };

    // 位置：区域录制贴选区外侧；全屏录制居中贴顶
    let (mut x, mut y);
    if full {
        x = bounds.x + (bounds.w - w) / 2;
        y = bounds.y + 12;
    } else if sel.right() + 12 + w <= bounds.right() {
        x = sel.right() + 12;
        y = sel.bottom() - h;
    } else if sel.x - 12 - w >= bounds.x {
        x = sel.x - 12 - w;
        y = sel.bottom() - h;
    } else {
        x = bounds.x + (bounds.w - w) / 2;
        y = bounds.bottom() - h - 8;
    }
    x = x.clamp(bounds.x + 4, (bounds.right() - w - 4).max(bounds.x + 4));
    y = y.clamp(bounds.y + 4, (bounds.bottom() - h - 4).max(bounds.y + 4));

    let Some(hwnd) = zx_ui::window::create_window(
        CLASS,
        "驻匣 · 录屏",
        WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        w,
        h,
    ) else {
        return Outcome::Discarded;
    };
    let mut bar = Box::new(Bar {
        hwnd,
        canvas: Canvas::new(w, h, 96),
        hover: 0,
        mini: full,
        last_move: std::time::Instant::now(),
        drag: None,
        kill_at: if full {
            Some(std::time::Instant::now() + std::time::Duration::from_millis(FULLSCREEN_HINT_MS))
        } else {
            None
        },
        mic,
    });
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    }
    bar.draw(&state);
    let ptr = Box::into_raw(bar);
    BAR.with(|c| c.set(ptr));

    unsafe { let _ = SetTimer(hwnd, 1, 120, None); }

    unsafe {
        let mut stale = MSG::default();
        while PeekMessageW(&mut stale, None, WM_REC_DONE, WM_REC_DONE, PM_REMOVE).as_bool() {}
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == -1 || r.0 == 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }

    let outcome = if state.discard.load(Ordering::SeqCst) {
        Outcome::Discarded
    } else {
        Outcome::Finished
    };
    let raw = BAR.with(|c| c.replace(std::ptr::null_mut()));
    if !raw.is_null() {
        unsafe {
            let b = Box::from_raw(raw);
            let h = b.hwnd;
            drop(b);
            let _ = DestroyWindow(h);
        }
    }
    outcome
}

/// 命中区。返回 0 = 无，1 = 主体（暂停/继续），2 = 暂停图标，
/// 3 = 停止，4 = 麦克风静音。
fn hit_button(mini: bool, mic: bool, x: i32, y: i32) -> i32 {
    if mini {
        // 胶囊只有一个"展开"动作，由上层处理
        return 0;
    }
    let _ = y;
    // 有麦克风时整体右移 24px：麦克风(132..152) 暂停(156..176) 停止(178..198)
    let shift = if mic { 24 } else { 0 };
    if mic && (132..152).contains(&x) {
        return 4;
    }
    if (132 + shift..152 + shift).contains(&x) {
        return 2;
    }
    if (154 + shift..176 + shift).contains(&x) {
        return 3;
    }
    if (4..128).contains(&x) {
        return 1; // 主体：暂停/继续
    }
    0
}

impl Bar {
    fn resize(&mut self, mini: bool, anchor_right: bool) {
        if self.mini == mini {
            return;
        }
        self.mini = mini;
        let (w, h) = if mini {
            (MINI_W, MINI_H)
        } else if self.mic {
            (W_MIC, H)
        } else {
            (W, H)
        };
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut r);
            let x = if anchor_right { r.right - w } else { r.left };
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                r.top,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.canvas = Canvas::new(w, h, 96);
        if let Some(st) = super::state() {
            self.draw(&st);
        }
    }

    fn draw(&mut self, state: &UiState) {
        use crate::design::{self, Tokens};
        let t: Tokens = design::tokens();
        let c = &mut self.canvas;
        c.clear(0x00000000);
        let (w, h) = (c.w as f32, c.h as f32);
        let radius = h / 2.0;
        c.fill_round(0.0, 0.0, w, h, radius, 0xDC141414);
        c.stroke_round(0.5, 0.5, w - 1.0, h - 1.0, radius, 1.0, 0x1AFFFFFF);

        let counting = state.countdown.load(Ordering::Relaxed);
        let paused = state.paused.load(Ordering::Relaxed);
        let frames = state.frames.load(Ordering::Relaxed);
        let elapsed = state.elapsed_ms.load(Ordering::Relaxed);
        let bytes = state.bytes.load(Ordering::Relaxed);

        let low_disk = state.disk_low.load(Ordering::Relaxed);
        let dot = if counting > 0 || low_disk {
            0xFFE8A33D
        } else if paused {
            0xFF8A8A8A
        } else {
            0xFFE24B4A
        };
        let cy = h / 2.0;
        c.fill_ellipse(8.0, cy - 3.5, 7.0, 7.0, dot);

        if self.mini {
            let text = if counting > 0 { format!("{counting}") } else { fmt_ms(elapsed) };
            design::text(
                c,
                &text,
                20.0,
                0.0,
                w - 24.0,
                h,
                10.5,
                if low_disk { 0xFFE8A33D } else { 0xFFD3D1C7 },
                zx_ui::TextAlign::Near, zx_ui::VAlign::Center, 500,
            );
        } else {
            let time = if counting > 0 { format!("{counting}s") } else { fmt_ms(elapsed) };
            design::text(
                c,
                &time,
                20.0,
                0.0,
                48.0,
                h,
                11.5,
                if low_disk { 0xFFE8A33D } else { 0xFFE8E8E6 },
                zx_ui::TextAlign::Near, zx_ui::VAlign::Center, 500,
            );
            // 参数与体积（小字灰）
            let info = format!("{}fps {}", state.fps.load(Ordering::Relaxed), fmt_bytes(bytes));
            design::text(
                c, &info, 68.0, 0.0, 58.0, h, 10.0, 0xFF888780,
                zx_ui::TextAlign::Near, zx_ui::VAlign::Center, 400,
            );
            // 麦克风静音开关（图标：话筒 + 静音斜杠）
            if self.mic {
                let muted = state.mic_muted.load(Ordering::Relaxed);
                let col = if muted {
                    0xFFE24B4A
                } else if self.hover == 4 {
                    0xFFE8E8E6
                } else {
                    0xFFB4B2A9
                };
                let mx = 141.0;
                // 话筒头 + 机身
                c.fill_round(mx, cy - 5.5, 4.0, 7.0, 2.0, col);
                // 支杆
                c.fill_rect(mx + 1.75, cy + 1.5, 0.8, 2.2, col);
                c.fill_rect(mx - 0.8, cy + 3.4, 5.4, 0.8, col);
                if muted {
                    // 斜杠：一眼看出"正在静音"
                    c.fill_polygon_stroked(
                        &[(mx - 1.5, cy + 5.0), (mx + 5.5, cy - 4.5), (mx + 6.3, cy - 3.7), (mx - 0.7, cy + 5.8)],
                        0xFFE24B4A,
                        0xFFE24B4A,
                        0.5,
                    );
                }
            }
            let shift = if self.mic { 24.0 } else { 0.0 };
            // 暂停 / 继续
            let px = 136.0 + shift;
            if paused {
                // 三角形 = 继续
                c.fill_polygon_stroked(
                    &[(px, cy - 5.0), (px + 8.0, cy), (px, cy + 5.0)],
                    0xFFB4B2A9,
                    0xFFB4B2A9,
                    1.0,
                );
            } else {
                c.fill_rect(px, cy - 5.0, 2.5, 10.0, 0xFFB4B2A9);
                c.fill_rect(px + 5.0, cy - 5.0, 2.5, 10.0, 0xFFB4B2A9);
            }
            // 停止（实心方块，hover 变红）
            let stop_color = if self.hover == 3 { 0xFFE24B4A } else { 0xFFF09595 };
            c.fill_round(158.0 + shift, cy - 5.0, 10.0, 10.0, 2.0, stop_color);
        }
        let _ = (frames, t);

        let mut r = RECT::default();
        unsafe { let _ = GetWindowRect(self.hwnd, &mut r); }
        self.canvas.present(self.hwnd, r.left, r.top);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    match msg {
        WM_TIMER => {
            let mut quit = false;
            let mut kill = false;
            with_bar(|b| {
                if let Some(at) = b.kill_at {
                    if std::time::Instant::now() >= at {
                        kill = true;
                    }
                }
                // 3 秒无操作收缩成胶囊（全屏模式已经在胶囊态，不用缩）
                if b.kill_at.is_none()
                    && !b.mini
                    && b.hover == 0
                    && b.drag.is_none()
                    && b.last_move.elapsed().as_millis() as u64 >= IDLE_MINI_MS
                {
                    b.resize(true, true);
                }
            });
            if let Some(st) = super::state() {
                if st.stop.load(Ordering::SeqCst) {
                    quit = true;
                } else {
                    with_bar(|b| b.draw(&st));
                }
            }
            if kill {
                // 全屏提示到期：销毁窗口，之后只能靠热键停
                let _ = KillTimer(hwnd, 1);
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            if quit {
                let _ = PostMessageW(hwnd, WM_REC_DONE, WPARAM(1), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_bar(|b| {
                let mut r = RECT::default();
                let _ = GetWindowRect(b.hwnd, &mut r);
                let (lx, ly) = (pt.x - r.left, pt.y - r.top);
                b.last_move = std::time::Instant::now();
                // 拖动移动窗口（超过 4px 才算拖，避免和点击冲突）
                if let Some((mx, my, wx, wy)) = b.drag {
                    let (dx, dy) = (pt.x - mx, pt.y - my);
                    if dx.abs() + dy.abs() > 4 {
                        let _ = SetWindowPos(
                            b.hwnd,
                            HWND_TOPMOST,
                            wx + dx,
                            wy + dy,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
                if b.mini && b.kill_at.is_none() {
                    // 移入胶囊 → 展开
                    b.resize(false, false);
                }
                let hit = hit_button(b.mini, b.mic, lx, ly);
                if hit != b.hover {
                    b.hover = hit;
                    if let Some(st) = super::state() {
                        b.draw(&st);
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            with_bar(|b| {
                let mut r = RECT::default();
                let _ = GetWindowRect(b.hwnd, &mut r);
                b.drag = Some((pt.x, pt.y, r.left, r.top));
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let act = with_bar(|b| {
                let mut r = RECT::default();
                let _ = GetWindowRect(b.hwnd, &mut r);
                let (lx, ly) = (pt.x - r.left, pt.y - r.top);
                let dragged = b
                    .drag
                    .map(|(mx, my, _, _)| (pt.x - mx).abs() + (pt.y - my).abs() > 4)
                    .unwrap_or(false);
                b.drag = None;
                if dragged {
                    return 0; // 刚才是拖动，不当作点击
                }
                hit_button(b.mini, b.mic, lx, ly)
            })
            .unwrap_or(0);
            let cmd = match act {
                1 | 2 => {
                    let paused = super::state().map(|s| s.paused.load(Ordering::SeqCst)).unwrap_or(false);
                    if paused { CMD_RESUME } else { CMD_PAUSE }
                }
                3 => CMD_FINISH,
                4 => CMD_MIC,
                _ => 0,
            };
            if cmd > 0 {
                let _ = PostMessageW(hwnd, WM_REC_CMD, WPARAM(cmd), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // 右键只留两个动作，避免录制中要读菜单
            let paused = super::state().map(|s| s.paused.load(Ordering::SeqCst)).unwrap_or(false);
            let toggle_id = if paused { CMD_RESUME } else { CMD_PAUSE } as u32;
            let items = vec![
                zx_ui::menu::Item::new(toggle_id, if paused { "继续录制" } else { "暂停录制" }),
                zx_ui::menu::Item::new(CMD_FINISH as u32, "停止并保存"),
                zx_ui::menu::Item::sep(),
                zx_ui::menu::Item::new(CMD_DISCARD as u32, "丢弃这段"),
            ];
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let cmd = zx_ui::menu::popup(hwnd, pt.x, pt.y, &items);
            if cmd > 0 {
                let _ = PostMessageW(hwnd, WM_REC_CMD, WPARAM(cmd as usize), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if w.0 as u16 == 0x1B {
                // Esc = 停止并保存（不丢弃，误触不该丢文件）
                let _ = PostMessageW(hwnd, WM_REC_CMD, WPARAM(CMD_FINISH), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_REC_CMD => {
            let cmd = w.0;
            let Some(st) = super::state() else { return LRESULT(0) };
            match cmd {
                CMD_PAUSE => {
                    st.paused.store(true, Ordering::SeqCst);
                }
                CMD_RESUME => {
                    st.paused.store(false, Ordering::SeqCst);
                }
                CMD_FINISH => {
                    st.stop.store(true, Ordering::SeqCst);
                    let _ = PostMessageW(hwnd, WM_REC_DONE, WPARAM(1), LPARAM(0));
                }
                CMD_DISCARD => {
                    st.discard.store(true, Ordering::SeqCst);
                    st.stop.store(true, Ordering::SeqCst);
                    let _ = PostMessageW(hwnd, WM_REC_DONE, WPARAM(1), LPARAM(0));
                }
                CMD_MIC => {
                    // 只是把混音里麦克风的增益归零，不重开设备——所以立刻生效
                    let now = st.mic_muted.load(Ordering::SeqCst);
                    st.mic_muted.store(!now, Ordering::SeqCst);
                }
                _ => {}
            }
            with_bar(|b| b.draw(&st));
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).unwrap_or_default());
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_REC_DONE => {
            let _ = KillTimer(hwnd, 1);
            unsafe { PostQuitMessage(0); }
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(st) = super::state() {
                st.stop.store(true, Ordering::SeqCst);
            }
            unsafe { PostQuitMessage(0); }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
