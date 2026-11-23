//! 自定义 HTTP 图床（Lsky Pro / EasyImage / 自建接口）。
//!
//! 只做一件事：把文件按 multipart/form-data 发到用户填的地址，再从返回 JSON 里取 URL。
//! 不列目录、不列图、不删除——这类自建图床没有统一协议，猜不如不猜。
//!
//! 覆盖的典型配置：
//! - Lsky Pro：`POST /api/v1/upload`，文件字段 `file`，头 `Authorization: Bearer <token>`，
//!   URL 路径 `data.links.url`；
//! - EasyImage：`POST /api/index.php/upload`，文件字段 `image`，头 `Authorization: <token>`。

use serde::Deserialize;
use serde_json::Value;

use super::super::http;
use super::super::link;
use super::super::provider::{
    Backend, Caps, Fail, RemoteImage, TestReport, UploadRequest, Uploaded,
};
use super::super::secrets;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Cfg {
    pub url: String,
    pub method: String,
    pub file_field: String,
    pub token_header: String,
    pub token_prefix: String,
    pub path_field: String,
    pub url_json_path: String,
    pub public_base: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, String>,
}

pub struct Custom {
    cfg: Cfg,
    token: String,
    proxy: String,
}

impl Custom {
    pub fn from_settings(cfg: &zx_core::ImgBedSettings) -> Result<Custom, Fail> {
        let raw = cfg
            .provider_value()
            .cloned()
            .ok_or_else(|| Fail::new("还没配置", "设置 → 图床 → 自定义 HTTP：先填上传接口"))?;
        let mut c: Cfg = serde_yaml::from_value(raw)
            .map_err(|e| Fail::new("配置格式有误", format!("自定义段解析失败：{e}")))?;
        c.url = c.url.trim().to_string();
        if c.url.is_empty() {
            return Err(Fail::new("还没配置", "上传接口地址没填"));
        }
        if !(c.url.starts_with("https://") || c.url.starts_with("http://")) {
            return Err(Fail::new("地址不合法", "上传接口必须以 http:// 或 https:// 开头"));
        }
        c.method = if c.method.eq_ignore_ascii_case("put") { "PUT".into() } else { "POST".into() };
        if c.file_field.trim().is_empty() {
            c.file_field = "file".into();
        }
        if c.url_json_path.trim().is_empty() {
            c.url_json_path = "data.url".into();
        }
        if c.token_header.trim().is_empty() {
            c.token_header = "Authorization".into();
        }
        let token = secrets::get("custom.token").unwrap_or_default();
        Ok(Custom { cfg: c, token, proxy: cfg.proxy.clone() })
    }

    fn upload_url(&self) -> String {
        self.cfg.url.clone()
    }

    fn extract_url(&self, body: &str, path: &str) -> Result<String, Fail> {
        let v: Value = serde_json::from_str(body)
            .map_err(|e| Fail::new("响应不是 JSON", format!("{e}；响应开头：{}", take(body))))?;
        // 支持 data.links.url 这种点路径，也支持数组下标 a.0.b
        let mut cur = &v;
        for seg in path.split('.') {
            if seg.is_empty() {
                continue;
            }
            cur = if let Ok(i) = seg.parse::<usize>() {
                cur.get(i)
            } else {
                cur.get(seg)
            }
            .ok_or_else(|| {
                Fail::new("响应里没有链接", format!("字段 {path} 不存在；响应：{}", take(body)))
            })?;
        }
        let url = cur
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| cur.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
            .ok_or_else(|| Fail::new("响应里没有链接", format!("{path} 不是字符串：{}", take(body))))?;
        if url.starts_with("http://") || url.starts_with("https://") {
            Ok(url)
        } else if !self.cfg.public_base.is_empty() {
            Ok(link::join_url(&self.cfg.public_base, url.trim_start_matches('/')))
        } else {
            Ok(url)
        }
    }
}

impl Backend for Custom {
    fn caps(&self) -> Caps {
        Caps { list_dirs: false, list_images: false, delete: false, public_url: true, max_mb: 100 }
    }

    fn test(&self) -> Result<TestReport, Fail> {
        // 不真发文件（会往用户图床里塞垃圾）：只探活 + 看鉴权是否被拒
        let mut headers: Vec<(&str, &str)> = Vec::new();
        let token_value;
        if !self.token.is_empty() {
            token_value = format!("{}{}", self.cfg.token_prefix, self.token);
            headers.push((self.cfg.token_header.as_str(), token_value.as_str()));
        }
        let resp = http::request("OPTIONS", &self.upload_url(), &headers, None, &self.proxy, 20_000)
            .map_err(|e| Fail::new("网络不通", e))?;
        if resp.status == 401 || resp.status == 403 {
            return Err(Fail::new("认证失败", format!("接口可达，但鉴权被拒（HTTP {}）", resp.status)));
        }
        let mut lines = vec![
            format!("接口：{} {}", self.cfg.method, self.cfg.url),
            format!("文件字段：{}", self.cfg.file_field),
            format!("URL 路径：{}", self.cfg.url_json_path),
            format!("探测：HTTP {}（真实写入请用「上传测试图」）", resp.status),
        ];
        if self.cfg.path_field.is_empty() {
            lines.push("路径：该接口不支持指定路径".into());
        }
        Ok(TestReport { lines })
    }

    fn upload(&self, req: &UploadRequest) -> Result<Uploaded, Fail> {
        let boundary = format!(
            "----Zhuxia{}",
            crate::path::short_hash(
                format!("{}{}", req.path(), req.bytes.len()).as_bytes()
            )
        );
        let mut body: Vec<u8> = Vec::with_capacity(req.bytes.len() + 1024);
        for (k, v) in &self.cfg.fields {
            push_field(&mut body, &boundary, k, v);
        }
        if !self.cfg.path_field.is_empty() && !req.dir.is_empty() {
            push_field(&mut body, &boundary, &self.cfg.path_field, &req.dir);
        }
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                self.cfg.file_field, req.name
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", req.content_type).as_bytes());
        body.extend_from_slice(&req.bytes);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let content_type = format!("multipart/form-data; boundary={boundary}");
        let token_value;
        let mut headers: Vec<(&str, &str)> = vec![("Content-Type", content_type.as_str())];
        if !self.token.is_empty() {
            token_value = format!("{}{}", self.cfg.token_prefix, self.token);
            headers.push((self.cfg.token_header.as_str(), token_value.as_str()));
        }
        // 自定义头放在后面，允许覆盖默认值（Content-Type 除外）
        let mut extra: Vec<(String, String)> = Vec::new();
        for (k, v) in &self.cfg.headers {
            if k.eq_ignore_ascii_case("content-type") || k.eq_ignore_ascii_case("content-length") {
                continue;
            }
            extra.push((k.clone(), v.clone()));
        }
        let mut all: Vec<(&str, &str)> = headers;
        for (k, v) in &extra {
            all.push((k.as_str(), v.as_str()));
        }

        let resp = http::request(
            &self.cfg.method,
            &self.upload_url(),
            &all,
            Some(&body),
            &self.proxy,
            60_000,
        )
        .map_err(|e| Fail::new("网络不通", e))?;
        let status = resp.status;
        let text = resp.text();
        if !(200..300).contains(&status) {
            return Err(match status {
                401 | 403 => Fail::new("认证失败", "token 无效或没有上传权限"),
                413 => Fail::new("文件太大", "接口拒绝了体积（413）"),
                429 => Fail::retry("被限流", "接口限流，稍后再试"),
                s => Fail::new("上传失败", format!("HTTP {s}：{}", take(&text))),
            });
        }
        let url = self.extract_url(&text, &self.cfg.url_json_path)?;
        Ok(Uploaded { path: req.path(), url })
    }

    fn list_dirs(&self, _dir: &str) -> Result<Vec<String>, Fail> {
        Err(Fail::new("不支持", "自定义接口没有目录列表能力，请手动输入路径"))
    }

    fn list_images(&self, _dir: &str) -> Result<Vec<RemoteImage>, Fail> {
        Err(Fail::new("不支持", "自定义接口没有列图能力"))
    }

    fn delete(&self, _path: &str) -> Result<(), Fail> {
        Err(Fail::new("不支持", "自定义接口没有删除能力"))
    }

    fn url(&self, path: &str) -> Result<String, Fail> {
        if self.cfg.public_base.is_empty() {
            Ok(path.to_string())
        } else {
            Ok(link::join_url(&self.cfg.public_base, path))
        }
    }
}

fn push_field(body: &mut Vec<u8>, boundary: &str, key: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{key}\"\r\n\r\n").as_bytes());
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn take(s: &str) -> String {
    s.trim().chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> Custom {
        Custom {
            cfg: Cfg {
                url: "https://img.x/upload".into(),
                method: "POST".into(),
                file_field: "file".into(),
                url_json_path: "data.links.url".into(),
                public_base: "https://cdn.x".into(),
                ..Default::default()
            },
            token: String::new(),
            proxy: String::new(),
        }
    }

    #[test]
    fn extracts_nested_url() {
        let c = custom();
        let body = r#"{"status":true,"data":{"links":{"url":"/i/a.webp"}}}"#;
        assert_eq!(c.extract_url(body, "data.links.url").unwrap(), "https://cdn.x/i/a.webp");
    }

    #[test]
    fn reports_missing_path() {
        let c = custom();
        let err = c.extract_url("{\"data\":{}}", "data.links.url").unwrap_err();
        assert_eq!(err.stage, "响应里没有链接");
    }
}
