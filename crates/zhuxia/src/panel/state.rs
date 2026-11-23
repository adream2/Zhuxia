//! 设置面板：整份状态、根 patch、自启/日志/退出这类通用命令。

use serde_json::{json, Value};

use super::*;
use crate::panel::appicon::appicon_view;
use crate::panel::capture::capture_view;
use crate::panel::clipboard::clip_view;
use crate::panel::gesture::{gesture_view, mode_str};
use crate::panel::imgconv::imgconv_view;

#[tauri::command]
pub(crate) fn get_state() -> Value {
    zx_core::cfg::reload(); // 常驻进程可能刚改过，读最新的
    state_json()
}

#[tauri::command]
pub(crate) fn patch(patch: Value) -> Value {
    // 写前对齐磁盘：常驻进程（托盘开关）与面板会同时写 settings.yaml，
    // 只拿进程内快照会把对方刚改的键覆盖掉。
    zx_core::cfg::reload();
    let before = zx_core::cfg::cfg();
    let mut base = serde_json::to_value(&before).unwrap_or_else(|_| json!({}));
    merge(&mut base, &patch);
    if let Ok(next) = serde_json::from_value::<zx_core::Settings>(base) {
        zx_core::cfg::set(next);
        taskmon::runtime::broadcast_reload();
    }
    with_save_error(state_json())
}

#[tauri::command]
pub(crate) fn set_autostart(on: bool) -> Value {
    if on { let _ = taskmon::autostart::enable(); } else { let _ = taskmon::autostart::disable(); }
    taskmon::autostart::refresh();
    state_json()
}

#[tauri::command]
pub(crate) fn open_logs() {
    let dir = zx_core::cfg::run_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

#[tauri::command]
pub(crate) fn open_url(url: String) {
    if url.starts_with("https://") { shell_open(&url); }
}

#[tauri::command]
pub(crate) fn quit_app() {
    // 通知常驻进程退出，然后面板自己也退出
    taskmon::runtime::broadcast_quit();
    std::thread::sleep(std::time::Duration::from_millis(120));
    std::process::exit(0);
}

pub(crate) fn read_runtime() -> Value {
    std::fs::read_to_string(zx_core::cfg::run_dir().join("runtime.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}))
}

pub(crate) fn state_json() -> Value {
    let cfg = zx_core::cfg::cfg();
    let rt = read_runtime();
    let theme = match cfg.theme() {
        Some(zx_core::Theme::Light) => "light",
        Some(zx_core::Theme::Dark) => "dark",
        None => "system",
    };
    json!({
        "meta": { "version": env!("CARGO_PKG_VERSION") },
        "view": {
            "interval": cfg.sample_interval_ms(),
            "autoStart": taskmon::autostart::is_enabled(),
            "updateCheck": cfg.update_check(),
            // 更新源不可配：只在 GitHub 分发，检查地址写死在 zx_core::cfg::UPDATE_API
            "updateRepo": zx_core::cfg::UPDATE_REPO,
            "theme": theme,
            "compact": cfg.compact(),
            "separators": cfg.separators(),
            "onLeft": cfg.on_left(),
            "snapToStart": cfg.snap_to_start(),
            "floating": cfg.floating(),
            "floatingTopmost": cfg.floating_topmost(),
            "floatingEdgeHide": cfg.floating_edge_hide(),
            "floatingFullscreenHide": cfg.floating_fullscreen_hide(),
            "opacity": cfg.floating_opacity(),
            "itemOrder": cfg.normalized_order().to_vec(),
            "taskbarDisplays": cfg.taskbar_displays.clone().unwrap_or_default()
                .into_iter().map(|d| json!({ "id": d.id, "name": d.name })).collect::<Vec<_>>(),
            "merge": cfg.merge_by_path(),
            "diskDisplay": mode_str(cfg.disk_display()),
            "diskIndex": cfg.disk_display_index.unwrap_or(0),
            "gpuDisplay": mode_str(cfg.gpu_display()),
            "gpuIndex": cfg.gpu_display_index.unwrap_or(0),
            "gpuContent": match cfg.gpu_content() {
                zx_core::GpuTaskbarContent::Usage => "usage",
                zx_core::GpuTaskbarContent::Temperature => "temperature",
                zx_core::GpuTaskbarContent::Both => "both",
            },
            "gpuTempIndex": cfg.gpu_temp_index.map(|v| v as i64).unwrap_or(-1),
            "gpuTempUnit": if cfg.gpu_temp_unit() == zx_core::GpuTempUnit::Fahrenheit { "fahrenheit" } else { "celsius" },
            "netAdapterId": cfg.net_adapter_id.clone().unwrap_or_default(),
            "publicIp": cfg.public_ip(),
            "clash": cfg.clash(),
            "clashAddress": cfg.clash_api_address.clone().unwrap_or_default(),
            "clashSecret": cfg.clash_api_secret.clone().unwrap_or_default(),
            "cpuSamplingEnabled": cfg.cpu_sampling_enabled != Some(false),
            "ramSamplingEnabled": cfg.ram_sampling_enabled != Some(false),
            "diskSamplingEnabled": cfg.disk_sampling_enabled != Some(false),
            "gpuSamplingEnabled": cfg.gpu_sampling_enabled != Some(false),
            "netSamplingEnabled": cfg.net_sampling_enabled != Some(false),
        },
        "clipboard": clip_view(),
        "capture": capture_view(),
        "gesture": gesture_view(),
        "imgconv": imgconv_view(),
        "appicon": appicon_view(),
        "runtime": rt,
    })
}

// ---------------- 图片转换 ----------------
