//! 接管 Win+V。
//!
//! Windows 把 Win+V 注册给了系统剪贴板浮窗，`RegisterHotKey` 拿不到它，所以这里用
//! `WH_KEYBOARD_LL` 低级键盘钩子：看到 `Win+V` 就吞掉这一对按键，把面板叫出来。
//!
//! 两个非显然的点：
//! 1. **不能阻塞钩子回调**：低级钩子有 300ms 超时，超时会静默摘钩。回调里只做状态机
//!    + `PostMessage`，UI 的创建/显示全在 UI 线程。
//! 2. **Win 抬手会弹开始菜单**：系统没看到 V，就认为用户只是点了一下 Win。所以在吞掉
//!    V 的同时注入一次 Ctrl 按下/抬起（带魔数，钩子自己会放行），explorer 看到"中间还有
//!    别的键"，就不会把这次 Win 当成单击。
//!
//! 只在设置里打开「接管 Win+V」时安装钩子；关掉立即卸载。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use crate::model::now_ms;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// 注入事件的标记：钩子看到它就放行，避免自己吞自己。
const MAGIC: usize = 0x5A58_564E; // "ZXVN"

static WANTED: AtomicBool = AtomicBool::new(false);
static TID: AtomicU32 = AtomicU32::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_DOWN_AT: AtomicI64 = AtomicI64::new(0);
/// 安装失败后的下次重试时间（维护线程每秒都会来问一次，别刷日志）。
static RETRY_AT: AtomicI64 = AtomicI64::new(0);
static SWALLOW_V: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    let was = WANTED.swap(on, Ordering::SeqCst);
    if !on {
        if was {
            stop();
        }
        return;
    }
    if was {
        // 已经想开着：如果上次安装失败，隔 60 秒再试一次。
        if HOOK.load(Ordering::SeqCst) == 0
            && TID.load(Ordering::SeqCst) == 0
            && crate::model::now_ms() >= RETRY_AT.load(Ordering::SeqCst)
        {
            RETRY_AT.store(crate::model::now_ms() + 60_000, Ordering::SeqCst);
            start();
        }
        return;
    }
    start();
}

/// 钩子当前是否安装（诊断用）。
pub fn active() -> bool {
    HOOK.load(Ordering::SeqCst) != 0
}

fn start() {
    if TID.load(Ordering::SeqCst) != 0 {
        return;
    }
    std::thread::Builder::new()
        .name("zx-clip-winv".into())
        .spawn(|| unsafe {
            let tid = GetCurrentThreadId();
            TID.store(tid, Ordering::SeqCst);
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), HINSTANCE::default(), 0)
            {
                Ok(h) => h,
                Err(e) => {
                    zx_core::log::warn(format!("接管 Win+V 失败（安装键盘钩子）：{e}"));
                    TID.store(0, Ordering::SeqCst);
                    // 保留 WANTED=true，60 秒后再试，不是永久「以为开着」。
                    RETRY_AT.store(crate::model::now_ms() + 60_000, Ordering::SeqCst);
                    return;
                }
            };
            HOOK.store(hook.0 as isize, Ordering::SeqCst);
            zx_core::log::info("已接管 Win+V（低级键盘钩子）");
            let mut msg = MSG::default();
            // WM_QUIT 由 stop() 投进来
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let h = HOOK.swap(0, Ordering::SeqCst);
            if h != 0 {
                let _ = UnhookWindowsHookEx(HHOOK(h as *mut core::ffi::c_void));
            }
            TID.store(0, Ordering::SeqCst);
            zx_core::log::info("已释放 Win+V");
        })
        .ok();
}

fn stop() {
    // 卸载时把按键状态清掉，免得下次启用时误判「Win 还按着」。
    WIN_DOWN.store(false, Ordering::SeqCst);
    SWALLOW_V.store(false, Ordering::SeqCst);
    let tid = TID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn other_modifier_held() -> bool {
    unsafe {
        GetAsyncKeyState(VK_CONTROL.0 as i32) < 0
            || GetAsyncKeyState(VK_MENU.0 as i32) < 0
            || GetAsyncKeyState(VK_SHIFT.0 as i32) < 0
    }
}

/// 注入一次 Ctrl 按下/抬起，给 explorer 一个"中间还有键"的信号。
fn tap_ctrl() {
    fn ev(up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_CONTROL,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: MAGIC,
                },
            },
        }
    }
    let events = [ev(false), ev(true)];
    unsafe {
        SendInput(&events, std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn hook_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, w, l);
    }
    let kb = &*(l.0 as *const KBDLLHOOKSTRUCT);
    // 自己注入的 Ctrl：直接放行
    if kb.dwExtraInfo == MAGIC {
        return CallNextHookEx(None, code, w, l);
    }
    let msg = w.0 as u32;
    let vk = kb.vkCode as u16;
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

    // 记住 Win 是否按着（左右都算）
    if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
        if down {
            WIN_DOWN.store(true, Ordering::SeqCst);
            WIN_DOWN_AT.store(now_ms(), Ordering::SeqCst);
        } else if up {
            WIN_DOWN.store(false, Ordering::SeqCst);
        }
        return CallNextHookEx(None, code, w, l);
    }

    // 兜底：Win 的抬起事件可能被别的钩子吃掉；超过 10 秒就直接认为它没按着。
    if WIN_DOWN.load(Ordering::SeqCst) && now_ms() - WIN_DOWN_AT.load(Ordering::SeqCst) > 10_000 {
        WIN_DOWN.store(false, Ordering::SeqCst);
    }

    // 只响应第一次按下：按住 Win+V 时系统会连续发 keydown，不能反复开关面板。
    if down
        && vk == b'V' as u16
        && WIN_DOWN.load(Ordering::SeqCst)
        && !SWALLOW_V.load(Ordering::SeqCst)
        && !other_modifier_held()
    {
        SWALLOW_V.store(true, Ordering::SeqCst);
        // 预防 Win 抬手弹开始菜单；同时非阻塞地叫面板
        tap_ctrl();
        crate::panel::toggle_async();
        return LRESULT(1);
    }
    if up && vk == b'V' as u16 && SWALLOW_V.load(Ordering::SeqCst) {
        SWALLOW_V.store(false, Ordering::SeqCst);
        return LRESULT(1);
    }

    CallNextHookEx(None, code, w, l)
}
