//! 模块入口：任务栏视图 + 5 个详情弹窗 + 设置。
//!
//! 作为"驻匣"的一个模块，它只对外暴露 `app::start()` / `app::open_settings()` / `app::exit()`，
//! 其余全部内部化；宿主（zhuxia.exe）不关心它怎么画、怎么采样。

pub mod app;
pub mod autostart;
pub mod detail;
pub mod dialogs;
pub mod http;
pub mod icmp;
pub mod layout;
pub mod netfw;
pub mod netstat;
pub mod overlay;
pub mod runtime;
pub mod sampling;
pub mod tray;
pub mod update;
pub mod snapshot;

/// 当前进程是否已提权（SRUM 的按进程网速只有管理员能读）。
pub fn win_admin() -> bool { zx_core::win::is_admin() }

/// SRUM 实时接口是否注册成功（失败只影响按进程网速）。
pub fn srum_ok() -> bool { sampling::srum::registered() }

static ETW_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_etw_ok(v: bool) { ETW_OK.store(v, std::sync::atomic::Ordering::Relaxed); }
pub fn etw_ok() -> bool { ETW_OK.load(std::sync::atomic::Ordering::Relaxed) }

/// 应用界面（弹窗/设置/对话框）当前的浅色状态：设置里强制优先，否则跟随系统。
pub fn ui_light() -> bool {
    match zx_core::cfg::cfg().theme() {
        Some(zx_core::Theme::Light) => true,
        Some(zx_core::Theme::Dark) => false,
        None => zx_core::win::apps_light(),
    }
}

/// 宿主注入的“设置面板”实现（Tauri）。返回 false 时回退到原生设置页。
static PANEL: std::sync::OnceLock<Box<dyn Fn() -> bool + Send + Sync>> = std::sync::OnceLock::new();

pub fn set_panel_opener(f: impl Fn() -> bool + Send + Sync + 'static) {
    let _ = PANEL.set(Box::new(f));
}

/// 宿主注入的"关掉设置面板"实现。面板是独立进程，退出常驻时必须一并结束它。
static PANEL_CLOSER: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

pub fn set_panel_closer(f: impl Fn() + Send + Sync + 'static) {
    let _ = PANEL_CLOSER.set(Box::new(f));
}

fn close_panel() {
    if let Some(f) = PANEL_CLOSER.get() { f(); }
}

/// 宿主注入的"模块动作"：托盘菜单点「截图 / 录屏」时转给宿主，
/// taskmon 自己不认识 capture（模块之间不直接依赖）。
/// 宿主注入的动作回调（托盘菜单把「截图 / 录屏」转出去）。
type ActionHook = Box<dyn Fn(&str) + Send + Sync>;
static ACTIONS: std::sync::OnceLock<ActionHook> = std::sync::OnceLock::new();

pub fn set_action_hook(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = ACTIONS.set(Box::new(f));
}

pub fn run_action(op: &str) {
    match ACTIONS.get() {
        Some(f) => f(op),
        None => zx_core::log::warn(format!("托盘动作 {op} 没有接线")),
    }
}

/// 托盘菜单需要的跨模块状态。
///
/// taskmon 不直接 `use gesture` / `imgbed`（模块之间不互相依赖），
/// 由宿主注入一个只读探针；没注入时退化为只读配置，至少不会 panic。
#[derive(Clone, Copy, Debug, Default)]
pub struct TrayState {
    /// `gesture.enabled`：启动时是否真的装了钩子。false = 改了要重启才生效。
    pub gesture_enabled: bool,
    /// `gesture.paused`：运行期暂停，这个可以立即切换。
    pub gesture_paused: bool,
    pub key_echo: bool,
    pub imgbed_enabled: bool,
    /// 图床是否已经填全了当前平台需要的字段（没配置时仍然可以进设置页）。
    pub imgbed_configured: bool,
    /// 截图模块是否加载（左键截图前判断，禁用时给个气泡而不是静默失败）。
    pub capture_enabled: bool,
}

static TRAY_STATE: std::sync::OnceLock<fn() -> TrayState> = std::sync::OnceLock::new();

pub fn set_tray_state(f: fn() -> TrayState) {
    let _ = TRAY_STATE.set(f);
}

pub fn tray_state() -> TrayState {
    if let Some(f) = TRAY_STATE.get() {
        return f();
    }
    let c = zx_core::cfg::cfg();
    let g = c.gesture();
    TrayState {
        gesture_enabled: g.enabled,
        gesture_paused: g.paused,
        key_echo: g.key_echo.enabled,
        imgbed_enabled: c.imgbed().enabled,
        // 没注入宿主时无法判断字段是否填全，按"已配置"处理，避免误导为未配置。
        imgbed_configured: true,
        capture_enabled: c.capture().enabled,
    }
}

pub fn open_settings() {
    if let Some(f) = PANEL.get() {
        if f() { return; }
        zx_core::log::warn("设置面板拉不起来，改为提示用户手动改配置");
    }
    settings_fallback();
}

/// 设置面板打不开时的兜底：说清原因 + 直接打开配置文件所在目录。
///
/// 原生设置页已经删掉了（Tauri 面板覆盖了全部设置，那 890 行只为一个几乎不会走的
/// 分支存在）。这里保留一条**能真的改到配置**的路，而不是死路一条。
fn settings_fallback() {
    let path = zx_core::cfg::settings_path();
    let dir = path.as_ref().and_then(|p| p.parent().map(|d| d.display().to_string()))
        .unwrap_or_default();
    let file = path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
    let body = [
        "设置面板依赖 WebView2 运行时。",
        "装一下 Microsoft Edge WebView2 Runtime 再试通常就好了。",
        "也可以直接编辑 settings.yaml（改完保存，程序 1 秒内自动生效）。",
    ];
    let mut lines: Vec<&str> = body.to_vec();
    let file_line;
    if !file.is_empty() { file_line = format!("配置文件：{file}"); lines.push(&file_line); }
    if crate::dialogs::card("设置面板打不开", &lines, &["打开配置文件夹", "知道了"]) == 0
        && !dir.is_empty()
    {
        zx_core::win::shell_open(&dir);
    }
}

/// 设置面板（独立进程）改完文件后：重新读配置并立即生效。
pub fn reload_and_apply() {
    let before = zx_core::cfg::cfg();
    zx_core::cfg::reload();
    let after = zx_core::cfg::cfg();
    if before.floating() != after.floating() {
        app::rebuild_overlays();
    } else {
        app::apply_live();
    }
    zx_core::log::info("配置已重新加载");
}

/// 写配置并立即落盘（UI 线程专用；采样线程只读）。
pub fn cfg_set(f: impl FnOnce(&mut zx_core::Settings)) {
    zx_core::cfg::update_cfg(f);
}

/// 采样线程看门狗：返回距上一次 tick 的毫秒数。
pub fn last_tick_age_ms() -> u64 {
    let last = sampling::LAST_TICK_MS.load(std::sync::atomic::Ordering::Relaxed);
    if last == 0 { 0 } else { sampling::proc::tick_ms().saturating_sub(last) }
}
