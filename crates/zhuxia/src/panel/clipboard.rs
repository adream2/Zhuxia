//! 设置面板：剪贴板历史统计与清理。

use serde_json::{json, Value};

// 本模块与外部队同名：显式指向 crate，否则 `use super::*` 会把它导成自己
use ::clipboard;

use super::*;

/// 只改剪贴板那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
pub(crate) fn clipboard_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.clipboard, patch, clip_view, || {})
}

/// 历史统计：面板进程只读 `clipboard/history.dat`（DPAPI 密文），不去碰常驻进程的存储实例。
#[tauri::command]
pub(crate) fn clipboard_state() -> Value {
    let dir = zx_core::cfg::clipboard_dir();
    // 历史是 DPAPI 密文，交给 clipboard 模块解密后统计（本进程不持有 Store）。
    let st = clipboard::store::read_disk_stats(&dir);
    json!({
        "total": st.total,
        "pinned": st.pinned,
        "sensitive": st.sensitive,
        "secrets": st.secret,
        "images": st.images,
        "files": st.files,
        "bytes": st.bytes,
        "dir": dir.to_string_lossy(),
        "alive": taskmon::runtime::instance_alive(),
    })
}

/// 清空历史：面板进程清不了常驻进程的内存，写哨兵让常驻自己清。
#[tauri::command]
pub(crate) fn clipboard_clear(keep_pinned: bool) -> Value {
    let dir = zx_core::cfg::clipboard_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("clear.flag"),
        if keep_pinned { "keep_pinned" } else { "all" },
    );
    json!({ "ok": true })
}

#[tauri::command]
pub(crate) fn clipboard_open_dir() {
    let dir = zx_core::cfg::clipboard_dir();
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

/// 设置页要展示的剪贴板配置（全部是生效值，不是 Option）。
pub(crate) fn clip_view() -> Value {
    let c = zx_core::cfg::cfg().clipboard();
    json!({
        "enabled": c.enabled,
        "captureFiles": c.capture_files,
        "captureRichText": c.capture_rich_text,
        "recordImages": c.record_images,
        "dedupe": c.dedupe,
        "persistent": c.persistent,
        "limitEnabled": c.limit_enabled,
        "limit": c.limit,
        "deleteAfterPaste": c.delete_after_paste,
        "moveToTopAfterPaste": c.move_to_top_after_paste,
        "sound": c.sound,
        "pasteSound": c.paste_sound,
        "soundVolume": c.sound_volume,
        "hotkey": c.hotkey,
        "plainPasteHotkey": c.plain_paste_hotkey,
        "sequentialHotkey": c.sequential_hotkey,
        "searchHotkey": c.search_hotkey,
        "sequentialMode": c.sequential_mode,
        "pasteMethod": c.paste_method,
        "privacy": c.privacy,
        "privacyKinds": c.privacy_kinds,
        "cleanupRules": c.cleanup_rules,
        "appPolicies": c.app_policies.iter().map(|p| json!({
            "app": p.app, "action": p.action, "rules": p.rules
        })).collect::<Vec<_>>(),
        "sort": c.sort,
        "quickPasteModifier": c.quick_paste_modifier,
        "pause": c.pause,
        "takeoverWinV": c.takeover_win_v,
        "windowPinned": c.window_pinned,
    })
}

// ---------------- 手势 ----------------
