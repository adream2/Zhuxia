//! 链接：模板 + 百分号编码 + 复制格式。
//!
//! 模板变量只有四个：`{{owner}} {{repo}} {{branch}} {{path}}`（GitHub 用）。
//! 其他平台由后端自己产出 URL（S3/WebDAV 拼 publicBase，自定义 HTTP 读服务端返回）。

/// 内置远程来源。模板是各 CDN 的公开 URL 结构，不构成受版权保护的代码。
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("jsdelivr", "jsDelivr", "https://cdn.jsdelivr.net/gh/{{owner}}/{{repo}}@{{branch}}/{{path}}"),
    (
        "jsdelivr-cn",
        "jsDelivr 国内节点",
        "https://jsd.cdn.zzko.cn/gh/{{owner}}/{{repo}}@{{branch}}/{{path}}",
    ),
    (
        "statically",
        "Statically",
        "https://cdn.statically.io/gh/{{owner}}/{{repo}}@{{branch}}/{{path}}",
    ),
    (
        "github",
        "GitHub Raw",
        "https://github.com/{{owner}}/{{repo}}/raw/{{branch}}/{{path}}",
    ),
    (
        "ghproxy",
        "gh-proxy 加速",
        "https://gh-proxy.com/https://raw.githubusercontent.com/{{owner}}/{{repo}}/{{branch}}/{{path}}",
    ),
    ("pages", "GitHub Pages", "https://{{owner}}.github.io/{{repo}}/{{path}}"),
];

/// `cdn + 自定义模板` → 实际模板。
pub fn template_for(cdn: &str, custom: &str) -> Result<String, String> {
    if cdn == "custom" {
        let t = custom.trim();
        if t.is_empty() {
            return Err("链接来源选了「自定义」，但模板是空的".into());
        }
        return validate(t);
    }
    PRESETS
        .iter()
        .find(|(id, _, _)| *id == cdn)
        .map(|(_, _, t)| t.to_string())
        .ok_or_else(|| format!("未知的链接来源：{cdn}"))
}

pub fn validate(template: &str) -> Result<String, String> {
    let t = template.trim();
    if !(t.starts_with("https://") || t.starts_with("http://")) {
        return Err("链接模板必须以 http:// 或 https:// 开头".into());
    }
    if !t.contains("{{path}}") {
        return Err("链接模板必须包含 {{path}}（否则所有图片会指向同一个地址）".into());
    }
    Ok(t.to_string())
}

pub fn resolve(template: &str, owner: &str, repo: &str, branch: &str, path: &str) -> String {
    let path = encode_path(path);
    template
        .replace("{{owner}}", &encode(owner))
        .replace("{{repo}}", &encode(repo))
        .replace("{{branch}}", &encode_ref(branch))
        .replace("{{path}}", &path)
}

/// 按复制格式渲染。`file_name` 用来做 alt / 标题（去扩展名）。
pub fn render(format: &str, url: &str, file_name: &str) -> String {
    let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
    match format {
        "html" => format!("<img src=\"{url}\" alt=\"{stem}\" />"),
        "bbcode" => format!("[img]{url}[/img]"),
        "markdown" => format!("![{stem}]({url})"),
        _ => url.to_string(),
    }
}

/// RFC 3986 非保留字符；其余按 UTF-8 字节 `%XX`。
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

fn encode_ref(branch: &str) -> String {
    branch.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// `base` + `/` + 已编码路径；base 允许带末尾斜杠。
pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{}", encode_path(path.trim_start_matches('/')))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsdelivr_encodes_chinese() {
        let t = template_for("jsdelivr", "").unwrap();
        let url = resolve(&t, "me", "images", "main", "images/截图 1.png");
        assert_eq!(
            url,
            "https://cdn.jsdelivr.net/gh/me/images@main/images/%E6%88%AA%E5%9B%BE%201.png"
        );
    }

    #[test]
    fn custom_requires_path_and_scheme() {
        assert!(validate("https://x.com/{{owner}}").is_err());
        assert!(validate("https://x.com/{{path}}").is_ok());
        assert!(validate("ftp://x/{{path}}").is_err());
    }

    #[test]
    fn formats_drop_extension() {
        let url = "https://x/a.webp";
        assert_eq!(render("markdown", url, "a.webp"), "![a](https://x/a.webp)");
        assert_eq!(render("html", url, "a.webp"), "<img src=\"https://x/a.webp\" alt=\"a\" />");
        assert_eq!(render("bbcode", url, "a.webp"), "[img]https://x/a.webp[/img]");
        assert_eq!(render("url", url, "a.webp"), url);
    }

    #[test]
    fn join_url_encodes_each_segment() {
        assert_eq!(
            join_url("https://cdn.x/", "a/中文 1.png"),
            "https://cdn.x/a/%E4%B8%AD%E6%96%87%201.png"
        );
        assert_eq!(join_url("https://cdn.x", ""), "https://cdn.x");
    }
}
