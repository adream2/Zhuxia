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

mod state;
mod traffic;
mod imgconv;
mod appicon;
mod imgbed;
mod capture;
mod clipboard;
mod gesture;

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
            state::get_state, state::patch, state::set_autostart, state::open_logs,
            state::open_url, state::quit_app,
            traffic::net_state, traffic::set_firewall, traffic::reset_net_stats,
            clipboard::clipboard_patch, clipboard::clipboard_state, clipboard::clipboard_clear, clipboard::clipboard_open_dir,
            capture::capture_patch, capture::capture_open_dir, capture::ocr_ai_probe, capture::capture_ai_key,
            capture::uielem_reset, capture::record_patch, capture::record_devices,
            capture::record_open_dir, capture::record_audio_devices, capture::ocr_model_state, capture::ocr_model_progress,
            capture::ocr_model_download, capture::ocr_model_import, capture::ocr_model_remove,
            gesture::gesture_patch,
            imgconv::imgconv_patch, imgconv::imgconv_menu, imgconv::imgconv_pick, imgconv::imgconv_run,
            appicon::appicon_patch, appicon::appicon_menu, appicon::appicon_pick, appicon::appicon_run,
            imgbed::imgbed_state, imgbed::imgbed_patch, imgbed::imgbed_provider_patch, imgbed::imgbed_secret_set,
            imgbed::imgbed_secret_clear, imgbed::imgbed_test, imgbed::imgbed_upload_test, imgbed::ib_pick_files,
            imgbed::ib_preview, imgbed::ib_upload_paths, imgbed::ib_upload_b64, imgbed::ib_clipboard,
            imgbed::ib_paths, imgbed::ib_history, imgbed::ib_remote, imgbed::ib_thumb,
            imgbed::ib_thumb_url, imgbed::ib_image, imgbed::ib_delete, imgbed::ib_delete_remote,
            imgbed::ib_clear_history, imgbed::ib_copy, imgbed::ib_open, imgbed::ib_open_dir,
            open_folder,
            copy_text,
            take_page_request
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

fn shell_open(path: &str) { zx_core::win::shell_open(path); }

// ---------------- 状态 ----------------

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

/// 复制任意文本（关于页的「复制诊断信息」用）。
#[tauri::command]
fn copy_text(text: String) -> Value {
    json!({ "ok": ::imgbed::clip::set_text(&text) })
}

/// 写配置前先对齐磁盘：面板与常驻进程（托盘）都会写同一份 settings.yaml。
fn begin_patch() -> zx_core::Settings {
    zx_core::cfg::reload();
    zx_core::cfg::cfg()
}

/// 把"上次落盘失败"的原因挂到返回的状态对象上（有才挂）。
///
/// 前端拿到的还是原来的结构，只多一个可选的 `saveError`；没有失败时完全不变，
/// 所以老的调用方不用改。取走即清空，避免同一个错误反复弹提示。
fn with_save_error(mut v: Value) -> Value {
    if let Some(e) = zx_core::cfg::take_save_error() {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("saveError".to_string(), Value::String(e));
        }
    }
    v
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
    with_save_error(view())
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
