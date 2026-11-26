//! 设置面板：图床（上传 / 图片浏览 / 平台设置，命令量大所以自成一模块）。

use serde_json::{json, Value};

// 本模块与外部队同名：显式指向 crate，否则 `use super::*` 会把它导成自己
use ::imgbed;

use super::*;

pub(crate) fn imgbed_view() -> Value {
    let c = zx_core::cfg::cfg().imgbed();
    let (hk_active, hk_wanted, hk_err) = imgbed::hotkey_state();
    let metas = serde_json::to_value(imgbed::metas()).unwrap_or_else(|_| json!([]));
    let resolved = imgbed::resolve_dir(&c, None)
        .map(|d| d.to_string())
        .unwrap_or_default();
    json!({
        "enabled": c.enabled,
        "provider": c.provider,
        "configured": imgbed::configured(),
        "path": imgbed::default_path(),
        "resolvedPath": resolved,
        "upload": {
            "naming": c.naming,
            "prefix": c.prefix,
            "convert": c.convert,
            "quality": c.quality,
            "maxSide": c.max_side,
            "maxMb": c.max_mb,
        },
        "copy": { "format": c.copy_format, "auto": c.auto_copy },
        "browser": { "source": c.browser_source, "sort": c.browser_sort, "pageSize": c.browser_page_size },
        "providers": c.providers,
        "metas": metas,
        "secrets": imgbed::secret_keys(),
        "hotkey": c.hotkey,
        "hotkeyActive": hk_active,
        "hotkeyWanted": hk_wanted,
        "hotkeyError": hk_err,
        "proxy": c.proxy,
        "alive": taskmon::runtime::instance_alive(),
        "history": imgbed::history_count(),
    })
}

#[tauri::command]
pub(crate) fn imgbed_state() -> Value {
    imgbed_view()
}

/// 只改全局段（upload / copy / browser / provider / hotkey / proxy / enabled）。
#[tauri::command]
pub(crate) fn imgbed_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.imgbed, patch, imgbed_view, || {})
}

/// 只改某个平台的字段（providers.<id> 是通用 map）。
#[tauri::command]
pub(crate) fn imgbed_provider_patch(provider: String, patch: Value) -> Value {
    let mut s = begin_patch();
    let mut c = s.imgbed.clone().unwrap_or_default();
    let mut base = c
        .providers
        .get(&provider)
        .map(|v| serde_json::to_value(v).unwrap_or_else(|_| json!({})))
        .unwrap_or_else(|| json!({}));
    if !base.is_object() {
        base = json!({});
    }
    merge(&mut base, &patch);
    match serde_json::from_value::<serde_yaml::Value>(base) {
        Ok(v) => {
            c.providers.insert(provider, v);
            s.imgbed = Some(c);
            zx_core::cfg::set(s);
            taskmon::runtime::broadcast_reload();
        }
        Err(e) => zx_core::log::warn(format!("图床：平台配置写回失败：{e}")),
    }
    imgbed_view()
}

/// 密钥只接受元数据里声明的 secret 字段，避免变成任意键值存储。
pub(crate) fn secret_allowed(key: &str) -> bool {
    if !key.contains('.') {
        return false;
    }
    imgbed::metas().iter().any(|m| {
        m.fields
            .iter()
            .any(|f| f.secret && format!("{}.{}", m.id, f.key) == key)
    })
}

#[tauri::command]
pub(crate) fn imgbed_secret_set(key: String, value: String) -> Value {
    if !secret_allowed(&key) {
        return json!({ "ok": false, "error": format!("不认识的密钥字段：{key}") });
    }
    match imgbed::set_secret(&key, &value) {
        Ok(()) => json!({ "ok": true, "state": imgbed_view() }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) fn imgbed_secret_clear(key: String) -> Value {
    if !secret_allowed(&key) {
        return json!({ "ok": false, "error": format!("不认识的密钥字段：{key}") });
    }
    match imgbed::clear_secret(&key) {
        Ok(()) => json!({ "ok": true, "state": imgbed_view() }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) fn imgbed_test() -> Value {
    match imgbed::test() {
        Ok(lines) => json!({ "ok": true, "lines": lines }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) async fn imgbed_upload_test() -> Value {
    let r = tauri::async_runtime::spawn_blocking(imgbed::upload_test_now)
        .await
        .unwrap_or_else(|e| Err(format!("{e}")));
    match r {
        Ok(url) => json!({ "ok": true, "url": url }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

// ---- 上传页 ----

/// 原生多选对话框。返回完整路径。
#[tauri::command]
pub(crate) fn ib_pick_files() -> Value {
    let paths = pick_files(
        "图片\0*.png;*.jpg;*.jpeg;*.gif;*.webp;*.bmp;*.tif;*.tiff;*.svg;*.avif\0所有文件\0*.*\0\0",
        "选择要上传的图片",
        true,
    );
    json!({ "paths": paths })
}

/// 读文件生成 160px 预览（WebP data URL）。大图不整张塞进 IPC。
#[tauri::command]
pub(crate) fn ib_preview(path: String) -> Value {
    let meta = std::fs::metadata(&path).ok();
    if meta.as_ref().map(|m| m.len() > 64 * 1024 * 1024).unwrap_or(true) {
        return json!({ "ok": false, "error": "文件不存在或超过 64 MB" });
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return json!({ "ok": false, "error": "读文件失败" });
    };
    match zx_media::decode(&bytes) {
        Ok(img) => match zx_media::thumbnail_webp(&img, 160) {
            Ok(webp) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(webp);
                json!({
                    "ok": true,
                    "dataUrl": format!("data:image/webp;base64,{b64}"),
                    "width": img.width,
                    "height": img.height,
                    "bytes": bytes.len(),
                })
            }
            Err(e) => json!({ "ok": false, "error": e.text() }),
        },
        Err(e) => json!({
            "ok": false,
            "error": e.text(),
            "width": zx_media::dimensions(&bytes).map(|d| d.0).unwrap_or(0),
            "height": zx_media::dimensions(&bytes).map(|d| d.1).unwrap_or(0),
            "bytes": bytes.len(),
        }),
    }
}

#[tauri::command]
pub(crate) async fn ib_upload_paths(paths: Vec<String>, dir: String) -> Value {
    let files: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let d = if dir.trim().is_empty() { None } else { Some(dir) };
    let items = tauri::async_runtime::spawn_blocking(move || {
        imgbed::upload_files(&files, d.as_deref(), "file")
    })
    .await
    .unwrap_or_default();
    json!({ "items": items })
}

#[tauri::command]
pub(crate) async fn ib_upload_b64(name: String, data: String, dir: String) -> Value {
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.split(',').next_back().unwrap_or("")) else {
        return json!({ "items": [imgbed::ItemResult::err("clipboard", &name, "图片数据解码失败")] });
    };
    let d = if dir.trim().is_empty() { None } else { Some(dir) };
    let items = tauri::async_runtime::spawn_blocking(move || {
        imgbed::upload_bytes(&name, bytes, d.as_deref(), "clipboard")
    })
    .await
    .unwrap_or_default();
    json!({ "items": items })
}

/// 从剪贴板取图，返回 PNG data URL（预览用）与原始 base64（上传用）。
#[tauri::command]
pub(crate) fn ib_clipboard() -> Value {
    let Some(bytes) = imgbed::clip::read_image_png() else {
        return json!({ "ok": false, "error": "剪贴板里没有图片" });
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let dims = zx_media::dimensions(&bytes).unwrap_or((0, 0));
    json!({
        "ok": true,
        "name": "剪贴板图片.png",
        "data": b64,
        "dataUrl": format!("data:image/png;base64,{b64}"),
        "width": dims.0,
        "height": dims.1,
        "bytes": bytes.len(),
    })
}

#[tauri::command]
pub(crate) fn ib_paths(dir: String) -> Value {
    let supported = imgbed::metas()
        .into_iter()
        .find(|m| m.id == zx_core::cfg::cfg().imgbed().provider)
        .map(|m| m.caps.list_dirs)
        .unwrap_or(false);
    if !supported {
        return json!({ "supported": false, "dirs": [] });
    }
    match imgbed::remote_dirs(dir.trim()) {
        Ok(dirs) => json!({ "supported": true, "dirs": dirs }),
        Err(e) => json!({ "supported": true, "dirs": [], "error": e }),
    }
}

// ---- 图片页 ----

#[tauri::command]
pub(crate) fn ib_history(q: String, provider: String, sort: String, offset: usize, limit: usize) -> Value {
    let (items, total) = imgbed::history_query(&q, &provider, &sort, offset, limit);
    json!({ "items": items, "total": total })
}

/// 远端浏览：一次把当前目录的**子目录**和**图片**都取回来。
///
/// 原来只调 `remote_images`，所以界面上永远只有根目录那一层图片——GitHub 图床
/// 按 `{yyyy}/{MM}` 分目录上传，子目录里的东西根本进不来。目录树要能一层层点进去。
#[tauri::command]
pub(crate) fn ib_remote(dir: String) -> Value {
    let caps = imgbed::metas()
        .into_iter()
        .find(|m| m.id == zx_core::cfg::cfg().imgbed().provider)
        .map(|m| m.caps)
        .unwrap_or_default();
    if !caps.list_images {
        return json!({ "supported": false, "dirs": [], "items": [] });
    }
    let dir = dir.trim();
    // 目录失败不该整页报错：图片照常显示，只把错误挂上去。
    let (dirs, dir_err) = if caps.list_dirs {
        match imgbed::remote_dirs(dir) {
            Ok(mut d) => {
                d.sort_by_key(|x| x.to_lowercase());
                (d, None)
            }
            Err(e) => (Vec::new(), Some(e)),
        }
    } else {
        (Vec::new(), None)
    };
    match imgbed::remote_images(dir) {
        Ok(items) => json!({ "supported": true, "dirs": dirs, "items": items, "dirError": dir_err }),
        Err(e) => json!({
            "supported": true,
            "dirs": dirs,
            "items": [],
            "error": e,
            "dirError": dir_err
        }),
    }
}

#[tauri::command]
pub(crate) fn ib_thumb(id: String) -> Value {
    let Some(rec) = imgbed::history::get(&id) else {
        return json!({ "ok": false });
    };
    match imgbed::thumbnail(&rec.path, &rec.provider, &rec.url) {
        Ok(webp) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(webp);
            json!({ "ok": true, "dataUrl": format!("data:image/webp;base64,{b64}") })
        }
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) fn ib_thumb_url(url: String) -> Value {
    match imgbed::thumbnail("", "", &url) {
        Ok(webp) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(webp);
            json!({ "ok": true, "dataUrl": format!("data:image/webp;base64,{b64}") })
        }
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) fn ib_image(url: String) -> Value {
    match imgbed::image_bytes(&url) {
        Ok(bytes) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            json!({ "ok": true, "dataUrl": format!("data:{};base64,{b64}", imgbed::path::kind_of(&bytes).map(|(_, m)| m).unwrap_or("application/octet-stream")) })
        }
        // 超限/状态码/网络的原因由 image_bytes 给全，别再统一糊成一句话
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) fn ib_delete(id: String) -> Value {
    json!({ "ok": imgbed::history_remove(&id) })
}

#[tauri::command]
pub(crate) fn ib_delete_remote(path: String) -> Value {
    match imgbed::delete_remote(&path) {
        Ok(()) => {
            // 远端删掉后，本地记录里同路径的也一并清掉
            let (items, _) = imgbed::history_query("", "", "newest", 0, 500);
            for r in items.into_iter().filter(|r| r.path == path) {
                imgbed::history_remove(&r.id);
            }
            json!({ "ok": true })
        }
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub(crate) fn ib_clear_history() -> Value {
    imgbed::history_clear();
    json!({ "ok": true })
}

#[tauri::command]
pub(crate) fn ib_copy(text: String) -> Value {
    json!({ "ok": imgbed::clip::set_text(&text) })
}

#[tauri::command]
pub(crate) fn ib_open(url: String) -> Value {
    if url.starts_with("https://") || url.starts_with("http://") {
        zx_core::win::shell_open(&url);
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "只允许打开 http/https 链接" })
    }
}

#[tauri::command]
pub(crate) fn ib_open_dir() -> Value {
    let dir = zx_core::cfg::run_dir().join("imgbed");
    let _ = std::fs::create_dir_all(&dir);
    zx_core::win::shell_open(&dir.to_string_lossy());
    json!({ "ok": true })
}

// ---------------- 截图 ----------------
