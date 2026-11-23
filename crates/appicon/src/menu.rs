//! 资源管理器右键菜单：HKCU 级联子菜单，三个输出形态各一项。
//!
//! 注册表读写与级联菜单的骨架都在 `zx_core::shell`，这里只描述本模块的那一份。
//! `.app` 是目录，Windows 不会按扩展名给它挂文件菜单，只能命令行用。

use crate::output::Mode;

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

fn items() -> Vec<(&'static str, String, String)> {
    [Mode::Png, Mode::Ico, Mode::All]
        .iter()
        .map(|m| (m.id(), m.label().to_string(), m.label().to_string()))
        .collect()
}

const SPEC: zx_core::shell::VerbSpec = zx_core::shell::VerbSpec {
    key: "ZhuxiaAppIcon",
    label: "提取应用图标",
    targets_key: "ZhuxiaAppIcon.Targets",
    cli_flag: "--appicon",
    probe_ext: "exe",
    exts: EXTS,
    items,
};

/// 装/刷新菜单。返回装了几个扩展名。
pub fn install() -> Result<usize, String> {
    zx_core::shell::install(&SPEC)
}

/// 卸掉菜单。幂等：没装过也返回 Ok。
pub fn remove() -> Result<(), String> {
    zx_core::shell::remove(&SPEC)
}

/// 菜单是否真的能用（键在、默认值为空、指向正确的子菜单、子项有 command）。
pub fn installed() -> bool {
    zx_core::shell::installed(&SPEC)
}

/// 菜单当前指向的 exe（读公共子菜单里第一项的 command）。
pub fn registered_exe() -> Option<String> {
    zx_core::shell::registered_exe(&SPEC)
}
