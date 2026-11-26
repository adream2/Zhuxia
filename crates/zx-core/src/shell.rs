//! 资源管理器右键菜单（HKCU 级联子菜单）的通用实现。
//!
//! imgconv 与 appicon 用的是同一套方案：`ExtendedSubCommandsKey` 指向一棵公共子菜单，
//! 每个扩展名下只挂一个指向它的父键。两份实现曾经各抄一遍（连 `get_sz` 里的缓冲长度
//! 校验都逐字重复），差异只有键名、文案和命令行参数，所以收敛到这里，各模块只留一个
//! `VerbSpec` 描述自己那部分。
//!
//! 两条容易踩的坑，写在这里免得以后又忘：
//! - **父键默认值必须不设**（微软文档要求）。设了外壳会认为这个动词没有 command，
//!   整项被丢掉，表现是"显示已注册、右键里没有"。
//! - `MultiSelectModel = Player`：多选时 Explorer 为每个文件各起一支进程，正好一张一个。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW, RRF_RT_REG_SZ};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use crate::win::wide;

/// 一个右键菜单动词的静态描述。
pub struct VerbSpec {
    /// 父键名，例如 `ZhuxiaImgConv`（挂在 `Software\Classes\.<ext>\shell\` 下）。
    pub key: &'static str,
    /// 右键里显示的动词名，例如「图片格式转换」。
    pub label: &'static str,
    /// 公共子菜单的键名，例如 `ZhuxiaImgConv.Targets`。
    pub targets_key: &'static str,
    /// 命令行参数名，例如 `--img-convert`。
    pub cli_flag: &'static str,
    /// 探测"是否已安装"用的扩展名（各模块取一个有代表性的）。
    pub probe_ext: &'static str,
    /// 挂菜单的扩展名（不带点）。
    pub exts: &'static [&'static str],
    /// 子菜单每一项：(id, 默认值文案, MUIVerb 文案)。id 同时是命令行参数。
    pub items: fn() -> Vec<(&'static str, String, String)>,
}

fn hkcu() -> HKEY {
    HKEY_CURRENT_USER
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "zhuxia.exe".into())
}

/// 读一个 REG_SZ；`name == None` 读默认值。
pub fn get_sz(sub: &str, name: Option<&str>) -> Option<String> {
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

/// 写一个 REG_SZ；`name == None` 写默认值。键不存在时自动创建。
pub fn set_sz(sub: &str, name: Option<&str>, value: &str) -> Result<(), String> {
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

/// 整个子树删掉。键本来就不存在也算成功（幂等）。
pub fn delete_tree(sub: &str) -> Result<(), String> {
    let subw = wide(sub);
    let rc: WIN32_ERROR = unsafe { RegDeleteTreeW(hkcu(), PCWSTR(subw.as_ptr())) };
    if rc == ERROR_SUCCESS || rc.0 == 2 {
        Ok(())
    } else {
        Err(format!("删除 {sub} 失败：{rc:?}"))
    }
}

pub fn key_exists(sub: &str) -> bool {
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

/// 更改后通知外壳刷新（否则要重启资源管理器才看到）。
pub fn notify_shell() {
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// 动词父键的位置：`SystemFileAssociations\<ext>\shell\<key>`。
///
/// **不能**挂在 `Software\Classes\.<ext>\shell\` 下：只要这个扩展名有 ProgID
/// （最常见的情况——`.png` 被 WPS 抢成 `WPS.PIC.png`、`.exe` 是 `exefile`、
/// `.jpg` 是 `jpegfile`），外壳就只读 ProgID 下的动词，扩展名键上那棵树被整棵忽略。
/// 这时注册表看着完全正确、`installed()` 也返回 true，右键里就是不出现。
///
/// `SystemFileAssociations` 是官方给"按扩展名补充动词"的位置，不受 ProgID 与
/// `Explorer\FileExts\<ext>\UserChoice` 影响。
fn verb_parent(spec: &VerbSpec, ext: &str) -> String {
    format!("Software\\Classes\\SystemFileAssociations\\.{ext}\\shell\\{}", spec.key)
}

/// 0.0.1 及更早把动词写在扩展名键上，装/卸时一并清掉（迁移）。
fn legacy_parent(spec: &VerbSpec, ext: &str) -> String {
    format!("Software\\Classes\\.{ext}\\shell\\{}", spec.key)
}

/// 公共子菜单在注册表里的实际根路径。
///
/// `ExtendedSubCommandsKey` 的值是「相对 HKCR 的短名」（`Xxx.Targets`），
/// 但**写注册表时必须补上 `Software\Classes\` 前缀**——少了它就会写到
/// `HKCU\Xxx.Targets`（一个和外壳无关的野键），菜单照旧指向旧位置。
fn targets_root(spec: &VerbSpec) -> String {
    format!("Software\\Classes\\{}", spec.targets_key)
}

/// 装/刷新菜单。返回装了几个扩展名。
pub fn install(spec: &VerbSpec) -> Result<usize, String> {
    let exe = exe_path();
    if exe.is_empty() {
        return Err("取不到 exe 路径".into());
    }

    // 1) 公共子菜单：每个目标一招
    for (i, (id, default, mui)) in (spec.items)().into_iter().enumerate() {
        let sub = format!("{}\\shell\\{:02}_{}", targets_root(spec), i + 1, id);
        set_sz(&sub, None, &default)?;
        set_sz(&sub, Some("MUIVerb"), &mui)?;
        set_sz(&sub, Some("Icon"), &format!("{exe},0"))?;
        let cmd = format!("\"{exe}\" {} {} \"%1\"", spec.cli_flag, id);
        set_sz(&format!("{sub}\\command"), None, &cmd)?;
    }

    // 2) 每个扩展名一个父键，指向公共子菜单
    let mut n = 0;
    for ext in spec.exts {
        let sub = verb_parent(spec, ext);
        // 先整棵删掉：老版本把菜单文案写进了父键默认值，那条记录会让外壳
        // 认为"这个动词没有 command"，整项直接被丢掉。重建才能保证默认值为空。
        delete_tree(&sub)?;
        // 迁移：把挂在扩展名键上的旧动词删掉，否则同一个菜单可能出现两次
        let _ = delete_tree(&legacy_parent(spec, ext));
        set_sz(&sub, Some("MUIVerb"), spec.label)?;
        set_sz(&sub, Some("Icon"), &format!("{exe},0"))?;
        // 空串 + ExtendedSubCommandsKey = 原生级联菜单
        set_sz(&sub, Some("SubCommands"), "")?;
        set_sz(&sub, Some("ExtendedSubCommandsKey"), spec.targets_key)?;
        set_sz(&sub, Some("MultiSelectModel"), "Player")?;
        set_sz(&sub, Some("Position"), "Top")?;
        n += 1;
    }

    notify_shell();
    Ok(n)
}

/// 卸掉菜单。幂等：没装过也返回 Ok。
pub fn remove(spec: &VerbSpec) -> Result<(), String> {
    let mut first_err = None;
    for ext in spec.exts {
        for sub in [verb_parent(spec, ext), legacy_parent(spec, ext)] {
            if let Err(e) = delete_tree(&sub) {
                first_err.get_or_insert(e);
            }
        }
    }
    for sub in [targets_root(spec), spec.targets_key.to_string()] {
        // 第二个是修这个前缀 bug 之前误写到 HKCU 根下的野键，一并清掉
        if let Err(e) = delete_tree(&sub) {
            first_err.get_or_insert(e);
        }
    }
    notify_shell();
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// 菜单当前指向的 exe（读第一项子菜单的 command）。装过但路径变了要重写。
pub fn registered_exe(spec: &VerbSpec) -> Option<String> {
    let first = (spec.items)().first()?.0.to_string();
    let sub = format!("{}\\shell\\01_{}\\command", targets_root(spec), first);
    let text = get_sz(&sub, None)?;
    let rest = text.trim_matches('\u{0}').strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 菜单是否**真的能用**（不是"键存在"）。
///
/// 三条都要满足：父键在、父键默认值为空、指向正确的公共子菜单，
/// 且子菜单里至少有一条写了 command。只看 `key_exists` 会骗人——
/// 老版本把文案写进父键默认值，键在、菜单却不出现。
pub fn installed(spec: &VerbSpec) -> bool {
    let parent = verb_parent(spec, spec.probe_ext);
    if !key_exists(&parent) {
        return false;
    }
    if get_sz(&parent, None).map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return false;
    }
    if get_sz(&parent, Some("ExtendedSubCommandsKey")).as_deref() != Some(spec.targets_key) {
        return false;
    }
    (spec.items)().iter().enumerate().any(|(i, (id, _, _))| {
        get_sz(&format!("{}\\shell\\{:02}_{}\\command", targets_root(spec), i + 1, id), None)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_items() -> Vec<(&'static str, String, String)> {
        Vec::new()
    }

    const SPEC: VerbSpec = VerbSpec {
        key: "ZhuxiaTest",
        label: "测试动词",
        targets_key: "ZhuxiaTest.Targets",
        cli_flag: "--test",
        probe_ext: "png",
        exts: &["png", "exe"],
        items: no_items,
    };

    /// 动词必须挂在 `SystemFileAssociations` 下。
    ///
    /// 这条是回归护栏：早先挂在 `Software\Classes\.<ext>\shell\` 上，只要扩展名有 ProgID
    /// （`.png` → WPS.PIC.png、`.exe` → exefile），外壳就只读 ProgID 的动词，
    /// 整棵扩展名键被忽略——注册表全对、`installed()` 也 true，右键里就是不出现。
    #[test]
    fn verb_lives_under_system_file_associations() {
        assert_eq!(
            verb_parent(&SPEC, "png"),
            r"Software\Classes\SystemFileAssociations\.png\shell\ZhuxiaTest"
        );
        assert_eq!(
            verb_parent(&SPEC, "exe"),
            r"Software\Classes\SystemFileAssociations\.exe\shell\ZhuxiaTest"
        );
        // 旧位置只用于卸载迁移，不能再往回写
        assert_eq!(legacy_parent(&SPEC, "png"), r"Software\Classes\.png\shell\ZhuxiaTest");
    }
}

#[cfg(test)]
mod targets_tests {
    use super::*;

    fn no_items() -> Vec<(&'static str, String, String)> {
        Vec::new()
    }

    const SPEC: VerbSpec = VerbSpec {
        key: "ZhuxiaTest",
        label: "测试",
        targets_key: "ZhuxiaTest.Targets",
        cli_flag: "--test",
        probe_ext: "png",
        exts: &["png"],
        items: no_items,
    };

    /// 子菜单必须写在 `Software\Classes\` 下。
    ///
    /// `ExtendedSubCommandsKey` 的值是相对 HKCR 的短名，写入时少写这个前缀，
    /// 就会落到 `HKCU\Xxx.Targets` 这个与外壳无关的野键上——菜单项看着正常、
    /// 点下去却还是旧路径。
    #[test]
    fn targets_root_is_under_classes() {
        assert_eq!(targets_root(&SPEC), r"Software\Classes\ZhuxiaTest.Targets");
    }
}
