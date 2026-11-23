//! 按键回显：全局键盘钩子 + 一个小分层窗口，把刚按下的键画成键帽。
//!
//! 用途偏"演示/录屏"：开会演示、录教程时，观众能看见你按了什么。
//!
//! 与其他钩子的关系：这是**独立的** `WH_KEYBOARD_LL`，和鼠标钩子互不影响；
//! 回调里只做「取 vk + 拼键帽文字 + PostMessage」，不做绘制、不查窗口
//! （低级键盘钩子的回调拖慢 300ms 会被系统静默摘钩）。
//!
//! **组合键合成一颗键帽**：按住 Ctrl 再按 C 显示成 `Ctrl+C`，而不是两颗分开的键帽。
//! 实现上按"当前按住哪些修饰键"记账：修饰键按下只入账不显示，非修饰键按下时把它们拼起来；
//! 单独按一下修饰键（没配任何键）松手时才补一颗 `Ctrl` / `Ctrl+Shift`。
//!
//! 自己注入的键（`LLKHF_INJECTED`）一律不回显：手势/边缘滚动会注入 Ctrl+Tab、
//! Alt+Tab 之类，那些是程序行为，不该出现在"我按了什么"里。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::Canvas;

use zx_core::cfg::KeyEchoSettings;

const CLASS: &str = "ZhuxiaKeyEcho";
const WM_KEY_SEEN: u32 = WM_APP + 210;
const TIMER_CLEAR: usize = 1;

/// 窗口尺寸（逻辑像素）：一条横向的键帽带，宽度固定，内容按位置对齐。
const BOX_W: i32 = 780;
const BOX_H: i32 = 76;
/// 最多同时显示几颗键帽。
const MAX_CHIPS: usize = 10;
/// 离屏幕边的距离（逻辑像素）。
const MARGIN: i32 = 48;

static ENABLED: AtomicBool = AtomicBool::new(false);
static DURATION_MS: AtomicI32 = AtomicI32::new(1200);
static FONT_PX: AtomicI32 = AtomicI32::new(15);
/// 0 上中 · 1 上左 · 2 上右 · 3 下中 · 4 下左 · 5 下右
static POSITION: AtomicU8 = AtomicU8::new(3);

static HWND_ATOM: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
/// 这一轮修饰键"有没有被用掉"（已经拼过组合键就不再单独补一颗）。
static MOD_USED: AtomicBool = AtomicBool::new(true);

/// 已经显示出来的键帽（文字 + 出现时间）。按 `duration_ms` 过期。
fn recent() -> &'static Mutex<Vec<(String, Instant)>> {
    static Q: OnceLock<Mutex<Vec<(String, Instant)>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(Vec::new()))
}

/// 当前按住的修饰键（存 Ctrl / Shift / Alt / Win 这些规范名）。
fn held_mods() -> &'static Mutex<Vec<&'static str>> {
    static M: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Vec::new()))
}

struct EchoWin {
    hwnd: HWND,
    canvas: Option<Canvas>,
    dpi: u32,
}

/// 模块入口：建（隐藏的）窗口 → 装键盘钩子 → 跑消息循环。
pub fn start() {
    apply(&crate::cfg().key_echo);
    std::thread::Builder::new()
        .name("zx-gesture-keyecho".into())
        .spawn(|| unsafe {
            if !ensure_window() {
                return;
            }
            install_hook();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        })
        .ok();
}

/// 配置热更新。关掉时清空队列并立刻藏窗口。
pub fn apply(s: &KeyEchoSettings) {
    ENABLED.store(s.enabled, Ordering::Relaxed);
    DURATION_MS.store(s.duration_ms as i32, Ordering::Relaxed);
    FONT_PX.store(s.font_size, Ordering::Relaxed);
    POSITION.store(position_code(s.position), Ordering::Relaxed);
    if !s.enabled {
        recent().lock().unwrap_or_else(|p| p.into_inner()).clear();
        post(WM_KEY_SEEN);
    }
}

/// 六个位置 → 内部编号。未知值按"下中"处理。
pub fn position_code(pos: &str) -> u8 {
    match pos {
        "top-center" => 0,
        "top-left" => 1,
        "top-right" => 2,
        "bottom-left" => 4,
        "bottom-right" => 5,
        _ => 3,
    }
}

fn hwnd() -> Option<HWND> {
    let h = HWND_ATOM.load(Ordering::SeqCst);
    if h == 0 { None } else { Some(HWND(h as *mut core::ffi::c_void)) }
}

fn post(msg: u32) {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = PostMessageW(h, msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn install_hook() {
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), None, 0) } {
        Ok(h) => {
            HOOK.store(h.0 as isize, Ordering::SeqCst);
            zx_core::log::info("按键回显：键盘钩子已安装");
        }
        Err(e) => zx_core::log::warn(format!("按键回显：键盘钩子安装失败（{e}）")),
    }
}

/// 修饰键的规范名。左右两个 vk 归一到同一个名字。
fn mod_name(vk: u16) -> Option<&'static str> {
    match vk {
        0x11 | 0xA2 | 0xA3 => Some("Ctrl"),
        0x10 | 0xA0 | 0xA1 => Some("Shift"),
        0x12 | 0xA4 | 0xA5 => Some("Alt"),
        0x5B | 0x5C => Some("Win"),
        _ => None,
    }
}

/// 按 Ctrl / Shift / Alt / Win 的固定顺序拼组合键文字。
fn chord_label(mods: &[&'static str], key: &str) -> String {
    const ORDER: [&str; 4] = ["Ctrl", "Shift", "Alt", "Win"];
    let mut parts: Vec<&str> = Vec::with_capacity(5);
    for m in ORDER {
        if mods.contains(&m) {
            parts.push(m);
        }
    }
    if !key.is_empty() {
        parts.push(key);
    }
    parts.join("+")
}

/// 把一颗键帽塞进队列（连续重复的同一颗只刷新它的存活时间，自动重复不刷屏）。
fn push_chip(label: String) {
    if label.is_empty() {
        return;
    }
    let mut q = recent().lock().unwrap_or_else(|p| p.into_inner());
    if q.last().map(|(l, _)| *l == label).unwrap_or(false) {
        if let Some(last) = q.last_mut() {
            last.1 = Instant::now();
        }
        return;
    }
    q.push((label, Instant::now()));
    if q.len() > MAX_CHIPS {
        q.remove(0);
    }
    drop(q);
    post(WM_KEY_SEEN);
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    if !ENABLED.load(Ordering::Relaxed) {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let msg = wparam.0 as u32;
    let info = *(lparam.0 as *const KBDLLHOOKSTRUCT);
    // 自检用：允许回显注入的键（`--example keyecho_probe` 靠它验证整条链路）
    let test_mode = std::env::var("ZHUXIA_KEYECHO_INJECT_TEST").is_ok();
    if info.flags.0 & LLKHF_INJECTED.0 != 0 && !test_mode {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let vk = info.vkCode as u16;
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

    if down {
        if let Some(m) = mod_name(vk) {
            let mut mods = held_mods().lock().unwrap_or_else(|p| p.into_inner());
            if !mods.contains(&m) {
                mods.push(m);
                MOD_USED.store(false, Ordering::SeqCst);
            }
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let name = key_name(vk);
        if !name.is_empty() {
            let mods = held_mods().lock().unwrap_or_else(|p| p.into_inner()).clone();
            if !mods.is_empty() {
                MOD_USED.store(true, Ordering::SeqCst);
            }
            push_chip(chord_label(&mods, &name));
        }
    } else if up {
        if let Some(m) = mod_name(vk) {
            let used = MOD_USED.swap(true, Ordering::SeqCst);
            let mut mods = held_mods().lock().unwrap_or_else(|p| p.into_inner());
            if used {
                mods.retain(|x| *x != m);
            } else {
                // 修饰键按下全程没配别的键：单独补一颗（Ctrl 单按也要看得见）
                let label = chord_label(&mods, "");
                drop(mods);
                if !label.is_empty() {
                    push_chip(label);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_KEY_SEEN => {
            with_win(|win| win.refresh());
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_CLEAR => {
            let _ = KillTimer(hwnd, TIMER_CLEAR);
            with_win(|win| win.refresh());
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            unsafe { let _ = SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default()); }
            LRESULT(1)
        }
        WM_NCHITTEST => LRESULT(-1), // HTTRANSPARENT：点穿，不挡鼠标
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

/// 窗口指针（单例）。`ensure_window` 写入，`with_win` 读取。
static WIN: OnceLock<Mutex<usize>> = OnceLock::new();

fn win_slot() -> &'static Mutex<usize> {
    WIN.get_or_init(|| Mutex::new(0))
}

fn with_win<R>(f: impl FnOnce(&mut EchoWin) -> R) -> Option<R> {
    let g = win_slot().lock().unwrap_or_else(|p| p.into_inner());
    if *g == 0 {
        return None;
    }
    Some(f(unsafe { &mut *(*g as *mut EchoWin) }))
}

fn ensure_window() -> bool {
    if hwnd().is_some() {
        return true;
    }
    zx_ui::window::register_class(CLASS, wnd_proc, None);
    let Some(hwnd) = zx_ui::window::create_window(
        CLASS, "驻匣 · 按键回显", WS_POPUP,
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        BOX_W, BOX_H,
    ) else {
        zx_core::log::warn("按键回显：窗口创建失败");
        return false;
    };
    let dpi = zx_ui::window::dpi_of(hwnd);
    unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
    let ptr = Box::into_raw(Box::new(EchoWin { hwnd, canvas: None, dpi }));
    *win_slot().lock().unwrap_or_else(|p| p.into_inner()) = ptr as usize;
    HWND_ATOM.store(hwnd.0 as isize, Ordering::SeqCst);
    true
}

/// 按配置把窗口摆好。**每次上屏都算一次**——以前只在建窗时摆，改设置就永远不动，
/// 表现就是"位置设置无效，始终在下中"。
fn place(hwnd: HWND, w: i32, h: i32, s: f32) {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let m = (MARGIN as f32 * s) as i32;
    let pos = POSITION.load(Ordering::Relaxed);
    let x = match pos {
        1 | 4 => vx + m,
        2 | 5 => vx + vw - w - m,
        _ => vx + (vw - w) / 2,
    };
    let y = if matches!(pos, 0 | 1 | 2) { vy + m } else { vy + vh - h - m };
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    }
}

impl EchoWin {
    /// 重画 + 决定上屏还是收起（没有还没过期的键帽就直接藏）。
    fn refresh(&mut self) {
        let life = Duration::from_millis(DURATION_MS.load(Ordering::Relaxed).clamp(400, 6000) as u64);
        let chips: Vec<String> = {
            let mut q = recent().lock().unwrap_or_else(|p| p.into_inner());
            q.retain(|(_, at)| at.elapsed() < life);
            q.iter().map(|(l, _)| l.clone()).collect()
        };
        if chips.is_empty() || !ENABLED.load(Ordering::Relaxed) {
            unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); }
            return;
        }
        self.draw(&chips);
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                                 SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = SetTimer(self.hwnd, TIMER_CLEAR,
                             life.as_millis().clamp(400, 6000) as u32, None);
        }
    }

    fn draw(&mut self, chips: &[String]) {
        let s = (self.dpi as f32 / 96.0).max(1.0);
        let font = FONT_PX.load(Ordering::Relaxed) as f32 * s;
        let hwnd = self.hwnd;
        let dpi = self.dpi;
        let (bw, bh) = ((BOX_W as f32 * s) as i32, (BOX_H as f32 * s) as i32);
        // 每次上屏都按当前设置摆一次窗口（位置改了立刻生效）
        place(hwnd, bw, bh, s);
        let c = self.canvas.get_or_insert_with(|| Canvas::new(bw, bh, dpi));
        c.clear(0);

        let mut sized: Vec<(f32, &str)> = Vec::with_capacity(chips.len());
        for label in chips {
            sized.push((c.measure(label, font, true) + 20.0 * s, label.as_str()));
        }
        if sized.is_empty() {
            return;
        }
        let gap = 6.0 * s;
        let total: f32 = sized.iter().map(|(w, _)| *w).sum::<f32>()
            + gap * (sized.len().saturating_sub(1)) as f32;
        let pos = POSITION.load(Ordering::Relaxed);
        let h = font + 16.0 * s;
        let y = (c.h as f32 - h) / 2.0;
        let pad = (MARGIN as f32 * s).min(12.0 * s);
        let mut x = match pos {
            1 | 4 => pad,                                // 左：贴左
            2 | 5 => (c.w as f32 - total - pad).max(pad), // 右：贴右
            _ => ((c.w as f32 - total) / 2.0).max(0.0),   // 中：居中
        };
        let accent = 0xFF007AFF;

        for (w, label) in sized {
            // 纯修饰键、或含修饰键的组合键 → 强调色；普通键 → 深色
            let accented = is_modifier_label(label) || label.contains('+');
            let bg = if accented { accent } else { 0xF21E1E20 };
            let radius = (9.0 * s).min(h / 2.0);
            c.fill_round(x, y + 2.0 * s, w, h, radius, 0x33000000);
            c.fill_round(x, y, w, h, radius, bg);
            c.text(label, x, y, w, h, font, 0xF5FFFFFF,
                   zx_ui::TextAlign::Center, zx_ui::VAlign::Center);
            x += w + gap;
        }

        let mut r = RECT::default();
        unsafe { let _ = GetWindowRect(hwnd, &mut r); }
        c.present(hwnd, r.left, r.top);
    }
}

fn is_modifier_label(label: &str) -> bool {
    matches!(label, "Ctrl" | "Shift" | "Alt" | "Win" | "Caps" | "Num")
}

/// 虚拟键码 → 键帽上的名字。认不出来的返回空串（宁可不显示，也别显示 `0x5B`）。
pub fn key_name(vk: u16) -> String {
    match vk {
        0x08 => "⌫".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x1B => "Esc".to_string(),
        0x20 => "Space".to_string(),
        0x10 | 0xA0 | 0xA1 => "Shift".to_string(),
        0x11 | 0xA2 | 0xA3 => "Ctrl".to_string(),
        0x12 | 0xA4 | 0xA5 => "Alt".to_string(),
        0x5B | 0x5C => "Win".to_string(),
        0x14 => "Caps".to_string(),
        0x90 => "Num".to_string(),
        0x91 => "Scroll".to_string(),
        0x21 => "PgUp".to_string(),
        0x22 => "PgDn".to_string(),
        0x23 => "End".to_string(),
        0x24 => "Home".to_string(),
        0x25 => "←".to_string(),
        0x26 => "↑".to_string(),
        0x27 => "→".to_string(),
        0x28 => "↓".to_string(),
        0x2D => "Ins".to_string(),
        0x2E => "Del".to_string(),
        0xBA => ";".to_string(),
        0xBB => "=".to_string(),
        0xBC => ",".to_string(),
        0xBD => "-".to_string(),
        0xBE => ".".to_string(),
        0xBF => "/".to_string(),
        0xC0 => "`".to_string(),
        0xDB => "[".to_string(),
        0xDC => "\\".to_string(),
        0xDD => "]".to_string(),
        0xDE => "'".to_string(),
        0x30..=0x39 => (vk as u8 as char).to_string(),
        0x41..=0x5A => (vk as u8 as char).to_string(),
        0x60..=0x69 => format!("{}", vk - 0x60),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_cover_the_common_set() {
        assert_eq!(key_name(0x41), "A");
        assert_eq!(key_name(0x31), "1");
        assert_eq!(key_name(0x11), "Ctrl");
        assert_eq!(key_name(0x10), "Shift");
        assert_eq!(key_name(0x25), "←");
        assert_eq!(key_name(0x70), "F1");
        assert_eq!(key_name(0x0D), "Enter");
        assert_eq!(key_name(0xFE), "");
    }

    #[test]
    fn chords_use_fixed_modifier_order() {
        // 按下的先后不重要，显示顺序固定 Ctrl→Shift→Alt→Win
        assert_eq!(chord_label(&["Shift", "Ctrl"], "Z"), "Ctrl+Shift+Z");
        assert_eq!(chord_label(&["Ctrl"], "C"), "Ctrl+C");
        assert_eq!(chord_label(&["Alt", "Ctrl", "Shift"], "Esc"), "Ctrl+Shift+Alt+Esc");
        // 只按修饰键（没配任何键）也要有文字
        assert_eq!(chord_label(&["Ctrl"], ""), "Ctrl");
        assert_eq!(chord_label(&[], "A"), "A");
        assert_eq!(chord_label(&[], ""), "");
    }

    #[test]
    fn all_six_positions_map_to_distinct_codes() {
        let all = [
            "top-center", "top-left", "top-right",
            "bottom-left", "bottom-center", "bottom-right",
        ];
        let mut codes: Vec<u8> = all.iter().map(|p| position_code(p)).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 6, "六个位置必须是六个不同的编号");
        assert_eq!(position_code("garbage"), 3); // 未知值退回下中
    }
}
