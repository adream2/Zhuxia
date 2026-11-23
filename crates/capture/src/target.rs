//! 悬停命中：鼠标下面是哪个控件、哪个顶层窗口、哪块显示器。
//!
//! 候选链从细到粗：有 HWND 的控件 → 顶层窗口 → 显示器。
//! 不引入 UI Automation，因此纯自绘控件只能命中到它所属的窗口。

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// 一个候选目标：屏幕矩形 + 悬停标签。
#[derive(Clone, Debug)]
pub struct Target {
    pub rect: RECT,
    pub label: String,
}

/// 从最细到最粗的候选链。
pub fn chain_at(px: i32, py: i32, own_pid: u32) -> Vec<Target> {
    let pt = POINT { x: px, y: py };
    let mut out = Vec::new();

    if let Some(root) = top_window_at(pt, own_pid) {
        let class = class_of(root);
        let title = title_of(root);
        out.push(Target {
            rect: frame_bounds(root),
            label: if title.trim().is_empty() { class } else { shorten(&title, 48) },
        });

        let mut parent = root;
        for _ in 0..8 {
            let child = unsafe { RealChildWindowFromPoint(parent, pt) };
            if child.is_invalid() || child == parent {
                break;
            }
            let r = window_rect(child);
            if r.right <= r.left || r.bottom <= r.top || !hit(&r, px, py) {
                break;
            }
            out.push(Target { rect: r, label: shorten(&class_of(child), 40) });
            parent = child;
        }
        out.reverse();
    }

    out.push(monitor_target(pt));
    out
}

/// 按 Z 序取第一个盖住该点的可见顶层窗口。
fn top_window_at(pt: POINT, own_pid: u32) -> Option<HWND> {
    unsafe {
        let desktop = GetDesktopWindow();
        let mut cur = GetTopWindow(desktop).unwrap_or_default();
        let mut guard = 0;
        while !cur.is_invalid() {
            guard += 1;
            if guard > 400 {
                break;
            }
            if usable(cur, pt, own_pid) {
                return Some(cur);
            }
            cur = match GetWindow(cur, GW_HWNDNEXT) {
                Ok(h) => h,
                Err(_) => break,
            };
        }
        None
    }
}

fn usable(hwnd: HWND, pt: POINT, own_pid: u32) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own_pid {
            return false;
        }

        let class = class_of(hwnd);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }

        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok()
            && cloaked != 0
        {
            return false;
        }

        let r = frame_bounds(hwnd);
        if r.right - r.left < 8 || r.bottom - r.top < 8 {
            return false;
        }
        hit(&r, pt.x, pt.y)
    }
}

/// 可见边框（去掉透明 resize 边）；失败退回 `GetWindowRect`。
fn frame_bounds(hwnd: HWND) -> RECT {
    unsafe {
        let mut r = RECT::default();
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
            && r.right > r.left;
        if ok {
            return r;
        }
    }
    window_rect(hwnd)
}

fn window_rect(hwnd: HWND) -> RECT {
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return RECT::default();
        }
        r
    }
}

fn monitor_target(pt: POINT) -> Target {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            return Target { rect: info.rcMonitor, label: "整块屏幕".to_string() };
        }
    }
    Target {
        rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
        label: "屏幕".into(),
    }
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn title_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 点是否在矩形内（右下开区间）。
pub fn hit(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn shorten(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
