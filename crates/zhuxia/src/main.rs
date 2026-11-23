//! 驻匣（Zhuxia）宿主进程。
//!
//! 顺序：DPI → 配置 → 提权闸门 → 单实例 → 首次引导 → 模块启动 → 消息循环。
//! 模块表按"可扩展"的形状摆好：新模块只要在 `modules` 里登记，宿主不关心它内部
//! 怎么画、怎么采样。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]


mod modules;
mod panel;

fn main() {
    // 设置面板：独立进程（关掉即退出，WebView2 的内存全部还给系统）
    if std::env::args().any(|a| a == "--settings") {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        // `--settings <page>`：直达某个标签页（general / appearance / sampling / about）
        if let Some(i) = std::env::args().position(|a| a == "--settings") {
            if let Some(page) = std::env::args().nth(i + 1).filter(|p| !p.starts_with("--")) {
                std::env::set_var("ZHUXIA_SETTINGS_PAGE", page);
            }
        }
        panel::run();
        return;
    }

    // 图片转换：资源管理器右键 / 快捷方式调起的短命进程。
    // 必须放在提权闸门**之前**——给右键菜单配 UAC 弹窗等于废掉这个功能。
    // 常驻进程在跑时这里只负责把任务丢进管道然后退出，转换与提示都在常驻侧完成。
    if std::env::args().any(|a| a == "--img-convert" || a == "--imgconv-menu") {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        let args: Vec<String> = std::env::args().collect();
        if let Some(code) = imgconv::handle_cli(&args) {
            std::process::exit(code);
        }
    }

    // 图床：命令行上传 / 连接测试 / 离线自检。上传不需要管理员，放在提权闸门之前。
    if std::env::args().any(|a| {
        a == "--imgbed" || a == "--imgbed-test" || a == "--imgbed-selftest"
            || a == "--imgbed-token" || a == "--imgbed-token-clear"
            || a == "--imgbed-secret" || a == "--imgbed-dirs" || a == "--imgbed-ls"
            || a == "--imgbed-backends"
    }) {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        let args: Vec<String> = std::env::args().collect();
        if let Some(code) = imgbed::handle_cli(&args) {
            std::process::exit(code);
        }
    }

    // 图标提取：同样是右键/快捷方式调起的短命进程，也必须放在提权闸门之前。
    if std::env::args().any(|a| {
        a == "--appicon" || a == "--appicon-menu" || a == "--appicon-selftest"
    }) {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        let args: Vec<String> = std::env::args().collect();
        if let Some(code) = appicon::handle_cli(&args) {
            std::process::exit(code);
        }
    }

    // 截图 UI 自检：只跑截图模块，建窗上屏 1.2 秒后自动关闭并退出。
    // 用来验证分层窗口的上屏链路（结果看 logs/ 里的 UpdateLayeredWindow 与"截图结束"）。
    if std::env::args().any(|a| a == "--capture-selftest-ui") {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        capture::overlay::register();
        capture::pin::register();
        // 自检也要能看到图床按钮：宿主正常启动时由 modules::install 接线，
        // 这条路径不走模块装载，补一个空钩子（只影响自检进程）。
        capture::set_upload_hook(|| true, |_| {});
        // 允许外部覆盖（截图核对美化面板时要让覆盖层多留几秒）
        if std::env::var("ZHUXIA_CAPTURE_SELFTEST_UI").is_err() {
            std::env::set_var("ZHUXIA_CAPTURE_SELFTEST_UI", "1");
        }
        if !capture::overlay::begin() {
            std::process::exit(1);
        }
        let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1800));
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    tid,
                    windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
        });
        zx_ui::window::run_loop();
        std::process::exit(0);
    }

    // OCR 模型管理（脚本友好，也方便排查网络问题）
    if std::env::args().any(|a| a == "--ocr-model-status") {
        zx_core::log::info(format!("{:?}", capture::ocr_model::status()));
        let st = capture::ocr_model::status();
        std::process::exit(if st.installed { 0 } else { 1 });
    }
    if std::env::args().any(|a| a == "--ocr-model-remove") {
        let _ = capture::ocr_model::remove();
        std::process::exit(0);
    }
    if let Some(i) = std::env::args().position(|a| a == "--ocr-model-import") {
        let dir = std::env::args().nth(i + 1).unwrap_or_default();
        let ok = capture::ocr_model::install_from_dir(std::path::Path::new(&dir));
        zx_core::log::info(format!("导入 OCR 模型：{ok:?}"));
        std::process::exit(if ok.is_ok() { 0 } else { 1 });
    }
    if let Some(i) = std::env::args().position(|a| a == "--ocr-model-download") {
        let src = std::env::args().nth(i + 1).unwrap_or_else(|| "auto".into());
        let (source, custom) = match src.as_str() {
            "hf" => (capture::ocr_model::Source::HuggingFace, String::new()),
            "mirror" => (capture::ocr_model::Source::HfMirror, String::new()),
            s if s.starts_with("http") => (capture::ocr_model::Source::Custom, s.to_string()),
            _ => (capture::ocr_model::Source::Auto, String::new()),
        };
        let ok = capture::ocr_model::download(source, &custom);
        zx_core::log::info(format!("下载 OCR 模型：{ok:?}"));
        std::process::exit(if ok.is_ok() { 0 } else { 1 });
    }

    // 手势诊断：把门控/配置/窗口状态一次打出来（不需要装钩子，也不改配置）
    if std::env::args().any(|a| a == "--gesture-doctor") {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        let report = gesture::doctor();
        println!("{report}");
        std::process::exit(0);
    }

    // 手势覆盖层自检：建全屏分层窗口，画一条折线 + 动作提示，2.5 秒后退出。
    // 不装钩子、不改配置——只验证「识别结果能画到屏幕上」这条链路。
    if std::env::args().any(|a| a == "--gesture-selftest") {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        if !gesture::selftest() {
            zx_core::log::warn("手势自检失败：覆盖层没建起来");
            std::process::exit(1);
        }
        let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2500));
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    tid,
                    windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
        });
        zx_ui::window::run_loop();
        std::process::exit(0);
    }

    // 截图自检：抓一帧 → 合成 → PNG 存盘，不弹窗口、不碰剪贴板。
    // 放在提权闸门之前：它只读屏幕，普通权限就够。
    if std::env::args().any(|a| a == "--capture-selftest") {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        let ok = capture::selftest();
        std::process::exit(if ok { 0 } else { 1 });
    }

    // 录屏自检（都不需要弹窗口）：
    //   --record-selftest-compose  四种布局各出一张 PNG
    //   --record-selftest-gif      20 帧 → GIF，再读回来核对帧数/延迟
    //   --record-selftest-camera   枚举设备 + 抓一帧 PNG
    if let Some(which) = std::env::args().find(|a| a.starts_with("--record-selftest")) {
        zx_ui::window::init_dpi();
        let _ = zx_core::cfg::load();
        let ok = match which.as_str() {
            "--record-selftest-compose" => capture::record::selftest_compose(),
            "--record-selftest-gif" => capture::record::selftest_gif(),
            "--record-selftest-camera" => capture::record::selftest_camera(),
            "--record-selftest-e2e" => capture::record::selftest_e2e(),
            "--record-selftest-audio" => capture::record::selftest_audio(),
            // 不带后缀 = 全跑一遍（摄像头没有设备时不算失败）
            _ => {
                let a = capture::record::selftest_compose();
                let b = capture::record::selftest_gif();
                let _ = capture::record::selftest_camera();
                // 音频只是报告，不算失败条件：没有声卡/麦克风的机器也该能跑完
                let _ = capture::record::selftest_audio();
                a && b
            }
        };
        std::process::exit(if ok { 0 } else { 1 });
    }

    // 脚本/开发便利：`--quit` 让已在运行的实例退出。
    // 必须放在提权闸门**之前**——否则它会先把自己 runas 重启一遍，永远走不到这行。
    if std::env::args().any(|a| a == "--quit") {
        zx_ui::window::init_dpi();
        taskmon::runtime::broadcast_quit();
        zx_core::log::info("已广播退出请求");
        return;
    }

    zx_ui::window::init_dpi();
    let (cfg, fresh) = zx_core::cfg::load();
    zx_core::log::info(format!("启动 v{}（全新安装={fresh}）", env!("CARGO_PKG_VERSION")));
    zx_ui::window::set_apps_dark(!zx_core::win::apps_light());

    // ---- 1. 提权闸门 ----
    // 未提权时：从未同意过 → 问一次；同意过 → 直接 runas 重启自己。
    // 开发调试可用 ZHUXIA_NO_ELEVATE=1 跳过。
    let skip_elevate = std::env::var("ZHUXIA_NO_ELEVATE").ok().as_deref() == Some("1");
    if !zx_core::win::is_admin() && !skip_elevate {
        let consent = cfg.elevation_consent == Some(true);
        let allowed = consent || taskmon::dialogs::consent();
        if !allowed {
            zx_core::log::info("用户拒绝提权，退出");
            return;
        }
        if !consent {
            zx_core::cfg::update_cfg(|c| {
                c.elevation_consent = Some(true);
                // 让提升后的进程还能看到首次引导
                c.startup_guide_completed = Some(false);
            });
        }
        if zx_core::win::relaunch_elevated() {
            zx_core::log::info("已请求提权重启");
            return;
        }
        zx_core::log::warn("提权失败，继续以普通权限运行");
    }

    // ---- 2. 单实例 ----
    if !taskmon::runtime::acquire_single_instance() {
        // 已经有一个在跑：带 --open-settings 就让它把设置面板叫出来，否则提示一下
        if std::env::args().any(|a| a == "--open-settings") {
            taskmon::runtime::signal_open_settings();
            taskmon::runtime::request_open_settings();
        } else {
            taskmon::runtime::signal_running_instance();
            taskmon::dialogs::already_running();
        }
        return;
    }

    // ---- 3. 首次启动引导 ----
    let cfg = zx_core::cfg::cfg();
    if fresh || cfg.startup_guide_completed != Some(true) {
        let pick = taskmon::dialogs::guide();
        // 第二步：要不要接管 Win+V。这属于改系统的默认行为，交给用户决定，
        // 不塞进默认值里（默认不接管，随时能在设置里改）。
        let takeover = taskmon::dialogs::guide_win_v();
        zx_core::cfg::update_cfg(|c| {
            c.floating_mode = Some(pick == 1);
            c.clipboard
                .get_or_insert_with(Default::default)
                .takeover_win_v = Some(takeover);
            c.startup_guide_completed = Some(true);
        });
    }

    // ---- 4. 启动模块 ----
    // 设置面板走 Tauri（按需创建）；起不来就回退到原生设置页
    taskmon::set_panel_opener(panel::open);
    taskmon::set_panel_closer(panel::close);
    // 托盘菜单的「截图 / 录屏」转给 capture（都走 trigger，投递到热键线程执行）
    taskmon::set_action_hook(|op| match op {
        "capture" => capture::trigger(),
        "record" => capture::trigger_record(),
        // 托盘开关：手势用运行期暂停（enabled 只在启动时读一次），回显独立切换。
        "toggle-gesture" => gesture::set_paused(!gesture::paused()),
        "toggle-keyecho" => gesture::set_key_echo_enabled(!gesture::key_echo_enabled()),
        // 图床是设置里的一页，不再是独立窗口：直接开设置并跳过去
        "imgbed" => { let _ = crate::panel::open_page("imgbed"); }
        "imgbed-clip" => imgbed::upload_clipboard(),
        // 开发者工具箱同样是设置里的一页（纯前端计算，没有常驻侧逻辑）
        "devtools" => { let _ = crate::panel::open_page("devtools"); }
        other => zx_core::log::warn(format!("未知的托盘动作：{other}")),
    });
    clipboard::set_settings_opener(|| {
        let _ = panel::open();
    });
    modules::install();
    // 托盘菜单要显示各模块的真实状态，但 taskmon 不直接依赖其它模块；
    // 由宿主提供一个只读探针（每次弹菜单现读，改完立即反映）。
    taskmon::set_tray_state(|| {
        let c = zx_core::cfg::cfg();
        let g = c.gesture();
        taskmon::TrayState {
            gesture_enabled: g.enabled,
            gesture_paused: g.paused,
            key_echo: g.key_echo.enabled,
            imgbed_enabled: c.imgbed().enabled,
            imgbed_configured: imgbed::configured(),
            capture_enabled: c.capture().enabled,
        }
    });
    taskmon::detail::register();
    taskmon::overlay::register();
    taskmon::app::start();
    // 命令行直达设置（也可作为快捷方式的参数）
    if std::env::args().any(|a| a == "--open-settings") {
        taskmon::app::open_settings();
    }
    // 命令行直达开发者工具箱（纯前端页，同样借设置面板的窗口）
    if std::env::args().any(|a| a == "--devtools") {
        let _ = panel::open_page("devtools");
    }
    // 命令行直达剪贴板面板（也可作为快捷方式的参数）
    if std::env::args().any(|a| a == "--clipboard") {
        clipboard::panel::show(true);
    }
    // 命令行直接截一张（也能放进快捷方式/计划任务）
    if std::env::args().any(|a| a == "--capture") {
        capture::trigger();
    }
    // 命令行直接进录屏（框选 → 开录）。必须投递到热键线程，否则会阻塞主消息循环。
    if std::env::args().any(|a| a == "--record") {
        capture::trigger_record();
    }
    // 命令行直达某个详情面板：--detail 0..4（0=CPU 1=内存 2=磁盘 3=GPU 4=网络）
    if let Some(i) = std::env::args().position(|a| a == "--detail") {
        if let Some(slot) = std::env::args().nth(i + 1).and_then(|v| v.parse::<i32>().ok()) {
            match taskmon::overlay::first_hwnd() {
                Some(h) => {
                    zx_core::log::info(format!("命令行 --detail {slot} → 覆盖层 {h:?}"));
                    taskmon::app::toggle_detail(slot, h);
                }
                None => zx_core::log::warn("命令行 --detail：还没有覆盖层窗口"),
            }
        }
    }

    // ---- 5. 消息循环 ----
    zx_ui::window::run_loop();
    zx_core::log::info("已退出");
}
