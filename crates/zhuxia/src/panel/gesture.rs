//! 设置面板：鼠标手势规则编辑与运行状态。

use serde_json::{json, Value};

// 本模块与外部队同名：显式指向 crate，否则 `use super::*` 会把它导成自己
use ::gesture;

use super::*;

/// 只改手势那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
pub(crate) fn gesture_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.gesture, patch, gesture_view, || {})
}

/// 常驻进程每 5 秒写的运行状态（面板进程看不到钩子/覆盖层）。
pub(crate) struct GestureRuntime {
    hook: bool,
    overlay: bool,
    rules: usize,
    groups: usize,
}

pub(crate) fn gesture_runtime() -> GestureRuntime {
    let mut rt = GestureRuntime { hook: false, overlay: false, rules: 0, groups: 0 };
    let path = zx_core::cfg::run_dir().join("gesture-status.txt");
    let Ok(text) = std::fs::read_to_string(path) else { return rt };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("hook=") {
            rt.hook = v.trim() == "true";
        } else if let Some(v) = line.strip_prefix("overlay=") {
            rt.overlay = v.trim() == "true";
        } else if let Some(v) = line.strip_prefix("rules=") {
            rt.rules = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("groups=") {
            rt.groups = v.trim().parse().unwrap_or(0);
        }
    }
    rt
}

pub(crate) fn gesture_view() -> Value {
    let cfg = zx_core::cfg::cfg();
    let c = cfg.gesture();
    let raw = cfg.gesture.clone().unwrap_or_default();
    let rules: Vec<Value> = gesture::model::parse_rules(&c).iter().map(rule_view).collect();
    let groups: Vec<Value> = gesture::model::parse_groups(&c)
        .iter()
        .map(|g| {
            json!({
                "name": g.name,
                "apps": g.apps,
                "rules": g.rules.iter().map(rule_view).collect::<Vec<_>>(),
            })
        })
        .collect();
    let rt = gesture_runtime();

    json!({
        "enabled": c.enabled,
        "sensitivity": c.sensitivity,
        "middleButton": c.middle_button,
        "disableInFullscreen": c.disable_in_fullscreen,
        "disableOnDesktop": c.disable_on_desktop,
        "paused": c.paused,
        "hint": c.hint,
        "hintDurationMs": c.hint_duration_ms,
        "targetWindow": c.target_window,
        "excludedApps": raw.excluded_apps.clone().unwrap_or_default(),
        "trail": {
            "activeColor": color_hex(c.trail_active_color),
            "inactiveColor": color_hex(c.trail_inactive_color),
            "thickness": c.trail_thickness,
        },
        // 边缘滚动 / 按键回显：把"解析后的实际状态"给页面，未配置时也能显示默认动作
        "edges": {
            "left": { "enabled": c.edges.left.is_some(), "action": c.edges.left.map(|a| a.key()).unwrap_or("volume") },
            "right": { "enabled": c.edges.right.is_some(), "action": c.edges.right.map(|a| a.key()).unwrap_or("brightness") },
            "top": { "enabled": c.edges.top.is_some(), "action": c.edges.top.map(|a| a.key()).unwrap_or("tab") },
            "bottom": { "enabled": c.edges.bottom.is_some(), "action": c.edges.bottom.map(|a| a.key()).unwrap_or("app") },
            "margin": c.edges.margin,
        },
        "keyEcho": {
            "enabled": c.key_echo.enabled,
            "position": c.key_echo.position,
            "durationMs": c.key_echo.duration_ms,
            "fontSize": c.key_echo.font_size,
        },
        // 规则表是否被用户显式写过：决定着在页面上显示"内置默认"还是"自定义"
        "rulesExplicit": raw.rules.is_some(),
        "rules": rules,
        "groups": groups,
        // 分组表是否被显式写过：没写过时页面上标"内置浏览器分组"
        "groupsExplicit": raw.groups.is_some(),
        "runtime": {
            "alive": taskmon::runtime::instance_alive(),
            "hook": rt.hook,
            "overlay": rt.overlay,
            "rules": rt.rules,
            "groups": rt.groups,
        },
    })
}

pub(crate) fn rule_view(r: &gesture::model::Rule) -> Value {
    use gesture::model::{Action as GAction, Scope};

    let pattern: String = r.pattern.iter().map(|d| d.arrow()).collect();
    let (kind, text, operation, keys) = match &r.action {
        GAction::Hotkey(keys) => {
            let names: Vec<String> = keys.iter().map(|k| vk_name(*k)).collect();
            ("hotkey", names.join("+"), String::new(), names)
        }
        GAction::Window(op) => {
            use gesture::model::WindowOp::*;
            let (name, code) = match op {
                ToggleTopmost => ("切换置顶", "toggle-topmost"),
                ToggleMaximize => ("最大化/还原", "toggle-maximize"),
                Minimize => ("最小化", "minimize"),
                Close => ("关闭窗口", "close"),
            };
            ("window", name.to_string(), code.to_string(), Vec::new())
        }
        GAction::Program { path, .. } => ("program", path.clone(), String::new(), Vec::new()),
        GAction::Volume(op) => {
            use gesture::model::VolumeOp::*;
            let (name, code) = match op {
                Increase => ("音量 +", "increase"),
                Decrease => ("音量 −", "decrease"),
                Mute => ("静音", "mute"),
            };
            ("volume", name.to_string(), code.to_string(), Vec::new())
        }
        GAction::Internal(op) => {
            let name = match op.as_str() {
                "capture" => "截图",
                "clipboard" => "剪贴板面板",
                "imgbed" => "图床",
                "devtools" => "开发工具",
                "settings" => "打开设置",
                "toggle-exclude" => "排除当前程序",
                "close-similar" => "关闭相似窗口",
                "edit-hosts" => "编辑 hosts",
                _ => "内置动作",
            };
            ("internal", name.to_string(), op.clone(), Vec::new())
        }
    };
    let scope = match &r.scope {
        Scope::Global => "global".to_string(),
        Scope::App(name) => format!("app:{name}"),
        Scope::Category(name) => format!("category:{name}"),
    };

    json!({
        "name": r.name,
        "scope": scope,
        "button": r.button.as_str(),
        "pattern": pattern,
        "wheel": r.wheel.map(|w| w.as_str()).unwrap_or(""),
        "onWheel": r.on_wheel,
        "action": { "type": kind, "text": text, "operation": operation, "keys": keys },
    })
}

pub(crate) fn color_hex(argb: u32) -> String {
    format!("#{:06X}", argb & 0x00FF_FFFF)
}

/// 虚拟键码 → 界面上看得懂的名字（只覆盖常用键，其余给十六进制）。
pub(crate) fn vk_name(vk: u16) -> String {
    match vk {
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x5B => "Win".into(),
        0x0D => "Enter".into(),
        0x09 => "Tab".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x08 => "Backspace".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0xAE => "VolumeDown".into(),
        0xAF => "VolumeUp".into(),
        0xAD => "Mute".into(),
        0x6F..=0x87 => format!("F{}", vk - 0x6F),
        v if (0x30..=0x39).contains(&v) || (0x41..=0x5A).contains(&v) => {
            char::from_u32(v as u32).map(|c| c.to_string()).unwrap_or_else(|| format!("0x{v:X}"))
        }
        v => format!("0x{v:X}"),
    }
}

pub(crate) fn mode_str(m: zx_core::MetricDisplayMode) -> &'static str {    match m {
        zx_core::MetricDisplayMode::Average => "average",
        zx_core::MetricDisplayMode::Max => "max",
        zx_core::MetricDisplayMode::Specific => "specific",
    }
}
