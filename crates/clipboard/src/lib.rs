//! 驻匣 · 剪贴板模块
//!
//! 原生 Win32 常驻实现（不引入 Electron / WebView，常驻开销与 taskmon 同一量级）：
//!
//! ```text
//! AddClipboardFormatListener ─▶ 采集线程 ─▶ 规则/去重 ─▶ history.dat（DPAPI 密文）+ 加密附件
//!                                                          │
//!          RegisterHotKey ─▶ 面板窗口（搜索 / 列表 / 详情 / 粘贴）◀┘
//! ```
//!
//! 对外只暴露 `start()`（宿主登记模块用）和少量命令（设置面板/热键回调）。

pub mod capture;
pub mod hotkeys;
pub mod model;
pub mod panel;
pub mod paste;
pub mod rules;
pub mod sound;
pub mod source;
pub mod secure;
pub mod store;
pub mod winv;
pub mod win_clip;

use std::sync::OnceLock;

use zx_core::ClipboardSettings;

/// 当前配置（每次现读，设置页改完立刻生效）。
pub fn cfg() -> ClipboardSettings {
    zx_core::cfg::cfg().clipboard()
}

static SETTINGS_OPENER: OnceLock<fn()> = OnceLock::new();

/// 宿主注入「打开设置面板」的实现（面板进程与常驻进程分开，模块自己不拉进程）。
pub fn set_settings_opener(f: fn()) {
    let _ = SETTINGS_OPENER.set(f);
}

pub fn open_settings() {
    if let Some(f) = SETTINGS_OPENER.get() {
        f();
    }
}

/// 模块入口：宿主在 `modules::install()` 里调一次。
pub fn start() {
    let cfg = cfg();
    if !cfg.enabled {
        zx_core::log::info("剪贴板模块已禁用（settings.yaml → clipboard.enabled）");
        return;
    }
    // 先建存储（会清理上次残留的会话图片与无引用附件）
    let _ = store::store().lock().map(|s| s.len());
    win_clip::set_paused(cfg.pause);
    winv::set_enabled(cfg.takeover_win_v);
    panel::init();
    source::start();
    capture::start();
    hotkeys::start();
    maintenance();
    zx_core::log::info(format!(
        "剪贴板模块已启动：主热键 {}，上限 {} 条，持久化={}",
        cfg.hotkey, cfg.limit, cfg.persistent
    ));
}

/// 维护线程：接管设置面板的「清空历史」哨兵，并让「暂停」立刻生效。
///
/// 面板是独立进程，改不了常驻进程的内存；清空走 `<exe>/clipboard/clear.flag`，
/// 与 taskmon 的 netstat 重置同一个套路。
fn maintenance() {
    std::thread::Builder::new()
        .name("zx-clip-maint".into())
        .spawn(|| {
            let mut last_mtime = settings_mtime();
            loop {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            // settings.yaml 变过就重读：面板进程与常驻进程权限不一致时广播会被 UIPI 拦，
            // 这条 mtime 轮询是兜底（与 taskmon 同一套路）。
            let mtime = settings_mtime();
            if mtime != last_mtime {
                last_mtime = mtime;
                zx_core::cfg::reload();
                hotkeys::reload();
                panel::refresh();
            }
            let cfg = cfg();
            win_clip::set_paused(cfg.pause);
            winv::set_enabled(cfg.takeover_win_v);
            let flag = zx_core::cfg::clipboard_dir().join("clear.flag");
            if let Ok(mode) = std::fs::read_to_string(&flag) {
                let _ = std::fs::remove_file(&flag);
                let keep_pinned = mode.trim() == "keep_pinned";
                let removed = clear_history(keep_pinned);
                panel::refresh();
                zx_core::log::info(format!("按设置面板请求清空剪贴板历史（保留收藏={keep_pinned}，删除 {removed} 条）"));
            }
            }
        })
        .ok();
}

fn settings_mtime() -> Option<std::time::SystemTime> {
    zx_core::cfg::settings_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// 清空历史（面板页「清空」按钮）。
pub fn clear_history(keep_pinned: bool) -> usize {
    let mut s = store::store().lock().unwrap_or_else(|p| p.into_inner());
    let before = s.len();
    s.clear(keep_pinned);
    before.saturating_sub(s.len())
}
