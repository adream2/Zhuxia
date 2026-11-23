//! 驻匣 · 鼠标手势模块
//!
//! 原生 Rust 常驻实现，不依赖脚本引擎或外部运行时：
//!
//! ```text
//! WH_MOUSE_LL 钩子（只采样 + 吞放）
//!    │  Event{Start/Move/Wheel/End}
//!    ▼
//! 解析线程 ── Recognizer（方向串）── Matcher（作用域 + 鼠标键 + 滚轮）── 动作线程
//!    │                                                              │
//!    └── PostMessage(WM_TRAIL/WM_HINT) ── 覆盖层（分层窗口 + GDI+）◀─┘
//! ```
//!
//! 与其它模块的关系：只通过 `zx_core` 读配置、写日志，不 `use` taskmon / clipboard / capture。
//! 内置动作（截图/剪贴板/图床/开发工具）由宿主注入，手势模块不认识别的模块。
//!
//! 不包含：手势录制、触发器/摩擦边、音量亮度 OSD、云同步、自动更新。

pub mod action;
pub mod edges;
pub mod engine;
pub mod hook;
pub mod keyecho;
pub mod matcher;
pub mod model;
pub mod overlay;
pub mod recognizer;
pub mod target;

use std::sync::mpsc::channel;
use std::sync::OnceLock;

/// 当前生效的手势配置（每次现读，改完配置立刻生效）。
pub fn cfg() -> zx_core::GestureSettings {
    zx_core::cfg::cfg().gesture()
}

static ACTION_HOOK: OnceLock<fn(&str)> = OnceLock::new();

/// 宿主注入内置动作的实现。
///
/// 手势模块不 `use` 别的模块（这是模块契约），所以 `internal` 动作只往外递一个字符串：
/// `capture` / `clipboard` / `settings`，由宿主 `modules::install()` 里接线到对应模块。
pub fn set_action_hook(f: fn(&str)) {
    let _ = ACTION_HOOK.set(f);
}

/// 执行内置动作（`action.rs` 与自检用）。没有宿主实现时只记日志。
pub(crate) fn run_internal(op: &str) {
    match ACTION_HOOK.get() {
        Some(f) => f(op),
        None => zx_core::log::warn(format!("手势内置动作「{op}」没有注册宿主实现")),
    }
}

/// 模块入口：宿主在 `modules::install()` 里调一次。
pub fn start() {
    let c = cfg();
    // 按键回显与手势开关无关：它是个独立的小工具，手势关着也该能用。
    keyecho::start();
    // 配置热更新也必须先起来：否则手势关着时改回显配置不会生效，
    // 与“独立小工具”的定位冲突（以前就是在这里提前 return 埋的坑）。
    maintenance();
    if !c.enabled {
        // 手势不加载，但按键回显要按配置先生效：它有独立的键盘钩子，
        // 不依赖 overlay / 手势引擎。以前这里直接 return，回显配置要等
        // 第一次 mtime 变化才被推下去。
        keyecho::apply(&c.key_echo);
        zx_core::log::info("手势模块已禁用（settings.yaml → gesture.enabled）");
        return;
    }

    overlay::register();
    let overlay_ok = overlay::create();
    apply_config(&c);

    let (tx, rx) = channel();
    let hook_ok = hook::install(tx);
    engine::start(rx);
    edges::start();

    zx_core::log::info(format!(
        "手势模块已启动：{} 条规则，灵敏度 {}，中键={}，覆盖层={}，钩子={}，边缘滚动={}，按键回显={}",
        matcher::rule_count(),
        c.sensitivity,
        c.middle_button,
        overlay_ok,
        hook_ok,
        if c.edges.any() { "开" } else { "关" },
        if c.key_echo.enabled { "开" } else { "关" }
    ));
    if !hook_ok {
        zx_core::log::warn("手势模块降级运行：钩子未安装，识别不会触发（检查安全软件/权限）");
    }
}

/// 把配置推给各静态表。规则表变化时同步"哪一侧有滚轮规则"和"中键是否启用"。
pub fn apply_config(c: &zx_core::GestureSettings) {
    let rules = model::parse_rules(c);
    let groups = model::parse_groups(c);
    if rules.is_empty() && groups.is_empty() {
        zx_core::log::warn("手势规则表为空：按住右键划动不会触发任何动作");
    }
    matcher::set_config(rules, groups);
    hook::set_wheel_rules(
        matcher::has_wheel_rules(model::MouseButton::Right),
        matcher::has_wheel_rules(model::MouseButton::Middle),
    );
    hook::set_middle_enabled(c.middle_button);
    // 识别与绘制都跑在热路径上，配置只在这里推一次，不在每次鼠标移动时重读。
    engine::set_sensitivity(c.sensitivity);
    overlay::set_style(c.trail_active_color, c.trail_inactive_color, c.trail_thickness);
    edges::apply(&c.edges);
    keyecho::apply(&c.key_echo);
}

/// 设置页改完配置后调用（常驻进程里由维护线程的 mtime 轮询触发）。
pub fn reload() {
    apply_config(&cfg());
}

/// 排除/恢复某个程序（`→↙→` 手势）。
///
/// 写回 `settings.yaml → gesture.excludedApps`；门控线程 256ms 内读到新配置，
/// 此后在该程序的窗口里不再起手势。
///
/// 注意：排除之后**没法再用同一个手势恢复**——手势已经不在那个程序里响应了，
/// 恢复入口在「设置 → 工具 → 鼠标手势 → 排除的程序」。
pub fn toggle_excluded(target_window: isize) -> Option<String> {
    let hwnd = if target::is_valid(windows::Win32::Foundation::HWND(
        target_window as *mut core::ffi::c_void,
    )) {
        windows::Win32::Foundation::HWND(target_window as *mut core::ffi::c_void)
    } else {
        target::foreground()
    };
    let app = target::process_name(hwnd)?;

    let mut list = cfg().excluded_apps;
    let added = match list.iter().position(|e| e == &app) {
        Some(pos) => {
            list.remove(pos);
            false
        }
        None => {
            list.push(app.clone());
            true
        }
    };
    let next = list.clone();
    zx_core::cfg::update_cfg(|s| {
        let g = s.gesture.get_or_insert_with(Default::default);
        g.excluded_apps = Some(next);
    });

    let text = if added {
        format!("已排除 {app}：在此程序里禁用手势（去设置页恢复）")
    } else {
        format!("已恢复 {app} 的手势")
    };
    zx_core::log::info(text.clone());
    overlay::show_hint(&text);
    overlay::settle(1800);
    Some(app)
}

/// 暂停/恢复。写回 `settings.yaml`，重启后保持。
///
/// 写完立刻 `reload()`：钩子读的是 `apply_config` 推进去的快照，
/// 只改配置要等下一次 mtime 轮询才生效——托盘点击需要“按下去就有反应”。
pub fn set_paused(paused: bool) {
    zx_core::cfg::update_cfg(|s| {
        let g = s.gesture.get_or_insert_with(Default::default);
        g.paused = Some(paused);
    });
    reload();
    zx_core::log::info(if paused { "手势已暂停（托盘/设置）" } else { "手势已恢复（托盘/设置）" });
}

/// 按键回显单独启停。它不依赖手势模块是否加载（回显有自己的键盘钩子）。
pub fn set_key_echo_enabled(on: bool) {
    zx_core::cfg::update_cfg(|s| {
        let g = s.gesture.get_or_insert_with(Default::default);
        let mut ke = g.key_echo.clone().unwrap_or_default();
        ke.enabled = Some(on);
        g.key_echo = Some(ke);
    });
    reload();
    zx_core::log::info(if on { "按键回显已开启" } else { "按键回显已关闭" });
}

pub fn key_echo_enabled() -> bool {
    cfg().key_echo.enabled
}

pub fn paused() -> bool {
    cfg().paused
}

/// 诊断信息（设置页显示）。
pub struct Status {
    pub enabled: bool,
    pub hook: bool,
    pub overlay: bool,
    pub rules: usize,
    pub groups: usize,
    pub paused: bool,
    pub sensitivity: i32,
}

pub fn status() -> Status {
    let c = cfg();
    Status {
        enabled: c.enabled,
        hook: hook::active(),
        overlay: overlay::ready(),
        rules: matcher::rule_count(),
        groups: matcher::group_count(),
        paused: c.paused,
        sensitivity: c.sensitivity,
    }
}

/// 诊断报告：把"为什么手势不响应"需要的所有事实一次列出来。
///
/// 输出同时写日志和 `<exe>/gesture-doctor.txt`，用户直接把这个文件发出来即可。
pub fn doctor() -> String {
    let c = cfg();
    let rules = model::parse_rules(&c);
    let groups = model::parse_groups(&c);
    let (allowed, reason, cursor, app) = hook::gate_report();
    let (vx, vy, vw, vh) = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let fg = target::foreground();
    // 常驻进程的实时状态从它每 5 秒写出的快照读：doctor 自己是独立进程，没有钩子。
    let status_path = zx_core::cfg::run_dir().join("gesture-status.txt");
    let age = std::fs::metadata(&status_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX);
    let live = std::fs::read_to_string(&status_path).unwrap_or_default();
    let live_line = if live.trim().is_empty() || age > 20 {
        format!("常驻进程状态: {}（驻匣没在运行？）", if live.trim().is_empty() { "没有读到".into() } else { format!("已过期 {age}s") })
    } else {
        let pick = |key: &str| {
            live.lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        format!(
            "常驻进程状态: 钩子={} 覆盖层={} 生效规则={} 分组={}",
            pick("hook="),
            pick("overlay="),
            pick("rules="),
            pick("groups=")
        )
    };

    // 规则明细：分组优先，所以先把分组列出来（用户最常问"某条手势到底装没装"）
    let arrows = |r: &model::Rule| r.pattern.iter().map(|d| d.arrow()).collect::<String>();
    let group_lines: String = groups
        .iter()
        .map(|g| {
            let items: Vec<String> = g
                .rules
                .iter()
                .map(|r| format!("{}={}", arrows(r), r.name))
                .collect();
            format!("分组「{}」{} 条：{}", g.name, g.rules.len(), items.join("  "))
        })
        .collect::<Vec<_>>()
        .join("
         ");

    let report = format!(
        "驻匣手势诊断 v{}
         ------------------------------
         模块: enabled={} paused={} 中键={}
         手感: 灵敏度={} 目标窗口={}
         拦截: 全屏禁用={} 桌面禁用={}
         轨迹: active=#{:06X} inactive=#{:06X} 线宽={}px
         规则: 通用 {} 条 / 分组 {} 个 / 排除 {} 个
         {}
         排除列表: {:?}
         {}
         虚拟桌面: {}x{} @ ({},{})
         光标: ({},{}) 所在进程={:?}
         门控结论: {}（{}）
         前台进程={:?} 前台全屏={} 前台是桌面={}
",
        env!("CARGO_PKG_VERSION"),
        c.enabled,
        c.paused,
        c.middle_button,
        c.sensitivity,
        c.target_window,
        c.disable_in_fullscreen,
        c.disable_on_desktop,
        c.trail_active_color & 0xFFFFFF,
        c.trail_inactive_color & 0xFFFFFF,
        c.trail_thickness,
        rules.len(),
        groups.len(),
        c.excluded_apps.len(),
        group_lines,
        c.excluded_apps,
        live_line,
        vw,
        vh,
        vx,
        vy,
        cursor.0,
        cursor.1,
        app,
        if allowed { "允许" } else { "拦截" },
        reason.unwrap_or("无"),
        target::process_name(fg),
        target::foreground_fullscreen(),
        target::is_desktop_surface(fg),
    );
    let path = zx_core::cfg::run_dir().join("gesture-doctor.txt");
    let _ = std::fs::write(&path, &report);
    zx_core::log::info(format!("手势诊断已写入 {}", path.display()));
    report
}

/// 无窗口自检：建覆盖层、画一条折线 + 提示，1.2 秒后自己收起来。
/// 调用方负责跑消息循环（`--gesture-selftest` 就是这么用的）。
pub fn selftest() -> bool {
    overlay::register();
    apply_config(&cfg());
    overlay::selftest()
}

/// 维护线程：`settings.yaml` 变了就重读（与 clipboard/taskmon 同一套路）。
///
/// 面板是独立进程，改完配置广播 `WM_APP_RELOAD`；权限不一致时广播会被 UIPI 拦，
/// 所以这里还有一条 1 秒一次的 mtime 轮询兜底。
fn maintenance() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("zx-gesture-maint".into())
        .spawn(|| {
            let mut last = settings_mtime();
            let mut last_status = std::time::Instant::now() - std::time::Duration::from_secs(9);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let mtime = settings_mtime();
                if mtime != last {
                    last = mtime;
                    zx_core::cfg::reload();
                    reload();
                    zx_core::log::info(format!("手势配置已重载：{} 条规则", matcher::rule_count()));
                }
                // 轨迹画布是一块虚拟桌面大小的 DIB（2560×1440 约 14MB，双 4K 约 66MB）。
                // 不划手势的用户本来就不付这笔钱；划过之后也不能永久占着——
                // 闲置 10 分钟且轨迹层不可见时由覆盖层线程释放，下次手势再按需分配。
                overlay::release_canvas_if_idle(10 * 60 * 1000);
                // 设置面板是独立进程，看不到这里的钩子/覆盖层状态：每 5 秒落一份快照。
                if last_status.elapsed() >= std::time::Duration::from_secs(5) {
                    last_status = std::time::Instant::now();
                    write_status_file();
                }
            }
        })
        .ok();
}

/// `<exe>/gesture-status.txt`：给面板进程读的运行状态（与 capture-hotkey.txt 同一套路）。
fn write_status_file() {
    let st = status();
    let text = format!(
        "hook={}\noverlay={}\nrules={}\ngroups={}\npaused={}\nsensitivity={}\n",
        st.hook, st.overlay, st.rules, st.groups, st.paused, st.sensitivity
    );
    let path = zx_core::cfg::run_dir().join("gesture-status.txt");
    let _ = std::fs::write(path, text);
}

fn settings_mtime() -> Option<std::time::SystemTime> {
    zx_core::cfg::settings_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static HOOK_HITS: AtomicUsize = AtomicUsize::new(0);

    fn counting_hook(_op: &str) {
        HOOK_HITS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn internal_action_calls_host_hook() {
        set_action_hook(counting_hook);
        run_internal("capture");
        assert_eq!(HOOK_HITS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn default_rules_include_screenshot_on_zigzag() {
        let rules = model::defaults();
        let shot = rules
            .iter()
            .find(|r| r.action == model::Action::Internal("capture".into()))
            .expect("默认规则里应当有一条触发截图的手势");
        assert_eq!(
            shot.pattern,
            vec![
                model::Direction::DownLeft,
                model::Direction::DownRight,
                model::Direction::DownLeft
            ]
        );
        assert_eq!(shot.button, model::MouseButton::Right);
        assert_eq!(shot.scope, model::Scope::Global);
    }
}
