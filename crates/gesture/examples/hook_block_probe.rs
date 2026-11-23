//! 对照实验：低级鼠标钩子"吞掉"移动事件，会不会让指针本身动不了？
//!
//! ```bash
//! cargo run -p gesture --example hook_block_probe -- --swallow
//! cargo run -p gesture --example hook_block_probe -- --pass
//! ```
//!
//! 输出两次 `GetCursorPos` 的差值。`--swallow` 时如果差值为 0，就说明
//! **吞掉 WM_MOUSEMOVE 会让光标停在原地**——这正是"按住右键鼠标动不了"的根因，
//! 也解释了为什么钩子里能收到坐标（MSLLHOOKSTRUCT.pt 是"请求的位置"）却看不到指针移动。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, MSG,
    MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

static SWALLOW: AtomicBool = AtomicBool::new(true);
static HOOK: AtomicIsize = AtomicIsize::new(0);

fn main() {
    let swallow = !std::env::args().any(|a| a == "--pass");
    SWALLOW.store(swallow, Ordering::SeqCst);
    println!("模式：{}", if swallow { "吞掉移动（--swallow）" } else { "放行移动（--pass）" });

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let tid = std::thread::Builder::new()
        .name("hook-block-probe".into())
        .spawn(move || unsafe {
            let tid = GetCurrentThreadId();
            let hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(proc_hook), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("装钩子失败：{e}");
                    let _ = ready_tx.send(0u32);
                    return;
                }
            };
            HOOK.store(hook.0 as isize, Ordering::SeqCst);
            let _ = ready_tx.send(tid);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let h = HOOK.swap(0, Ordering::SeqCst);
            if h != 0 {
                let _ = UnhookWindowsHookEx(HHOOK(h as *mut core::ffi::c_void));
            }
        })
        .expect("spawn");

    let hook_tid = match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(0) | Err(_) => {
            eprintln!("钩子没装上");
            return;
        }
        Ok(tid) => tid,
    };
    std::thread::sleep(Duration::from_millis(200));

    unsafe {
        let mut start = POINT::default();
        let _ = GetCursorPos(&mut start);
        // 从当前位置往右下走 120px：全程按住右键
        let target = (start.x + 120, start.y + 120);
        send_mouse(MOUSEEVENTF_RIGHTDOWN, 0, 0);
        for i in 1..=20 {
            let x = start.x + (target.0 - start.x) * i / 20;
            let y = start.y + (target.1 - start.y) * i / 20;
            let (nx, ny) = normalize(x, y);
            send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, nx, ny);
            std::thread::sleep(Duration::from_millis(12));
        }
        send_mouse(MOUSEEVENTF_RIGHTUP, 0, 0);
        std::thread::sleep(Duration::from_millis(120));

        let mut end = POINT::default();
        let _ = GetCursorPos(&mut end);
        println!(
            "起点 ({},{}) → 终点 ({},{})，位移 Δ=({},{})",
            start.x,
            start.y,
            end.x,
            end.y,
            end.x - start.x,
            end.y - start.y
        );
        println!(
            "结论：{}",
            if (end.x - start.x).abs() >= 50 || (end.y - start.y).abs() >= 50 {
                "指针跟着动了（吞掉移动不影响光标）"
            } else {
                "指针被钉住了（吞掉移动会让光标停住）"
            }
        );

        if hook_tid != 0 {
            let _ = PostThreadMessageW(hook_tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    let _ = tid.join();
}

unsafe extern "system" fn proc_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let msg = wparam.0 as u32;
    let swallow = SWALLOW.load(Ordering::SeqCst);
    // 只在这四类事件上做对照：移动 + 三个按键（其它一律放行）
    let hit = matches!(msg, WM_MOUSEMOVE | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP);
    let _ = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    if hit && swallow {
        return LRESULT(1);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn normalize(x: i32, y: i32) -> (i32, i32) {
    unsafe {
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let nx = ((x - vx) * 65535 / (vw.max(1) - 1)).clamp(0, 65535);
        let ny = ((y - vy) * 65535 / (vh.max(1) - 1)).clamp(0, 65535);
        (nx, ny)
    }
}

fn send_mouse(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, dx: i32, dy: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}
