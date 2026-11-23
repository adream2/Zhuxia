//! 应用级操作：启动覆盖层、开设置、切详情、退出。

use std::sync::atomic::{AtomicI32, Ordering};

use windows::Win32::Foundation::HWND;

use crate::overlay;
use crate::snapshot;

static SELECTED: AtomicI32 = AtomicI32::new(-1);

pub fn selected() -> i32 { SELECTED.load(Ordering::Relaxed) }
pub fn set_selected(v: i32) {
    SELECTED.store(v, Ordering::Relaxed);
    overlay::redraw_all();
}

/// 启动：采样线程 + 覆盖层 + 每秒的健康检查。
pub fn start() {
    // 上一次运行留下的退出哨兵不算数
    crate::runtime::clear_quit_flag();
    // 历史流量与封禁集合
    crate::netstat::load();
    overlay::register();
    crate::tray::create();
    // 计划任务状态：后台查一次，别挡住首帧
    std::thread::spawn(crate::autostart::refresh);
    crate::sampling::spawn();
    rebuild_overlays();

    // 启动后异步查一次更新（配置留空则完全不联网）
    if let Some(h) = crate::overlay::first_hwnd() {
        crate::update::check_async(h.0 as isize);
    }

    // 调试钩子：ZHUXIA_DEBUG_DETAIL=0..4 直接弹出对应详情，ZHUXIA_DEBUG_SETTINGS=1 打开设置
    if let Ok(v) = std::env::var("ZHUXIA_DEBUG_DETAIL") {
        if let Ok(slot) = v.parse::<i32>() {
            if let Some(h) = crate::overlay::first_hwnd() {
                toggle_detail(slot, h);
            }
        }
    }
    if std::env::var("ZHUXIA_DEBUG_SETTINGS").is_ok() {
        // 直达某一页请用 `--settings <page>`（Tauri 面板的参数）
        open_settings();
    }
    if std::env::var("ZHUXIA_DEBUG_FLOAT").ok().as_deref() == Some("1") {
        crate::cfg_set(|c| c.floating_mode = Some(true));
        rebuild_overlays();
    }

    // 配置文件外部变更（设置面板进程写入 / 手工编辑）→ 1 秒内自动生效
    std::thread::Builder::new().name("zx-cfgwatch".into()).spawn(|| {
        let mut last = std::fs::metadata(zx_core::cfg::settings_path().unwrap_or_default())
            .and_then(|m| m.modified()).ok();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            let now = std::fs::metadata(zx_core::cfg::settings_path().unwrap_or_default())
                .and_then(|m| m.modified()).ok();
            if now.is_some() && now != last {
                last = now;
                crate::reload_and_apply();
            }
            // 第二个实例请求打开设置（跨权限级别也可靠）
            if crate::runtime::take_open_settings_request() {
                crate::open_settings();
            }
            // 设置面板点了"清空流量统计"
            if crate::netstat::take_reset_request() {
                crate::netstat::reset();
            }
            // 外部请求退出（--quit / 设置面板的"退出驻匣"）
            if crate::runtime::take_quit_request() {
                zx_core::log::info("收到外部退出请求");
                crate::runtime::post_quit_to_ui();
                return;
            }
        }
    }).ok();

    // 一个轻量看门狗：采样线程卡死超过 5 秒就记一笔日志（不自动重启，避免抖动）。
    std::thread::Builder::new().name("zx-watchdog".into()).spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let age = crate::last_tick_age_ms();
        if age > 6000 {
            zx_core::log::warn(format!("采样线程已 {age}ms 未出帧"));
        }
    }).ok();
}

/// 按当前配置（重）建覆盖层：任务栏形态 or 悬浮形态。
pub fn rebuild_overlays() {
    overlay::destroy_all();
    let cfg = zx_core::cfg::cfg();
    if cfg.floating() {
        profile("悬浮形态");
        overlay::create_floating();
    } else {
        profile("任务栏形态");
        overlay::create_taskbar();
    }
}

fn profile(what: &str) {
    zx_core::log::info(format!("覆盖层切换为{what}"));
}

/// 设置变化后：重排 + 立即重绘 + 详情窗口同步。
pub fn apply_live() {
    overlay::refresh_all();
    crate::detail::refresh_all();
}

pub fn toggle_detail(slot: i32, anchor: HWND) {
    if selected() == slot {
        crate::detail::close_all();
        set_selected(-1);
        return;
    }
    crate::detail::close_all();
    if crate::detail::open(slot, anchor) {
        set_selected(slot);
    } else {
        set_selected(-1);
    }
}

pub fn open_settings() {
    crate::open_settings();
}

pub fn exit() {
    zx_core::log::info("退出");
    crate::netstat::save(true);
    // 让采样线程收尾：ETW trace session 与 SRUM 实时注册都在 Sampler 的 Drop 里释放，
    // 进程直接结束会把这些留在系统里（SRUM 的实时桶有上限，反复启停会耗尽）。
    if !crate::sampling::shutdown(800) {
        zx_core::log::warn("采样线程未在 800ms 内退出，继续关闭（系统级注册可能残留）");
    }
    // 设置面板是独立进程：关掉它，不然会留一个孤儿窗口
    crate::close_panel();
    crate::detail::close_all();
    overlay::destroy_all();
    crate::tray::destroy();
    unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0); }
}

/// 供详情窗口读取的当前快照。
pub fn snapshot() -> std::sync::Arc<snapshot::Snapshot> { snapshot::latest() }

