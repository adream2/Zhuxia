//! 采集规则：内容分类、预览、CF_HTML 编解码、隐私识别、清理规则、来源程序策略。
//!
//! 这里全是纯函数（文件末尾有单测），采集线程与面板共用同一套判定，避免
//! 「列表里显示的分类」和「实际存下来的分类」不一致。
//!
//! 判定顺序是有意为之：先看是不是链接，再累计代码特征分，最后才落到普通文本。
//! 改动阈值前先看 `tests` 里的边界用例。

use std::sync::OnceLock;

use regex_lite::Regex;

use crate::model::Kind;
use zx_core::ClipAppPolicy;

/// 超过这个字符数就截断（`PREVIEW_CUT_CHARS` + 省略号）。
const PREVIEW_MAX_CHARS: usize = 200;
const PREVIEW_CUT_CHARS: usize = 180;

// ------------------------------------------------------------------ 基础归一化

pub fn normalize_line_endings(text: &str) -> String {
    // 先整体替换 CRLF，再处理单独的 CR，避免 `\r\r\n` 变成两个换行。
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_whitespace_only(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

fn has_edge_whitespace(text: &str) -> bool {
    const EDGES: [char; 3] = [' ', '\t', '\n'];
    text.starts_with(EDGES) || text.ends_with(EDGES)
}

/// 把连续空白压成一个空格并去掉首尾。
fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out.trim().to_string()
}

/// 行首行尾的空白在列表里看不见，用 `·` 标出来；每行最多标 8 个。
fn visible_whitespace(text: &str) -> String {
    const MARK: char = '·';
    const MAX_MARKS: usize = 8;
    let mut out = String::with_capacity(text.len() + 4);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let leading = line.len() - line.trim_start_matches([' ', '\t']).len();
        let trailing = line.len() - line.trim_end_matches([' ', '\t']).len();
        if line.is_empty() {
            continue;
        }
        out.extend(std::iter::repeat_n(MARK, leading.min(MAX_MARKS)));
        out.push_str(line.trim_matches([' ', '\t']));
        out.extend(std::iter::repeat_n(MARK, trailing.min(MAX_MARKS)));
    }
    out
}

// ------------------------------------------------------------------ 分类

/// 形如 `scheme://`，且 scheme 只由字母数字和 `+ - .` 组成。
fn has_url_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// 代码特征关键词：命中一个加一分。
const CODE_HINTS: [&str; 18] = [
    "import ",
    "const ",
    "let ",
    "var ",
    "function ",
    "class ",
    "pub fn ",
    "impl ",
    "#include",
    "package ",
    "interface ",
    "namespace ",
    "void ",
    "return ",
    "if (",
    "for (",
    "while (",
    "=>",
];

fn code_score(text: &str) -> u32 {
    let mut score = CODE_HINTS.iter().filter(|h| text.contains(**h)).count() as u32;
    if text.contains(';') {
        score += 1;
    }
    if text.contains('{') && text.contains('}') {
        score += 1;
    }
    // 闭合标签比普通符号更能说明是标记/代码，给两分。
    if text.contains("</") && text.contains('>') {
        score += 2;
    }
    score
}

pub fn detect_content_type(text: &str) -> Kind {
    let trimmed = text.trim();
    if trimmed.starts_with("www.") || has_url_scheme(trimmed) {
        return Kind::Url;
    }
    if code_score(text) >= 2 {
        return Kind::Code;
    }
    // 单行 JSON / JS 对象：花括号包住、带键值对和引号。
    if trimmed.starts_with('{') && trimmed.ends_with('}') && text.contains(':') && text.contains('"')
    {
        return Kind::Code;
    }
    Kind::Text
}

/// 按扩展名把被复制的文件分成 图片 / 视频 / 普通文件。
pub fn classify_path(path: &str) -> Kind {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or_default();
    match ext {
        "gif" | "png" | "jpg" | "jpeg" | "bmp" | "webp" | "ico" | "tif" | "tiff" => Kind::Image,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg" => {
            Kind::Video
        }
        _ => Kind::File,
    }
}

pub fn is_image_file(path: &str) -> bool {
    classify_path(path) == Kind::Image
}

// ------------------------------------------------------------------ HTML

const RICH_PREVIEW_FALLBACK: &str = "[富文本]";

/// 这些标签后面要换行，否则整段文字会挤成一行。
const BLOCK_TAGS: [&str; 13] = [
    "br",
    "p",
    "div",
    "tr",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "blockquote",
];

/// 从 `<tag attr>` / `</tag>` 里取标签名。
fn tag_name(tag: &str) -> &str {
    let t = tag.trim_start_matches('<').trim_start_matches('/');
    t.split([' ', '>', '/']).next().unwrap_or("")
}

fn strip_tags(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0usize;
    while i < html.len() {
        let rest = &html[i..];
        // 只有确实找到 '>' 才当标签吃掉；`a < b` 这种保持原样。
        if rest.starts_with('<') {
            if let Some(len) = rest.find('>') {
                let name = tag_name(&lower[i..i + len + 1]);
                if BLOCK_TAGS.contains(&name) {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else if name == "td" || name == "th" {
                    out.push('\t');
                }
                i += len + 1;
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 连续空行压成一个，行尾空白去掉。
fn squeeze_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(t);
        out.push('\n');
    }
    out.trim().to_string()
}

/// 去掉标签取纯文本，块级标签换成换行（富文本的 `content` 字段就是这么来的）。
pub fn html_to_text(html: &str) -> String {
    squeeze_blank_lines(&decode_entities(&strip_tags(html)))
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// 解码 `&#65;` / `&#x4e2d;` 这类数字实体。
///
/// 只认最多 6 位；解不出字符时整段吃掉（与浏览器的容错行为一致）。
fn decode_numeric_entities(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        if bytes[i] != b'&' || bytes[i + 1] != b'#' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        let hex = bytes.get(j) == Some(&b'x');
        if hex {
            j += 1;
        }
        let digits = j;
        while j < bytes.len() && j - digits < 6 && is_hex(bytes[j]) {
            j += 1;
        }
        if j == digits || bytes.get(j) != Some(&b';') {
            i += 1;
            continue;
        }
        out.push_str(&text[copied..i]);
        let body = &text[digits..j];
        let code = if hex {
            u32::from_str_radix(body, 16).ok()
        } else {
            body.parse::<u32>().ok()
        };
        if let Some(c) = code.and_then(char::from_u32) {
            out.push(c);
        }
        i = j + 1;
        copied = i;
    }
    out.push_str(&text[copied..]);
    out
}

pub fn decode_entities(text: &str) -> String {
    // 顺序有讲究：`&amp;lt;` 应该得到 `&lt;` 而不是 `<`，所以 `&amp;` 放最后。
    const NAMED: [(&str, &str); 7] = [
        ("&nbsp;", " "),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&amp;", "&"),
    ];
    let mut s = text.to_string();
    for (from, to) in NAMED {
        if s.contains(from) {
            s = s.replace(from, to);
        }
    }
    decode_numeric_entities(&s)
}

/// 富文本的纯文本正文：优先从 HTML 抽取，抽不出就用原 content。
pub fn derive_rich_text(content: &str, html: Option<&str>) -> String {
    if let Some(html) = html {
        let text = html_to_text(html);
        if !text.trim().is_empty() {
            return normalize_line_endings(&text).trim().to_string();
        }
    }
    normalize_line_endings(content).trim().to_string()
}

/// 只有一张图的 HTML（浏览器「复制图片」常带这种包装）：不该当富文本记录。
pub fn html_is_image_only(html: &str) -> bool {
    if !html_to_text(html).trim().is_empty() {
        return false;
    }
    let lower = html.to_ascii_lowercase();
    lower.contains("<img") && !lower.contains("<table")
}

/// CF_HTML 头部里的四个字节偏移。偏移都从整个 payload 开头算。
#[derive(Default)]
struct CfHeader {
    start_html: Option<usize>,
    end_html: Option<usize>,
    start_frag: Option<usize>,
    end_frag: Option<usize>,
}

fn parse_cf_header(text: &str) -> CfHeader {
    let mut h = CfHeader::default();
    // 头部只有十来行，限制 32 行足够，也避免正文里的冒号被当成键值对。
    for line in text.lines().take(32) {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let Some(n) = v.trim().parse::<usize>().ok() else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "starthtml" => h.start_html = Some(n),
            "endhtml" => h.end_html = Some(n),
            "startfragment" => h.start_frag = Some(n),
            "endfragment" => h.end_frag = Some(n),
            _ => {}
        }
    }
    h
}

/// 解析剪贴板的 `HTML Format`（CF_HTML）。
pub fn parse_cf_html(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw).to_string();
    let h = parse_cf_header(&text);

    // 片段优先：多数程序只填 StartFragment/EndFragment。
    if let (Some(a), Some(b)) = (h.start_frag, h.end_frag) {
        if a < b && b <= raw.len() {
            let frag = String::from_utf8_lossy(&raw[a..b]).to_string();
            if !frag.trim().is_empty() {
                return Some(frag);
            }
        }
    }
    let clamp = |n: usize| n.min(raw.len());
    let a = h.start_html.map(clamp).unwrap_or(0);
    let b = h.end_html.map(clamp).unwrap_or(raw.len());
    if a < b {
        let doc = String::from_utf8_lossy(&raw[a..b]).to_string();
        if !doc.trim().is_empty() {
            return Some(doc);
        }
    }
    None
}

/// 生成 CF_HTML（写回剪贴板用）。偏移固定 10 位十进制，和浏览器、Office 一致。
pub fn encode_cf_html(fragment: &str) -> String {
    // 头部长度是格式约定值：偏移从这个位置开始算，改长度会让接收端错位。
    const HEADER_LEN: usize = 105;
    const OPEN: &str = "<html><body>\r\n<!--StartFragment-->";
    const CLOSE: &str = "<!--EndFragment-->\r\n</body></html>";

    let start_html = HEADER_LEN;
    let start_frag = start_html + OPEN.len();
    let end_frag = start_frag + fragment.len();
    let end_html = end_frag + CLOSE.len();
    format!(
        "Version:0.9\r\n\
         StartHTML:{start_html:010}\r\n\
         EndHTML:{end_html:010}\r\n\
         StartFragment:{start_frag:010}\r\n\
         EndFragment:{end_frag:010}\r\n\
         {OPEN}{fragment}{CLOSE}"
    )
}

// ------------------------------------------------------------------ 预览

fn truncate_preview(text: &str) -> String {
    if text.chars().count() <= PREVIEW_MAX_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(PREVIEW_CUT_CHARS).collect();
    format!("{cut}…")
}

/// 文件类预览：「名字」或「名字 等 N 项」。
fn files_preview(content: &str) -> String {
    let paths: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let first = paths
        .first()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| (*p).to_string())
        })
        .unwrap_or_else(|| "[文件]".to_string());
    let text = if paths.len() > 1 {
        format!("{first} 等 {} 项", paths.len())
    } else {
        first
    };
    truncate_preview(&text)
}

/// 富文本预览：抽正文 → 压空白；空白内容退化成占位或可见空白。
fn rich_preview(content: &str, html: Option<&str>) -> String {
    let line = normalize_line_endings(content);
    let clean = derive_rich_text(content, html);
    if clean.is_empty() || collapse_ws(&clean).is_empty() {
        return if is_whitespace_only(&line) {
            visible_whitespace(&line)
        } else {
            RICH_PREVIEW_FALLBACK.to_string()
        };
    }
    if has_edge_whitespace(&clean) {
        visible_whitespace(&clean)
    } else {
        collapse_ws(&clean)
    }
}

pub fn build_preview(kind: Kind, content: &str, html: Option<&str>) -> String {
    match kind {
        Kind::Image => "[图片]".to_string(),
        Kind::File | Kind::Video => files_preview(content),
        Kind::RichText => truncate_preview(&rich_preview(content, html)),
        _ => {
            let line = normalize_line_endings(content);
            // 全空白或首尾带空白时，用点标出来，否则用户看不出这条为什么"内容一样"。
            let text = if is_whitespace_only(&line) || has_edge_whitespace(&line) {
                visible_whitespace(&line)
            } else {
                collapse_ws(&line)
            };
            truncate_preview(&text)
        }
    }
}

// ------------------------------------------------------------------ 指纹

/// FNV-1a 64 位。选它是因为跨进程、跨重启都稳定（`DefaultHasher` 不保证）。
fn fnv1a(bytes: impl Iterator<Item = u8>) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x1000_0000_01b3;
    let mut h = OFFSET;
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// 文本指纹：换行归一化后再算，这样 CRLF/LF 的同一段文字不会重复入库。
pub fn text_hash(text: &str) -> u64 {
    let normalized = normalize_line_endings(text);
    fnv1a(normalized.bytes())
}

pub fn bytes_hash(bytes: &[u8]) -> u64 {
    fnv1a(bytes.iter().copied())
}

// ------------------------------------------------------------------ 隐私

struct Patterns {
    url: Regex,
    phone: Regex,
    idcard: Regex,
    email: Regex,
    secret: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        url: Regex::new(r"(?i)(?:[a-zA-Z][a-zA-Z0-9+\-.]*://|www\.)\S+").unwrap(),
        // 允许 +86、空格、括号、连字符分隔的国内手机号。
        phone: Regex::new(r"(?:\+?86)?[-\s\(]*1[3-9]\d{1}[-\s\)]*\d{4}[-\s]*\d{4}").unwrap(),
        idcard: Regex::new(
            r"\b[1-9]\d{5}[1-9]\d{3}((0\d)|(1[0-2]))(([0|1|2]\d)|3[0-1])\d{3}([0-9Xx])\b",
        )
        .unwrap(),
        email: Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap(),
        secret: Regex::new(
            r"(?ix)((?:sk|pk|ghp|gho|github_pat|AIza|AKIA|ya29)[-_][\w\-]{20,}|(?:password|secret|api[_-]?key|access[_-]?key|token|bearer)[\s:=]+[\w\-]{16,})",
        )
        .unwrap(),
    })
}

/// 弱口令式判定：长度 8–64、单行、无空格，且大写/小写/数字/符号各至少一个。
fn looks_like_password(text: &str) -> bool {
    (8..=64).contains(&text.len())
        && !text.contains(' ')
        && !text.contains('\n')
        && text.chars().any(|c| c.is_uppercase())
        && text.chars().any(|c| c.is_lowercase())
        && text.chars().any(|c| c.is_numeric())
        && text.chars().any(|c| !c.is_alphanumeric())
}

pub fn contains_sensitive(text: &str, kinds: &[String], custom: &[String]) -> bool {
    // 超长文本不扫（正则回溯代价高），内联数据也不算（图片 base64）。
    if text.len() > 5000 || text.starts_with("data:") {
        return false;
    }
    let p = patterns();
    let on = |k: &str| kinds.iter().any(|t| t == k);
    if on("url") && p.url.is_match(text) {
        return true;
    }
    if on("phone") && p.phone.is_match(text) {
        return true;
    }
    if on("idcard") && p.idcard.is_match(text) {
        return true;
    }
    if on("email") && p.email.is_match(text) {
        return true;
    }
    if on("secret") && p.secret.is_match(text) {
        return true;
    }
    if on("password") && looks_like_password(text) {
        return true;
    }
    custom.iter().any(|rule| match Regex::new(rule) {
        Ok(re) => re.is_match(text),
        // 用户写的正则不合法就当没写，不能让整条采集失败。
        Err(_) => false,
    })
}

/// 预览脱敏：手机号 / 身份证 / 邮箱中间打码。`content` 里的原值不动。
pub fn mask_preview(text: &str) -> String {
    static PHONE: OnceLock<Regex> = OnceLock::new();
    static IDCARD: OnceLock<Regex> = OnceLock::new();
    static EMAIL: OnceLock<Regex> = OnceLock::new();

    let phone = PHONE.get_or_init(|| Regex::new(r"(?:\+?86)?(1[3-9]\d)(\d{4})(\d{4})").unwrap());
    let idcard = IDCARD.get_or_init(|| Regex::new(r"\b(\d{4})\d{10}(\w{4})\b").unwrap());
    let email = EMAIL.get_or_init(|| Regex::new(r"([\w.%+-]{1,2})[\w.%+-]*(@[\w.-]+)").unwrap());

    let s = phone.replace_all(text, "$1****$3").to_string();
    let s = idcard.replace_all(&s, "$1**********$2").to_string();
    email.replace_all(&s, "$1***$2").to_string()
}

// ------------------------------------------------------------------ 清理规则

#[derive(Debug, Clone, PartialEq)]
pub enum CleanupAction {
    Replace(String),
    /// 「不记录」：命中即整条丢弃。
    Ignore,
}

fn unescape_rule(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\t", "\t")
}

/// 一行一条：
/// - `pattern => 替换`
/// - `> pattern` 命中即不记录
/// - `# action: ignore` 注释行，作用于下一条规则（设置页写出来的形式）
pub fn parse_cleanup_rules(raw: &str) -> Vec<(Regex, CleanupAction)> {
    let mut rules = Vec::new();
    let mut ignore_next = false;
    for line in raw.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if let Some((k, v)) = comment.split_once(':') {
                if k.trim().eq_ignore_ascii_case("action") {
                    ignore_next = v.trim().eq_ignore_ascii_case("ignore");
                }
            }
            continue;
        }

        // `=>` 优先于 `>`：一句 `> a => b` 里用户想表达的是替换。
        let (pattern, action) = if let Some((p, r)) = line.split_once("=>") {
            let action = if ignore_next {
                CleanupAction::Ignore
            } else {
                CleanupAction::Replace(unescape_rule(r.trim()))
            };
            (p.trim(), action)
        } else if let Some(p) = line.strip_prefix('>') {
            (p.trim(), CleanupAction::Ignore)
        } else {
            (line, CleanupAction::Replace(String::new()))
        };
        // 这个标记只影响紧随其后的一条规则。
        ignore_next = false;

        if pattern.is_empty() {
            continue;
        }
        if let Ok(re) = Regex::new(pattern) {
            rules.push((re, action));
        }
    }
    rules
}

/// 应用规则：命中 Ignore 返回 None（整条不记录），否则返回替换后的文本。
pub fn apply_cleanup_rules(text: &str, rules: &[(Regex, CleanupAction)]) -> Option<String> {
    let mut out = text.to_string();
    for (re, action) in rules {
        if !re.is_match(&out) {
            continue;
        }
        match action {
            CleanupAction::Ignore => return None,
            CleanupAction::Replace(rep) => out = re.replace_all(&out, rep.as_str()).to_string(),
        }
    }
    Some(out)
}

/// 来源程序策略：进程名或完整路径包含 `app` 即命中（不区分大小写）。
///
/// `ignore` 对所有类型生效；`cleanup` 只作用于文本（图片没有可清理的正文）。
pub fn policy_matches(policy: &ClipAppPolicy, app: &str, path: Option<&str>, kind: Kind) -> bool {
    let needle = policy.app.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    let haystack = format!(
        "{} {}",
        app.to_ascii_lowercase(),
        path.unwrap_or_default().to_ascii_lowercase()
    );
    if !haystack.contains(&needle) {
        return false;
    }
    policy.action.eq_ignore_ascii_case("ignore") || kind.is_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_urls_and_code_like() {
        assert_eq!(detect_content_type("https://example.com/a"), Kind::Url);
        assert_eq!(detect_content_type("www.example.com"), Kind::Url);
        assert_eq!(detect_content_type("fn main() { let x = 1; }"), Kind::Code);
        assert_eq!(detect_content_type("今天天气不错"), Kind::Text);
        // 只有 :// 没有合法 scheme 的不算链接
        assert_eq!(detect_content_type("://no-scheme"), Kind::Text);
        assert_eq!(detect_content_type("not-a/url://x"), Kind::Text);
    }

    #[test]
    fn cf_html_round_trip_uses_byte_offsets() {
        for raw in ["中文片段 <b>加粗</b>", "<p>x</p>", "😀 emoji"] {
            let encoded = encode_cf_html(raw);
            assert_eq!(parse_cf_html(encoded.as_bytes()).as_deref(), Some(raw));
        }
        // 空片段没有可返回的片段内容，退化成整份 HTML 文档
        let parsed = parse_cf_html(encode_cf_html("").as_bytes()).unwrap();
        assert!(parsed.starts_with("<html><body>"));
    }

    #[test]
    fn html_preview_strips_tags_but_keeps_text() {
        let html = "<p>第一段</p><p>第二&nbsp;段</p>";
        assert_eq!(html_to_text(html), "第一段\n第二 段");
        assert!(!html_is_image_only(html));
        assert!(html_is_image_only("<img src=\"x.png\">"));
        // 每个单元格前一个制表符；块级标签（tr）只在已有内容后才补换行
        assert_eq!(html_to_text("<tr><td>a</td><td>b</td></tr>"), "a\t\tb");
    }

    #[test]
    fn numeric_entities_decode_and_swallow_unknown() {
        assert_eq!(decode_entities("&#65;&#x42;&#x4e2d;"), "AB中");
        assert_eq!(decode_entities("&#xZZ;"), "&#xZZ;");
        // 合法码点即使超出 CJK 范围也照常解码（U+1E240）
        assert_eq!(decode_entities("x&#123456;y"), "x\u{1e240}y");
        // 十进制位置写了十六进制字母：匹配上但解不出，整段吃掉
        assert_eq!(decode_entities("x&#1a;y"), "xy");
    }

    #[test]
    fn cleanup_rules_support_replace_and_ignore() {
        let rules = parse_cleanup_rules("secret => ***\n> 机密");
        assert_eq!(apply_cleanup_rules("a secret b", &rules).as_deref(), Some("a *** b"));
        assert_eq!(apply_cleanup_rules("这是机密", &rules), None);
    }

    #[test]
    fn cleanup_comment_marks_next_replacement_rule_only() {
        let rules = parse_cleanup_rules("# action: ignore\n密码\\d+ => ***\n普通 => X");
        // 只影响紧随其后的那条 `=>` 规则
        assert_eq!(apply_cleanup_rules("密码123", &rules), None);
        assert_eq!(apply_cleanup_rules("普通", &rules).as_deref(), Some("X"));
    }

    #[test]
    fn sensitive_detection_matches_phone_and_email() {
        let kinds = vec!["phone".to_string(), "email".to_string()];
        assert!(contains_sensitive("我的手机 13812345678", &kinds, &[]));
        assert!(contains_sensitive("a.b@example.com", &kinds, &[]));
        assert!(!contains_sensitive("普通文字", &kinds, &[]));
        // 未启用的类别不参与判定
        assert!(!contains_sensitive("a.b@example.com", &["phone".to_string()], &[]));
    }

    #[test]
    fn preview_mask_hides_middle_of_phone_and_email() {
        assert_eq!(mask_preview("手机 13812345678"), "手机 138****5678");
        assert_eq!(mask_preview("ab@example.com"), "ab***@example.com");
    }

    #[test]
    fn text_hash_is_stable_across_line_endings_but_keeps_edge_spaces() {
        assert_eq!(text_hash("a\r\nb"), text_hash("a\nb"));
        assert_ne!(text_hash(" a"), text_hash("a"));
    }

    #[test]
    fn preview_marks_edge_whitespace() {
        assert_eq!(build_preview(Kind::Text, "  hi  ", None), "··hi··");
        assert_eq!(build_preview(Kind::Image, "/tmp/a.png", None), "[图片]");
        // 内容与 HTML 都空：可见空白仍是空，不硬塞占位符
        assert_eq!(build_preview(Kind::RichText, "", Some("<p></p>")), "");
        assert_eq!(
            build_preview(Kind::File, "C:/a/1.pdf\nC:/a/2.pdf", None),
            "1.pdf 等 2 项"
        );
    }

    #[test]
    fn policy_ignores_non_text_only_for_cleanup() {
        let mut p = ClipAppPolicy {
            app: "KeePass".into(),
            action: "ignore".into(),
            rules: String::new(),
        };
        assert!(policy_matches(&p, "keepass.exe", None, Kind::Image));
        p.action = "cleanup".into();
        assert!(!policy_matches(&p, "keepass.exe", None, Kind::Image));
        assert!(policy_matches(&p, "keepass.exe", None, Kind::Text));
    }
}
