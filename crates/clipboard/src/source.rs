//! 来源程序追踪：谁复制的这一条。
//!
//! 参考实现用 `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` 常驻记录前台窗口，再按 PID
//! 反查进程名与完整路径；这里同一套做法（回调里只存 HWND，解析放调用侧）。

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, SetForegroundWindow, MSG,
    EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

static FOREGROUND: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Debug, Default)]
pub struct SourceApp {
    pub app: String,
    pub path: Option<String>,
}

pub fn start() {
    std::thread::Builder::new()
        .name("zx-clip-source".into())
        .spawn(|| unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(hook_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            if hook.is_invalid() {
                zx_core::log::warn("前台窗口钩子安装失败，来源程序将退化为“未知”");
                return;
            }
            FOREGROUND.store(GetForegroundWindow().0 as isize, Ordering::SeqCst);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
            }
            let _ = UnhookWinEvent(hook);
        })
        .ok();
}

unsafe extern "system" fn hook_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND && !hwnd.is_invalid() {
        FOREGROUND.store(hwnd.0 as isize, Ordering::SeqCst);
    }
}

pub fn foreground_raw() -> HWND {
    let h = FOREGROUND.load(Ordering::SeqCst);
    if h != 0 {
        HWND(h as *mut core::ffi::c_void)
    } else {
        unsafe { GetForegroundWindow() }
    }
}

pub fn set_foreground(hwnd: HWND) {
    if !hwnd.is_invalid() {
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn own_pid() -> u32 {
    std::process::id()
}

/// 解析某个窗口的进程信息（跳过自己）。
pub fn resolve(hwnd: HWND) -> Option<SourceApp> {
    if hwnd.is_invalid() {
        return None;
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == own_pid() {
            return None;
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return None;
        };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let mut path = None;
        if QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok()
        {
            path = Some(String::from_utf16_lossy(&buf[..size as usize]));
        }
        let _ = CloseHandle(handle);
        let path = path?;
        let app = std::path::Path::new(&path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未知".into());
        Some(SourceApp { app, path: Some(path) })
    }
}

/// 当前「应该被记作来源」的程序：前台窗口不是自己时用它，是自己是就取上一次。
pub fn current() -> SourceApp {
    let fg = unsafe { GetForegroundWindow() };
    resolve(fg)
        .or_else(|| {
            let tracked = foreground_raw();
            if tracked != fg {
                resolve(tracked)
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// 这个窗口是不是本进程自己的（面板/对话框）。
pub fn is_own_window(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == own_pid()
    }
}

/// 打开一个程序所在目录 / 在资源管理器中显示。
pub fn reveal(path: &str) {
    // 直接交给 explorer，不经过 cmd，避免路径里的 & / ^ 被 shell 解释。
    let arg = format!("/select,\"{path}\"");
    let _ = std::process::Command::new("explorer.exe").arg(arg).spawn();
}
