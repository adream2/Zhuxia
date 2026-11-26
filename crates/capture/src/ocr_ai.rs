//! OCR 结果的翻译与 AI 处理。
//!
//! 网络调用不放在这里。`capture` 是个纯截图库，不该背上图床/HTTP 那一套依赖；
//! 真正的请求由外壳（zhuxia）在启动时注入进来：
//!
//! ```ignore
//! ocr_ai::install(translate_fn, ai_fn);
//! ```
//!
//! 没注入时（单独跑 capture 的自检、或者用户没配 AI），调用返回一句明确的错误，
//! 界面上直接显示这句话，不会静默失败。

use std::sync::OnceLock;

/// 翻译：入参是原文 + 目标语言代码（`auto` / `zh` / `en` …），返回译文。
///
/// 第二个返回值是**警告**（可选）：长文本按块翻译时会有"部分块没翻成功"的情况，
/// 结果里那些块会保留原文。以前这条信息只进日志，界面照样写"已翻译"，
/// 用户看到中英混杂也不知道怎么回事——所以它必须能冒到界面上。
///
/// 目标语言是**每次翻译时选的**（结果窗口里的下拉），不是全局写死——同一张截图
/// 这次要英文、下次要日文，都可能。
pub type TranslateHook = fn(&str, &str) -> Result<(String, Option<String>), String>;

/// AI 整理：入参是原文，返回整理后的文本。
pub type Hook = fn(&str) -> Result<String, String>;

static TRANSLATE: OnceLock<TranslateHook> = OnceLock::new();
static AI: OnceLock<Hook> = OnceLock::new();

/// 由外壳在启动时调用。重复调用只会保留第一次（单进程里注入一次就够）。
pub fn install(translate: TranslateHook, ai: Hook) {
    let _ = TRANSLATE.set(translate);
    let _ = AI.set(ai);
}

/// `to` 是目标语言代码；传 `auto` 表示按内容判方向。
pub fn translate(text: &str, to: &str) -> Result<(String, Option<String>), String> {
    if text.trim().is_empty() {
        return Err("没有可翻译的内容".into());
    }
    match TRANSLATE.get() {
        Some(f) => f(text, to),
        None => Err("翻译还没接上：请在设置里配置翻译接口".into()),
    }
}

/// 结果窗口下拉菜单用的语言表：(代码, 显示名)。
pub const LANGS: &[(&str, &str)] = &[
    ("auto", "自动"),
    ("zh", "中文"),
    ("en", "英文"),
    ("ja", "日语"),
    ("ko", "韩语"),
    ("fr", "法语"),
    ("de", "德语"),
    ("es", "西班牙语"),
    ("ru", "俄语"),
];

/// 语言代码 → 显示名（下拉按钮上要写"翻译·英"这种）。
pub fn lang_label(code: &str) -> &'static str {
    LANGS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| *n)
        .unwrap_or("自动")
}

/// 让 AI 把这段 OCR 文本整理成"能直接用"的样子。
///
/// 用途是**修 OCR 的错**，不是聊天：错字、断行、表格串行、代码缩进，这些是 OCR
/// 输出最常见的毛病，而它们恰好是大模型最擅长的清理工作。提示词在配置里可改。
pub fn polish(text: &str) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("没有可整理的内容".into());
    }
    match AI.get() {
        Some(f) => f(text),
        None => Err("AI 还没接上：请在设置里配置 AI 接口".into()),
    }
}
