//! 结果提示卡（实现见 `zx_ui::toast`，这里只固定本模块的类名/标题/停留时长）。

use std::sync::atomic::AtomicBool;

use zx_ui::toast::Style;

static BUSY: AtomicBool = AtomicBool::new(false);

const STYLE: Style = Style {
    class: "ZhuxiaImgConvToast",
    thread: "zx-imgconv-toast",
    title: "驻匣 · 图片转换",
    timeout_ms: 4200,
    width: 420.0,
    height_base: 58.0,
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
