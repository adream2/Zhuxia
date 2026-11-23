//! S3 兼容存储：AWS S3 / Cloudflare R2 / 腾讯云 COS / MinIO / Backblaze B2。
//!
//! 只实现三种操作：PutObject、ListObjectsV2、DeleteObject。SigV4 自己签，
//! 不引 AWS SDK（那是几 MB 的运行时代价）。签名算法用 RFC 4231 的 HMAC 向量单测过。
//!
//! 两个容易错的地方：
//! - canonical URI 必须对每个路径段百分号编码，`/` 保留；
//! - 虚拟主机风格要把 bucket 前缀拼到 host 上，path-style（MinIO）则拼到路径上，
//!   两条路的 canonical URI 不一样。

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::super::http;
use super::super::link;
use super::super::provider::{
    Backend, Caps, Fail, RemoteImage, TestReport, UploadRequest, Uploaded,
};
use super::super::secrets;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Cfg {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub public_base: String,
    pub path_style: bool,
    pub path: String,
}

pub struct S3 {
    cfg: Cfg,
    secret: String,
    proxy: String,
    /// 解析后的 endpoint：(scheme, host, port, base_path)
    endpoint: (String, String, u16, String),
}

impl S3 {
    pub fn from_settings(cfg: &zx_core::ImgBedSettings) -> Result<S3, Fail> {
        let raw = cfg
            .provider_value()
            .cloned()
            .ok_or_else(|| Fail::new("还没配置", "设置 → 图床 → S3：先填 endpoint / bucket / AK"))?;
        let c: Cfg = serde_yaml::from_value(raw)
            .map_err(|e| Fail::new("配置格式有误", format!("S3 段解析失败：{e}")))?;
        let c = Cfg {
            endpoint: c.endpoint.trim().to_string(),
            region: if c.region.trim().is_empty() { "auto".into() } else { c.region.trim().to_string() },
            bucket: c.bucket.trim().to_string(),
            access_key_id: c.access_key_id.trim().to_string(),
            public_base: c.public_base.trim().trim_end_matches('/').to_string(),
            path_style: c.path_style,
            path: c.path,
        };
        if c.endpoint.is_empty() || c.bucket.is_empty() || c.access_key_id.is_empty() {
            return Err(Fail::new("还没配置", "S3 的 endpoint / bucket / Access Key 都要填"));
        }
        let endpoint = parse_endpoint(&c.endpoint)?;
        let secret = secrets::get("s3.secretAccessKey")
            .ok_or_else(|| Fail::new("还没配置", "S3 Secret Access Key 没填"))?;
        Ok(S3 { cfg: c, secret, proxy: cfg.proxy.clone(), endpoint })
    }

    fn host(&self) -> String {
        if self.cfg.path_style {
            self.endpoint.1.clone()
        } else {
            format!("{}.{}", self.cfg.bucket, self.endpoint.1)
        }
    }

    /// 签名里的 host 要带非默认端口（MinIO 常见 9000）。
    fn host_header(&self) -> String {
        let default = (self.endpoint.0 == "https" && self.endpoint.2 == 443)
            || (self.endpoint.0 == "http" && self.endpoint.2 == 80);
        if default {
            self.host()
        } else {
            format!("{}:{}", self.host(), self.endpoint.2)
        }
    }

    /// object key → URL（不带签名；公开桶/CDN 可直接访问）
    fn object_url(&self, key: &str) -> String {
        if !self.cfg.public_base.is_empty() {
            return link::join_url(&self.cfg.public_base, key);
        }
        let scheme = &self.endpoint.0;
        let port = if (scheme == "https" && self.endpoint.2 == 443)
            || (scheme == "http" && self.endpoint.2 == 80)
        {
            String::new()
        } else {
            format!(":{}", self.endpoint.2)
        };
        let host = self.host();
        let base = if self.cfg.path_style {
            format!("{scheme}://{host}{port}{}/{}", self.endpoint.3.trim_end_matches('/'), self.cfg.bucket)
        } else {
            format!("{scheme}://{host}{port}{}", self.endpoint.3.trim_end_matches('/'))
        };
        link::join_url(&base, key)
    }

    fn signed(
        &self,
        method: &str,
        key_or_root: &str,
        query: &[(&str, &str)],
        content_type: Option<&str>,
        bytes: Option<&[u8]>,
    ) -> Result<http::Response, Fail> {
        let (amz_date, datestamp) = utc_stamp();
        let payload_hash = hex(&Sha256::digest(bytes.unwrap_or(b"")));
        let canonical_uri = self.canonical_uri(key_or_root);
        let canonical_query = canonical_query(query);

        let mut headers = String::new();
        if let Some(ct) = content_type {
            headers.push_str(&format!("content-type:{}\n", ct.trim()));
        }
        headers.push_str(&format!("host:{}\n", self.host_header()));
        headers.push_str(&format!("x-amz-content-sha256:{payload_hash}\n"));
        headers.push_str(&format!("x-amz-date:{amz_date}\n"));
        let signed_headers = if content_type.is_some() {
            "content-type;host;x-amz-content-sha256;x-amz-date"
        } else {
            "host;x-amz-content-sha256;x-amz-date"
        };

        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{headers}\n{signed_headers}\n{payload_hash}"
        );
        let scope = format!("{datestamp}/{}/s3/aws4_request", self.cfg.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical_request.as_bytes()))
        );
        let k_date = hmac(format!("AWS4{}", self.secret).as_bytes(), datestamp.as_bytes());
        let k_region = hmac(&k_date, self.cfg.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.cfg.access_key_id
        );

        let mut url = self.object_url_base(key_or_root);
        if !key_or_root.is_empty() {
            if !url.ends_with('/') {
                url.push('/');
            }
            url.push_str(&link::encode_path(key_or_root.trim_start_matches('/')));
        }
        if !canonical_query.is_empty() {
            if !url.ends_with('/') {
                url.push('/');
            }
            url.push('?');
            url.push_str(&canonical_query);
        }

        let mut hdrs: Vec<(&str, &str)> = vec![
            ("Authorization", authorization.as_str()),
            ("x-amz-content-sha256", payload_hash.as_str()),
            ("x-amz-date", amz_date.as_str()),
        ];
        if let Some(ct) = content_type {
            hdrs.push(("Content-Type", ct));
        }
        http::request(method, &url, &hdrs, bytes, &self.proxy, 60_000).map_err(|e| {
            Fail::new("网络不通", format!("{e}（连不上 S3 endpoint，检查地址/代理）"))
        })
    }

    /// 不带 query 的 URL（虚拟主机 / path-style 都已处理）
    fn object_url_base(&self, _key: &str) -> String {
        let scheme = &self.endpoint.0;
        let port = if (scheme == "https" && self.endpoint.2 == 443)
            || (scheme == "http" && self.endpoint.2 == 80)
        {
            String::new()
        } else {
            format!(":{}", self.endpoint.2)
        };
        let base_path = self.endpoint.3.trim_end_matches('/');
        if self.cfg.path_style {
            format!(
                "{scheme}://{}{port}{base_path}/{}",
                self.endpoint.1, self.cfg.bucket
            )
        } else {
            format!("{scheme}://{}{port}{base_path}", self.host())
        }
    }

    fn canonical_uri(&self, key: &str) -> String {
        // endpoint 可能带 base path（如 /s3）；签名必须与真实请求路径一致。
        let mut p = self.endpoint.3.clone();
        if p.is_empty() {
            p.push('/');
        }
        if self.cfg.path_style {
            if !p.ends_with('/') {
                p.push('/');
            }
            p.push_str(&link::encode(&self.cfg.bucket));
        }
        if !p.ends_with('/') {
            p.push('/');
        }
        if !key.is_empty() {
            p.push_str(&link::encode_path(key.trim_start_matches('/')));
        }
        p
    }

    fn check(&self, resp: http::Response, what: &str) -> Result<http::Response, Fail> {
        if (200..300).contains(&resp.status) {
            return Ok(resp);
        }
        let body = resp.text();
        let msg = xml_error(&body).unwrap_or_else(|| body.chars().take(160).collect());
        Err(match resp.status {
            403 if msg.contains("SignatureDoesNotMatch") => Fail::new(
                "签名错误",
                "检查 Secret Access Key、region 与本机时间（差超过 15 分钟会失败）",
            ),
            403 => Fail::new("没有权限", format!("AK 没有这个桶的{what}权限（{msg}）")),
            404 => Fail::new("目标不存在", format!("bucket 或路径不存在（{msg}）")),
            409 => Fail::new("冲突", msg),
            s if s >= 500 => Fail::retry("服务端错误", format!("S3 HTTP {s}：{msg}")),
            s => Fail::new("请求失败", format!("HTTP {s}：{msg}")),
        })
    }
}

impl Backend for S3 {
    fn caps(&self) -> Caps {
        Caps { list_dirs: true, list_images: true, delete: true, public_url: true, max_mb: 100 }
    }

    fn test(&self) -> Result<TestReport, Fail> {
        // ListObjectsV2 取一条：能证明 endpoint、签名、桶权限都对
        let resp = self.signed("GET", "", &[("list-type", "2"), ("max-keys", "1")], None, None)?;
        let resp = self.check(resp, "读")?;
        let _ = resp;
        let demo = self.url("demo/test.webp")?;
        let mut lines = vec![
            format!("Endpoint：{}", self.cfg.endpoint),
            format!("Bucket：{}（region {}）", self.cfg.bucket, self.cfg.region),
            format!("风格：{}", if self.cfg.path_style { "path-style" } else { "virtual-host" }),
        ];
        if self.cfg.public_base.is_empty() {
            lines.push("公开域名：未填（链接只能在控制台里看）".into());
        } else {
            lines.push(format!("公开域名：{}", self.cfg.public_base));
        }
        lines.push(format!("链接示例：{demo}"));
        Ok(TestReport { lines })
    }

    fn upload(&self, req: &UploadRequest) -> Result<Uploaded, Fail> {
        let key = req.path();
        let resp = self.signed("PUT", &key, &[], Some(req.content_type), Some(&req.bytes))?;
        self.check(resp, "写")?;
        Ok(Uploaded { path: key.clone(), url: self.url(&key)? })
    }

    fn list_dirs(&self, dir: &str) -> Result<Vec<String>, Fail> {
        let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_matches('/')) };
        let resp = self.signed(
            "GET",
            "",
            &[("delimiter", "/"), ("list-type", "2"), ("prefix", &prefix)],
            None,
            None,
        )?;
        let body = self.check(resp, "读")?.text();
        Ok(parse_list(&body).0)
    }

    fn list_images(&self, dir: &str) -> Result<Vec<RemoteImage>, Fail> {
        let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_matches('/')) };
        let resp = self.signed(
            "GET",
            "",
            &[("list-type", "2"), ("prefix", &prefix)],
            None,
            None,
        )?;
        let body = self.check(resp, "读")?.text();
        let (_, files) = parse_list(&body);
        Ok(files
            .into_iter()
            .filter(|(k, _)| is_image(k))
            .map(|(k, size)| RemoteImage {
                name: k.rsplit('/').next().unwrap_or(&k).to_string(),
                url: self.url(&k).unwrap_or_default(),
                path: k,
                bytes: size,
                modified: None,
            })
            .collect())
    }

    fn delete(&self, path: &str) -> Result<(), Fail> {
        let resp = self.signed("DELETE", path, &[], None, None)?;
        self.check(resp, "删除")?;
        Ok(())
    }

    fn url(&self, path: &str) -> Result<String, Fail> {
        Ok(self.object_url(path))
    }
}

// ---------------------------------------------------------------- 工具

fn parse_endpoint(s: &str) -> Result<(String, String, u16, String), Fail> {
    let (scheme, rest) = if let Some(r) = s.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = s.strip_prefix("http://") {
        ("http", r)
    } else {
        return Err(Fail::new("Endpoint 不合法", "必须以 http:// 或 https:// 开头"));
    };
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.parse::<u16>().unwrap_or(443))
        }
        _ => (host_port.to_string(), if scheme == "https" { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(Fail::new("Endpoint 不合法", "缺少主机名"));
    }
    Ok((scheme.into(), host, port, path.trim_end_matches('/').to_string()))
}

fn canonical_query(q: &[(&str, &str)]) -> String {
    let mut items: Vec<(String, String)> = q
        .iter()
        .map(|(k, v)| (link::encode(k), link::encode(v)))
        .collect();
    items.sort();
    items
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let out = outer.finalize();
    let mut r = [0u8; 32];
    r.copy_from_slice(&out);
    r
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// UTC 时间戳（避免为两行日期格式化拉 chrono）。
fn utc_stamp() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (y, m, d) = civil_from_days(days);
    (
        format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60),
        format!("{y:04}{m:02}{d:02}"),
    )
}

/// Howard Hinnant 的 days → civil date（1970-01-01 起）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// ListObjectsV2 XML → (目录前缀列表, [(key, size)])。不引 XML 树，只跟局部名走。
fn parse_list(xml: &str) -> (Vec<String>, Vec<(String, u64)>) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut dirs = Vec::new();
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut in_common = false;
    let mut in_contents = false;
    let mut tag = String::new();
    let mut key = String::new();
    let mut size = 0u64;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = e.local_name().as_ref().to_string();
                match local.as_str() {
                    "CommonPrefixes" => in_common = true,
                    "Contents" => in_contents = true,
                    "Key" | "Prefix" | "Size" => tag = local,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                let text = quick_xml::escape::unescape(t.as_ref()).map(|s| s.into_owned()).unwrap_or_else(|_| t.as_ref().to_string());
                match tag.as_str() {
                    "Prefix" if in_common => dirs.push(strip_prefix(&text)),
                    "Key" if in_contents => key = text,
                    "Size" if in_contents => size = text.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let local = e.local_name().as_ref().to_string();
                match local.as_str() {
                    "CommonPrefixes" => in_common = false,
                    "Contents" => {
                        if !key.is_empty() {
                            files.push((std::mem::take(&mut key), size));
                            size = 0;
                        }
                        in_contents = false;
                    }
                    _ => {}
                }
                if local == "Key" || local == "Prefix" || local == "Size" {
                    tag.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    (dirs, files)
}

fn strip_prefix(p: &str) -> String {
    p.trim_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

fn xml_error(body: &str) -> Option<String> {
    let start = body.find("<Message>")? + 9;
    let end = body[start..].find("</Message>")? + start;
    Some(body[start..end].to_string())
}

fn is_image(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg", ".avif", ".tif", ".tiff"]
        .iter()
        .any(|e| l.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231() {
        // RFC 4231 case 1
        let key = [0x0bu8; 20];
        let out = hmac(&key, b"Hi There");
        assert_eq!(
            hex(&out),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn sha256_empty_vector() {
        assert_eq!(hex(&Sha256::digest(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn canonical_query_is_sorted_and_encoded() {
        let q = [("prefix", "a b/"), ("list-type", "2"), ("delimiter", "/")];
        assert_eq!(canonical_query(&q), "delimiter=%2F&list-type=2&prefix=a%20b%2F");
    }

    #[test]
    fn parses_list_objects_xml() {
        let xml = r#"<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
<CommonPrefixes><Prefix>img/2026/</Prefix></CommonPrefixes>
<Contents><Key>img/2026/a.webp</Key><Size>123</Size><LastModified>2026-11-22T00:00:00Z</LastModified></Contents>
</ListBucketResult>"#;
        let (dirs, files) = parse_list(xml);
        assert_eq!(dirs, vec!["2026"]);
        assert_eq!(files, vec![("img/2026/a.webp".to_string(), 123)]);
    }

    #[test]
    fn civil_dates_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }

    #[test]
    fn endpoint_parsing() {
        let (s, h, p, base) = parse_endpoint("https://acct.r2.cloudflarestorage.com").unwrap();
        assert_eq!((s.as_str(), h.as_str(), p, base.as_str()), ("https", "acct.r2.cloudflarestorage.com", 443, ""));
        let (_, h, p, base) = parse_endpoint("http://127.0.0.1:9000/s3").unwrap();
        assert_eq!((h.as_str(), p, base.as_str()), ("127.0.0.1", 9000, "/s3"));
    }
}
