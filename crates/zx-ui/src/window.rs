//! 原生窗口生命周期：注册类、建窗、消息循环、DPI、台前检测。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_core::win::wide;

// 自定义消息：采样线程 → UI 线程
pub const WM_SNAPSHOT: u32 = WM_APP + 1;
pub const WM_REPOSITION: u32 = WM_APP + 2;
pub const WM_SET_METRICS: u32 = WM_APP + 3;
pub const WM_APP_UPDATE_READY: u32 = WM_APP + 7;
pub const WM_APP_RELOAD: u32 = WM_APP + 9;
pub const WM_APP_QUIT: u32 = WM_APP + 10;
pub const WM_APP_OPEN_SETTINGS: u32 = WM_APP + 11;

pub fn init_dpi() {
    unsafe { let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2); }
}

/// 注册窗口类。类名已存在时视为成功（窗口重建不会因为重复注册失败）。
pub fn register_class(
    name: &str,
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    background: Option<windows::Win32::Graphics::Gdi::HBRUSH>,
) {
    unsafe {
        let cls = wide(name);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(proc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: background.unwrap_or_default(),
            lpszClassName: PCWSTR(cls.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn create_window(
    class: &str,
    title: &str,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
    w: i32,
    h: i32,
) -> Option<HWND> {
    unsafe {
        let cls = wide(class);
        let t = wide(title);
        let hinst = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            ex_style, PCWSTR(cls.as_ptr()), PCWSTR(t.as_ptr()), style,
            0, 0, w.max(1), h.max(1), None, None, hinst, None,
        ).ok()?;
        Some(hwnd)
    }
}

pub fn run_loop() {
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub fn dpi_of(hwnd: HWND) -> u32 {
    unsafe {
        let d = GetDpiForWindow(hwnd);
        if d == 0 { 96 } else { d }
    }
}

pub fn work_area(hwnd: HWND) -> RECT {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut info).as_bool() { info.rcWork } else { RECT { left: 0, top: 0, right: 1920, bottom: 1080 } }
    }
}


/// 给窗口装应用图标（exe 内嵌的资源 ID 1）。工具窗/分层窗不显示在任务栏，
/// 但设置页、Alt+Tab、以及被截图时都能带上图标。
/// 从 exe 资源（ID 1）加载指定像素尺寸的图标；失败返回 0。
///
/// `LR_SHARED` 让系统缓存句柄——调用方不必（也不能）DestroyIcon。
/// 尺寸必须显式给：`LR_DEFAULTSIZE` 只认 `SM_CXICON`(32)，拿它当 16px 托盘图标会被
/// 二次缩放，糊且显小。
pub fn load_app_icon(cx: i32, cy: i32) -> isize {
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return 0 };
        // MAKEINTRESOURCE(1)：资源 ID 1（见 crates/zhuxia/app.rc）。指针值就是整数本身，
        // 不是真指针，clippy 的 dangling-pointer 提示在这里不适用。
        #[allow(clippy::manual_dangling_ptr)]
        let res = windows::core::PCWSTR(1 as *const u16);
        match LoadImageW(hinst, res, IMAGE_ICON,
                         cx.max(0), cy.max(0), LR_DEFAULTCOLOR | LR_SHARED) {
            Ok(h) => h.0 as isize,
            Err(_) => 0,
        }
    }
}

/// 给窗口设大/小两种图标。
pub fn set_window_icon(hwnd: HWND) {
    unsafe {
        let big = load_app_icon(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
        let small = load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        if big != 0 {
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(big));
        }
        if small != 0 {
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(small));
        }
    }
}

/// 让原生菜单/滚动条跟随深色（uxtheme 的未文档化导出 #135）。
pub fn set_apps_dark(dark: bool) {
    unsafe {
        let lib = LoadLibraryW(windows::core::w!("uxtheme.dll"));
        if lib.is_err() { return; }
        // SetPreferredAppMode(AllowDark = 1 / ForceDark = 2 / ForceLight = 3 / Default = 0)
        let name = windows::core::PCSTR(135 as *const u8);
        if let Some(f) = GetProcAddress(lib.unwrap(), name) {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(if dark { 2 } else { 3 });
        }
    }
}

/// 前台窗口是否全屏（盖住整个显示器、且不是桌面/任务栏自己）。
pub fn foreground_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() { return false; }
        let cls = wide("Shell_TrayWnd");
        let mut buf = [0u16; 128];
        let n = GetClassNameW(fg, &mut buf);
        let name = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if name == "Shell_TrayWnd" || name == "WorkerW" || name == "Progman" { return false; }
        let _ = cls;

        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() { return false; }
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut info).as_bool() { return false; }
        let m = info.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}
