//! Windows 基础工具：时间、DPI、注册表、提权、系统主题、窗口矩形。

use windows::core::{PCWSTR, w};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT, WIN32_ERROR};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{ShellExecuteExW, ShellExecuteW, SHELLEXECUTEINFOW};
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

/// 给一个参数补引号，规则与 `CommandLineToArgvW` 的反解析一致。
///
/// 不做这一步直接拼字符串，`--settings "a b"` 会被拆成两个参数，路径里的引号/反斜杠
/// 也会被吃掉。Windows 的规则是：引号内的 `\` 只有在引号前才需要翻倍。
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::with_capacity(a.len() + 2);
    out.push('"');
    // 本文件不依赖 `std::iter::repeat_n`（较新工具链才有），直接循环追加
    fn push_n(out: &mut String, ch: char, n: usize) {
        for _ in 0..n {
            out.push(ch);
        }
    }
    let mut backslashes = 0usize;
    for ch in a.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // 引号前：反斜杠翻倍，再补一个转义引号
                push_n(&mut out, '\\', backslashes * 2 + 1);
                out.push('"');
                backslashes = 0;
            }
            c => {
                push_n(&mut out, '\\', backslashes);
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // 结尾：反斜杠要翻倍，否则会把收尾的引号转义掉
    push_n(&mut out, '\\', backslashes * 2);
    out.push('"');
    out
}

/// 把参数列表拼成一条命令行（每个参数按需加引号）。
pub fn quote_args(args: &[String]) -> String {
    args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")
}

/// 提权重启后子进程的标记参数。
///
/// 只有它能让单实例获取多等一会儿（父进程还没退干净）。普通重复启动不背这个延迟。
pub const AFTER_ELEVATE_ARG: &str = "--after-elevate";

/// 以管理员身份重启自己（UAC），**保留原命令行参数**。
///
/// 以前用 `ShellExecuteW` 且不传参数，于是 `zhuxia.exe --open-settings` 在未提权时
/// 走 UAC 重启后参数就丢了——用户点了"打开设置"却什么都没发生。
///
/// 返回是否成功发起。父进程不必等子进程：子进程拿单实例互斥时会自己重试
/// （见 `taskmon::runtime::acquire_single_instance`）。
pub fn relaunch_elevated(args: &[String]) -> bool {
    // 打一个"我是提权后的自己"的标记：单实例那一步据此放宽重试窗口
    let mut args: Vec<String> = args.iter().filter(|a| *a != AFTER_ELEVATE_ARG).cloned().collect();
    args.push(AFTER_ELEVATE_ARG.to_string());
    let Ok(exe) = std::env::current_exe() else { return false };
    let file = wide(&exe.to_string_lossy());
    let params = wide(&quote_args(&args));
    let verb = w!("runas");
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            lpVerb: verb,
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        ShellExecuteExW(&mut info).is_ok()
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

#[cfg(test)]
mod tests {
    use super::quote_args;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_args_are_not_quoted() {
        assert_eq!(quote_args(&v(&["--open-settings", "abc"])), "--open-settings abc");
    }

    #[test]
    fn args_with_spaces_get_quoted() {
        // 提权重启时最容易踩到的就是带空格的路径
        assert_eq!(
            quote_args(&v(&["--settings", r"C:\Program Files\a b.txt"])),
            r#"--settings "C:\Program Files\a b.txt""#
        );
    }

    #[test]
    fn empty_arg_is_preserved_as_empty_pair() {
        assert_eq!(quote_args(&v(&["--x", ""])), "--x \"\"");
    }

    #[test]
    fn quotes_and_trailing_backslashes_survive_a_roundtrip() {
        // 不含空格的参数不加引号，结尾反斜杠也就不需要处理
        assert_eq!(quote_args(&v(&[r"C:\dir\"])), r"C:\dir\");
        // 一旦要加引号，结尾反斜杠必须翻倍，否则会把收尾引号转义掉
        assert_eq!(quote_args(&v(&[r"C:\my dir\"])), r#""C:\my dir\\""#);
        // 参数里的引号要转义
        assert_eq!(quote_args(&v(&[r#"a"b"#])), r#""a\"b""#);
    }
}
