//! 平台抽象：能力、元数据、上传/列举/删除。
//!
//! 设计原则：**UI 按能力渲染，不按平台写死**。平台不支持列目录就不显示"浏览"按钮，
//! 不支持远端浏览就不显示那个分段，不支持删除就不显示删除——而不是点了才报错。

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Caps {
    pub list_dirs: bool,
    pub list_images: bool,
    pub delete: bool,
    pub public_url: bool,
    pub max_mb: u32,
}

impl Default for Caps {
    fn default() -> Self {
        Caps { list_dirs: false, list_images: false, delete: false, public_url: false, max_mb: 25 }
    }
}

#[derive(Debug, Clone)]
pub struct Fail {
    /// 短句：认证失败 / 目标不存在 / 无写权限 / 网络不通 / 限流 / 编码失败…
    pub stage: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl Fail {
    pub fn new(stage: &'static str, message: impl Into<String>) -> Fail {
        Fail { stage, message: message.into(), retryable: false }
    }

    pub fn retry(stage: &'static str, message: impl Into<String>) -> Fail {
        Fail { stage, message: message.into(), retryable: true }
    }

    pub fn text(&self) -> String {
        format!("{}：{}", self.stage, self.message)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TestReport {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct UploadRequest {
    /// 目标目录（已渲染，无首尾斜杠；空 = 根）
    pub dir: String,
    /// 文件名（含扩展名）
    pub name: String,
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

impl UploadRequest {
    /// 平台内的完整路径。
    pub fn path(&self) -> String {
        if self.dir.is_empty() { self.name.clone() } else { format!("{}/{}", self.dir, self.name) }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Uploaded {
    /// 平台内的路径（GitHub 是仓库路径，S3 是 object key，WebDAV 是相对路径）
    pub path: String,
    /// 平台直接给出的 URL（自定义 HTTP 用；其它平台可能为空，由 `url()` 拼）
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteImage {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub modified: Option<u64>,
    pub url: String,
}

pub trait Backend: Send + Sync {
    fn caps(&self) -> Caps;
    fn test(&self) -> Result<TestReport, Fail>;
    fn upload(&self, req: &UploadRequest) -> Result<Uploaded, Fail>;
    fn list_dirs(&self, dir: &str) -> Result<Vec<String>, Fail>;
    fn list_images(&self, dir: &str) -> Result<Vec<RemoteImage>, Fail>;
    fn delete(&self, path: &str) -> Result<(), Fail>;
    /// 平台内路径 → 可访问 URL。
    fn url(&self, path: &str) -> Result<String, Fail>;
}

// ---------------------------------------------------------------- 设置页元数据

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldKind {
    Text,
    Password,
    Bool,
    Select,
    /// 键值对（额外表单字段 / 请求头）
    Kv,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub value: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    /// 密钥字段：值存 secrets.dat，不进 settings.yaml
    pub secret: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SelectOption>,
}

impl FieldSpec {
    pub fn text(key: &'static str, label: &'static str, hint: &'static str) -> FieldSpec {
        FieldSpec { key, label, hint, kind: FieldKind::Text, required: false, secret: false, options: Vec::new() }
    }
    pub fn required(mut self) -> FieldSpec {
        self.required = true;
        self
    }
    pub fn password(key: &'static str, label: &'static str, hint: &'static str) -> FieldSpec {
        FieldSpec { key, label, hint, kind: FieldKind::Password, required: true, secret: true, options: Vec::new() }
    }
    pub fn bool(key: &'static str, label: &'static str, hint: &'static str) -> FieldSpec {
        FieldSpec { key, label, hint, kind: FieldKind::Bool, required: false, secret: false, options: Vec::new() }
    }
    pub fn select(key: &'static str, label: &'static str, hint: &'static str, options: Vec<SelectOption>) -> FieldSpec {
        FieldSpec { key, label, hint, kind: FieldKind::Select, required: false, secret: false, options }
    }
    pub fn kv(key: &'static str, label: &'static str, hint: &'static str) -> FieldSpec {
        FieldSpec { key, label, hint, kind: FieldKind::Kv, required: false, secret: false, options: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub caps: Caps,
    pub default_path: &'static str,
    pub fields: Vec<FieldSpec>,
}

/// 所有平台的元数据（设置页据此渲染表单，加平台不用改前端）。
pub fn metas() -> Vec<ProviderMeta> {
    vec![
        ProviderMeta {
            id: "github",
            name: "GitHub",
            caps: Caps { list_dirs: true, list_images: true, delete: true, public_url: true, max_mb: 100 },
            default_path: "images/{yyyy}/{MM}",
            fields: vec![
                FieldSpec::text("owner", "账号 owner", "用户名或组织名").required(),
                FieldSpec::text("repo", "仓库 repo", "只填仓库名").required(),
                FieldSpec::text("branch", "分支", "留空 = 仓库默认分支"),
                FieldSpec::text("path", "上传路径", "支持 {yyyy} {MM} {dd} {HH} {mm}"),
                FieldSpec::select(
                    "cdn",
                    "链接来源",
                    "只影响读图地址，不影响上传",
                    vec![
                        SelectOption { value: "jsdelivr", label: "jsDelivr" },
                        SelectOption { value: "jsdelivr-cn", label: "jsDelivr 国内节点" },
                        SelectOption { value: "statically", label: "Statically" },
                        SelectOption { value: "github", label: "GitHub Raw" },
                        SelectOption { value: "ghproxy", label: "gh-proxy 加速" },
                        SelectOption { value: "pages", label: "GitHub Pages" },
                        SelectOption { value: "custom", label: "自定义模板" },
                    ],
                ),
                FieldSpec::text("cdnCustom", "自定义链接模板", "必须含 {{path}}"),
                FieldSpec::text("commitMessage", "提交说明", "commit message"),
                FieldSpec::password("token", "Token", "fine-grained token，Contents: Read and write"),
            ],
        },
        ProviderMeta {
            id: "s3",
            name: "S3 兼容",
            caps: Caps { list_dirs: true, list_images: true, delete: true, public_url: true, max_mb: 100 },
            default_path: "img/{yyyy}/{MM}",
            fields: vec![
                FieldSpec::text("endpoint", "Endpoint", "https://<account>.r2.cloudflarestorage.com").required(),
                FieldSpec::text("region", "Region", "R2 填 auto；AWS 填 us-east-1 等").required(),
                FieldSpec::text("bucket", "Bucket", "桶名").required(),
                FieldSpec::text("accessKeyId", "Access Key ID", "访问密钥 ID").required(),
                FieldSpec::password("secretAccessKey", "Secret Access Key", "只存 DPAPI 加密文件"),
                FieldSpec::text("publicBase", "公开域名", "CDN / 自定义域名，留空则只有后台链接"),
                FieldSpec::text("path", "上传路径", "支持日期变量"),
                FieldSpec::bool("pathStyle", "Path-style", "MinIO 等需要开启；R2/COS 一般关闭"),
            ],
        },
        ProviderMeta {
            id: "webdav",
            name: "WebDAV",
            caps: Caps { list_dirs: true, list_images: true, delete: true, public_url: true, max_mb: 100 },
            default_path: "img/{yyyy}/{MM}",
            fields: vec![
                FieldSpec::text("url", "WebDAV 地址", "https://dav.example.com/dav").required(),
                FieldSpec::text("username", "用户名", "坚果云填邮箱").required(),
                FieldSpec::password("password", "密码", "坚果云用应用密码"),
                FieldSpec::text("publicBase", "直链前缀", "网盘的公开访问域名，可留空"),
                FieldSpec::text("path", "上传路径", "支持日期变量"),
            ],
        },
        ProviderMeta {
            id: "custom",
            name: "自定义 HTTP",
            caps: Caps { list_dirs: false, list_images: false, delete: false, public_url: true, max_mb: 100 },
            default_path: "",
            fields: vec![
                FieldSpec::text("url", "上传接口", "例如 https://img.example.com/api/v1/upload").required(),
                FieldSpec::select(
                    "method",
                    "方法",
                    "多数图床用 POST",
                    vec![SelectOption { value: "POST", label: "POST" }, SelectOption { value: "PUT", label: "PUT" }],
                ),
                FieldSpec::text("fileField", "文件字段名", "Lsky: file；EasyImage: image"),
                FieldSpec::text("tokenHeader", "鉴权头", "例如 Authorization"),
                FieldSpec::text("tokenPrefix", "鉴权前缀", "例如 Bearer（后面自动补空格）"),
                FieldSpec::password("token", "Token", "写进鉴权头的值"),
                FieldSpec::kv("fields", "额外表单字段", "例如 album=blog"),
                FieldSpec::kv("headers", "额外请求头", "不要在这里放 token"),
                FieldSpec::text("pathField", "路径字段名", "空 = 不支持指定路径"),
                FieldSpec::text("urlJsonPath", "URL 的 JSON 路径", "例如 data.links.url"),
                FieldSpec::text("publicBase", "拼接前缀", "返回的是相对路径时补上"),
            ],
        },
    ]
}
