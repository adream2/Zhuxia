//! 全局低级鼠标钩子（`WH_MOUSE_LL`）与门控。
//!
//! 低级钩子挂在系统输入链上，回调必须极短：
//!
//! 1. 回调里只更新原子变量、往队列塞事件，不查窗口、不读进程、不匹配规则；
//! 2. 排除程序、全屏、桌面等判断放到 32ms 一次的后台门控线程，钩子只读最后快照；
//! 3. 门控快照超过 250ms 未更新时按“允许”处理——门控线程挂了不能把手势锁死。
//!
//! 系统还有一条隐性规则：回调超过 `LowLevelHooksTimeout` 会被静默摘钩，不报错。
//! 因此每 30 秒“先装新钩子、再卸旧钩子”重装一次，既清超时计数，也不留空窗。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HHOOK, MSLLHOOKSTRUCT, MSG, WH_MOUSE_LL, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::model::MouseButton;
use crate::target;

/// 钩子线程输出的原始事件，坐标是屏幕物理像素。
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Start { button: MouseButton, x: i32, y: i32, tick: u64 },
    Move { x: i32, y: i32, tick: u64 },
    End { button: MouseButton, x: i32, y: i32, tick: u64 },
    Wheel { delta: i32 },
}

static TX: OnceLock<Sender<Event>> = OnceLock::new();
static TID: AtomicU32 = AtomicU32::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);

/// 0 = 空闲，1 = 右键手势，2 = 中键手势。
static ACTIVE: AtomicU8 = AtomicU8::new(0);
/// 两键叠按时，先按下的那个键的抬手要被吞掉。
static SUPPRESS_RELEASE: AtomicU8 = AtomicU8::new(0);
static WHEEL_ALLOW: AtomicBool = AtomicBool::new(false);
static WHEEL_RIGHT: AtomicBool = AtomicBool::new(false);
static WHEEL_MIDDLE: AtomicBool = AtomicBool::new(false);
/// 中键默认不接管，避免影响粘贴/自动滚动。
static MIDDLE_ENABLED: AtomicBool = AtomicBool::new(false);
/// 手势结束后的残留滚轮抑制截止时间。
static SUPPRESS_WHEEL_UNTIL: AtomicU64 = AtomicU64::new(0);
/// 最近一次按下时间，用来识别丢失的抬手。
static LAST_BEGIN: AtomicU64 = AtomicU64::new(0);

const STALE_SESSION_MS: u64 = 800;
const STALE_WHEEL_MS: u64 = 8_000;
const HOOK_HEAL_MS: u64 = 30_000;
const WM_APP_HEAL: u32 = 0x8001;

static GATE_BLOCKED: AtomicBool = AtomicBool::new(false);
static GATE_TICK: AtomicU64 = AtomicU64::new(0);
static GATE_STARTED: AtomicBool = AtomicBool::new(false);

const GATE_REFRESH_MS: u64 = 32;
const GATE_MAX_AGE_MS: u64 = 250;
const WHEEL_SUPPRESS_MS: u64 = 300;
#[cfg(test)]
const WHEEL_NOTCH: i32 = 120;

fn trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("ZHUXIA_GESTURE_TRACE").ok().as_deref() == Some("1"))
}

pub(crate) fn trace_enabled() -> bool {
    trace()
}

pub(crate) fn trace_log(msg: impl AsRef<str>) {
    if trace() {
        zx_core::log::info(format!("[gesture] {}", msg.as_ref()));
    }
}

fn now() -> u64 {
    unsafe { GetTickCount64() }
}

fn looks_lost(gap: u64) -> bool {
    gap >= STALE_SESSION_MS
}

/// 在钩子线程上交叉重装：先装新的，成功后再卸旧的。
unsafe fn reinstall_hook() {
    let old = HOOK.load(Ordering::SeqCst);
    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) } {
        Ok(new) => {
            HOOK.store(new.0 as isize, Ordering::SeqCst);
            if old != 0 {
                let removed =
                    unsafe { UnhookWindowsHookEx(HHOOK(old as *mut core::ffi::c_void)) }.is_ok();
                if !removed {
                    zx_core::log::info("手势钩子曾被系统移除，已重新装回（此前手势会无响应）");
                }
            }
            trace_log("钩子已定期重装");
        }
        Err(e) => {
            zx_core::log::warn(format!("手势钩子重装失败：{e}（沿用旧钩子）"));
        }
    }
}

/// 安装钩子并启动门控线程。
pub fn install(tx: Sender<Event>) -> bool {
    let _ = TX.set(tx);
    start_gate();
    if TID.load(Ordering::SeqCst) != 0 {
        return true;
    }

    let (ready_tx, ready_rx) = channel::<bool>();
    let spawned = std::thread::Builder::new()
        .name("zx-gesture-hook".into())
        .spawn(move || unsafe {
            let tid = GetCurrentThreadId();
            TID.store(tid, Ordering::SeqCst);

            // 低级钩子的回调固定在安装线程上执行，所以必须有消息循环。
            let hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    zx_core::log::warn(format!("手势钩子安装失败：{e}（手势模块停用）"));
                    TID.store(0, Ordering::SeqCst);
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            HOOK.store(hook.0 as isize, Ordering::SeqCst);
            let _ = ready_tx.send(true);
            zx_core::log::info("手势钩子已安装（WH_MOUSE_LL）");

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_APP_HEAL {
                    reinstall_hook();
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let h = HOOK.swap(0, Ordering::SeqCst);
            if h != 0 {
                let _ = UnhookWindowsHookEx(HHOOK(h as *mut core::ffi::c_void));
            }
            TID.store(0, Ordering::SeqCst);
            zx_core::log::info("手势钩子已卸载");
        })
        .is_ok();

    if !spawned {
        return false;
    }
    ready_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap_or(false)
}

pub fn active() -> bool {
    HOOK.load(Ordering::SeqCst) != 0
}

pub fn uninstall() {
    let tid = TID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// 只有存在滚轮规则的那一侧才吞滚轮。
pub fn set_wheel_rules(right: bool, middle: bool) {
    WHEEL_RIGHT.store(right, Ordering::SeqCst);
    WHEEL_MIDDLE.store(middle, Ordering::SeqCst);
}

pub fn set_middle_enabled(on: bool) {
    MIDDLE_ENABLED.store(on, Ordering::SeqCst);
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(CallNextHookEx(None, code, wparam, lparam), { hook_proc_inner(code, wparam, lparam) })
}

unsafe fn hook_proc_inner(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {

    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let started = std::time::Instant::now();
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };

    // 只放行自己注入的事件；不能按 LLMHF_INJECTED 一刀切，驱动/远程桌面的右键也带标记。
    if info.dwExtraInfo == crate::action::INJECT_MAGIC {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let (x, y) = (info.pt.x, info.pt.y);
    let swallow = match msg {
        WM_RBUTTONDOWN => begin(MouseButton::Right, x, y),
        WM_MBUTTONDOWN => begin(MouseButton::Middle, x, y),
        WM_MOUSEMOVE => moved(x, y),
        WM_RBUTTONUP => end(MouseButton::Right, x, y),
        WM_MBUTTONUP => end(MouseButton::Middle, x, y),
        WM_MOUSEWHEEL => wheel(info.mouseData, x, y),
        _ => false,
    };

    // 回调超过 20ms 会拖慢全系统鼠标；1 秒最多记一条。
    let elapsed = started.elapsed().as_millis();
    if elapsed >= 20 {
        static LAST: AtomicU64 = AtomicU64::new(0);
        let at = now();
        if at.saturating_sub(LAST.swap(at, Ordering::Relaxed)) > 1000 {
            zx_core::log::warn(format!(
                "手势钩子回调耗时 {elapsed}ms（消息 0x{msg:04X}）——这会拖慢系统鼠标"
            ));
        }
    }

    if swallow { LRESULT(1) } else { CallNextHookEx(None, code, wparam, lparam) }
}

fn emit(event: Event) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(event);
    }
}

fn gate_open() -> bool {
    let at = GATE_TICK.load(Ordering::SeqCst);
    if at == 0 {
        return true;
    }
    if now().saturating_sub(at) > GATE_MAX_AGE_MS {
        return true;
    }
    !GATE_BLOCKED.load(Ordering::SeqCst)
}

fn begin(button: MouseButton, x: i32, y: i32) -> bool {
    if button == MouseButton::Middle && !MIDDLE_ENABLED.load(Ordering::SeqCst) {
        return false;
    }
    if !gate_open() || !crate::engine::alive() {
        return false;
    }

    let id = match button {
        MouseButton::Right => 1u8,
        MouseButton::Middle => 2u8,
    };

    // 同一键连发：继续吞，但不重开会话，否则轨迹会被反复清空。
    if ACTIVE.load(Ordering::SeqCst) == id {
        let gap = now().saturating_sub(LAST_BEGIN.load(Ordering::SeqCst));
        if !looks_lost(gap) {
            WHEEL_ALLOW.store(true, Ordering::SeqCst);
            return true;
        }
        trace_log(format!("上一次手势没收到抬手（{gap}ms 前按下），按新会话重新开始"));
    }

    let prev = ACTIVE.swap(id, Ordering::SeqCst);
    LAST_BEGIN.store(now(), Ordering::SeqCst);
    if prev != 0 && prev != id {
        // 两键叠按：前一个键的抬手要吞掉，避免目标程序收到凭空单击。
        SUPPRESS_RELEASE.store(prev, Ordering::SeqCst);
    }
    WHEEL_ALLOW.store(true, Ordering::SeqCst);
    trace_log(format!("开始手势 button={button:?} at ({x},{y})"));
    emit(Event::Start { button, x, y, tick: now() });
    true
}

/// 移动只采样，不吞：吞 WM_MOUSEMOVE 在远程桌面/部分驱动上会让指针发僵。
fn moved(x: i32, y: i32) -> bool {
    if ACTIVE.load(Ordering::SeqCst) == 0 {
        return false;
    }
    emit(Event::Move { x, y, tick: now() });
    false
}

fn end(button: MouseButton, x: i32, y: i32) -> bool {
    let id = match button {
        MouseButton::Right => 1u8,
        MouseButton::Middle => 2u8,
    };
    if SUPPRESS_RELEASE.swap(0, Ordering::SeqCst) == id {
        WHEEL_ALLOW.store(false, Ordering::SeqCst);
        return true;
    }
    if ACTIVE.swap(0, Ordering::SeqCst) != id {
        return false;
    }
    LAST_BEGIN.store(0, Ordering::SeqCst);
    WHEEL_ALLOW.store(false, Ordering::SeqCst);
    trace_log(format!("结束手势 button={button:?} at ({x},{y})"));
    emit(Event::End { button, x, y, tick: now() });
    true
}

fn wheel(mouse_data: u32, x: i32, y: i32) -> bool {
    let delta = ((mouse_data >> 16) & 0xFFFF) as i16 as i32;
    let active = ACTIVE.load(Ordering::SeqCst);
    if active == 0 {
        // 边缘滚动优先；没有配置时 `hit` 直接返回 None。
        if let Some(side) = crate::edges::hit(x, y) {
            crate::edges::on_wheel(side, delta);
            return true;
        }
        return now() < SUPPRESS_WHEEL_UNTIL.load(Ordering::SeqCst);
    }
    // 会话卡住（抬手丢了）时放行滚轮，否则轨迹和滚轮一起失灵。
    if now().saturating_sub(LAST_BEGIN.load(Ordering::SeqCst)) > STALE_WHEEL_MS {
        return false;
    }
    let enabled = match active {
        1 => WHEEL_RIGHT.load(Ordering::SeqCst),
        2 => WHEEL_MIDDLE.load(Ordering::SeqCst),
        _ => false,
    };
    if !WHEEL_ALLOW.load(Ordering::SeqCst) || !enabled {
        return false;
    }
    emit(Event::Wheel { delta });
    true
}

/// 手势结束后 300ms 内的滚轮一律吞掉。
pub fn suppress_wheel_for_a_moment() {
    SUPPRESS_WHEEL_UNTIL.store(now() + WHEEL_SUPPRESS_MS, Ordering::SeqCst);
}

/// 诊断：按当前光标位置算一次门控结论。
pub fn gate_report() -> (bool, Option<&'static str>, (i32, i32), Option<String>) {
    unsafe {
        let mut pt = windows::Win32::Foundation::POINT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt).is_err() {
            return (false, Some("读不到光标位置"), (0, 0), None);
        }
        let policy = Policy::load();
        let hwnd = target::window_at(pt.x, pt.y);
        let app = target::process_name(hwnd);
        let verdict = policy.evaluate();
        (verdict.is_none(), verdict, (pt.x, pt.y), app)
    }
}

/// 门控线程：32ms 一次判断当前窗口能否起手势；配置每 256ms 读一次。
fn start_gate() {
    if GATE_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("zx-gesture-gate".into())
        .spawn(|| {
            let mut policy = Policy::load();
            let mut loaded_at = now();
            let mut last_blocked = false;
            let mut healed_at = now();
            loop {
                if now().saturating_sub(loaded_at) >= 256 {
                    policy = Policy::load();
                    loaded_at = now();
                }
                if now().saturating_sub(healed_at) >= HOOK_HEAL_MS {
                    healed_at = now();
                    let tid = TID.load(Ordering::SeqCst);
                    if tid != 0 {
                        unsafe {
                            let _ = PostThreadMessageW(tid, WM_APP_HEAL, WPARAM(0), LPARAM(0));
                        }
                    }
                }

                let verdict = policy.evaluate();
                let blocked = verdict.is_some();
                if blocked != last_blocked {
                    if blocked {
                        zx_core::log::info(format!(
                            "手势在此窗口被跳过：{}（改设置或换窗口即可恢复）",
                            verdict.unwrap_or("未知原因")
                        ));
                    } else {
                        zx_core::log::info("手势已恢复响应");
                    }
                    last_blocked = blocked;
                }
                GATE_BLOCKED.store(blocked, Ordering::SeqCst);
                GATE_TICK.store(now(), Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(GATE_REFRESH_MS));
            }
        })
        .ok();
}

struct Policy {
    enabled: bool,
    paused: bool,
    disable_fullscreen: bool,
    disable_desktop: bool,
    excluded: Vec<String>,
}

impl Policy {
    fn load() -> Policy {
        let g = zx_core::cfg::cfg().gesture();
        Policy {
            enabled: g.enabled,
            paused: g.paused,
            disable_fullscreen: g.disable_in_fullscreen,
            disable_desktop: g.disable_on_desktop,
            excluded: g.excluded_apps,
        }
    }

    fn evaluate(&self) -> Option<&'static str> {
        if !self.enabled {
            return Some("已禁用");
        }
        if self.paused {
            return Some("已暂停");
        }
        unsafe {
            let mut pt = windows::Win32::Foundation::POINT::default();
            if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt).is_err() {
                return None;
            }
            let hwnd = target::window_at(pt.x, pt.y);
            if hwnd.is_invalid() {
                return None;
            }
            if !self.excluded.is_empty() {
                if let Some(app) = target::process_name(hwnd) {
                    if self.excluded.iter().any(|e| app.contains(e.trim_end_matches(".exe"))) {
                        return Some("排除的程序");
                    }
                }
            }
            if self.disable_desktop && target::is_desktop_surface(hwnd) {
                return Some("桌面");
            }
        }
        if self.disable_fullscreen && target::foreground_fullscreen() {
            return Some("全屏应用");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_delta_decodes_as_signed_word() {
        assert_eq!(((120u32 << 16) >> 16) as i16 as i32, 120);
        assert_eq!(((0xFFFFu32 << 16) >> 16) as i16, -1);
    }

    #[test]
    fn stale_session_threshold() {
        assert!(!looks_lost(0));
        assert!(!looks_lost(40));
        assert!(!looks_lost(799));
        assert!(looks_lost(800));
        assert!(looks_lost(5_000));
    }

    #[test]
    fn notch_accumulation_logic() {
        let mut acc = 0;
        let mut notches = 0;
        for d in [120, 120, 120] {
            acc += d;
            notches += acc / WHEEL_NOTCH;
            acc %= WHEEL_NOTCH;
        }
        assert_eq!(notches, 3);
        assert_eq!(acc, 0);
    }
}
