//! 设置面板：图片格式转换（批量操作页 + 资源管理器右键菜单开关）。

use serde_json::{json, Value};

// 本模块与外部队同名：显式指向 crate，否则 `use super::*` 会把它导成自己
use ::imgconv;

use super::*;

/// 图片转换那一段的视图（含右键菜单是否在册）。
pub(crate) fn imgconv_view() -> Value {
    let c = zx_core::cfg::cfg().imgconv();
    json!({
        "enabled": c.enabled,
        "quality": c.quality,
        "webpLossless": c.webp_lossless,
        "overwrite": c.overwrite,
        "outDir": c.out_dir.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "installed": imgconv::menu_installed(),
        "icoSizes": c.ico_sizes,
        "icoOptions": [16, 24, 32, 48, 64, 128, 256],
        "targets": imgconv::fmt::ALL.iter().map(|t| t.label()).collect::<Vec<_>>(),
    })
}

/// 只改 `imgconv` 段；启用状态变化时顺手装/卸右键菜单。
#[tauri::command]
pub(crate) fn imgconv_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.imgconv, patch, imgconv_view, || {
        // 菜单是注册表状态，不跟配置自动同步：这里按最新配置对齐一次。
        let on = zx_core::cfg::cfg().imgconv().enabled;
        if on && !imgconv::menu_installed() {
            let _ = imgconv::install_menu();
        } else if !on && imgconv::menu_installed() {
            let _ = imgconv::remove_menu();
        }
    })
}

/// 手动重写/移除菜单。
#[tauri::command]
pub(crate) fn imgconv_menu(on: bool) -> Value {
    if on {
        let _ = imgconv::install_menu();
    } else {
        let _ = imgconv::remove_menu();
    }
    imgconv_view()
}

/// 操作页：挑图片（可多选）。
#[tauri::command]
pub(crate) fn imgconv_pick() -> Value {
    let paths = pick_files(
        "图片\0*.png;*.jpg;*.jpeg;*.jpe;*.jfif;*.gif;*.bmp;*.dib;*.tif;*.tiff;*.ico;*.webp;*.svg;*.svgz;*.avif;*.heic;*.heif\0所有文件\0*.*\0\0",
        "选择要转换的图片",
        true,
    );
    json!({ "paths": paths })
}

/// 操作页：按目标格式批量转换，返回逐条结果。
///
/// 走的是和右键菜单同一个 `convert_batch`，所以质量/覆盖/输出目录这些设置
/// 两边完全一致——设置页里改什么，这里就按什么转。
#[tauri::command]
pub(crate) async fn imgconv_run(paths: Vec<String>, target: String) -> Value {
    let Some(fmt) = imgconv::Target::parse(&target) else {
        return json!({ "items": [], "error": format!("不认识的目标格式：{target}") });
    };
    let files: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    // JoinError = 批量线程 panic 或被取消。以前用 unwrap_or_default() 吞掉，
    // 前端会显示"成功 0 个 · 失败 0 个"，像是任务没跑，实际是线程崩了。
    let results = match tauri::async_runtime::spawn_blocking(move || imgconv::convert_batch(&files, fmt)).await {
        Ok(r) => r,
        Err(e) => return json!({ "items": [], "error": format!("转换线程异常退出：{e}") }),
    };
    let mut ok = 0usize;
    let items: Vec<Value> = results
        .into_iter()
        .map(|(src, res)| match res {
            Ok(o) => {
                ok += 1;
                json!({
                    "ok": true,
                    "source": src.to_string_lossy(),
                    "output": o.output.to_string_lossy(),
                    "name": o.output.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "srcBytes": o.src_bytes,
                    "outBytes": o.out_bytes,
                    "width": o.w,
                    "height": o.h,
                    "ms": o.ms,
                    "notice": o.notice,
                })
            }
            Err(e) => json!({
                "ok": false,
                "source": src.to_string_lossy(),
                "error": e.reason(),
                "detail": e.detail().unwrap_or(""),
            }),
        })
        .collect();
    json!({ "items": items, "ok": ok })
}

// ---------------- 图标提取 ----------------
