//! 本地上传记录：`<exe>/imgbed/history.json`。
//!
//! 图片页的"本地上传记录"就是它——跨平台，只记"我传过什么"，不等于远端目录。
//! 明文 JSON（只有 URL/路径，没有密钥），上限 5000 条，整份原子写。

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

pub const MAX_ITEMS: usize = 5000;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Record {
    pub id: String,
    pub provider: String,
    pub path: String,
    pub url: String,
    pub name: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub format: String,
    /// screenshot / file / clipboard / test
    pub source: String,
    /// Unix 秒
    pub created_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    version: u32,
    items: Vec<Record>,
}

fn path() -> std::path::PathBuf {
    zx_core::cfg::run_dir().join("imgbed").join("history.json")
}

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn load() -> Vec<Record> {
    let Ok(text) = std::fs::read_to_string(path()) else { return Vec::new() };
    let disk: Disk = serde_json::from_str(&text).unwrap_or_default();
    disk.items
}

fn save(items: &[Record]) {
    let dir = zx_core::cfg::run_dir().join("imgbed");
    let _ = std::fs::create_dir_all(&dir);
    let disk = Disk { version: 1, items: items.to_vec() };
    let Ok(text) = serde_json::to_vec(&disk) else { return };
    let tmp = path().with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_err() {
        return;
    }
    if std::fs::rename(&tmp, path()).is_err() {
        let _ = std::fs::copy(&tmp, path());
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_id(url: &str, created_at: u64) -> String {
    format!("{created_at}-{}", crate::path::short_hash(url.as_bytes()))
}

pub fn add(mut rec: Record) {
    let _g = lock().lock().unwrap_or_else(|p| p.into_inner());
    let mut items = load();
    if rec.id.is_empty() {
        rec.id = new_id(&rec.url, rec.created_at);
    }
    // 同 URL 只留最新一条（重复上传同一张：更新而不是堆叠）
    items.retain(|r| r.url != rec.url);
    items.insert(0, rec);
    items.truncate(MAX_ITEMS);
    save(&items);
}

pub fn len() -> usize {
    load().len()
}

pub fn get(id: &str) -> Option<Record> {
    load().into_iter().find(|r| r.id == id)
}

pub fn remove(id: &str) -> bool {
    let _g = lock().lock().unwrap_or_else(|p| p.into_inner());
    let mut items = load();
    let before = items.len();
    items.retain(|r| r.id != id);
    if items.len() == before {
        return false;
    }
    save(&items);
    true
}

pub fn clear() {
    let _g = lock().lock().unwrap_or_else(|p| p.into_inner());
    save(&[]);
}

/// 分页 + 过滤 + 排序。返回 (当前页, 过滤后的总数)。
pub fn query(
    q: &str,
    provider: &str,
    sort: &str,
    offset: usize,
    limit: usize,
) -> (Vec<Record>, usize) {
    let q = q.trim().to_lowercase();
    let mut items: Vec<Record> = load()
        .into_iter()
        .filter(|r| provider.is_empty() || r.provider == provider)
        .filter(|r| {
            q.is_empty()
                || r.name.to_lowercase().contains(&q)
                || r.path.to_lowercase().contains(&q)
                || r.url.to_lowercase().contains(&q)
        })
        .collect();
    match sort {
        "oldest" => items.sort_by_key(|r| r.created_at),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "size" => items.sort_by_key(|r| std::cmp::Reverse(r.bytes)),
        _ => items.sort_by_key(|r| std::cmp::Reverse(r.created_at)),
    }
    let total = items.len();
    let page = items.into_iter().skip(offset).take(limit.clamp(1, 200)).collect();
    (page, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_for_same_url() {
        let a = new_id("https://x/a.png", 100);
        let b = new_id("https://x/a.png", 100);
        let c = new_id("https://x/b.png", 100);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
