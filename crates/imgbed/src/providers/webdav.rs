//! WebDAV：Alist / Nextcloud / 坚果云这类。
//!
//! 只用到四个动词：PROPFIND（列目录）、MKCOL（补目录）、PUT（上传）、DELETE。
//! 认证走 Basic；坚果云必须用"应用密码"，不是登录密码。

use base64::Engine;
use serde::Deserialize;

use super::super::http;
use super::super::link;
use super::super::provider::{
    Backend, Caps, Fail, RemoteImage, TestReport, UploadRequest, Uploaded,
};
use super::super::secrets;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Cfg {
    pub url: String,
    pub username: String,
    pub public_base: String,
    pub path: String,
}

pub struct WebDav {
    cfg: Cfg,
    password: String,
    proxy: String,
}

impl WebDav {
    pub fn from_settings(cfg: &zx_core::ImgBedSettings) -> Result<WebDav, Fail> {
        let raw = cfg
            .provider_value()
            .cloned()
            .ok_or_else(|| Fail::new("还没配置", "设置 → 图床 → WebDAV：先填地址与用户名"))?;
        let mut c: Cfg = serde_yaml::from_value(raw)
            .map_err(|e| Fail::new("配置格式有误", format!("WebDAV 段解析失败：{e}")))?;
        c.url = c.url.trim().trim_end_matches('/').to_string();
        c.username = c.username.trim().to_string();
        c.public_base = c.public_base.trim().trim_end_matches('/').to_string();
        if c.url.is_empty() {
            return Err(Fail::new("还没配置", "WebDAV 地址没填"));
        }
        if !(c.url.starts_with("https://") || c.url.starts_with("http://")) {
            return Err(Fail::new("地址不合法", "WebDAV 地址必须以 http:// 或 https:// 开头"));
        }
        let password = secrets::get("webdav.password").unwrap_or_default();
        Ok(WebDav { cfg: c, password, proxy: cfg.proxy.clone() })
    }

    fn auth(&self) -> String {
        let raw = format!("{}:{}", self.cfg.username, self.password);
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw))
    }

    fn req(
        &self,
        method: &str,
        rel: &str,
        body: Option<&[u8]>,
        content_type: Option<&str>,
    ) -> Result<http::Response, String> {
        let url = link::join_url(&self.cfg.url, rel);
        let auth = self.auth();
        let mut headers: Vec<(&str, &str)> = vec![("Authorization", auth.as_str())];
        if let Some(ct) = content_type {
            headers.push(("Content-Type", ct));
        }
        http::request(method, &url, &headers, body, &self.proxy, 45_000)
    }

    fn check(&self, resp: http::Response, what: &str) -> Result<http::Response, Fail> {
        if (200..300).contains(&resp.status) {
            return Ok(resp);
        }
        Err(match resp.status {
            401 => Fail::new("认证失败", "用户名或密码不对（坚果云要用应用密码）"),
            403 => Fail::new("没有权限", format!("这个账号不能{what}该路径")),
            404 => Fail::new("目标不存在", "WebDAV 地址或目录不存在"),
            405 | 301 if what == "建目录" => Fail::new("目录已存在", "MKCOL 返回 405/301，按已存在处理"),
            409 => Fail::new("父目录不存在", "先用「浏览」确认路径，或让客户端自动补建"),
            423 => Fail::new("被锁定", "文件被其它客户端锁住，稍后再试"),
            507 => Fail::new("空间不足", "网盘配额用完了"),
            s => Fail::new("请求失败", format!("WebDAV HTTP {s}")),
        })
    }

    fn ensure_dir(&self, dir: &str) -> Result<(), Fail> {
        if dir.is_empty() {
            return Ok(());
        }
        let mut so_far = String::new();
        for seg in dir.split('/') {
            if seg.is_empty() {
                continue;
            }
            if !so_far.is_empty() {
                so_far.push('/');
            }
            so_far.push_str(seg);
            let resp = self
                .req("MKCOL", &so_far, None, None)
                .map_err(|e| Fail::new("网络不通", e))?;
            // 201 = 建好；405 = 已存在；301 = 有些服务端对已存在返回重定向
            if matches!(resp.status, 200..=299 | 405 | 301) {
                continue;
            }
            return Err(match self.check(resp, "建目录") {
                Err(e) => e,
                Ok(_) => Fail::new("建目录失败", "MKCOL 未被接受"),
            });
        }
        Ok(())
    }
}

impl Backend for WebDav {
    fn caps(&self) -> Caps {
        Caps { list_dirs: true, list_images: true, delete: true, public_url: true, max_mb: 100 }
    }

    fn test(&self) -> Result<TestReport, Fail> {
        let resp = self
            .req("PROPFIND", "", Some(PROPFIND_BODY.as_bytes()), Some("application/xml; charset=utf-8"))
            .map_err(|e| Fail::new("网络不通", e))?;
        self.check(resp, "读取")?;
        let demo = self.url("img/demo.webp")?;
        Ok(TestReport {
            lines: vec![
                format!("地址：{}", self.cfg.url),
                format!("账号：{}", if self.cfg.username.is_empty() { "<匿名>" } else { &self.cfg.username }),
                format!(
                    "直链：{}",
                    if self.cfg.public_base.is_empty() { "未填（部分网盘不提供外链）" } else { &self.cfg.public_base }
                ),
                format!("链接示例：{demo}"),
            ],
        })
    }

    fn upload(&self, req: &UploadRequest) -> Result<Uploaded, Fail> {
        self.ensure_dir(&req.dir)?;
        let path = req.path();
        let resp = self
            .req("PUT", &path, Some(&req.bytes), Some(req.content_type))
            .map_err(|e| Fail::new("网络不通", e))?;
        self.check(resp, "写入")?;
        Ok(Uploaded { path: path.clone(), url: self.url(&path)? })
    }

    fn list_dirs(&self, dir: &str) -> Result<Vec<String>, Fail> {
        let resp = self
            .req("PROPFIND", dir, Some(PROPFIND_BODY.as_bytes()), Some("application/xml; charset=utf-8"))
            .map_err(|e| Fail::new("网络不通", e))?;
        let body = self.check(resp, "读取")?.text();
        let mut entries = parse_propfind(&body);
        drop_self(&mut entries, dir);
        Ok(entries
            .into_iter()
            .filter(|e| e.collection)
            .filter_map(|e| last_segment(&e.href).filter(|s| !s.is_empty()))
            .collect())
    }

    fn list_images(&self, dir: &str) -> Result<Vec<RemoteImage>, Fail> {
        let resp = self
            .req("PROPFIND", dir, Some(PROPFIND_BODY.as_bytes()), Some("application/xml; charset=utf-8"))
            .map_err(|e| Fail::new("网络不通", e))?;
        let body = self.check(resp, "读取")?.text();
        let mut entries = parse_propfind(&body);
        drop_self(&mut entries, dir);
        let mut out = Vec::new();
        for e in entries {
            if e.collection {
                continue;
            }
            let Some(name) = last_segment(&e.href) else { continue };
            if !is_image(&name) {
                continue;
            }
            let path = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
            out.push(RemoteImage {
                name,
                url: self.url(&path).unwrap_or_default(),
                path,
                bytes: e.size.unwrap_or(0),
                modified: None,
            });
        }
        Ok(out)
    }

    fn delete(&self, path: &str) -> Result<(), Fail> {
        let resp = self.req("DELETE", path, None, None).map_err(|e| Fail::new("网络不通", e))?;
        self.check(resp, "删除")?;
        Ok(())
    }

    fn url(&self, path: &str) -> Result<String, Fail> {
        let base = if self.cfg.public_base.is_empty() { &self.cfg.url } else { &self.cfg.public_base };
        Ok(link::join_url(base, path))
    }
}

struct DavEntry {
    href: String,
    collection: bool,
    size: Option<u64>,
}

fn parse_propfind(xml: &str) -> Vec<DavEntry> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<DavEntry> = None;
    let mut tag = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = e.local_name().as_ref().to_string();
                match local.as_str() {
                    "response" => cur = Some(DavEntry { href: String::new(), collection: false, size: None }),
                    // <d:collection/> 是自闭合标签，必须走 Empty 分支
                    "collection" => {
                        if let Some(c) = cur.as_mut() {
                            c.collection = true;
                        }
                    }
                    "href" | "getcontentlength" => tag = local,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                let text = quick_xml::escape::unescape(t.as_ref()).map(|s| s.into_owned()).unwrap_or_else(|_| t.as_ref().to_string());
                if let Some(c) = cur.as_mut() {
                    match tag.as_str() {
                        "href" => c.href.push_str(&text),
                        "getcontentlength" => c.size = text.trim().parse().ok(),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let local = e.local_name().as_ref().to_string();
                if local == "response" {
                    if let Some(c) = cur.take() {
                        if !c.href.is_empty() {
                            out.push(c);
                        }
                    }
                }
                if local == "href" || local == "getcontentlength" {
                    tag.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

/// href 的最后一段（去掉查询与末尾斜杠，百分号解码）。
fn last_segment(href: &str) -> Option<String> {
    let path = href.split(['?', '#']).next().unwrap_or(href);
    let trimmed = path.trim_end_matches('/');
    let seg = trimmed.rsplit('/').next().unwrap_or("");
    if seg.is_empty() {
        return None;
    }
    Some(percent_decode(seg))
}

/// PROPFIND 会带上"被请求的目录自己"。按 href 后缀找出来删掉；找不到就删第一条
/// （RFC 4918 的示例与各家实现都把它放第一条）。
fn drop_self(entries: &mut Vec<DavEntry>, dir: &str) {
    if let Some(i) = entries.iter().position(|e| is_self(&e.href, dir)) {
        entries.remove(i);
    } else if !entries.is_empty() {
        entries.remove(0);
    }
}

fn is_self(href: &str, dir: &str) -> bool {
    let path = href.split(['?', '#']).next().unwrap_or(href);
    let decoded = percent_decode(path.trim_end_matches('/'));
    let want = format!("/{}", dir.trim_matches('/'));
    decoded.ends_with(&want)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn is_image(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg", ".avif", ".tif", ".tiff"]
        .iter()
        .any(|e| l.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_propfind_with_namespaces() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
 <d:response><d:href>/dav/img/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
 <d:response><d:href>/dav/img/a%20b.webp</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1234</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let list = parse_propfind(xml);
        assert_eq!(list.len(), 2);
        assert!(list[0].collection);
        assert_eq!(last_segment(&list[0].href).as_deref(), Some("img"));
        assert_eq!(last_segment(&list[1].href).as_deref(), Some("a b.webp"));
        assert_eq!(list[1].size, Some(1234));
    }

    #[test]
    fn percent_decode_utf8() {
        assert_eq!(percent_decode("%E4%B8%AD%E6%96%87.webp"), "中文.webp");
    }
}
