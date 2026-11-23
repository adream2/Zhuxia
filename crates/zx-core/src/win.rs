//! Windows 基础工具：时间、DPI、注册表、提权、系统主题、窗口矩形。

use windows::core::{PCWSTR, w};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT, WIN32_ERROR};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

pub fn local_time_string() -> String {
    unsafe {
        let t = GetLocalTime();
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
    }
}

pub fn local_date_string() -> String {
    unsafe {
        let t = GetLocalTime();
        format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
    }
}

/// 读 HKCU/HKLM 下的 DWORD；缺失或失败返回 `default`。
pub fn reg_dword(root: usize, sub: &str, value: &str, default: u32) -> u32 {
    let sub_w = wide(sub);
    let val_w = wide(value);
    let hkey = windows::Win32::System::Registry::HKEY(root as *mut _);
    let mut data: u32 = 0;
    let mut cb = 4u32;
    unsafe {
        let rc: WIN32_ERROR = RegGetValueW(
            hkey,
            PCWSTR(sub_w.as_ptr()),
            PCWSTR(val_w.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut cb),
        );
        if rc.is_ok() { data } else { default }
    }
}

pub fn hkcu_dword(sub: &str, value: &str, default: u32) -> u32 {
    reg_dword(HKEY_CURRENT_USER.0 as usize, sub, value, default)
}

/// 用系统默认程序打开一个路径（文件、文件夹或 URL）。
pub fn shell_open(target: &str) {
    let t = wide(target);
    unsafe {
        ShellExecuteW(HWND::default(), w!("open"), PCWSTR(t.as_ptr()),
                      PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// 读 REG_BINARY（EDID 之类的原始块）；缺失或失败返回 `None`。
pub fn reg_bytes(root: windows::Win32::System::Registry::HKEY, sub: &str, value: &str) -> Option<Vec<u8>> {
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_BINARY};
    let sub_w = wide(sub);
    let val_w = wide(value);
    let mut cb = 0u32;
    unsafe {
        // 先问长度（缓冲区为 NULL），再按长度取
        let rc = RegGetValueW(root, PCWSTR(sub_w.as_ptr()), PCWSTR(val_w.as_ptr()),
                              RRF_RT_REG_BINARY, None, None, Some(&mut cb));
        if !rc.is_ok() || cb == 0 || cb > 64 * 1024 { return None; }
        let mut buf = vec![0u8; cb as usize];
        let rc = RegGetValueW(root, PCWSTR(sub_w.as_ptr()), PCWSTR(val_w.as_ptr()),
                              RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut cb));
        if !rc.is_ok() { return None; }
        buf.truncate(cb as usize);
        Some(buf)
    }
}

/// 系统任务栏是否为浅色（`SystemUsesLightTheme`，缺失按深色）。
pub fn taskbar_light() -> bool {
    hkcu_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "SystemUsesLightTheme",
        0,
    ) != 0
}

/// 应用（窗口）主题是否浅色（`AppsUseLightTheme`，缺失按浅色）。
pub fn apps_light() -> bool {
    hkcu_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
        1,
    ) != 0
}

/// 当前进程是否真的能行使管理员权限。
///
/// 注意：不能用 TokenElevation —— UAC 关闭时令牌是"完整管理员"但 TokenIsElevated=0，
/// 会误判成普通权限（实测会因此跳过 ETW / 把 SRUM 的 flags 选错）。
/// 改成查令牌里 Administrators 组是否启用：UAC 开且未提权时该 SID 是 deny-only，返回 false。
pub fn is_admin() -> bool {
    use windows::Win32::Security::CheckTokenMembership;
    unsafe {
        let mut is_member = windows::Win32::Foundation::BOOL(0);
        let administrators = windows::Win32::Security::SID_IDENTIFIER_AUTHORITY { Value: [0, 0, 0, 0, 0, 5] };
        let mut psid = windows::Win32::Security::PSID::default();
        if windows::Win32::Security::AllocateAndInitializeSid(
            &administrators, 2, 32, 544, 0, 0, 0, 0, 0, 0, &mut psid).is_err() {
            return token_elevated();
        }
        let ok = CheckTokenMembership(None, psid, &mut is_member).is_ok();
        let _ = windows::Win32::Security::FreeSid(psid);
        if ok { is_member.as_bool() } else { token_elevated() }
    }
}

fn token_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() { return false; }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// 以管理员身份重启自己（UAC）。返回是否成功发起。
pub fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let file = wide(&exe.to_string_lossy());
    let verb = w!("runas");
    unsafe {
        let r = ShellExecuteW(HWND::default(), verb, PCWSTR(file.as_ptr()), windows::core::PCWSTR::null(), windows::core::PCWSTR::null(), SW_SHOWNORMAL);
        r.0 as isize > 32
    }
}

pub fn dpi_scale_i(v: f32, dpi: u32) -> i32 { (v * dpi as f32 / 96.0).round() as i32 }

pub fn rect_of(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r).is_ok() && r.right > r.left {
            Some(r)
        } else {
            None
        }
    }
}


/// 首个满足条件的子窗口（按类名，忽略标题——26H2 上部分骨架窗口按类名找不到）。
pub fn find_child_by_class(parent: HWND, class: &str) -> HWND {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let cls = wide(class);
    unsafe {
        let mut h = FindWindowExW(parent, HWND::default(), PCWSTR(cls.as_ptr()), windows::core::PCWSTR::null()).unwrap_or_default();
        if !h.is_invalid() { return h; }
        let mut cur = GetWindow(parent, GW_CHILD).unwrap_or_default();
        while !cur.is_invalid() {
            let mut buf = [0u16; 128];
            let n = GetClassNameW(cur, &mut buf);
            if n > 0 && String::from_utf16_lossy(&buf[..n as usize]).eq_ignore_ascii_case(class) {
                return cur;
            }
            cur = match GetWindow(cur, GW_HWNDNEXT) { Ok(v) => v, Err(_) => break };
        }
        h = HWND::default();
        h
    }
}
