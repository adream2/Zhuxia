//! 资源管理器右键菜单：**只写 HKCU**，不装 COM 扩展、不需要管理员、卸载一条命令。
//!
//! 用法是 Windows 原生的「级联菜单」（`ExtendedSubCommandsKey`）：
//!
//! ```text
//! HKCU\Software\Classes\.png\shell\ZhuxiaImgConv
//!     (默认)               = <必须不设>            ← 关键：设了会被外壳判定成"没有命令"
//!     MUIVerb              = 图片格式转换
//!     Icon                 = <exe>,0
//!     SubCommands          = ""                     ← 空串：子菜单由下一行决定
//!     ExtendedSubCommandsKey = ZhuxiaImgConv.Targets
//!     MultiSelectModel     = Player                 ← 多选时每个文件各启动一次
//!
//! HKCU\Software\Classes\ZhuxiaImgConv.Targets\shell\01_png
//!     MUIVerb              = 转换为 PNG
//!     command\(默认)        = "<exe>" --img-convert png "%1"
//! ```
//!
//! **父键的默认值必须是空的（未设置）**——微软文档里专门强调这一点。
//! 以前这里把默认值写成了菜单文案，键在注册表里明明存在、`installed()` 也返回 true，
//! 但资源管理器认为这是一个没有 command 的动词，直接把整项丢掉：
//! 表现就是"显示已注册，右键里什么都没有"。
//!
//! 子菜单只写一份，所有扩展名共用；每个扩展名只写一个父键。这样既不会出现
//! "同一个菜单出现两遍"（`SystemFileAssociations\image` 与 `.webp` 同时命中），
//! 卸载也只要按扩展名删父键 + 删一次公共子菜单。
//!
//! 已知限制：Windows 11 的**新版**右键菜单只认 MSIX 打包的 `IExplorerCommand`，
//! 注册表动词会落在「显示更多选项」里。要进一级菜单得再写一个 COM 扩展，
//! 那是另一个工程（进程内 DLL + 崩溃隔离），不在这一版范围内。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

use crate::fmt::{Target, ALL};

/// 公共子菜单（只写一份）。
const TARGETS_KEY: &str = "Software\\Classes\\ZhuxiaImgConv.Targets";
/// 每个扩展名下的父键名。
const VERB: &str = "ZhuxiaImgConv";
const VERB_LABEL: &str = "图片格式转换";

/// 挂菜单的扩展名。图像类的常见格式全覆盖；HEIC/AVIF 之类系统未必能读，
/// 但挂上没坏处——真点了会得到"不支持读取 .heic"的明确提示，比找不到菜单好。
const EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "ico", "webp", "avif",
    "heic", "heif", "jxl", "jp2", "psd", "tga", "dds", "hdr", "exr", "svg", "svgz",
];

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "zhuxia.exe".into())
}

/// 装/刷新菜单。返回装了几个扩展名。
pub fn install() -> Result<usize, String> {
    let exe = exe_path();
    if exe.is_empty() {
        return Err("取不到 exe 路径".into());
    }

    // 1) 公共子菜单：每个目标格式一招
    for (i, t) in ALL.iter().enumerate() {
        let sub = format!("{TARGETS_KEY}\\shell\\{:02}_{}", i + 1, t.id());
        set_sz(&sub, None, t.label())?;
        set_sz(&sub, Some("MUIVerb"), &format!("转换为 {}", t.label()))?;
        set_sz(&sub, Some("Icon"), &format!("{exe},0"))?;
        let cmd = format!("\"{exe}\" --img-convert {} \"%1\"", t.id());
        set_sz(&format!("{sub}\\command"), None, &cmd)?;
    }

    // 2) 每个扩展名一个父键，指向公共子菜单
    let mut n = 0;
    for ext in EXTS {
        let sub = format!("Software\\Classes\\.{ext}\\shell\\{VERB}");
        // 先整棵删掉：老版本把菜单文案写进了父键默认值，那条记录会让外壳
        // 认为"这个动词没有 command"，整项直接被丢掉。重建才能保证默认值为空。
        delete_tree(&sub)?;
        set_sz(&sub, Some("MUIVerb"), VERB_LABEL)?;
        set_sz(&sub, Some("Icon"), &format!("{exe},0"))?;
        // 空串 + ExtendedSubCommandsKey = 原生级联菜单
        set_sz(&sub, Some("SubCommands"), "")?;
        set_sz(&sub, Some("ExtendedSubCommandsKey"), "ZhuxiaImgConv.Targets")?;
        // Player：多选时 Explorer 为每个文件各跑一次命令，正好一支进程一张图。
        set_sz(&sub, Some("MultiSelectModel"), "Player")?;
        // 排在菜单靠上的位置
        set_sz(&sub, Some("Position"), "Top")?;
        n += 1;
    }

    notify_shell();
    Ok(n)
}

/// 卸掉菜单。幂等：没装过也返回 Ok。
pub fn remove() -> Result<(), String> {
    let mut first_err = None;
    for ext in EXTS {
        let sub = format!("Software\\Classes\\.{ext}\\shell\\{VERB}");
        if let Err(e) = delete_tree(&sub) {
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

/// 菜单当前指向的 exe（读公共子菜单里 `01_png` 的 command）。装过但路径变了要重写。
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
        if rc != ERROR_SUCCESS || !(4..=32 * 1024).contains(&cb) {
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
        // 形如 `"C:\path\zhuxia.exe" --img-convert png "%1"`
        let text = text.trim_matches(|c: char| c == '\0');
        let rest = text.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }
}

/// 菜单是否**真的能用**（不是"键存在"）。
///
/// 三条都要满足：父键在、父键默认值为空、指向正确的公共子菜单，
/// 且子菜单里至少有一条写了 command。只看 `key_exists` 会骗人——
/// 老版本把文案写进父键默认值，键在、菜单却不出现。
pub fn installed() -> bool {
    let parent = format!("Software\\Classes\\.png\\shell\\{VERB}");
    if !key_exists(&parent) {
        return false;
    }
    if get_sz(&parent, None).map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return false;
    }
    if get_sz(&parent, Some("ExtendedSubCommandsKey")).as_deref() != Some("ZhuxiaImgConv.Targets") {
        return false;
    }
    ALL.iter().enumerate().any(|(i, t)| {
        get_sz(&format!("{TARGETS_KEY}\\shell\\{:02}_{}\\command", i + 1, t.id()), None)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    })
}

/// 更改后通知外壳刷新（否则要重启资源管理器才看到）。
fn notify_shell() {
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/* ---------------- 注册表小工具（zx-core 只提供了"读"，写放这里） ---------------- */

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hkcu() -> HKEY {
    HKEY_CURRENT_USER
}

/// 读一个 REG_SZ；`name == None` 读默认值。空串与"没设置"都会返回 `Some("")` / `None`，
/// 调用方要区分时看 `Option`。
fn get_sz(sub: &str, name: Option<&str>) -> Option<String> {
    let subw = wide(sub);
    let namew = wide(name.unwrap_or(""));
    let name_ptr = if name.is_some() { PCWSTR(namew.as_ptr()) } else { PCWSTR::null() };
    unsafe {
        let mut cb = 0u32;
        let rc = RegGetValueW(
            hkcu(),
            PCWSTR(subw.as_ptr()),
            name_ptr,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut cb),
        );
        if rc != ERROR_SUCCESS || !(2..=32 * 1024).contains(&cb) {
            return None;
        }
        let mut buf = vec![0u16; (cb as usize).div_ceil(2)];
        let rc = RegGetValueW(
            hkcu(),
            PCWSTR(subw.as_ptr()),
            name_ptr,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut cb),
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// 写一个 REG_SZ；`name == None` 写默认值。
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
    let rc: WIN32_ERROR = unsafe { RegDeleteTreeW(hkcu(), PCWSTR(subw.as_ptr())) };
    // 键本来就不存在（2 = ERROR_FILE_NOT_FOUND）也算成功。
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

/// 每个目标格式一行，给设置页/文档用。
pub fn targets() -> Vec<Target> {
    ALL.to_vec()
}
