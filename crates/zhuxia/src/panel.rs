//! Tauri 2 设置面板（**独立进程**）。
//!
//! 常驻进程只负责 `open()`：另起一个 `zhuxia.exe --settings`。面板关掉即进程退出，
//! WebView2 的内存（实测 6 个进程、约 160MB 私有）随之全部还给系统——
//! 常驻部分依旧只有 12MB 左右。
//!
//! 面板进程改完 `settings.yaml` 后广播一条 `WM_APP_RELOAD`；常驻进程收到就重读配置。
//! 两边权限不一致（广播被 UIPI 拦）时，常驻侧还有 1 秒一次的 mtime 轮询兜底。

use serde_json::{json, Value};
use tauri::Manager;

const WINDOW: &str = "settings";
const WINDOW_TITLE: &str = "驻匣 · 设置";

/// 常驻进程：打开设置面板。
///
/// 已经有面板就把它提到前台，不再拉第二个进程——以前每点一次"设置"就多一个
/// `--settings`（每个 ~40MB，还共享一条 WebView2 浏览器进程），而 `close()` 只关得掉
/// 一个，开三次就漏两个在后台。
pub fn open() -> bool {
    open_page("")
}

/// 打开设置面板并跳到某一页（托盘「图床…」这类入口用）。
///
/// 图床不是独立窗口了：它就是设置里的一页，所以这里只有两种行为——
/// 面板已经在，就写一个"跳页请求"让它自己切过去；不在，就带页参数拉起进程。
pub fn open_page(page: &str) -> bool {
    if let Some(h) = find_panel() {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE};
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
        if !page.is_empty() {
            request_page(page);
        }
        zx_core::log::info(format!("设置面板已存在，提到前台（请求页：{page}）"));
        return true;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--settings");
    if !page.is_empty() {
        cmd.arg(page);
    }
    match cmd.spawn() {
        Ok(child) => {
            zx_core::log::info(format!("已拉起设置面板进程 pid={}", child.id()));
            true
        }
        Err(e) => {
            zx_core::log::warn(format!("启动设置面板进程失败：{e}"));
            false
        }
    }
}

/// 面板已经在跑时，用一个文件把"要跳到哪页"递过去（面板每 1.2 秒取一次）。
fn request_page(page: &str) {
    let path = zx_core::cfg::run_dir().join("panel-page.txt");
    if let Err(e) = std::fs::write(&path, page) {
        zx_core::log::warn(format!("跳页请求写入失败：{e}"));
    }
}

#[tauri::command]
fn take_page_request() -> String {
    let path = zx_core::cfg::run_dir().join("panel-page.txt");
    let Ok(text) = std::fs::read_to_string(&path) else { return String::new() };
    let _ = std::fs::remove_file(&path);
    text.trim().to_string()
}

fn find_panel() -> Option<windows::Win32::Foundation::HWND> {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        let title = zx_core::win::wide(WINDOW_TITLE);
        match FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
            Ok(h) if !h.is_invalid() => Some(h),
            _ => None,
        }
    }
}

/// 常驻进程侧：把已经打开的面板窗口全关掉（面板是独立进程，不会自己知道主人退出了）。
///
/// 按窗口标题枚举：面板只有一个窗口，标题唯一。发 `WM_CLOSE` 后 Tauri 关掉最后一个
/// 窗口，事件循环自然返回、进程退出。**枚举而不是 `FindWindow`**：后者只关一个，
/// 多个残留面板会一直留在后台。
pub fn close() {
    struct Ctx {
        title: Vec<u16>,
        closed: usize,
    }

    unsafe extern "system" fn close_enum(
        hwnd: windows::Win32::Foundation::HWND,
        lp: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, PostMessageW, WM_CLOSE};
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut buf = [0u16; 128];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n > 0 && buf[..n as usize] == ctx.title[..] {
            let _ = PostMessageW(
                hwnd,
                WM_CLOSE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            );
            ctx.closed += 1;
        }
        windows::Win32::Foundation::BOOL(1)
    }

    let mut ctx = Ctx {
        title: WINDOW_TITLE.encode_utf16().collect(),
        closed: 0,
    };
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(close_enum),
            windows::Win32::Foundation::LPARAM(&mut ctx as *mut Ctx as isize),
        );
    }
    if ctx.closed > 0 {
        zx_core::log::info(format!("关闭设置面板窗口 {} 个", ctx.closed));
    }
}

/// 面板进程：主线程跑 Tauri 事件循环，窗口关掉即返回。
pub fn run() {
    let ctx = tauri::generate_context!();
    let built = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_state, patch, set_autostart, open_logs, open_url, quit_app,
            net_state, set_firewall, reset_net_stats,
            clipboard_patch, clipboard_state, clipboard_clear, clipboard_open_dir,
            capture_patch, capture_open_dir,
            record_patch, record_devices, record_open_dir, record_audio_devices,
            gesture_patch,
            imgconv_patch, imgconv_menu,
            imgconv_pick, imgconv_run,
            appicon_patch, appicon_menu,
            appicon_pick, appicon_run, open_folder,
            imgbed_state, imgbed_patch, imgbed_provider_patch,
            imgbed_secret_set, imgbed_secret_clear, imgbed_test, imgbed_upload_test,
            ib_pick_files, ib_preview, ib_upload_paths, ib_upload_b64, ib_clipboard,
            ib_paths, ib_history, ib_remote, ib_thumb, ib_thumb_url, ib_image,
            ib_delete, ib_delete_remote, ib_clear_history, ib_copy, ib_open, ib_open_dir,
            copy_text,
            take_page_request,
            ocr_model_state, ocr_model_progress, ocr_model_download, ocr_model_import, ocr_model_remove
        ])
        .setup(|app| {
            start_owner_watchdog();
            if let Some(w) = ensure_window(app.handle()) {
                // 无边框窗口默认没有 hIcon，任务栏会显示"缺省应用"图标。
                // 从 exe 资源里取大/小两套设上去。
                if let Ok(h) = w.hwnd() {
                    // tauri 用的是另一份 windows crate，HWND 不是同一个类型：取裸指针再包一层
                    zx_ui::window::set_window_icon(
                        windows::Win32::Foundation::HWND(h.0));
                }
            }
            Ok(())
        })
        // 关最后一个窗口就把整个进程退掉。
        //
        // 以前前端 ✕ 调的是 `hide()`，窗口只是藏起来，面板进程 + 6 个 WebView2
        // 子进程会一直活着（约 160MB 私有内存），和"关掉即归零"的设计相反。
        // 这里做兜底：不管前端怎么关（✕ / Alt+F4 / 脚本），CloseRequested 都直接退出。
        .on_window_event(|_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                std::process::exit(0);
            }
        })
        .build(ctx);
    match built {
        Ok(app) => app.run(|_, _| {}),
        Err(e) => {
            zx_core::log::warn(format!("设置面板初始化失败：{e}"));
            // 面板进程自己就是"面板"，这里无法再拉一次；直接把提示交给宿主
            // （下一次 open_settings 会走 settings_fallback）
            taskmon::dialogs::card("设置面板初始化失败",
                &["这通常是因为缺少 WebView2 运行时。",
                  "装一下 Microsoft Edge WebView2 Runtime 后重试。"],
                &["知道了"]);
        }
    }
}

/// 常驻进程没了就自己退出。
///
/// 面板是独立进程，托盘菜单点"退出"时 `WM_CLOSE` 可能来不及送到（或用户从别处杀掉常驻）。
/// 这里每 2 秒查一次单实例互斥体，连续 3 次打不开就认为主人已经走了。
fn start_owner_watchdog() {
    // 调试/截图用：没有常驻进程也要把面板留住（`ZHUXIA_PANEL_NO_WATCHDOG=1`）。
    if std::env::var("ZHUXIA_PANEL_NO_WATCHDOG").ok().as_deref() == Some("1") {
        zx_core::log::info("设置面板：watchdog 已按环境变量关闭");
        return;
    }
    std::thread::Builder::new().name("zx-panel-watch".into()).spawn(|| {
        // 启动后先给常驻一点时间（--settings 是常驻自己拉起来的，互斥体一定在）
        let mut misses = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if taskmon::runtime::instance_alive() {
                misses = 0;
            } else {
                misses += 1;
                if misses >= 3 {
                    zx_core::log::info("常驻进程已退出，设置面板跟随关闭");
                    std::process::exit(0);
                }
            }
        }
    }).ok();
}

fn ensure_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    if let Some(w) = app.get_webview_window(WINDOW) { return Some(w); }
    // 支持 `--settings appearance` / `--settings sampling` 直达某一页
    let url = match std::env::var("ZHUXIA_SETTINGS_PAGE").ok().filter(|p| !p.is_empty()) {
        Some(page) => format!("index.html?page={page}"),
        None => "index.html".to_string(),
    };
    // 截图/调试用：临时把窗口开高，一屏放下整页（`ZHUXIA_PANEL_SIZE=1000x1300`）
    let (dw, dh) = std::env::var("ZHUXIA_PANEL_SIZE")
        .ok()
        .and_then(|v| {
            let mut it = v.split('x');
            Some((it.next()?.trim().parse::<f64>().ok()?, it.next()?.trim().parse::<f64>().ok()?))
        })
        .unwrap_or((1000.0, 680.0));
    let mut b = tauri::WebviewWindowBuilder::new(app, WINDOW, tauri::WebviewUrl::App(url.into()))
        .title("驻匣 · 设置")
        .inner_size(dw, dh)
        .min_inner_size(820.0, 560.0)
        .decorations(false)
        .center()
        // WebView2 的缓存/配置放到 exe 同目录：卸载就是删目录，不污染 %LOCALAPPDATA%；
        // 面板没有任何 localStorage 依赖，迁移是安全的。
        .data_directory(zx_core::cfg::run_dir().join("webview2"))
        // 面板是纯本地页，不需要后台网络、组件更新、SmartScreen 与账号按钮。
        // 少几个常驻辅助进程，浏览器进程基线也低一截（预期 20–50 MB）。
        // 注意：刻意不传 --disable-gpu，滚动与渐变预览的观感优先；
        // 也不把 --js-flags 的老生代压太小，开发者工具箱要处理大 JSON。
        .additional_browser_args(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,msEdgeIdentityButton \
             --disable-background-networking --disable-component-update --disable-sync \
             --no-first-run --renderer-process-limit=1 --js-flags=--max-old-space-size=256",
        );
    // 截图/调试用：常驻没跑时也能把面板摁在最前面（`ZHUXIA_PANEL_TOPMOST=1`）
    if std::env::var("ZHUXIA_PANEL_TOPMOST").ok().as_deref() == Some("1") {
        b = b.always_on_top(true);
    }
    b.build().ok()
}

// ---------------- 命令 ----------------

#[tauri::command]
fn get_state() -> Value {
    zx_core::cfg::reload(); // 常驻进程可能刚改过，读最新的
    state_json()
}

#[tauri::command]
fn patch(patch: Value) -> Value {
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
    state_json()
}

#[tauri::command]
fn set_autostart(on: bool) -> Value {
    if on { let _ = taskmon::autostart::enable(); } else { let _ = taskmon::autostart::disable(); }
    taskmon::autostart::refresh();
    state_json()
}

#[tauri::command]
fn open_logs() {
    let dir = zx_core::cfg::run_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

#[tauri::command]
fn open_url(url: String) {
    if url.starts_with("https://") { shell_open(&url); }
}

#[tauri::command]
fn quit_app() {
    // 通知常驻进程退出，然后面板自己也退出
    taskmon::runtime::broadcast_quit();
    std::thread::sleep(std::time::Duration::from_millis(120));
    std::process::exit(0);
}

fn shell_open(path: &str) { zx_core::win::shell_open(path); }

// ---------------- 状态 ----------------

/// 流量页的数据：常驻进程每 5 秒写进 `runtime.json` 的 `net` 段。
///
/// 面板是独立进程，自己没有采样线程，所以这里只做"读缓存 + 报告状态"。
#[tauri::command]
fn net_state() -> Value {
    let rt = read_runtime();
    json!({
        "net": rt.get("net").cloned().unwrap_or_else(|| json!({ "flows": [] })),
        // 常驻没在跑的时候，数据是旧的，界面要说清楚
        "alive": taskmon::runtime::instance_alive(),
        "admin": taskmon::win_admin(),
    })
}

/// 加/解除某个程序的出站阻止。netsh 要几百毫秒，前端应当先置"处理中"。
#[tauri::command]
fn set_firewall(path: String, block: bool) -> Value {
    // 只下规则：面板进程没有采样数据，走完整版会把 netstat.json 覆盖成空
    let ok = taskmon::netstat::set_blocked_rule(&path, block);
    json!({ "ok": ok, "admin": taskmon::win_admin() })
}

/// 清空历史统计：面板进程清不了常驻进程的内存，写哨兵让它自己清。
#[tauri::command]
fn reset_net_stats() -> Value {
    taskmon::netstat::request_reset();
    json!({ "ok": true })
}

fn read_runtime() -> Value {
    std::fs::read_to_string(zx_core::cfg::run_dir().join("runtime.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}))
}

fn state_json() -> Value {
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

/// 图片转换那一段的视图（含右键菜单是否在册）。
fn imgconv_view() -> Value {
    let c = zx_core::cfg::cfg().imgconv();
    json!({
        "enabled": c.enabled,
        "quality": c.quality,
        "webpLossless": c.webp_lossless,
        "overwrite": c.overwrite,
        "outDir": c.out_dir.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "installed": imgconv::menu_installed(),
        "icoSizes": c.ico_sizes,
        "icoOptions": [16, 24, 32, 48, 64, 128, 256],
        "targets": imgconv::fmt::ALL.iter().map(|t| t.label()).collect::<Vec<_>>(),
    })
}

/// 只改 `imgconv` 段；启用状态变化时顺手装/卸右键菜单。
#[tauri::command]
fn imgconv_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.imgconv, patch, imgconv_view, || {
        // 菜单是注册表状态，不跟配置自动同步：这里按最新配置对齐一次。
        let on = zx_core::cfg::cfg().imgconv().enabled;
        if on && !imgconv::menu_installed() {
            let _ = imgconv::install_menu();
        } else if !on && imgconv::menu_installed() {
            let _ = imgconv::remove_menu();
        }
    })
}

/// 手动重写/移除菜单。
#[tauri::command]
fn imgconv_menu(on: bool) -> Value {
    if on {
        let _ = imgconv::install_menu();
    } else {
        let _ = imgconv::remove_menu();
    }
    imgconv_view()
}

/// 操作页：挑图片（可多选）。
#[tauri::command]
fn imgconv_pick() -> Value {
    let paths = pick_files(
        "图片\0*.png;*.jpg;*.jpeg;*.jpe;*.jfif;*.gif;*.bmp;*.dib;*.tif;*.tiff;*.ico;*.webp;*.svg;*.svgz;*.avif;*.heic;*.heif\0所有文件\0*.*\0\0",
        "选择要转换的图片",
        true,
    );
    json!({ "paths": paths })
}

/// 操作页：按目标格式批量转换，返回逐条结果。
///
/// 走的是和右键菜单同一个 `convert_batch`，所以质量/覆盖/输出目录这些设置
/// 两边完全一致——设置页里改什么，这里就按什么转。
#[tauri::command]
async fn imgconv_run(paths: Vec<String>, target: String) -> Value {
    let Some(fmt) = imgconv::Target::parse(&target) else {
        return json!({ "items": [], "error": format!("不认识的目标格式：{target}") });
    };
    let files: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let results = tauri::async_runtime::spawn_blocking(move || imgconv::convert_batch(&files, fmt))
        .await
        .unwrap_or_default();
    let mut ok = 0usize;
    let items: Vec<Value> = results
        .into_iter()
        .map(|(src, res)| match res {
            Ok(o) => {
                ok += 1;
                json!({
                    "ok": true,
                    "source": src.to_string_lossy(),
                    "output": o.output.to_string_lossy(),
                    "name": o.output.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "srcBytes": o.src_bytes,
                    "outBytes": o.out_bytes,
                    "width": o.w,
                    "height": o.h,
                    "ms": o.ms,
                    "notice": o.notice,
                })
            }
            Err(e) => json!({
                "ok": false,
                "source": src.to_string_lossy(),
                "error": e.reason(),
                "detail": e.detail().unwrap_or(""),
            }),
        })
        .collect();
    json!({ "items": items, "ok": ok })
}

// ---------------- 图标提取 ----------------

fn appicon_view() -> Value {
    let c = zx_core::cfg::cfg().appicon();
    json!({
        "enabled": c.enabled,
        "mode": c.mode,
        "maxSize": c.max_size,
        "icoSizes": c.ico_sizes,
        "icoOptions": [16, 24, 32, 48, 64, 128, 256],
        "outDir": c.out_dir.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "overwrite": c.overwrite,
        "installed": appicon::menu_installed(),
        "modes": [
            {"id": "png", "label": "PNG 图片"},
            {"id": "ico", "label": "ICO 多尺寸"},
            {"id": "all", "label": "全部尺寸"}
        ],
        "exts": appicon::menu::EXTS.len(),
        "formats": [
            "Windows：exe / dll / ocx / cpl / scr / sys / mui / lnk / msi / msix / appx",
            "Android：apk / aab",
            "Apple：app / dmg / icns / ipa",
            "Linux：deb / rpm / AppImage"
        ]
    })
}

#[tauri::command]
fn appicon_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.appicon, patch, appicon_view, || {
        let on = zx_core::cfg::cfg().appicon().enabled;
        if on && !appicon::menu_installed() {
            let _ = appicon::install_menu();
        } else if !on && appicon::menu_installed() {
            let _ = appicon::remove_menu();
        }
    })
}

#[tauri::command]
fn appicon_menu(on: bool) -> Value {
    if on {
        let _ = appicon::install_menu();
    } else {
        let _ = appicon::remove_menu();
    }
    appicon_view()
}

/// 操作页：挑程序 / 安装包 / 图标文件（可多选）。
#[tauri::command]
fn appicon_pick() -> Value {
    let paths = pick_files(
        "程序与安装包\0*.exe;*.dll;*.ocx;*.cpl;*.scr;*.sys;*.mui;*.lnk;*.msi;*.msix;*.appx;*.apk;*.aab;*.ipa;*.app;*.dmg;*.icns;*.deb;*.rpm;*.AppImage;*.ico\0所有文件\0*.*\0\0",
        "选择要提取图标的文件",
        true,
    );
    json!({ "paths": paths })
}

/// 操作页：按形态批量提取图标，返回逐条结果。
#[tauri::command]
async fn appicon_run(paths: Vec<String>, mode: String) -> Value {
    let Some(m) = appicon::Mode::parse(&mode) else {
        return json!({ "items": [], "error": format!("不认识的输出形态：{mode}") });
    };
    let files: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let results = tauri::async_runtime::spawn_blocking(move || appicon::extract_batch(&files, m))
        .await
        .unwrap_or_default();
    let mut ok = 0usize;
    let items: Vec<Value> = results
        .into_iter()
        .map(|(src, res)| match res {
            Ok(o) => {
                ok += 1;
                json!({
                    "ok": true,
                    "source": src.to_string_lossy(),
                    "outputs": o.outputs.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
                    "name": o.outputs.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "detail": o.detail,
                    "frames": o.frames,
                    "largest": [o.largest.0, o.largest.1],
                    "srcBytes": o.src_bytes,
                    "ms": o.ms,
                    "notice": o.notice,
                })
            }
            Err(e) => json!({
                "ok": false,
                "source": src.to_string_lossy(),
                "error": e.reason(),
                "detail": e.detail().unwrap_or(""),
            }),
        })
        .collect();
    json!({ "items": items, "ok": ok })
}

/// 在资源管理器里打开一个目录（结果列表的「打开输出目录」）。
///
/// 只接受存在的目录，别的一律拒绝：这个口子只用来"看结果"，不做通用外壳。
#[tauri::command]
fn open_folder(path: String) -> Value {
    let p = std::path::PathBuf::from(path.trim());
    if p.is_dir() {
        zx_core::win::shell_open(&p.to_string_lossy());
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "目录不存在" })
    }
}

// ---------------- 图床（多平台 / 三页窗口） ----------------

fn imgbed_view() -> Value {
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
fn imgbed_state() -> Value {
    imgbed_view()
}

/// 只改全局段（upload / copy / browser / provider / hotkey / proxy / enabled）。
#[tauri::command]
fn imgbed_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.imgbed, patch, imgbed_view, || {})
}

/// 只改某个平台的字段（providers.<id> 是通用 map）。
#[tauri::command]
fn imgbed_provider_patch(provider: String, patch: Value) -> Value {
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
fn secret_allowed(key: &str) -> bool {
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
fn imgbed_secret_set(key: String, value: String) -> Value {
    if !secret_allowed(&key) {
        return json!({ "ok": false, "error": format!("不认识的密钥字段：{key}") });
    }
    match imgbed::set_secret(&key, &value) {
        Ok(()) => json!({ "ok": true, "state": imgbed_view() }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
fn imgbed_secret_clear(key: String) -> Value {
    if !secret_allowed(&key) {
        return json!({ "ok": false, "error": format!("不认识的密钥字段：{key}") });
    }
    match imgbed::clear_secret(&key) {
        Ok(()) => json!({ "ok": true, "state": imgbed_view() }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
fn imgbed_test() -> Value {
    match imgbed::test() {
        Ok(lines) => json!({ "ok": true, "lines": lines }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
async fn imgbed_upload_test() -> Value {
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
fn ib_pick_files() -> Value {
    let paths = pick_files(
        "图片\0*.png;*.jpg;*.jpeg;*.gif;*.webp;*.bmp;*.tif;*.tiff;*.svg;*.avif\0所有文件\0*.*\0\0",
        "选择要上传的图片",
        true,
    );
    json!({ "paths": paths })
}

/// 原生文件对话框（`filter` 用 NUL 分隔的「说明\0模式\0…\0\0」格式）。
///
/// 图床、图片转换、图标提取三个页面共用这一段：它们只是筛选器和标题不同，
/// 多文件解析（文件名 + 后续文件名列表）的细节没必要抄三遍。
fn pick_files(filter: &str, title: &str, multiselect: bool) -> Vec<String> {
    use windows::Win32::UI::Controls::Dialogs::*;
    let filter: Vec<u16> = filter.encode_utf16().collect();
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 64 * 1024];
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: windows::core::PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | if multiselect { OFN_ALLOWMULTISELECT } else { OPEN_FILENAME_FLAGS(0) },
        ..Default::default()
    };
    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    if !ok.as_bool() {
        return Vec::new();
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let first = String::from_utf16_lossy(&buf[..end]);
    let mut rest: Vec<String> = Vec::new();
    let mut i = end + 1;
    while i < buf.len() {
        let stop = match buf[i..].iter().position(|&c| c == 0) {
            Some(n) => i + n,
            None => break,
        };
        if stop == i {
            break;
        }
        rest.push(String::from_utf16_lossy(&buf[i..stop]));
        i = stop + 1;
    }
    if rest.is_empty() {
        vec![first]
    } else {
        rest.iter().map(|f| format!("{first}\\{f}")).collect()
    }
}

/// 读文件生成 160px 预览（WebP data URL）。大图不整张塞进 IPC。
#[tauri::command]
fn ib_preview(path: String) -> Value {
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
async fn ib_upload_paths(paths: Vec<String>, dir: String) -> Value {
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
async fn ib_upload_b64(name: String, data: String, dir: String) -> Value {
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.split(',').last().unwrap_or("")) else {
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
fn ib_clipboard() -> Value {
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
fn ib_paths(dir: String) -> Value {
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
fn ib_history(q: String, provider: String, sort: String, offset: usize, limit: usize) -> Value {
    let (items, total) = imgbed::history_query(&q, &provider, &sort, offset, limit);
    json!({ "items": items, "total": total })
}

#[tauri::command]
fn ib_remote(dir: String) -> Value {
    let supported = imgbed::metas()
        .into_iter()
        .find(|m| m.id == zx_core::cfg::cfg().imgbed().provider)
        .map(|m| m.caps.list_images)
        .unwrap_or(false);
    if !supported {
        return json!({ "supported": false, "items": [] });
    }
    match imgbed::remote_images(dir.trim()) {
        Ok(items) => json!({ "supported": true, "items": items }),
        Err(e) => json!({ "supported": true, "items": [], "error": e }),
    }
}

#[tauri::command]
fn ib_thumb(id: String) -> Value {
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
fn ib_thumb_url(url: String) -> Value {
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
fn ib_image(url: String) -> Value {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return json!({ "ok": false, "error": "不是 http/https 链接" });
    }
    match imgbed::image_bytes(&url) {
        Ok(bytes) if bytes.len() <= 30 * 1024 * 1024 => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            json!({ "ok": true, "dataUrl": format!("data:{};base64,{b64}", imgbed::path::kind_of(&bytes).map(|(_, m)| m).unwrap_or("application/octet-stream")) })
        }
        Ok(_) => json!({ "ok": false, "error": "原图超过 30 MB，请用「打开原图」" }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
fn ib_delete(id: String) -> Value {
    json!({ "ok": imgbed::history_remove(&id) })
}

#[tauri::command]
fn ib_delete_remote(path: String) -> Value {
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
fn ib_clear_history() -> Value {
    imgbed::history_clear();
    json!({ "ok": true })
}

#[tauri::command]
fn ib_copy(text: String) -> Value {
    json!({ "ok": imgbed::clip::set_text(&text) })
}

/// 复制任意文本（关于页的「复制诊断信息」用）。
#[tauri::command]
fn copy_text(text: String) -> Value {
    json!({ "ok": imgbed::clip::set_text(&text) })
}

#[tauri::command]
fn ib_open(url: String) -> Value {
    if url.starts_with("https://") || url.starts_with("http://") {
        zx_core::win::shell_open(&url);
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "只允许打开 http/https 链接" })
    }
}

#[tauri::command]
fn ib_open_dir() -> Value {
    let dir = zx_core::cfg::run_dir().join("imgbed");
    let _ = std::fs::create_dir_all(&dir);
    zx_core::win::shell_open(&dir.to_string_lossy());
    json!({ "ok": true })
}

// ---------------- 截图 ----------------

/// 只改截图那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
fn capture_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.capture, patch, capture_view, || {})
}

#[tauri::command]
fn capture_open_dir() {
    let dir = zx_core::cfg::cfg().capture().save_dir;
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

// ---------------- 录屏（GIF + 摄像头合成） ----------------

/// 只改 `capture.record` 那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
fn record_patch(patch: Value) -> Value {
    // record 是 capture 的嵌套段：访问器顺手把父段补出来。
    patch_section(|s| &mut s.capture.get_or_insert_with(Default::default).record, patch, record_view, || {})
}

/// 摄像头列表只能在常驻进程里问（MF 要 `MFStartup`，两个进程同时枚举会抢设备）。
///
/// 面板进程自己调一次也可以（OpenCV 不需要），但那样每次开设置页都要等几百毫秒，
/// 而且要处理"常驻正在录"的冲突。这里走一条独立枚举，失败就给空列表 + 提示。
#[tauri::command]
fn record_devices() -> Value {
    let list = capture::record::camera::list();
    json!({
        "devices": list.iter().map(|d| d.name.clone()).collect::<Vec<_>>(),
        "ok": !list.is_empty(),
    })
}

#[tauri::command]
fn record_open_dir() {
    let dir = zx_core::cfg::cfg().capture().save_dir;
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

fn record_view() -> Value {
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
fn record_audio_devices() -> Value {
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
fn ocr_model_state() -> Value {
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
fn ocr_model_progress() -> Value {
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
fn ocr_model_download(source: String, custom: String) -> Value {
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
fn ocr_model_import(dir: String) -> Value {
    let r = capture::ocr_model::install_from_dir(std::path::Path::new(&dir));
    json!({ "ok": r.is_ok(), "error": r.err() })
}

#[tauri::command]
fn ocr_model_remove() -> Value {
    let _ = capture::ocr_model::remove();
    json!({ "ok": true })
}

/// 常驻进程把热键状态写在 exe 目录，设置页（另一个进程）读它。
fn hotkey_state() -> (String, String) {
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

fn capture_view() -> Value {
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

/// 只改剪贴板那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
fn clipboard_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.clipboard, patch, clip_view, || {})
}

/// 历史统计：面板进程只读 `clipboard/history.dat`（DPAPI 密文），不去碰常驻进程的存储实例。
#[tauri::command]
fn clipboard_state() -> Value {
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
fn clipboard_clear(keep_pinned: bool) -> Value {
    let dir = zx_core::cfg::clipboard_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("clear.flag"),
        if keep_pinned { "keep_pinned" } else { "all" },
    );
    json!({ "ok": true })
}

#[tauri::command]
fn clipboard_open_dir() {
    let dir = zx_core::cfg::clipboard_dir();
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir.to_string_lossy());
}

/// 设置页要展示的剪贴板配置（全部是生效值，不是 Option）。
fn clip_view() -> Value {
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

/// 只改手势那一段，且只写用户改过的键（保持 settings.yaml 稀疏）。
#[tauri::command]
fn gesture_patch(patch: Value) -> Value {
    patch_section(|s| &mut s.gesture, patch, gesture_view, || {})
}

/// 常驻进程每 5 秒写的运行状态（面板进程看不到钩子/覆盖层）。
struct GestureRuntime {
    hook: bool,
    overlay: bool,
    rules: usize,
    groups: usize,
}

fn gesture_runtime() -> GestureRuntime {
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

fn gesture_view() -> Value {
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

fn rule_view(r: &gesture::model::Rule) -> Value {
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

fn color_hex(argb: u32) -> String {
    format!("#{:06X}", argb & 0x00FF_FFFF)
}

/// 虚拟键码 → 界面上看得懂的名字（只覆盖常用键，其余给十六进制）。
fn vk_name(vk: u16) -> String {
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

fn mode_str(m: zx_core::MetricDisplayMode) -> &'static str {    match m {
        zx_core::MetricDisplayMode::Average => "average",
        zx_core::MetricDisplayMode::Max => "max",
        zx_core::MetricDisplayMode::Specific => "specific",
    }
}

/// 写配置前先对齐磁盘：面板与常驻进程（托盘）都会写同一份 settings.yaml。
fn begin_patch() -> zx_core::Settings {
    zx_core::cfg::reload();
    zx_core::cfg::cfg()
}

/// 写一段配置并返回该模块的视图。
///
/// 所有 `*_patch` 共用这一条路径：读盘对齐 → 合并补丁 → 反序列化 → 整份落盘 →
/// 广播重载。模板代码只留在这里，各模块只提供一个槽位访问器和一个视图函数。
fn patch_section<T, G>(get: G, patch: Value, view: fn() -> Value, after: impl FnOnce()) -> Value
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    G: FnOnce(&mut zx_core::Settings) -> &mut Option<T>,
{
    let mut s = begin_patch();
    let mut changed = false;
    {
        let slot = get(&mut s);
        // 只序列化当前段（None → null），不整包克隆 Settings。
        let mut base = serde_json::to_value(slot.as_ref()).unwrap_or(Value::Null);
        if !base.is_object() {
            base = json!({});
        }
        merge(&mut base, &patch);
        match serde_json::from_value::<T>(base) {
            Ok(next) => {
                *slot = Some(next);
                changed = true;
            }
            Err(e) => zx_core::log::warn(format!("配置写回失败：{e}")),
        }
    }
    if changed {
        zx_core::cfg::set(s);
        taskmon::runtime::broadcast_reload();
        after();
    }
    view()
}

fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) if v.is_object() && slot.is_object() => merge(slot, v),
                    _ => { b.insert(k.clone(), v.clone()); }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_map_merge_survives_json_roundtrip() {
        let mut c = zx_core::ImgBedCfg::default();
        let mut base = json!({});
        merge(&mut base, &json!({ "owner": "me", "repo": "imgs", "pathStyle": true }));
        let v: serde_yaml::Value = serde_json::from_value(base).unwrap();
        c.providers.insert("s3".into(), v);
        let back: serde_json::Value =
            serde_json::to_value(c.providers.get("s3").unwrap()).unwrap();
        assert_eq!(back["owner"], "me");
        assert_eq!(back["pathStyle"], true);
    }

    #[test]
    fn root_patch_keeps_types() {
        let mut base = serde_json::to_value(zx_core::ImgBedCfg::default()).unwrap();
        merge(&mut base, &json!({ "upload": { "quality": 90, "convert": "keep" }, "provider": "s3" }));
        let c: zx_core::ImgBedCfg = serde_json::from_value(base).unwrap();
        assert_eq!(c.upload.quality, Some(90));
        assert_eq!(c.upload.convert.as_deref(), Some("keep"));
        assert_eq!(c.provider.as_deref(), Some("s3"));
    }
}
