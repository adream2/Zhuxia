//! 设置面板：图标提取（批量操作页 + 右键菜单开关）。

use serde_json::{json, Value};

// 本模块与外部队同名：显式指向 crate，否则 `use super::*` 会把它导成自己
use ::appicon;

use super::*;

pub(crate) fn appicon_view() -> Value {
    let c = zx_core::cfg::cfg().appicon();
    json!({
        "enabled": c.enabled,
        "mode": c.mode,
        "maxSize": c.max_size,
        "icoSizes": c.ico_sizes,
        "icoOptions": [16, 24, 32, 48, 64, 128, 256],
        "outDir": c.out_dir.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "overwrite": c.overwrite,
        "installed": appicon::menu_installed(),
        "modes": [
            {"id": "png", "label": "PNG 图片"},
            {"id": "ico", "label": "ICO 多尺寸"},
            {"id": "all", "label": "全部尺寸"}
        ],
        "exts": appicon::menu::EXTS.len(),
        "formats": [
            "Windows：exe / dll / ocx / cpl / scr / sys / mui / lnk / msi / msix / appx",
            "Android：apk / aab",
            "Apple：app / dmg / icns / ipa",
            "Linux：deb / rpm / AppImage"
        ]
    })
}

#[tauri::command]
pub(crate) fn appicon_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.appicon, patch, appicon_view, || {
        let on = zx_core::cfg::cfg().appicon().enabled;
        if on && !appicon::menu_installed() {
            let _ = appicon::install_menu();
        } else if !on && appicon::menu_installed() {
            let _ = appicon::remove_menu();
        }
    })
}

#[tauri::command]
pub(crate) fn appicon_menu(on: bool) -> Value {
    if on {
        let _ = appicon::install_menu();
    } else {
        let _ = appicon::remove_menu();
    }
    appicon_view()
}

/// 操作页：挑程序 / 安装包 / 图标文件（可多选）。
#[tauri::command]
pub(crate) fn appicon_pick() -> Value {
    let paths = pick_files(
        "程序与安装包\0*.exe;*.dll;*.ocx;*.cpl;*.scr;*.sys;*.mui;*.lnk;*.msi;*.msix;*.appx;*.apk;*.aab;*.ipa;*.app;*.dmg;*.icns;*.deb;*.rpm;*.AppImage;*.ico\0所有文件\0*.*\0\0",
        "选择要提取图标的文件",
        true,
    );
    json!({ "paths": paths })
}

/// 操作页：按形态批量提取图标，返回逐条结果。
#[tauri::command]
pub(crate) async fn appicon_run(paths: Vec<String>, mode: String) -> Value {
    let Some(m) = appicon::Mode::parse(&mode) else {
        return json!({ "items": [], "error": format!("不认识的输出形态：{mode}") });
    };
    let files: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let results = match tauri::async_runtime::spawn_blocking(move || appicon::extract_batch(&files, m)).await {
        Ok(r) => r,
        Err(e) => return json!({ "items": [], "error": format!("提取线程异常退出：{e}") }),
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
                    "outputs": o.outputs.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
                    "name": o.outputs.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "detail": o.detail,
                    "frames": o.frames,
                    "largest": [o.largest.0, o.largest.1],
                    "srcBytes": o.src_bytes,
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
