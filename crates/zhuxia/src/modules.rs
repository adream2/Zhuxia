//! 模块表：驻匣的扩展点。
//!
//! 每个模块只暴露一个 `start()`，宿主不关心它的窗口、线程与配置键；
//! 模块之间通过 `zx_core::event` 的主题通信，互不直接依赖。

pub fn install() {
    // 模块只在这里登记；宿主不关心它们内部。
    // taskmon 的启动在 main.rs（它要拿覆盖层句柄做 --detail），这里只登记其余模块。
    clipboard::start();
    capture::start();

    // 手势的「内置动作」由宿主接线：gesture 只往外递动作名，不认识 capture/clipboard/panel。
    // 这样 `internal: capture` 的默认规则（↙↘↙ 折线截图）不需要模块之间互相依赖。
    gesture::set_action_hook(|op| {
        // 真机排查用：手势命中内置动作时日志里能直接看到是哪一条。
        zx_core::log::info(format!("手势内置动作：{op}"));
        match op {
            "capture" => {
                // 手势轨迹画在一个盖满虚拟桌面的分层窗口上，GDI 抓屏会把轨迹一起抓下来。
                // 先让轨迹层收起来、确认它不可见了再触发截图；放独立线程里等，
                // 不阻塞手势解析线程（那里每次卡顿都会掉事件）。
                std::thread::Builder::new()
                    .name("zx-gesture-capture".into())
                    .spawn(|| {
                        gesture::overlay::hide_blocking(150);
                        capture::trigger();
                    })
                    .ok();
            }
            "clipboard" => clipboard::panel::show(true),
            "imgbed" => { let _ = crate::panel::open_page("imgbed"); }
            "devtools" => { let _ = crate::panel::open_page("devtools"); }
            "settings" => {
                let _ = crate::panel::open();
            }
            other => zx_core::log::warn(format!("未知的手势内置动作：{other}")),
        }
    });
    gesture::start();

    // 图片转换：起右键菜单 + 管道队列。它只暴露 start()，宿主不认识内部。
    imgconv::start();
    // 图标提取：同样是右键菜单 + 管道队列，边界一致。
    appicon::start();

    // 图床：截图模块只认两个闭包（现在能不能传 / 收 PNG），不认识 imgbed。
    // 「能不能传」每次都现读配置，所以启停图床不需要重启，工具条上的按钮会跟着消失/出现。
    capture::set_upload_hook(|| zx_core::cfg::cfg().imgbed().enabled, imgbed::upload_png);
    imgbed::start();

    // OCR 的翻译 / AI 整理：截图库只留两个函数指针，网络在这边做（同一套 WinHTTP）。
    crate::ocr_ai::install();

    zx_core::log::info(
        "已装载模块：taskmon（任务栏系统监视）、clipboard（剪贴板历史）、capture（截图贴图）、gesture（鼠标手势）、imgconv（图片格式转换）、appicon（图标提取）、imgbed（多平台图床：GitHub / S3 / WebDAV / 自定义 HTTP）",
    );
}
