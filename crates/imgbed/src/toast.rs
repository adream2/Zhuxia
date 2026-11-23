//! 结果提示卡（实现见 `zx_ui::toast`，这里只固定本模块的类名/标题/尺寸）。

use std::sync::atomic::AtomicBool;

use zx_ui::toast::Style;

pub use zx_ui::toast::ellipsis;

static BUSY: AtomicBool = AtomicBool::new(false);

const STYLE: Style = Style {
    class: "ZhuxiaImgBedToast",
    thread: "zx-imgbed-toast",
    title: "驻匣 · 图床",
    timeout_ms: 4500,
    width: 468.0,
    height_base: 60.0,
    busy: &BUSY,
};

/// 弹一张卡片并在当前线程等它消失（命令行兜底路径用：进程不能先退出）。
pub fn show_blocking(title: &str, lines: &[String], error: bool) {
    zx_ui::toast::show_blocking(&STYLE, title, lines, error)
}

/// 弹一张卡片。`error = true` 时左侧竖条用警示色。
pub fn show(title: &str, lines: &[String], error: bool) {
    zx_ui::toast::show(&STYLE, title, lines, error)
}

/// 让外部能主动收掉（退出时用）。
pub fn dismiss() {
    zx_ui::toast::dismiss(STYLE.class)
}
