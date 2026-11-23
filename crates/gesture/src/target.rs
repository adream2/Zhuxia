//! 窗口判定：手势起点、窗口控制目标、桌面/Shell 界面识别。
//!
//! 这里的目标不是“拿到窗口句柄”，而是**避免把系统 Shell 当成普通应用**：
//! 对任务栏、任务视图、Alt+Tab 这类窗口做隐藏/关闭，会让 explorer 直接重启。
//!
//! 判定分三层：
//! 1. 窗口类名命中 Shell 名单 → 拦截；
//! 2. 进程属于 Shell 宿主，且不是 explorer 的文件夹窗口 → 拦截；
//! 3. 桌面窗口沿 owner/parent/root-owner 链向上找，命中桌面类 → 拦截。
//!
//! 进程名走 `OpenProcess + QueryFullProcessImageNameW`，带 1 秒缓存；
//! 门控线程按几十毫秒轮询，不能每次都付一次进程查询。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, POINT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, GetParent, GetWindow, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, IsZoomed, WindowFromPoint, GA_ROOT, GA_ROOTOWNER, GW_OWNER,
};

/// 桌面表面的窗口类。
const DESKTOP: &[&str] = &["Progman", "WorkerW", "SHELLDLL_DefView", "SysListView32"];

/// Shell 持有的窗口类；命中即禁止窗口控制，与进程无关。
const SHELL_WINDOW_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "SHELLDLL_DefView",
    "SysListView32",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "MultitaskingViewFrame",
    "TaskListWnd",
    "TaskListThumbnailWnd",
    "TaskSwitcherWnd",
    "XamlExplorerHostIslandWindow",
    "Windows.UI.Core.CoreWindow",
    "Windows.Internal.Shell.TabProxyWindow",
    "Xaml_WindowedPopupClass",
    "Windows.UI.Composition.DesktopWindowContentBridge",
    "ForegroundStaging",
    "ApplicationManager_DesktopShellWindow",
];

/// Shell 宿主进程（explorer 的文件夹窗口除外）。
const SHELL_PROCESSES: &[&str] = &[
    "explorer",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "searchui",
    "searchhost",
    "searchapp",
    "textinputhost",
    "sihost",
    "shellhost",
    "lockapp",
];

/// explorer 中属于文件浏览/对话框的窗口类。
const EXPLORER_WINDOWS: &[&str] = &["CabinetWClass", "ExploreWClass", "#32770"];

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn is_valid(hwnd: HWND) -> bool {
    !hwnd.is_invalid() && unsafe { IsWindow(hwnd).as_bool() }
}

pub fn root_of(hwnd: HWND) -> HWND {
    if hwnd.is_invalid() {
        return hwnd;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if root.is_invalid() { hwnd } else { root }
}

/// 屏幕坐标 → 顶层窗口；控件会先爬到根。
pub fn window_at(x: i32, y: i32) -> HWND {
    let hit = unsafe { WindowFromPoint(POINT { x, y }) };
    if hit.is_invalid() {
        return hit;
    }
    let root = root_of(hit);
    if root.is_invalid() { hit } else { root }
}

pub fn class_name(hwnd: HWND) -> String {
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn now() -> u64 {
    unsafe { GetTickCount64() }
}

/// 窗口进程名（小写、去掉 .exe），1 秒缓存。
pub fn process_name(hwnd: HWND) -> Option<String> {
    if hwnd.is_invalid() {
        return None;
    }
    let key = hwnd.0 as isize;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let map = cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((name, at)) = map.get(&key) {
            if now().saturating_sub(*at) < 1_000 {
                return Some(name.clone());
            }
        }
    }

    let name = query_process_name(hwnd);
    if let Some(name) = &name {
        let mut map = cache.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 256 {
            map.clear();
        }
        map.insert(key, (name.clone(), now()));
    }
    name
}

fn query_process_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len)
    };
    let _ = unsafe { CloseHandle(handle) };
    if ok.is_err() || len == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_ascii_lowercase();
    Some(file.trim_end_matches(".exe").to_string())
}

static CACHE: OnceLock<Mutex<HashMap<isize, (String, u64)>>> = OnceLock::new();

fn cloaked(hwnd: HWND) -> bool {
    let mut value: u32 = 0;
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut value as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && value != 0
}

fn visible(hwnd: HWND) -> bool {
    (unsafe { IsWindowVisible(hwnd).as_bool() }) && !cloaked(hwnd)
}

pub fn is_desktop_surface(hwnd: HWND) -> bool {
    let mut cur = hwnd;
    for _ in 0..16 {
        if cur.is_invalid() {
            return false;
        }
        if DESKTOP.iter().any(|c| c.eq_ignore_ascii_case(&class_name(cur))) {
            return true;
        }
        let owner = unsafe { GetWindow(cur, GW_OWNER) }.unwrap_or_default();
        let parent = unsafe { GetParent(cur) }.unwrap_or_default();
        let root_owner = unsafe { GetAncestor(cur, GA_ROOTOWNER) };
        cur = if !owner.is_invalid() {
            owner
        } else if !parent.is_invalid() {
            parent
        } else if !root_owner.is_invalid() && root_owner != cur {
            root_owner
        } else {
            return false;
        };
    }
    false
}

fn shell_ui(class: &str, process: Option<&str>) -> bool {
    if SHELL_WINDOW_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class)) {
        return true;
    }
    let Some(proc_name) = process else { return false };
    if !SHELL_PROCESSES.iter().any(|p| p.eq_ignore_ascii_case(proc_name)) {
        return false;
    }
    // explorer 的文件夹窗口放行，其余 explorer 窗口（含未来新增的 XAML Shell 类）拦截。
    !(proc_name.eq_ignore_ascii_case("explorer")
        && EXPLORER_WINDOWS.iter().any(|c| c.eq_ignore_ascii_case(class)))
}

/// 窗口能否接收窗口控制/前置激活。
pub fn is_control_target(hwnd: HWND) -> bool {
    if !is_valid(hwnd) {
        return false;
    }
    let root = root_of(hwnd);
    if root.is_invalid() || !visible(root) {
        return false;
    }

    let mut cur = root;
    let mut depth = 0;
    while !cur.is_invalid() && depth < 32 {
        depth += 1;
        if is_desktop_surface(cur) {
            return false;
        }
        let class = class_name(cur);
        let process = process_name(cur);
        if shell_ui(&class, process.as_deref()) {
            return false;
        }
        cur = unsafe { GetWindow(cur, GW_OWNER) }.unwrap_or_default();
    }
    true
}

/// 前台是否真正的全屏应用。最大化窗口即使矩形等于屏幕也不算。
pub fn foreground_fullscreen() -> bool {
    let fg = foreground();
    if fg.is_invalid() {
        return false;
    }
    if unsafe { IsZoomed(fg).as_bool() } {
        return false;
    }
    zx_ui::window::foreground_fullscreen()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_classes_are_blocked_regardless_of_process() {
        assert!(shell_ui("Shell_TrayWnd", Some("explorer")));
        assert!(shell_ui("MultitaskingViewFrame", None));
        assert!(!shell_ui("CabinetWClass", Some("explorer")));
        assert!(!shell_ui("Chrome_WidgetWin_1", Some("chrome")));
    }
}
