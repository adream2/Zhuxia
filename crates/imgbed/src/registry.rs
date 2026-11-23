//! 平台工厂：按配置里的 `provider` 造出对应的 `Backend`。

use zx_core::ImgBedSettings;

use crate::provider::{Backend, Fail};
use crate::providers::{custom::Custom, github::Github, s3::S3, webdav::WebDav};

pub fn build(cfg: &ImgBedSettings) -> Result<Box<dyn Backend>, Fail> {
    match cfg.provider.as_str() {
        "github" => Ok(Box::new(Github::from_settings(cfg)?)),
        "s3" => Ok(Box::new(S3::from_settings(cfg)?)),
        "webdav" => Ok(Box::new(WebDav::from_settings(cfg)?)),
        "custom" => Ok(Box::new(Custom::from_settings(cfg)?)),
        other => Err(Fail::new("未知平台", format!("provider = {other}"))),
    }
}

/// 某个平台在配置里的默认路径（没填就用元数据里的默认值）。
pub fn default_path(cfg: &ImgBedSettings, id: &str) -> String {
    // 键存在（哪怕是空串）= 用户明确选过，空串表示仓库根/桶根，不能再回填默认值。
    if let Some(v) = cfg
        .providers
        .get(id)
        .and_then(|v| v.as_mapping())
        .and_then(|m| m.get(serde_yaml::Value::String("path".into())))
    {
        return v.as_str().unwrap_or("").trim().to_string();
    }
    crate::provider::metas()
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| m.default_path.to_string())
        .unwrap_or_default()
}
