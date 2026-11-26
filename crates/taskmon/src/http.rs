//! 只做 GET 文本的薄封装——真身在 [`zx_core::http`]。
//!
//! 这里以前是一份独立的 78 行 WinHTTP 实现，和 `imgbed` 那份并存，结果质量分叉：
//! 不检查状态码（404 的 body 也当成功）、4 MB 静默截断、**还漏了端口**
//! （`host:9090` 被整个当主机名传进 `WinHttpConnect`，于是 Clash 的默认端点永远连不上）。
//! 现在统一走 `zx_core::http::request_limited`，这些边界只需要维护一处。

use zx_core::http;

/// 采样用的响应体上限。traffic 相关接口都是小 JSON，8 MB 是"防止对端吐无限数据"的兜底。
const MAX_BODY: usize = 8 * 1024 * 1024;

pub fn get_text(url: &str, timeout_ms: u32) -> Option<String> {
    get_with_headers(url, timeout_ms, &[])
}

pub fn get_with_headers(url: &str, timeout_ms: u32, headers: &[(&str, &str)]) -> Option<String> {
    let resp = http::request_limited(
        "GET",
        url,
        headers,
        None,
        // 空 = 用 WinHTTP 的系统代理配置（AUTOMATIC_PROXY），与采样场景一致
        "",
        timeout_ms,
        MAX_BODY,
    )
    .ok()?;
    // 以前不看状态码：404/500 的 HTML 错误页会被当成 JSON body 往上递，
    // 表现是"解析失败"而不是"接口 404"。
    if !(200..300).contains(&resp.status) {
        log_failure(url, &format!("HTTP {}", resp.status));
        return None;
    }
    let text = resp.text();
    if text.is_empty() {
        log_failure(url, "空响应");
        return None;
    }
    Some(text)
}

/// 对本机地址降噪：Clash 没开是常态，不该每分钟刷一条警告。
fn log_failure(url: &str, why: &str) {
    if url.contains("127.0.0.1") || url.contains("localhost") {
        return;
    }
    zx_core::log::warn_once(
        Box::leak(format!("http:{url}").into_boxed_str()),
        format!("WinHTTP GET 失败：{url}（{why}）"),
    );
}
