//! 启动时检查更新：读 GitHub Releases 的 `releases/latest`，与编译版本比较。
//!
//! 发布只有一个来源（GitHub），来源地址写死在 `zx_core::cfg::UPDATE_API`：
//! 用户没有"更新源"这一项可填，也就没有填错就永远收不到更新的坑。
//! `updateCheckEnabled: false` 仍然可以整体关掉检查。

use std::sync::Mutex;

static PENDING: Mutex<Option<String>> = Mutex::new(None);

/// 在后台线程查一次；有新版本时把版本号放在 PENDING，由 UI 线程弹提醒。
pub fn check_async(notify_hwnd: isize) {
    let cfg = zx_core::cfg::cfg();
    if !cfg.update_check() { return; }
    std::thread::Builder::new().name("zx-update".into()).spawn(move || {
        let url = format!("{}/releases/latest", zx_core::cfg::UPDATE_API.trim_end_matches('/'));
        let Some(body) = crate::http::get_text(&url, 5000) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else { return };
        let Some(tag) = v.get("tag_name").and_then(|t| t.as_str()) else { return };
        if !is_newer(tag, env!("CARGO_PKG_VERSION")) { return; }
        if zx_core::cfg::cfg().ignored_update_version.as_deref() == Some(tag) { return; }
        if let Ok(mut g) = PENDING.lock() { *g = Some(tag.to_string()); }
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                windows::Win32::Foundation::HWND(notify_hwnd as *mut _),
                zx_ui::window::WM_APP_UPDATE_READY, windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0));
        }
    }).ok();
}

/// UI 线程：取走待提醒的版本号。
pub fn take() -> Option<String> { PENDING.lock().ok().and_then(|mut g| g.take()) }

fn is_newer(tag: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect()
    };
    let a = parse(tag);
    let b = parse(current);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y { return x > y; }
    }
    false
}
