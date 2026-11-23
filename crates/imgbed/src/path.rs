//! 路径模板、文件名、扩展名判定。
//!
//! 路径是"平台 + 模板"两段拼起来的：模板在设置里（`providers.<id>.path`），
//! 上传页可以临时覆盖。渲染后统一校验，避免把 `..` 或控制字符写进别人的仓库/桶。

use zx_core::ImgBedSettings;

use crate::provider::Fail;

/// 最多几层目录。超过多半是把绝对路径贴进来了。
const MAX_DEPTH: usize = 8;

/// 渲染 `{yyyy}` 这类日期变量并校验。
pub fn render(template: &str) -> Result<String, Fail> {
    let now = zx_core::win::local_time_string(); // 2026-11-22 16:30:05
    let digits: String = now.chars().filter(|c| c.is_ascii_digit()).collect();
    let d = if digits.len() >= 14 { &digits[..14] } else { digits.as_str() };
    let mut s = template.trim().to_string();
    if s.is_empty() {
        return Ok(String::new());
    }
    for (k, v) in [
        ("{yyyy}", &d[..4]),
        ("{MM}", &d[4..6]),
        ("{dd}", &d[6..8]),
        ("{HH}", &d[8..10]),
        ("{mm}", &d[10..12]),
    ] {
        s = s.replace(k, v);
    }
    // 老配置可能残留 {{path}} 这类花括号，直接拒绝比传错地方好
    if s.contains('{') || s.contains('}') {
        return Err(Fail::new("路径模板有误", format!("渲染后还剩花括号：{s}")));
    }
    validate(&s)
}

/// 校验并规范化（去首尾 `/`，拒绝空段/`.`/`..`/反斜杠/控制字符）。
pub fn validate(dir: &str) -> Result<String, Fail> {
    let dir = dir.trim().trim_matches('/');
    if dir.is_empty() {
        return Ok(String::new());
    }
    if dir.contains('\\') {
        return Err(Fail::new("路径不合法", "用 / 分隔目录，不能有反斜杠"));
    }
    let mut out = Vec::new();
    for seg in dir.split('/') {
        let seg = seg.trim();
        if seg.is_empty() {
            return Err(Fail::new("路径不合法", "有连续的 / 或多余的 /"));
        }
        if seg == "." || seg == ".." {
            return Err(Fail::new("路径不合法", "不能包含 . 或 .."));
        }
        if seg.chars().any(|c| (c as u32) < 32 || (c as u32) == 127) {
            return Err(Fail::new("路径不合法", "包含控制字符"));
        }
        if seg.chars().count() > 80 {
            return Err(Fail::new("路径不合法", "单段目录名超过 80 字符"));
        }
        out.push(seg);
    }
    if out.len() > MAX_DEPTH {
        return Err(Fail::new("路径不合法", format!("目录太深（最多 {MAX_DEPTH} 层）")));
    }
    Ok(out.join("/"))
}

/// 文件名主体（不含扩展名）。
///
/// 时间戳模式：`20261122-163005-a1b2`；原名模式：`原名-a1b2`。hash 是内容前 4 位，
/// 同一秒传两张不同的图不会撞，相同的图撞名也无所谓（内容一样）。
pub fn file_stem(cfg: &ImgBedSettings, original: Option<&str>, bytes: &[u8]) -> String {
    let hash = short_hash(bytes);
    let raw = sanitize(&cfg.prefix, 24);
    let prefix = if raw.is_empty() { String::new() } else { format!("{}-", raw.trim_end_matches('-')) };
    if cfg.naming == "original" {
        let stem = original
            .map(stem_of)
            .map(|s| sanitize(&s, 60))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "image".into());
        return format!("{prefix}{stem}-{hash}");
    }
    let now = zx_core::win::local_time_string();
    let digits: String = now.chars().filter(|c| c.is_ascii_digit()).collect();
    let d = if digits.len() >= 14 { &digits[..14] } else { digits.as_str() };
    format!("{prefix}{}-{}-{hash}", &d[..8], &d[8..])
}

/// 按魔数判扩展名与 Content-Type。判不出来就拒绝——这不是通用文件上传工具。
pub fn kind_of(bytes: &[u8]) -> Result<(&'static str, &'static str), Fail> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Ok(("png", "image/png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Ok(("jpg", "image/jpeg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok(("gif", "image/gif"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Ok(("webp", "image/webp"))
    } else if bytes.len() >= 26 && &bytes[..2] == b"BM" {
        Ok(("bmp", "image/bmp"))
    } else if bytes.len() >= 12 && (&bytes[..4] == b"II*\0" || &bytes[..4] == b"MM\0*") {
        Ok(("tiff", "image/tiff"))
    } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        Ok(("svg", "image/svg+xml"))
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis") {
        Ok(("avif", "image/avif"))
    } else {
        Err(Fail::new("不是图片", "只接受 PNG / JPEG / GIF / WebP / BMP / TIFF / SVG / AVIF"))
    }
}

/// FNV-1a 64 → 16 位十六进制（缓存键用，避免 4 位撞车）。
pub fn hash16(bytes: &[u8]) -> String {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("{h:016x}")
}

/// FNV-1a 32 前 4 位十六进制。
pub fn short_hash(bytes: &[u8]) -> String {
    let mut h: u32 = 0x811C_9DC5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")[..4].to_string()
}

fn stem_of(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rfind('.') {
        Some(i) if i > 0 => base[..i].to_string(),
        _ => base.to_string(),
    }
}

fn sanitize(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if out.chars().count() >= max {
            break;
        }
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('-');
        }
    }
    out.trim_matches('-').trim_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zx_core::{ImgBedCfg, ImgBedUploadCfg};

    fn cfg() -> ImgBedSettings {
        zx_core::Settings {
            imgbed: Some(ImgBedCfg {
                upload: ImgBedUploadCfg { prefix: Some("blog".into()), ..Default::default() },
                ..Default::default()
            }),
            ..Default::default()
        }
        .imgbed()
    }

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    #[test]
    fn renders_date_tokens() {
        let out = render("images/{yyyy}/{MM}/{dd}").unwrap();
        let parts: Vec<&str> = out.split('/').collect();
        assert_eq!(parts.len(), 4, "{out}");
        assert_eq!(parts[0], "images");
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 2);
        assert_eq!(parts[3].len(), 2);
    }

    #[test]
    fn rejects_dangerous_paths() {
        assert!(render("a/../b").is_err());
        assert!(render("a//b").is_err());
        assert!(render("a\\b").is_err());
        assert!(render("a/b/c/d/e/f/g/h/i").is_err());
        assert!(render("{yyyy}/{MM}/x").is_ok());
    }

    #[test]
    fn timestamp_name_has_prefix_hash() {
        let n = file_stem(&cfg(), Some("截图 1.png"), PNG);
        assert!(n.starts_with("blog-20"), "{n}");
        assert_eq!(n.rsplit('-').next().unwrap().len(), 4);
    }

    #[test]
    fn original_name_is_kept() {
        let mut c = cfg();
        c.naming = "original".into();
        let n = file_stem(&c, Some("My Screenshot.PNG"), PNG);
        assert!(n.starts_with("blog-My-Screenshot-"), "{n}");
    }

    #[test]
    fn sniffs_types() {
        assert_eq!(kind_of(PNG).unwrap(), ("png", "image/png"));
        assert_eq!(kind_of(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap().0, "jpg");
        assert_eq!(kind_of(b"GIF89a....").unwrap().0, "gif");
        assert!(kind_of(b"not an image").is_err());
    }
}
