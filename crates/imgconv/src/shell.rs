//! 资源管理器右键菜单：HKCU 级联子菜单，每个目标格式一招。
//!
//! 注册表读写与级联菜单的骨架都在 `zx_core::shell`，这里只描述本模块的那一份
//! （键名、扩展名、命令行参数、子菜单文案）。

use crate::fmt::{Target, ALL};

/// 挂菜单的扩展名。图像类的常见格式全覆盖；HEIC/AVIF 之类系统未必能读，
/// 但挂上没坏处——真点了会得到"不支持读取 .heic"的明确提示，比找不到菜单好。
const EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "ico", "webp", "avif",
    "heic", "heif", "jxl", "jp2", "psd", "tga", "dds", "hdr", "exr", "svg", "svgz",
];

fn items() -> Vec<(&'static str, String, String)> {
    ALL.iter()
        .map(|t| (t.id(), t.label().to_string(), format!("转换为 {}", t.label())))
        .collect()
}

const SPEC: zx_core::shell::VerbSpec = zx_core::shell::VerbSpec {
    key: "ZhuxiaImgConv",
    label: "图片格式转换",
    targets_key: "ZhuxiaImgConv.Targets",
    cli_flag: "--img-convert",
    probe_ext: "png",
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

/// 菜单当前指向的 exe（读公共子菜单里第一项的 command）。装过但路径变了要重写。
pub fn registered_exe() -> Option<String> {
    zx_core::shell::registered_exe(&SPEC)
}

/// 菜单是否真的能用（键在、默认值为空、指向正确的子菜单、子项有 command）。
pub fn installed() -> bool {
    zx_core::shell::installed(&SPEC)
}

/// 每个目标格式一行，给设置页/文档用。
pub fn targets() -> Vec<Target> {
    ALL.to_vec()
}
