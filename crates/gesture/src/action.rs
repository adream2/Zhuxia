//! 动作执行：快捷键、窗口控制、启动程序、音量、内置动作。
//!
//! 每个动作在独立线程执行。钩子回调有超时限制，`SendInput` / `ShellExecute`
//! 这类调用不能阻塞解析线程，否则用户会感觉“划完手势卡一下”。
//!
//! 三个容易踩的点：
//!
//! - 注入键盘事件必须同时填 `wScan`：只填 VK 时，按物理键识别输入的框架
//!   （Flutter 等）会把所有注入字母当成同一个键；
//! - 注入失败要补发抬键，避免 Ctrl/Alt 留在按下状态；
//! - 快捷键只发给前台：先尝试切前台，150ms 内失败就放弃并记日志。

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC_EX, MOUSEINPUT,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsWindowVisible, IsZoomed, PostMessageW, SetForegroundWindow,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GW_OWNER, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE,
    WS_EX_TOPMOST,
};

use crate::model::{Action, Rule, VolumeOp, WindowOp};
use crate::target;

/// 注入事件的标记。钩子只放行带这个标记的事件。
pub const INJECT_MAGIC: usize = 0x5A58_4753;

/// 把规则派到独立线程执行。
pub fn dispatch(rule: Rule, target_window: isize, activate: bool) {
    let _ = std::thread::Builder::new()
        .name("zx-gesture-act".into())
        .spawn(move || {
            let hwnd = HWND(target_window as *mut core::ffi::c_void);
            if let Err(e) = execute(&rule, hwnd, activate) {
                zx_core::log::warn(format!("手势「{}」执行失败：{e}", rule.name));
            }
        });
}

fn execute(rule: &Rule, hwnd: HWND, activate: bool) -> Result<(), String> {
    let hwnd = if target::is_valid(hwnd) { hwnd } else { HWND::default() };

    if matches!(rule.action, Action::Window(_)) {
        if hwnd.is_invalid() || !target::is_control_target(hwnd) {
            zx_core::log::info(format!("手势「{}」跳过：目标不是可控制的应用窗口", rule.name));
            return Ok(());
        }
    }

    if activate
        && !hwnd.is_invalid()
        && !matches!(rule.action, Action::Internal(_) | Action::Program { .. })
        && target::is_control_target(hwnd)
    {
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
    }

    match &rule.action {
        Action::Hotkey(keys) => {
            if activate && !hwnd.is_invalid() && !ensure_foreground(hwnd) {
                zx_core::log::warn(format!(
                    "手势「{}」跳过：目标窗口未能切到前台，快捷键会打到别的窗口",
                    rule.name
                ));
                return Ok(());
            }
            send_hotkey(keys)
        }
        Action::Window(op) => {
            window_op(hwnd, *op);
            Ok(())
        }
        Action::Program { path, args } => launch(path, args),
        Action::Volume(op) => send_volume(*op),
        Action::Internal(op) => {
            match op.as_str() {
                "toggle-exclude" => {
                    crate::toggle_excluded(hwnd.0 as isize);
                }
                "close-similar" => close_siblings(hwnd),
                "edit-hosts" => open_hosts()?,
                // 其余内置动作由宿主注入的实现执行。
                _ => crate::run_internal(op),
            }
            Ok(())
        }
    }
}

/// 关闭同进程的其它普通顶层窗口。
fn close_siblings(current: HWND) {
    let current = if target::is_valid(current) { current } else { target::foreground() };
    if current.is_invalid() {
        return;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(current, Some(&mut pid)) };
    if pid == 0 {
        return;
    }

    struct Found {
        pid: u32,
        current: HWND,
        hwnds: Vec<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        let found = unsafe { &mut *(param.0 as *mut Found) };
        if hwnd == found.current || !target::is_valid(hwnd) {
            return BOOL(1);
        }
        let mut other = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut other)) };
        if other != found.pid {
            return BOOL(1);
        }
        let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
        let ownerless = unsafe { GetWindow(hwnd, GW_OWNER) }
            .map(|owner| owner.is_invalid())
            .unwrap_or(true);
        let titled = unsafe { GetWindowTextLengthW(hwnd) } > 0;
        if visible && ownerless && titled && target::is_control_target(hwnd) {
            found.hwnds.push(hwnd);
        }
        BOOL(1)
    }

    let mut found = Found { pid, current, hwnds: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut Found as isize));
    }
    for hwnd in &found.hwnds {
        unsafe {
            let _ = PostMessageW(*hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    zx_core::log::info(format!("关闭相似窗口：pid={pid}，本轮 {} 个", found.hwnds.len()));
}

/// 用记事本打开 hosts；普通权限时走 UAC。
fn open_hosts() -> Result<(), String> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let path = format!("{root}\\System32\\drivers\\etc\\hosts");
    if !std::path::Path::new(&path).exists() {
        return Err(format!("找不到 hosts 文件：{path}"));
    }
    if zx_core::win::is_admin() {
        return std::process::Command::new("notepad.exe")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("启动记事本失败：{e}"));
    }
    let verb = zx_core::win::wide("runas");
    let file = zx_core::win::wide("notepad.exe");
    let params = zx_core::win::wide(&format!("\"{path}\""));
    let r = unsafe {
        ShellExecuteW(
            None,
            windows::core::PCWSTR(verb.as_ptr()),
            windows::core::PCWSTR(file.as_ptr()),
            windows::core::PCWSTR(params.as_ptr()),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as isize <= 32 {
        Err(format!("提权打开记事本失败（ShellExecute 返回 {}）", r.0 as isize))
    } else {
        Ok(())
    }
}

fn ensure_foreground(hwnd: HWND) -> bool {
    if foreground_is(hwnd) {
        return true;
    }
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        if foreground_is(hwnd) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    foreground_is(hwnd)
}

fn foreground_is(hwnd: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg == hwnd {
            return true;
        }
        if fg.is_invalid() {
            return false;
        }
        let mut a = 0u32;
        let mut b = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut a));
        GetWindowThreadProcessId(hwnd, Some(&mut b));
        a != 0 && a == b
    }
}

fn window_op(hwnd: HWND, op: WindowOp) {
    unsafe {
        match op {
            WindowOp::ToggleTopmost => {
                let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                let topmost = (style & WS_EX_TOPMOST.0) != 0;
                let after = if topmost { HWND_NOTOPMOST } else { HWND_TOPMOST };
                let _ = SetWindowPos(
                    hwnd,
                    after,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            WindowOp::ToggleMaximize => {
                let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
            }
            WindowOp::Minimize => {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            WindowOp::Close => {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn launch(path: &str, args: &[String]) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("程序路径为空".into());
    }
    let mut cmd = std::process::Command::new(path);
    cmd.args(args);
    if let Some(dir) = std::path::Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            cmd.current_dir(dir);
        }
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("启动 {path} 失败：{e}"))
}

/// 发送组合键：正序按下、逆序抬起。
pub(crate) fn send_hotkey(keys: &[u16]) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut batch = Vec::with_capacity(keys.len() * 2);
    for &vk in keys {
        batch.push(key_event(vk, false));
    }
    for &vk in keys.iter().rev() {
        batch.push(key_event(vk, true));
    }

    for attempt in 1..=2 {
        let sent = unsafe { SendInput(&batch, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == batch.len() {
            return Ok(());
        }
        let ups: Vec<INPUT> = keys.iter().rev().map(|&vk| key_event(vk, true)).collect();
        unsafe {
            SendInput(&ups, std::mem::size_of::<INPUT>() as i32);
        }
        if attempt == 1 {
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    Err(format!("SendInput 只插入了部分按键（{} 个）", keys.len()))
}

fn key_event(vk: u16, up: bool) -> INPUT {
    let mapped = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
    let extended = (mapped & 0xFF00) != 0 || extended_key(vk);
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: (mapped & 0xFF) as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_MAGIC,
            },
        },
    }
}

fn extended_key(vk: u16) -> bool {
    matches!(
        vk,
        0x2D | 0x2E | 0x24 | 0x23 | 0x21 | 0x22 | 0x26 | 0x28 | 0x25 | 0x27
        | 0x5B | 0x5C | 0x5D | 0x90 | 0x2C | 0xA3 | 0xA5
    )
}

/// 音量键注入。步长由系统决定。
pub(crate) fn send_volume(op: VolumeOp) -> Result<(), String> {
    let vk = match op {
        VolumeOp::Increase => 0xAF,
        VolumeOp::Decrease => 0xAE,
        VolumeOp::Mute => 0xAD,
    };
    let batch = [key_event(vk, false), key_event(vk, true)];
    let sent = unsafe { SendInput(&batch, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == batch.len() {
        Ok(())
    } else {
        Err("音量键注入失败".into())
    }
}

/// 位移不足时重放被吞掉的单击。
pub fn replay_click(middle: bool) {
    let (down, up) = if middle {
        (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP)
    } else {
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
    };
    let batch = [mouse_event(down), mouse_event(up)];
    unsafe {
        SendInput(&batch, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse_event(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_MAGIC,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_window_filter() {
        // 可见 + 无 owner + 有标题
        assert!(should_close(true, true, true));
        assert!(!should_close(false, true, true));
        assert!(!should_close(true, false, true));
        assert!(!should_close(true, true, false));
    }

    fn should_close(visible: bool, ownerless: bool, titled: bool) -> bool {
        visible && ownerless && titled
    }
}
