//! GitHub 仓库当图床：Contents API（单文件）+ Git Data API（批量预留给 CLI）。
//!
//! 关键细节（都按 GitHub 文档核对过）：
//! - Contents 响应 200 也要核对 `content.size`，字节数不对说明传的不是原图；
//! - 删除必须先取 `sha` 再 DELETE；
//! - 列目录用 `/contents/{dir}`，只认 `type == "dir"`；
//! - 分支名里的 `/` 是 ref 的一部分，不能整段编码。

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Value};

use super::super::http;
use super::super::link;
use super::super::provider::{
    Backend, Caps, Fail, RemoteImage, TestReport, UploadRequest, Uploaded,
};
use super::super::secrets;

const API: &str = "https://api.github.com";

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Cfg {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub cdn: String,
    pub cdn_custom: String,
    pub commit_message: String,
}

impl Cfg {
    fn normalized(mut self) -> Cfg {
        if self.cdn.trim().is_empty() {
            self.cdn = "jsdelivr".into();
        }
        if self.commit_message.trim().is_empty() {
            self.commit_message = "Upload image via Zhuxia".into();
        }
        self.owner = self.owner.trim().to_string();
        self.repo = self.repo.trim().to_string();
        self.branch = self.branch.trim().to_string();
        self
    }
}

pub struct Github {
    cfg: Cfg,
    token: String,
    proxy: String,
    /// 空 branch 时从仓库信息里取默认分支，只取一次
    branch_cache: OnceLock<String>,
}

impl Github {
    pub fn from_settings(cfg: &zx_core::ImgBedSettings) -> Result<Github, Fail> {
        let raw = cfg
            .provider_value()
            .cloned()
            .ok_or_else(|| Fail::new("还没配置", "设置 → 图床 → GitHub：先填账号和仓库"))?;
        let parsed: Cfg = serde_yaml::from_value(raw)
            .map_err(|e| Fail::new("配置格式有误", format!("GitHub 段解析失败：{e}")))?;
        let c = parsed.normalized();
        if c.owner.is_empty() || c.repo.is_empty() {
            return Err(Fail::new("还没配置", "GitHub 账号和仓库名都要填"));
        }
        link::template_for(&c.cdn, &c.cdn_custom).map_err(|e| Fail::new("链接规则有误", e))?;
        let token = secrets::get("github.token")
            .ok_or_else(|| Fail::new("还没配置", "GitHub Token 没填（设置 → 图床）"))?;
        Ok(Github { cfg: c, token, proxy: cfg.proxy.clone(), branch_cache: OnceLock::new() })
    }

    fn repo_path(&self) -> String {
        format!("/repos/{}/{}", link::encode(&self.cfg.owner), link::encode(&self.cfg.repo))
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, Fail> {
        // 测试/企业版可替换 API 根；生产永远默认 api.github.com。
        let base = std::env::var("ZHUXIA_GITHUB_API").unwrap_or_else(|_| API.to_string());
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let body = body.map(|v| serde_json::to_vec(&v).unwrap_or_default());
        let auth = format!("Bearer {}", self.token);
        let headers = [
            ("Authorization", auth.as_str()),
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", "2022-11-28"),
            ("User-Agent", "Zhuxia-ImgBed"),
            ("Content-Type", "application/json"),
        ];
        let resp = http::request(method, &url, &headers, body.as_deref(), &self.proxy, 30_000)
            .map_err(|e| Fail::new("网络不通", format!("{e}（上传要连 api.github.com，可能需要代理）")))?;
        let text = resp.text();
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&resp.status) {
            return Ok(json);
        }
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let rate = resp.rate_remaining == Some(0) || text.to_ascii_lowercase().contains("rate limit");
        Err(match resp.status {
            401 => Fail::new("认证失败", "Token 无效或过期；需要 Contents: Read and write"),
            403 if rate => {
                let wait = resp
                    .rate_reset
                    .map(|r| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        format!("约 {} 分钟后恢复", r.saturating_sub(now).div_ceil(60))
                    })
                    .unwrap_or_else(|| "稍后恢复".into());
                Fail::retry("被限流", format!("GitHub 限流，{wait}"))
            }
            403 => Fail::new("没有写权限", format!("这个 token 不能写该仓库（{msg}）")),
            404 => Fail::new(
                "目标不存在",
                format!("仓库 / 分支 / 路径不存在，或 token 看不到它（{}）", short(&msg)),
            ),
            409 => Fail::retry("冲突", "同名文件刚被创建，重试一次"),
            422 => Fail::new("被拒绝", format!("GitHub 拒绝写入：{}", short(&msg))),
            s if s >= 500 => Fail::retry("服务端错误", format!("GitHub HTTP {s}")),
            s => Fail::new("上传失败", format!("HTTP {s}：{}", short(&msg))),
        })
    }

    fn branch_name(&self) -> Result<String, Fail> {
        if !self.cfg.branch.is_empty() {
            return Ok(self.cfg.branch.clone());
        }
        if let Some(b) = self.branch_cache.get() {
            return Ok(b.clone());
        }
        let v = self.call("GET", &self.repo_path(), None)?;
        let b = v
            .get("default_branch")
            .and_then(|x| x.as_str())
            .unwrap_or("main")
            .to_string();
        let _ = self.branch_cache.set(b.clone());
        Ok(b)
    }

    fn content_path(&self, path: &str) -> String {
        format!("{}/contents/{}", self.repo_path(), link::encode_path(path))
    }

    fn ref_path(&self, branch: &str) -> String {
        format!("{}/git/ref/heads/{}", self.repo_path(), link::encode_path(branch))
    }
}

fn short(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() { "没有更多信息".into() } else { s.chars().take(160).collect() }
}

impl Backend for Github {
    fn caps(&self) -> Caps {
        Caps { list_dirs: true, list_images: true, delete: true, public_url: true, max_mb: 100 }
    }

    fn test(&self) -> Result<TestReport, Fail> {
        let login = self
            .call("GET", "/user", None)?
            .get("login")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let repo = self.call("GET", &self.repo_path(), None)?;
        let full = repo.get("full_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let private = repo.get("private").and_then(|x| x.as_bool()).unwrap_or(false);
        let can_push = repo.pointer("/permissions/push").and_then(|x| x.as_bool()).unwrap_or(false);
        let size_kb = repo.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
        let branch = self.branch_name()?;
        let exists = match self.call("GET", &self.ref_path(&branch), None) {
            Ok(_) => true,
            Err(e) if e.stage == "目标不存在" => false,
            Err(e) => return Err(e),
        };
        if !exists {
            return Err(Fail::new("分支不存在", format!("分支 {branch} 在这个仓库里不存在")));
        }
        let demo_path = format!("{}/demo.webp", self.cfg.path.trim_matches('/'));
        let demo = self.url(demo_path.trim_start_matches('/'))?;
        Ok(TestReport {
            lines: vec![
                format!("账号：{login}"),
                format!("仓库：{full}（{}）", if private { "私有" } else { "公开" }),
                format!("分支：{branch}"),
                format!("写权限：{}", if can_push { "有" } else { "没读到（fine-grained token 常见，能传就行）" }),
                format!("仓库体积：{:.1} MB", size_kb as f64 / 1024.0),
                format!("链接示例：{}", demo),
            ],
        })
    }

    fn upload(&self, req: &UploadRequest) -> Result<Uploaded, Fail> {
        use base64::Engine;
        let branch = self.branch_name()?;
        let path = req.path();
        let body = json!({
            "message": self.cfg.commit_message,
            "content": base64::engine::general_purpose::STANDARD.encode(&req.bytes),
            "branch": branch,
        });
        let v = self.call("PUT", &self.content_path(&path), Some(body))?;
        let got = v.pointer("/content/path").and_then(|x| x.as_str()).unwrap_or("");
        if !got.is_empty() && got != path {
            return Err(Fail::new("响应异常", format!("服务端写入 {got}，不是 {path}")));
        }
        if let Some(sz) = v.pointer("/content/size").and_then(|x| x.as_u64()) {
            if sz != req.bytes.len() as u64 {
                return Err(Fail::new(
                    "内容不完整",
                    format!("服务端收到 {sz} 字节，本地 {} 字节", req.bytes.len()),
                ));
            }
        }
        Ok(Uploaded { path: path.clone(), url: self.url(&path)? })
    }

    fn list_dirs(&self, dir: &str) -> Result<Vec<String>, Fail> {
        let path = if dir.is_empty() { self.repo_path() + "/contents" } else { self.content_path(dir) };
        let v = match self.call("GET", &format!("{path}?ref={}", link::encode(&self.branch_name()?)), None) {
            Ok(v) => v,
            Err(e) if e.stage == "目标不存在" => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let Some(arr) = v.as_array() else { return Ok(Vec::new()) };
        Ok(arr
            .iter()
            .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("dir"))
            .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .collect())
    }

    fn list_images(&self, dir: &str) -> Result<Vec<RemoteImage>, Fail> {
        let path = if dir.is_empty() { self.repo_path() + "/contents" } else { self.content_path(dir) };
        let v = match self.call("GET", &format!("{path}?ref={}", link::encode(&self.branch_name()?)), None) {
            Ok(v) => v,
            Err(e) if e.stage == "目标不存在" => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let Some(arr) = v.as_array() else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for e in arr {
            if e.get("type").and_then(|t| t.as_str()) != Some("file") {
                continue;
            }
            let p = e.get("path").and_then(|x| x.as_str()).unwrap_or("");
            if !is_image(p) {
                continue;
            }
            out.push(RemoteImage {
                path: p.to_string(),
                name: e.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                bytes: e.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
                modified: None,
                url: self.url(p).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    fn delete(&self, path: &str) -> Result<(), Fail> {
        let branch = self.branch_name()?;
        let cur = self.call("GET", &format!("{}?ref={}", self.content_path(path), link::encode(&branch)), None)?;
        let sha = cur.get("sha").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if sha.is_empty() {
            return Err(Fail::new("删除失败", "拿不到文件 sha"));
        }
        self.call(
            "DELETE",
            &self.content_path(path),
            Some(json!({ "message": format!("Delete {path} via Zhuxia"), "sha": sha, "branch": branch })),
        )?;
        Ok(())
    }

    fn url(&self, path: &str) -> Result<String, Fail> {
        let tpl = link::template_for(&self.cfg.cdn, &self.cfg.cdn_custom)
            .map_err(|e| Fail::new("链接规则有误", e))?;
        let branch = self.branch_name()?;
        Ok(link::resolve(&tpl, &self.cfg.owner, &self.cfg.repo, &branch, path))
    }
}

fn is_image(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg", ".avif", ".tif", ".tiff"]
        .iter()
        .any(|e| lower.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_normalizes_defaults() {
        let c = Cfg::default().normalized();
        assert_eq!(c.cdn, "jsdelivr");
        assert_eq!(c.commit_message, "Upload image via Zhuxia");
    }

    #[test]
    fn image_detection() {
        assert!(is_image("a/B.PNG"));
        assert!(!is_image("a/readme.md"));
    }
}
