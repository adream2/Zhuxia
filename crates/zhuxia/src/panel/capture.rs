//! 设置面板：截图、录屏、OCR 模型三条命令线（同一份 capture 配置段）。

use serde_json::{json, Value};

// 本模块与外部队同名：显式指向 crate，否则 `use super::*` 会把它导成自己
use ::capture;

use super::*;

/// 只改截图那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
pub(crate) fn capture_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.capture, patch, capture_view, || {})
}

#[tauri::command]
pub(crate) fn capture_open_dir() {
    let dir = zx_core::cfg::cfg().capture().save_dir;
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

// ---------------- 录屏（GIF + 摄像头合成） ----------------

/// 只改 `capture.record` 那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
pub(crate) fn record_patch(patch: Value) -> Value {
    // record 是 capture 的嵌套段：访问器顺手把父段补出来。
    patch_section(|s| &mut s.capture.get_or_insert_with(Default::default).record, patch, record_view, || {})
}

/// 摄像头列表只能在常驻进程里问（MF 要 `MFStartup`，两个进程同时枚举会抢设备）。
///
/// 面板进程自己调一次也可以（OpenCV 不需要），但那样每次开设置页都要等几百毫秒，
/// 而且要处理"常驻正在录"的冲突。这里走一条独立枚举，失败就给空列表 + 提示。
#[tauri::command]
pub(crate) fn record_devices() -> Value {
    let list = capture::record::camera::list();
    json!({
        "devices": list.iter().map(|d| d.name.clone()).collect::<Vec<_>>(),
        "ok": !list.is_empty(),
    })
}

#[tauri::command]
pub(crate) fn record_open_dir() {
    let dir = zx_core::cfg::cfg().capture().save_dir;
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

pub(crate) fn record_view() -> Value {
    let r = zx_core::cfg::cfg().record();
    let quality = if r.bpp < 0.09 {
        "lite"
    } else if r.bpp > 0.18 {
        "high"
    } else {
        "standard"
    };
    // 给界面一个"这档画质大概多少 Mbps"的直观值（按 1080p 估）
    let mbps_1080 = capture::record::mp4::bitrate_for(1920, 1080, r.fps, r.bpp);
    json!({
        "enabled": r.enabled,
        "format": r.format.key(),
        "quality": quality,
        "mbps1080": (mbps_1080 as f64 / 1_000_000.0 * 10.0).round() / 10.0,
        "fps": r.fps,
        "maxSeconds": r.max_seconds,
        "maxWidth": r.max_width,
        "countdown": r.countdown,
        "cursor": r.cursor,
        "camera": {
            "enabled": r.camera.enabled,
            "device": r.camera.device,
            "width": r.camera.width,
            "height": r.camera.height,
            "fps": r.camera.fps,
        },
        "layout": {
            "mode": r.layout.mode,
            "corner": r.layout.corner,
            "size": r.layout.size,
            "shape": r.layout.shape,
            "mirror": r.layout.mirror,
            "border": r.layout.border,
            "shadow": r.layout.shadow,
            "margin": r.layout.margin,
        },
        "audio": {
            "system": r.audio.system,
            "mic": r.audio.mic,
            "micDevice": r.audio.mic_device,
            "systemGain": r.audio.system_gain,
            "micGain": r.audio.mic_gain,
            // GIF 没有音轨，界面要能提前提示
            "supported": r.format == zx_core::RecordFormat::Video,
        },
    })
}

/// 音频设备列表。
///
/// 单独的 command 而不是塞进 `record_view`：WASAPI 枚举每次要 50~100ms，
/// 而 `record_view` 是每次改配置都会走一遍的。
#[tauri::command]
pub(crate) fn record_audio_devices() -> Value {
    let mics = capture::record::audio::devices(true);
    let speaker = capture::record::audio::default_speaker().unwrap_or_default();
    json!({
        // 设备 id 是 WASAPI endpoint id，名字给界面看
        "mics": mics.iter().map(|d| json!({ "id": d.id, "name": d.name })).collect::<Vec<_>>(),
        // 系统声音从"默认播放设备"录，告诉用户是哪个，省得他找错地方
        "speaker": speaker,
        "ok": !mics.is_empty(),
    })
}

// ---------------- OCR 增强模型（可选下载） ----------------

#[tauri::command]
pub(crate) fn ocr_model_state() -> Value {
    let st = capture::ocr_model::status();
    json!({
        "installed": st.installed,
        "bytes": st.bytes,
        "dir": st.dir.to_string_lossy(),
        "det": st.det,
        "rec": st.rec,
        "keys": st.keys,
        "engine": if st.installed { "PaddleOCR" } else { "系统 OCR" },
    })
}

#[tauri::command]
pub(crate) fn ocr_model_progress() -> Value {
    let p = capture::ocr_model::progress();
    json!({
        "running": p.running,
        "done": p.done,
        "total": p.total,
        "stage": p.stage,
        "error": p.error,
    })
}

/// 下载是长任务：立刻返回，前端轮询 `ocr_model_progress`。
#[tauri::command]
pub(crate) fn ocr_model_download(source: String, custom: String) -> Value {
    let src = match source.as_str() {
        "hf" => capture::ocr_model::Source::HuggingFace,
        "mirror" => capture::ocr_model::Source::HfMirror,
        "custom" => capture::ocr_model::Source::Custom,
        _ => capture::ocr_model::Source::Auto,
    };
    std::thread::Builder::new()
        .name("zx-ocr-dl".into())
        .spawn(move || {
            let _ = capture::ocr_model::download(src, &custom);
        })
        .ok();
    json!({ "started": true })
}

#[tauri::command]
pub(crate) fn ocr_model_import(dir: String) -> Value {
    let r = capture::ocr_model::install_from_dir(std::path::Path::new(&dir));
    json!({ "ok": r.is_ok(), "error": r.err() })
}

#[tauri::command]
pub(crate) fn ocr_model_remove() -> Value {
    let _ = capture::ocr_model::remove();
    json!({ "ok": true })
}

/// 常驻进程把热键状态写在 exe 目录，设置页（另一个进程）读它。
pub(crate) fn hotkey_state() -> (String, String) {
    let path = zx_core::cfg::run_dir().join("capture-hotkey.txt");
    let Ok(text) = std::fs::read_to_string(path) else { return (String::new(), String::new()) };
    let mut active = String::new();
    let mut error = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("active=") {
            active = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("error=") {
            error = v.trim().to_string();
        }
    }
    (active, error)
}

pub(crate) fn capture_view() -> Value {
    let cfg = zx_core::cfg::cfg();
    let c = cfg.capture();
    // saveDirRaw 是用户填的值（可能为空 = 用默认），saveDir 是解析后的绝对路径。
    // 输入框要回显前者，否则"留空用默认"这个语义一保存就变成写死默认路径。
    let raw = cfg.capture.clone().unwrap_or_default().save_dir.unwrap_or_default();
    let (hotkey_active, hotkey_error) = hotkey_state();
    json!({
        "enabled": c.enabled,
        "hotkey": c.hotkey,
        "hotkeyActive": hotkey_active,
        "hotkeyError": hotkey_error,
        "saveDir": c.save_dir.to_string_lossy(),
        "saveDirRaw": raw,
        "autoSave": c.auto_save,
        "magnifier": c.magnifier,
        "detectWindow": c.detect_window,
        "record": record_view(),
    })
}

// ---------------- 剪贴板 ----------------
