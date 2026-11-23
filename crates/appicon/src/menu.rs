//! 资源管理器右键菜单：HKCU 级联子菜单，三个输出形态各一项。
//!
//! 与 imgconv 同一套方案（`ExtendedSubCommandsKey`）：只写 HKCU、不需要管理员、
//! 卸载 = 删键。`.app` 是目录，Windows 不会按扩展名给它挂文件菜单，只能命令行用。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

use crate::output::Mode;

const TARGETS_KEY: &str = "Software\\Classes\\ZhuxiaAppIcon.Targets";
const VERB: &str = "ZhuxiaAppIcon";
const VERB_LABEL: &str = "提取应用图标";

/// 挂菜单的扩展名：Windows / macOS / Linux / Android / iOS 的主流包与可执行文件。
pub const EXTS: &[&str] = &[
    // Windows
    "exe", "dll", "ocx", "cpl", "scr", "sys", "mui", "lnk", "msi", "msix", "appx",
    // Android / iOS
    "apk", "aab", "ipa",
    // macOS
    "app", "dmg", "icns",
    // Linux
    "appimage", "deb", "rpm",
    // 图标本身
    "ico",
];

pub fn install() -> Result<usize, String> {
    let exe = exe_path();
    if exe.is_empty() {
        return Err("取不到 exe 路径".into());
    }
    for (i, mode) in [Mode::Png, Mode::Ico, Mode::All].iter().enumerate() {
        let sub = format!("{TARGETS_KEY}\\shell\\{:02}_{}", i + 1, mode.id());
        set_sz(&sub, None, mode.label())?;
        set_sz(&sub, Some("MUIVerb"), mode.label())?;
        set_sz(&sub, Some("Icon"), &format!("{exe},0"))?;
        let cmd = format!("\"{exe}\" --appicon {} \"%1\"", mode.id());
        set_sz(&format!("{sub}\\command"), None, &cmd)?;
    }
    let mut n = 0;
    for ext in EXTS {
        let sub = format!("Software\\Classes\\.{ext}\\shell\\{VERB}");
        set_sz(&sub, None, VERB_LABEL)?;
        set_sz(&sub, Some("MUIVerb"), VERB_LABEL)?;
        set_sz(&sub, Some("Icon"), &format!("{exe},0"))?;
        set_sz(&sub, Some("SubCommands"), "")?;
        set_sz(&sub, Some("ExtendedSubCommandsKey"), "ZhuxiaAppIcon.Targets")?;
        set_sz(&sub, Some("MultiSelectModel"), "Player")?;
        set_sz(&sub, Some("Position"), "Top")?;
        n += 1;
    }
    notify_shell();
    Ok(n)
}

pub fn remove() -> Result<(), String> {
    let mut first_err = None;
    for ext in EXTS {
        if let Err(e) = delete_tree(&format!("Software\\Classes\\.{ext}\\shell\\{VERB}")) {
            first_err.get_or_insert(e);
        }
    }
    if let Err(e) = delete_tree(TARGETS_KEY) {
        first_err.get_or_insert(e);
    }
    notify_shell();
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub fn installed() -> bool {
    key_exists(&format!("Software\\Classes\\.exe\\shell\\{VERB}"))
}

pub fn registered_exe() -> Option<String> {
    let sub = wide(&format!("{TARGETS_KEY}\\shell\\01_png\\command"));
    unsafe {
        let mut cb = 0u32;
        let rc = RegGetValueW(
            hkcu(),
            PCWSTR(sub.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut cb),
        );
        if rc != ERROR_SUCCESS || cb < 4 || cb > 32 * 1024 {
            return None;
        }
        let mut buf = vec![0u16; (cb as usize).div_ceil(2)];
        let rc = RegGetValueW(
            hkcu(),
            PCWSTR(sub.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut cb),
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let text = String::from_utf16_lossy(&buf);
        let rest = text.trim_matches('\u{0}').strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }
}

fn notify_shell() {
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "zhuxia.exe".into())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
fn hkcu() -> HKEY {
    HKEY_CURRENT_USER
}

fn set_sz(sub: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let subw = wide(sub);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            hkcu(),
            PCWSTR(subw.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!("创建 {sub} 失败：{rc:?}"));
    }
    let namew = wide(name.unwrap_or(""));
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let name_ptr = if name.is_some() { PCWSTR(namew.as_ptr()) } else { PCWSTR::null() };
    let rc = unsafe {
        RegSetKeyValueW(
            hkey,
            PCWSTR::null(),
            name_ptr,
            REG_SZ.0,
            Some(data.as_ptr() as *const c_void),
            (data.len() * 2) as u32,
        )
    };
    unsafe { let _ = RegCloseKey(hkey); }
    if rc == ERROR_SUCCESS { Ok(()) } else { Err(format!("写 {sub} 失败：{rc:?}")) }
}

fn delete_tree(sub: &str) -> Result<(), String> {
    let subw = wide(sub);
    let rc = unsafe { RegDeleteTreeW(hkcu(), PCWSTR(subw.as_ptr())) };
    if rc == ERROR_SUCCESS || rc.0 == 2 {
        Ok(())
    } else {
        Err(format!("删除 {sub} 失败：{rc:?}"))
    }
}

fn key_exists(sub: &str) -> bool {
    let subw = wide(sub);
    let mut h = HKEY::default();
    let rc = unsafe { RegOpenKeyExW(hkcu(), PCWSTR(subw.as_ptr()), 0, KEY_READ, &mut h) };
    if rc == ERROR_SUCCESS {
        unsafe { let _ = RegCloseKey(h); }
        true
    } else {
        false
    }
}
