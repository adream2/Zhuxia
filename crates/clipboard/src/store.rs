//! 历史存储：`<exe>/clipboard/history.dat`（DPAPI 密文）+ 加密附件。
//!
//! 对照参考实现的 `clipboard_repo.rs` + SQLite 迁移：数据模型、去重/合并、收藏、
//! 标签、备注、保留条数、排序与搜索语义全部保留，只是把 SQLite 换成
//! 「内存索引 + 整份原子写」——几千条记录（默认上限 1000）完全放得下，
//! 换来零 C 依赖、零迁移脚本、exe 体积不变。
//!
//! 会话模式（设置里关掉「持久化」）时记录只留在内存（最多 500 条），
//! 图片写 `session/attachments` 并在启动时清空，与上游一致。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zx_core::ClipboardSettings;

use crate::model::{now_ms, Entry, Kind};
use crate::rules;
use crate::secure;

const SESSION_MAX: usize = 500;

/// 单条记录写进历史文件的体积上限（正文 + 富文本）。
///
/// 历史文件是**整份重写 + 整份 DPAPI 加密**，代价和总体积线性相关。实测
/// （`crates/clipboard` 的探针）：1000 条小文本（0.25MB）约 4ms，可以忽略；
/// 但一条 10MB 的文本会让**之后每一次复制**都付出 ~576ms（DPAPI 约 57MB/s），
/// 而且全程握着 store 的锁，面板这时候打开就会卡住。
///
/// 所以超过上限的记录**留在内存里照常可用**（粘贴、搜索都不受影响），只是不进
/// 历史文件——重启后消失。落库时给它打了「不保存」标签，面板里能看见。
pub const MAX_PERSIST_BYTES: usize = 1024 * 1024;

fn persistable(e: &Entry) -> bool {
    let html = e.html.as_ref().map_or(0, |h| h.len());
    e.content.len() + html <= MAX_PERSIST_BYTES
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Recent,
    Created,
    Used,
    Frequent,
}

impl Sort {
    pub fn from_name(s: &str) -> Sort {
        match s {
            "created" => Sort::Created,
            "used" => Sort::Used,
            "frequent" => Sort::Frequent,
            _ => Sort::Recent,
        }
    }
    pub fn next(self) -> Sort {
        match self {
            Sort::Recent => Sort::Created,
            Sort::Created => Sort::Used,
            Sort::Used => Sort::Frequent,
            Sort::Frequent => Sort::Recent,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Sort::Recent => "最新",
            Sort::Created => "最早",
            Sort::Used => "最近使用",
            Sort::Frequent => "最常用",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Pinned,
    /// 手动收进「密钥」分类的条目（和收藏一样是可逆标记）。
    Secrets,
    Texts,
    Urls,
    Images,
    Files,
    Tag(String),
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: String,
    pub filter: Option<Filter>,
    pub sort: Sort,
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    next_id: i64,
    #[serde(default)]
    entries: Vec<Entry>,
    /// 标签表也一起加密：标签名同样可能带隐私（例如「公司账号」）。
    #[serde(default)]
    tags: Vec<String>,
}

pub struct Store {
    dir: PathBuf,
    attach: PathBuf,
    session_attach: PathBuf,
    entries: Vec<Entry>,
    session: Vec<Entry>,
    next_id: i64,
    tags: Vec<String>,
    hash_index: HashMap<u64, i64>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

pub fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(Store::open()))
}

impl Store {
    fn open() -> Store {
        Store::open_at(zx_core::cfg::clipboard_dir())
    }

    fn open_at(dir: PathBuf) -> Store {
        let attach = dir.join("attachments");
        let session_attach = dir.join("session").join("attachments");
        let _ = std::fs::create_dir_all(&attach);
        // 上一次运行留下的会话图片已经没人引用，直接清掉。
        let _ = std::fs::remove_dir_all(dir.join("session"));
        let _ = std::fs::create_dir_all(&session_attach);
        // 外部编辑 / 打开图片会落下临时明文文件，启动时清掉。
        for tmp in ["edit", "open"] {
            let _ = std::fs::remove_dir_all(dir.join(tmp));
            let _ = std::fs::create_dir_all(dir.join(tmp));
        }

        let mut s = Store {
            dir: dir.clone(),
            attach,
            session_attach,
            entries: Vec::new(),
            session: Vec::new(),
            next_id: 1,
            tags: vec!["sensitive".into(), "密码".into()],
            hash_index: HashMap::new(),
        };
        s.load();
        s.migrate_attachments();
        s
    }

    /// 加密后的历史文件；旧版的 history.json 只在迁移时读一次。
    fn history_path(&self) -> PathBuf {
        self.dir.join("history.dat")
    }

    fn legacy_history_path(&self) -> PathBuf {
        self.dir.join("history.json")
    }

    fn legacy_tags_path(&self) -> PathBuf {
        self.dir.join("tags.json")
    }

    fn load(&mut self) {
        let mut needs_migration = false;
        let mut loaded = false;

        if let Ok(bytes) = std::fs::read(self.history_path()) {
            let mut plaintext = secure::unprotect(&bytes);
            let parsed = plaintext
                .as_ref()
                .and_then(|plain| serde_json::from_slice::<Disk>(plain).ok());
            if let Some(buf) = plaintext.as_mut() {
                // 解析完就把解密出来的明文缓冲区清掉。
                secure::zero(buf);
            }
            match parsed {
                Some(disk) => {
                    self.apply_disk(disk);
                    loaded = true;
                }
                None => {
                    // 解不开（换了用户/机器）或结构损坏：先改名留档，绝不能被下面的
                    // 自动保存覆盖掉。
                    let backup = self.dir.join(format!("history.unreadable-{}.dat", now_ms()));
                    let _ = std::fs::rename(self.history_path(), &backup);
                    zx_core::log::warn(format!(
                        "剪贴板历史无法解密或已损坏，已留档到 {}",
                        backup.display()
                    ));
                }
            }
        }

        // 旧版明文：读出来立刻改写成密文，再把明文文件覆写删除。
        if !loaded {
            if let Ok(bytes) = std::fs::read(self.legacy_history_path()) {
                if let Ok(disk) = serde_json::from_slice::<Disk>(&bytes) {
                    self.apply_disk(disk);
                    needs_migration = true;
                    zx_core::log::info("检测到旧版明文历史，正在加密迁移");
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.legacy_tags_path()) {
            if let Ok(list) = serde_json::from_str::<Vec<String>>(&text) {
                for t in list {
                    self.ensure_tag(&t);
                }
            }
            needs_migration = true;
        }

        self.rebuild_index();
        self.gc_attachments();

        if needs_migration {
            self.save();
            secure::shred(&self.legacy_history_path());
            secure::shred(&self.legacy_tags_path());
        }
    }

    fn apply_disk(&mut self, disk: Disk) {
        self.entries = disk.entries;
        self.next_id = disk
            .next_id
            .max(self.entries.iter().map(|e| e.id).max().unwrap_or(0) + 1);
        for t in disk.tags {
            self.ensure_tag(&t);
        }
    }

    /// 旧版附件是明文 PNG/GIF；启动时逐个加密（已经是密文的跳过）。
    fn migrate_attachments(&self) {
        let mut n = 0usize;
        for dir in [&self.attach, &self.session_attach] {
            let Ok(rd) = std::fs::read_dir(dir) else { continue };
            for f in rd.flatten() {
                let path = f.path();
                if !path.is_file() {
                    continue;
                }
                // 只读文件头判断，别为了看魔数把几十 MB 的图片整个读进来。
                let mut head = [0u8; 8];
                let read = std::fs::File::open(&path)
                    .and_then(|mut f| {
                        use std::io::Read;
                        f.read(&mut head)
                    })
                    .unwrap_or(0);
                if read >= 8 && secure::is_protected(&head) {
                    continue;
                }
                if secure::protect_file_in_place(&path) {
                    n += 1;
                }
            }
        }
        if n > 0 {
            zx_core::log::info(format!("已加密 {n} 个历史附件"));
        }
    }

    fn rebuild_index(&mut self) {
        self.hash_index.clear();
        for e in &self.entries {
            if e.hash != 0 {
                self.hash_index.insert(e.hash, e.id);
            }
        }
    }

    fn gc_attachments(&self) {
        let Ok(rd) = std::fs::read_dir(&self.attach) else { return };
        let referenced: Vec<String> = self
            .entries
            .iter()
            .flat_map(|e| [e.content.clone(), e.html.clone().unwrap_or_default()])
            .collect();
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if !referenced.iter().any(|t| t.contains(&name)) {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }

    /// 整份加密后原子写。DPAPI 失败就放弃本次写盘（绝不退回明文）。
    pub fn save(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        // 过大的记录只留在内存里，别让它们把每次复制都拖成几百毫秒。
        let entries: Vec<Entry> = self.entries.iter().filter(|e| persistable(e)).cloned().collect();
        let disk = Disk {
            version: 2,
            next_id: self.next_id,
            entries,
            tags: self.tags.clone(),
        };
        let Ok(mut json) = serde_json::to_vec(&disk) else { return };
        let sealed = match secure::protect(&json) {
            Some(v) => v,
            None => {
                zx_core::log::warn("DPAPI 加密失败，本次不写盘");
                secure::zero(&mut json);
                return;
            }
        };
        // 明文只在这一小段时间存在，加密完立刻抹掉。
        secure::zero(&mut json);
        if !write_atomic(&self.history_path(), &sealed) {
            zx_core::log::warn("剪贴板历史写盘失败（磁盘满/无权限？）");
        }
    }

    pub fn len(&self) -> usize {
        self.active().len()
    }

    pub fn is_empty(&self) -> bool {
        self.active().is_empty()
    }

    fn active(&self) -> &Vec<Entry> {
        if crate::cfg().persistent {
            &self.entries
        } else {
            &self.session
        }
    }

    /// 把图片字节加密后写进附件目录，返回路径。按明文哈希命名，天然去重。
    pub fn save_attachment(&self, bytes: &[u8], ext: &str) -> Option<String> {
        let ext = normalize_ext(ext)?;
        let dir = if crate::cfg().persistent {
            &self.attach
        } else {
            &self.session_attach
        };
        let _ = std::fs::create_dir_all(dir);
        let name = format!("{:016x}.{}", rules::bytes_hash(bytes), ext);
        let path = dir.join(&name);
        if !path.exists() {
            let sealed = secure::protect(bytes)?;
            // 写失败就别把不存在的路径交给条目。
            if !write_atomic(&path, &sealed) {
                return None;
            }
        }
        Some(path.to_string_lossy().to_string())
    }

    /// 读一个附件；旧版明文文件也能读（仅限本模块目录，就地加密）。
    pub fn read_attachment(path: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(path).ok()?;
        if secure::is_protected(&bytes) {
            return secure::unprotect(&bytes);
        }
        // 旧版明文附件：就地加密。但绝不能碰用户自己的图片（外部路径只读）。
        let p = Path::new(path);
        if p.starts_with(zx_core::cfg::clipboard_dir()) {
            let _ = secure::protect_file_in_place(p);
        }
        Some(bytes)
    }

    /// 这个附件是不是密文（决定还能不能写 CF_HDROP）。
    pub fn attachment_is_protected(path: &str) -> bool {
        std::fs::File::open(path)
            .and_then(|mut f| {
                use std::io::Read;
                let mut head = [0u8; 8];
                f.read_exact(&mut head).map(|_| head)
            })
            .map(|head| secure::is_protected(&head))
            .unwrap_or(false)
    }

    /// 把一条采集结果落库（去重/合并在这里完成）。
    /// 返回 `None` 表示按策略丢弃（例如没有附件可存的图片）。
    pub fn add(&mut self, mut entry: Entry) -> Option<i64> {
        let settings = crate::cfg();
        let persistent = settings.persistent;

        // 标签先入表，避免与下面记录列表的可变借用打架。
        let tags: Vec<String> = entry.tags.clone();
        for t in &tags {
            self.ensure_tag(t);
        }

        // ---- 去重 / 合并 ----
        if settings.dedupe && entry.hash != 0 {
            let mut found: Option<usize> = None;
            {
                let list = if persistent { &self.entries } else { &self.session };
                for (i, old) in list.iter().enumerate() {
                    let compatible = old.kind == entry.kind
                        || (old.kind.is_text() && entry.kind.is_text());
                    if !compatible || old.hash != entry.hash {
                        continue;
                    }
                    // 富文本要求 HTML 也一致，否则同一段文字的不同版式会被合成一条。
                    if entry.kind == Kind::RichText
                        && old.kind == Kind::RichText
                        && rules::normalize_line_endings(&old.html.clone().unwrap_or_default())
                            != rules::normalize_line_endings(&entry.html.clone().unwrap_or_default())
                    {
                        continue;
                    }
                    found = Some(i);
                    break;
                }
            }
            if let Some(i) = found {
                let list = if persistent { &mut self.entries } else { &mut self.session };
                let old = &mut list[i];
                // 纯文本再次复制时，保留原有的富文本格式（上游 preserve_rich_format）。
                if entry.kind != Kind::RichText && old.kind == Kind::RichText {
                    entry.kind = old.kind;
                    entry.html = old.html.clone();
                }
                let id = old.id;
                old.content = entry.content;
                old.kind = entry.kind;
                old.html = entry.html;
                old.source_app = entry.source_app;
                old.source_path = entry.source_path;
                old.preview = entry.preview;
                old.sort_at = entry.sort_at;
                old.use_count = old.use_count.saturating_add(1);
                old.ext = entry.ext;
                for t in &entry.tags {
                    if !old.tags.iter().any(|x| x == t) {
                        old.tags.push(t.clone());
                    }
                }
                let updated = old.clone();
                if persistent {
                    self.hash_index.insert(updated.hash, id);
                }
                self.save();
                return Some(id);
            }
        }

        // ---- 新记录 ----
        entry.id = self.next_id;
        self.next_id += 1;
        let now = now_ms();
        if entry.created_at == 0 {
            entry.created_at = now;
        }
        if entry.sort_at == 0 {
            entry.sort_at = now;
        }
        let id = entry.id;
        let hash = entry.hash;
        if persistent {
            self.entries.push(entry);
            if hash != 0 {
                self.hash_index.insert(hash, id);
            }
        } else {
            self.session.push(entry);
            if self.session.len() > SESSION_MAX {
                self.session.remove(0);
            }
        }
        self.enforce_limit();
        self.save();
        Some(id)
    }

    pub fn ensure_tag(&mut self, tag: &str) {
        let tag = tag.trim();
        if tag.is_empty() {
            return;
        }
        if !self.tags.iter().any(|t| t == tag) {
            self.tags.push(tag.to_string());
        }
    }

    fn enforce_limit(&mut self) {
        let settings = crate::cfg();
        if !settings.persistent || !settings.limit_enabled {
            return;
        }
        let limit = settings.limit as usize;
        // 只淘汰「未收藏、未收进密钥、且无标签」的记录，与上游一致。
        // 密钥是用户手工整理的，和收藏一样不该被自动淘汰。
        let mut removable: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.pinned && !e.secret && e.tags.is_empty())
            .map(|(i, _)| i)
            .collect();
        let count = removable.len();
        if count <= limit {
            return;
        }
        removable.sort_by_key(|&i| self.entries[i].sort_at);
        let drop_n = count - limit;
        let mut drops: Vec<i64> = removable.iter().take(drop_n).map(|&i| self.entries[i].id).collect();
        drops.sort_unstable();
        for id in drops {
            self.remove_by_id(id, true);
        }
    }

    fn remove_by_id(&mut self, id: i64, gc: bool) {
        let mut removed_files = Vec::new();
        self.entries.retain(|e| {
            if e.id == id {
                if e.is_external() {
                    removed_files.push(e.content.clone());
                }
                false
            } else {
                true
            }
        });
        self.hash_index.retain(|_, v| *v != id);
        if gc {
            self.gc_paths(&removed_files);
        }
    }

    fn gc_paths(&self, files: &[String]) {
        for f in files {
            let name = Path::new(f).file_name().map(|n| n.to_string_lossy().to_string());
            let Some(name) = name else { continue };
            let still = self
                .entries
                .iter()
                .any(|e| e.content.contains(&name) || e.html.as_deref().unwrap_or("").contains(&name));
            if !still {
                let _ = std::fs::remove_file(f);
            }
        }
    }

    // ------------------------------------------------------------ 查询

    pub fn list(&self, q: &Query) -> Vec<Entry> {
        let needle = q.text.trim().to_lowercase();
        let filter = q.filter.clone().unwrap_or(Filter::All);
        let settings = crate::cfg();
        let mut out: Vec<Entry> = self
            .active()
            .iter()
            .filter(|e| match &filter {
                Filter::All => true,
                Filter::Pinned => e.pinned,
                Filter::Secrets => e.secret,
                // 文本 = 纯文本 / 代码 / 富文本；链接单独一类
                Filter::Texts => matches!(e.kind, Kind::Text | Kind::Code | Kind::RichText),
                Filter::Urls => e.kind == Kind::Url,
                Filter::Images => e.kind == Kind::Image,
                Filter::Files => matches!(e.kind, Kind::File | Kind::Video),
                Filter::Tag(t) => e.tags.iter().any(|x| x.eq_ignore_ascii_case(t)),
            })
            .filter(|e| {
                if needle.is_empty() {
                    return true;
                }
                if e.tags.iter().any(|t| t.to_lowercase().contains(&needle)) {
                    return true;
                }
                if e.source_app.to_lowercase().contains(&needle) {
                    return true;
                }
                if e.note.to_lowercase().contains(&needle) {
                    return true;
                }
                // 密钥的标题也参与搜索：平时内容是被遮住的，只能靠名字找。
                if !e.title.is_empty() && e.title.to_lowercase().contains(&needle) {
                    return true;
                }
                searchable(e).to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();

        let sort = if q.sort == Sort::Recent {
            Sort::from_name(&settings.sort)
        } else {
            q.sort
        };
        out.sort_by(|a, b| {
            let head = b.pinned.cmp(&a.pinned).then_with(|| b.pinned_order.cmp(&a.pinned_order));
            if head != std::cmp::Ordering::Equal {
                return head;
            }
            let tail = match sort {
                Sort::Recent => b.sort_at.cmp(&a.sort_at),
                Sort::Created => a.created_at.cmp(&b.created_at),
                Sort::Used => b.last_used_at.cmp(&a.last_used_at),
                Sort::Frequent => b.use_count.cmp(&a.use_count),
            };
            tail.then_with(|| b.id.cmp(&a.id))
        });
        out
    }

    pub fn get(&self, id: i64) -> Option<Entry> {
        self.active().iter().find(|e| e.id == id).cloned()
    }

    pub fn find_hash(&self, hash: u64) -> Option<i64> {
        // 持久模式走索引；会话模式没有索引，退化成线性查找（最多 500 条）。
        self.hash_index.get(&hash).copied().or_else(|| {
            self.active()
                .iter()
                .find(|e| e.hash == hash)
                .map(|e| e.id)
        })
    }

    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut map: HashMap<&str, usize> = HashMap::new();
        for t in &self.tags {
            map.entry(t.as_str()).or_insert(0);
        }
        for e in self.active() {
            for t in &e.tags {
                *map.entry(t.as_str()).or_insert(0) += 1;
            }
        }
        let mut out: Vec<(String, usize)> =
            map.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    // ------------------------------------------------------------ 变更

    pub fn toggle_pin(&mut self, id: i64) -> bool {
        let max_order = self
            .entries
            .iter()
            .chain(self.session.iter())
            .map(|e| e.pinned_order)
            .max()
            .unwrap_or(0);
        let mut now_pinned = false;
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id == id {
                e.pinned = !e.pinned;
                now_pinned = e.pinned;
                e.pinned_order = if e.pinned { max_order + 1 } else { 0 };
                if e.pinned && !crate::cfg().persistent {
                    // 会话里的图片被收藏时，把文件复制进持久附件目录。
                    if e.is_external() {
                        if let Some(path) = persist_session_file(&self.attach, &e.content) {
                            e.content = path;
                        }
                    }
                }
                break;
            }
        }
        self.save();
        now_pinned
    }

    pub fn set_note(&mut self, id: i64, note: &str) {
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id == id {
                e.note = note.trim().to_string();
            }
        }
        self.save();
    }

    pub fn set_tags(&mut self, id: i64, tags: &[String]) {
        let mut clean: Vec<String> = Vec::new();
        for t in tags {
            let t = t.trim();
            if !t.is_empty() && !clean.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                clean.push(t.to_string());
            }
        }
        for t in &clean {
            self.ensure_tag(t);
        }
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id == id {
                e.tags = clean.clone();
            }
        }
        self.save();
    }

    /// 收进 / 移出「密钥」，同时写入标题（描述沿用备注字段）。
    /// 只改元数据，`content` 原样保留——粘贴时写出去的仍只有正文。
    pub fn set_secret(&mut self, id: i64, secret: bool, title: &str) {
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id == id {
                e.secret = secret;
                e.title = if secret { title.trim().to_string() } else { String::new() };
            }
        }
        self.save();
    }

    pub fn set_hotkey(&mut self, id: i64, hotkey: &str) {
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id == id {
                e.hotkey = hotkey.trim().to_string();
            }
        }
        self.save();
    }

    /// 所有设置了个体快捷键的条目（热键线程启动/重载时用）。
    pub fn hotkey_entries(&self) -> Vec<(i64, String)> {
        self.active()
            .iter()
            .filter(|e| !e.hotkey.trim().is_empty())
            .map(|e| (e.id, e.hotkey.clone()))
            .collect()
    }

    pub fn delete(&mut self, id: i64) {
        self.remove_by_id(id, true);
        let _ = self.session.iter().position(|e| e.id == id).map(|i| {
            let e = self.session.remove(i);
            if e.is_external() {
                let _ = std::fs::remove_file(e.content);
            }
        });
        self.save();
    }

    pub fn clear(&mut self, keep_pinned: bool) {
        let mut files = Vec::new();
        self.entries.retain(|e| {
            if keep_pinned && e.pinned {
                return true;
            }
            if e.is_external() {
                files.push(e.content.clone());
            }
            false
        });
        self.session.clear();
        self.rebuild_index();
        self.gc_paths(&files);
        self.save();
    }

    /// 粘贴 / 打开之后：use_count+1、last_used_at=now，按设置决定是否置顶。
    pub fn touch(&mut self, id: i64) {
        let move_top = crate::cfg().move_to_top_after_paste;
        let now = now_ms();
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id == id {
                e.use_count = e.use_count.saturating_add(1);
                e.last_used_at = now;
                if move_top && !e.pinned {
                    e.sort_at = now;
                }
            }
        }
        self.save();
    }

    /// 外部编辑器改完内容写回。
    pub fn update_content(&mut self, id: i64, content: &str) {
        for e in self.entries.iter_mut().chain(self.session.iter_mut()) {
            if e.id != id {
                continue;
            }
            e.content = content.to_string();
            e.html = None;
            e.hash = rules::text_hash(content);
            e.preview = rules::build_preview(e.kind, content, None);
        }
        self.rebuild_index();
        self.save();
    }

}

fn persist_session_file(attach: &Path, path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let ext = Path::new(path).extension()?.to_str()?;
    let name = format!("{:016x}.{}", rules::bytes_hash(&bytes), normalize_ext(ext)?);
    let dst = attach.join(name);
    if !dst.exists() {
        let _ = std::fs::create_dir_all(attach);
        // 会话附件在盘上是密文，搬到持久目录也必须保持密文。
        let sealed = secure::protect(&bytes)?;
        if !write_atomic(&dst, &sealed) {
            return None;
        }
    }
    Some(dst.to_string_lossy().to_string())
}

fn normalize_ext(ext: &str) -> Option<&'static str> {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => Some("png"),
        "gif" => Some("gif"),
        "jpg" | "jpeg" => Some("jpg"),
        "webp" => Some("webp"),
        "bmp" => Some("bmp"),
        _ => None,
    }
}

fn searchable(e: &Entry) -> String {
    if !e.kind.is_text() {
        return String::new();
    }
    // 必须搜完整正文：preview 只有前 ~200 字，用 preview 会漏掉长文本后半段。
    if e.kind == Kind::RichText {
        let text = rules::derive_rich_text(&e.content, e.html.as_deref());
        if !text.trim().is_empty() {
            return text;
        }
    }
    e.content.clone()
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    let tmp = path.with_extension("tmp");
    if std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes))
        .is_err()
    {
        return false;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let ok = std::fs::copy(&tmp, path).is_ok();
        let _ = std::fs::remove_file(&tmp);
        return ok;
    }
    true
}

/// 设置页要的统计（只读、解密后计数，不碰运行中的 Store）。
#[derive(Default, Clone, Copy)]
pub struct DiskStats {
    pub total: usize,
    pub pinned: usize,
    pub sensitive: usize,
    pub secret: usize,
    pub images: usize,
    pub files: usize,
    pub bytes: u64,
}

/// 直接读盘统计（设置面板进程用）：解密 history.dat 后数一遍。
pub fn read_disk_stats(dir: &Path) -> DiskStats {
    let disk = std::fs::read(dir.join("history.dat"))
        .ok()
        .and_then(|b| secure::unprotect(&b))
        .and_then(|plain| serde_json::from_slice::<Disk>(&plain).ok())
        .or_else(|| {
            std::fs::read(dir.join("history.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Disk>(&b).ok())
        });
    let mut st = DiskStats::default();
    if let Some(disk) = disk {
        st.total = disk.entries.len();
        for e in &disk.entries {
            if e.pinned {
                st.pinned += 1;
            }
            if e.tags.iter().any(|t| t == "sensitive") {
                st.sensitive += 1;
            }
            if e.secret {
                st.secret += 1;
            }
            match e.kind {
                Kind::Image => st.images += 1,
                Kind::File | Kind::Video => st.files += 1,
                _ => {}
            }
            st.bytes += e.content.len() as u64;
        }
    }
    st.bytes += dir_size(&dir.join("attachments"));
    st
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter_map(|f| f.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// 合并同一来源连续两次复制的抖动：同 hash 且 2 秒内视为同一次系统事件。
pub fn is_burst(last_hash: u64, hash: u64, last_ms: i64, now: i64) -> bool {
    hash != 0 && hash == last_hash && now.saturating_sub(last_ms) < 2000
}

/// 当前配置（每次读，保证设置页改动立刻生效）。
pub fn settings() -> ClipboardSettings {
    crate::cfg()
}


#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("zhuxia-clip-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        Store::open_at(dir)
    }

    fn entry(kind: Kind, content: &str, source: &str) -> Entry {
        let now = now_ms();
        Entry {
            kind,
            content: content.into(),
            source_app: source.into(),
            created_at: now,
            sort_at: now,
            preview: rules::build_preview(kind, content, None),
            hash: rules::text_hash(content),
            ..Default::default()
        }
    }

    // 历史文件是整份重写 + 整份 DPAPI 加密（实测 10MB 要约 576ms），所以超大记录
    // 只留在内存里：粘贴、搜索照常，重启后消失。这条规则要是破了，用户复制一个大
    // 日志文件之后每一次复制都会卡半秒。
    #[test]
    fn oversized_entries_stay_in_memory_but_skip_the_file() {
        let mut st = temp_store("oversize");
        st.add(entry(Kind::Text, "small", "t")).unwrap();
        let huge = "x".repeat(MAX_PERSIST_BYTES + 1);
        st.add(entry(Kind::Text, &huge, "t")).unwrap();
        assert_eq!(st.list(&Query::default()).len(), 2, "两条都在内存里，照常可用");

        st.save();
        let reloaded = Store::open_at(st.dir.clone());
        let sizes: Vec<usize> = reloaded.entries.iter().map(|e| e.content.len()).collect();
        assert_eq!(sizes, vec![5], "重启后只剩小那条；超大记录不进文件");
    }

    #[test]
    fn recopying_the_same_text_moves_it_to_top_instead_of_duplicating() {
        let mut s = temp_store("dedupe");
        let a = s.add(entry(Kind::Text, "hello", "Code")).unwrap();
        let b = s.add(entry(Kind::Text, "world", "Code")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let again = s.add(entry(Kind::Text, "hello", "WeChat")).unwrap();
        assert_eq!(a, again);
        let list = s.list(&Query::default());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, a, "再次复制应回到最前");
        assert_eq!(list[0].use_count, 1);
        assert_eq!(list[0].source_app, "WeChat");
        assert_eq!(list[1].id, b);
    }

    #[test]
    fn plain_recopy_keeps_the_rich_format() {
        let mut s = temp_store("rich");
        let mut rich = entry(Kind::RichText, "加粗", "Word");
        rich.html = Some("<b>加粗</b>".into());
        s.add(rich).unwrap();
        let plain = s.add(entry(Kind::Text, "加粗", "Notepad")).unwrap();
        let e = s.get(plain).unwrap();
        assert_eq!(e.kind, Kind::RichText);
        assert_eq!(e.html.as_deref(), Some("<b>加粗</b>"));
    }

    #[test]
    fn search_covers_content_note_tags_and_source() {
        let mut s = temp_store("search");
        let id = s.add(entry(Kind::Text, "secret token", "KeePass")).unwrap();
        s.set_note(id, "工作账号");
        s.set_tags(id, &["密码".to_string(), "sensitive".to_string()]);
        let hit = |q: &str| s.list(&Query { text: q.into(), ..Default::default() }).len();
        assert_eq!(hit("token"), 1);
        assert_eq!(hit("工作"), 1);
        assert_eq!(hit("keepass"), 1);
        assert_eq!(hit("sensitive"), 1);
        assert_eq!(hit("nothing"), 0);
    }

    #[test]
    fn search_covers_text_beyond_the_preview() {
        let mut s = temp_store("search-full");
        let content = format!("{}NEEDLE-AT-THE-END", "a".repeat(400));
        s.add(entry(Kind::Text, &content, "Editor")).unwrap();
        // preview 只有前 ~200 字；搜索必须落到完整正文。
        let hit = s.list(&Query { text: "needle-at-the-end".into(), ..Default::default() });
        assert_eq!(hit.len(), 1);
    }

    #[test]
    fn pinned_entries_sort_first_and_keep_their_order() {
        let mut s = temp_store("pin");
        let a = s.add(entry(Kind::Text, "a", "x")).unwrap();
        let b = s.add(entry(Kind::Text, "b", "x")).unwrap();
        s.toggle_pin(a);
        s.toggle_pin(b);
        let list = s.list(&Query::default());
        assert_eq!(list[0].id, b);
        assert_eq!(list[1].id, a);
        s.toggle_pin(b);
        assert_eq!(s.list(&Query::default())[0].id, a);
    }

    #[test]
    fn deleting_an_entry_removes_it_and_its_hash() {
        let mut s = temp_store("delete");
        let id = s.add(entry(Kind::Text, "gone", "x")).unwrap();
        let h = s.get(id).unwrap().hash;
        s.delete(id);
        assert!(s.get(id).is_none());
        assert!(s.find_hash(h).is_none());
        assert!(s.add(entry(Kind::Text, "gone", "x")).is_some());
    }

    #[test]
    fn attachments_are_named_by_content_hash() {
        let s = temp_store("attach");
        let a = s.save_attachment(&[1, 2, 3], "png").expect("saved");
        let b = s.save_attachment(&[1, 2, 3], "png").expect("saved");
        assert_eq!(a, b);
        assert!(std::path::Path::new(&a).exists());
    }

    #[test]
    fn secret_mark_keeps_content_and_title_independent() {
        let mut s = temp_store("secret");
        let id = s.add(entry(Kind::Text, "ghp_deadbeef", "1Password")).unwrap();
        s.set_secret(id, true, "GitHub Token");
        s.set_note(id, "工作账号");
        let e = s.get(id).unwrap();
        assert!(e.secret);
        assert_eq!(e.title, "GitHub Token");
        // 关键：正文一个字节都不能被标题/描述改写——粘贴只写 content。
        assert_eq!(e.content, "ghp_deadbeef");
        assert_eq!(e.note, "工作账号");
        // 移出密钥只清标记与标题，正文与备注保留。
        s.set_secret(id, false, "ignored");
        let e = s.get(id).unwrap();
        assert!(!e.secret);
        assert!(e.title.is_empty());
        assert_eq!(e.content, "ghp_deadbeef");
    }

    #[test]
    fn secrets_filter_and_title_search() {
        let mut s = temp_store("secret-filter");
        let a = s.add(entry(Kind::Text, "sk-live-1", "Stripe")).unwrap();
        let b = s.add(entry(Kind::Text, "plain text", "Notepad")).unwrap();
        s.set_secret(a, true, "Stripe Key");
        let secrets = s.list(&Query { filter: Some(Filter::Secrets), ..Default::default() });
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].id, a);
        // 标题参与搜索：内容被遮住时也能靠名字找。
        let hit = |q: &str| s.list(&Query { text: q.into(), ..Default::default() }).len();
        assert_eq!(hit("stripe key"), 1);
        assert_eq!(hit("sk-live"), 1);
        assert_eq!(hit("plain"), 1);
        assert_eq!(hit("nothing"), 0);
        let _ = b;
    }

    #[test]
    fn recopying_content_does_not_lose_secret_metadata() {
        let mut s = temp_store("secret-dedupe");
        let id = s.add(entry(Kind::Text, "same-token", "A")).unwrap();
        s.set_secret(id, true, "Token");
        s.set_note(id, "说明");
        let again = s.add(entry(Kind::Text, "same-token", "B")).unwrap();
        assert_eq!(again, id);
        let e = s.get(id).unwrap();
        assert!(e.secret);
        assert_eq!(e.title, "Token");
        assert_eq!(e.note, "说明");
        assert_eq!(e.source_app, "B");
    }

    #[test]
    fn history_and_tags_are_encrypted_on_disk() {
        let mut s = temp_store("encrypted");
        s.add(entry(Kind::Text, "SUPER-SECRET-TOKEN-123", "KeePass")).unwrap();
        let path = s.dir.join("history.dat");
        let raw = std::fs::read(&path).expect("history.dat");
        assert!(secure::is_protected(&raw), "落盘必须是密文");
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("SUPER-SECRET-TOKEN-123"), "明文不能出现在文件里");
        assert!(!path.with_file_name("history.json").exists());
        assert!(!s.dir.join("tags.json").exists());
        // 重新打开仍能解密读回
        let again = Store::open_at(s.dir.clone());
        let e = again.get(1).expect("entry");
        assert_eq!(e.content, "SUPER-SECRET-TOKEN-123");
    }

    #[test]
    fn attachments_are_encrypted_but_readable() {
        let s = temp_store("attach-encrypted");
        let plain: Vec<u8> = "SECRET-IMAGE-BYTES".as_bytes().to_vec();
        let path = s.save_attachment(&plain, "png").expect("saved");
        let raw = std::fs::read(&path).expect("file");
        assert!(secure::is_protected(&raw));
        assert!(!raw.windows(9).any(|w| w == b"SECRET-IM"));
        assert_eq!(Store::read_attachment(&path).as_deref(), Some(plain.as_slice()));
        assert!(Store::attachment_is_protected(&path));
    }

    #[test]
    fn legacy_plaintext_history_is_migrated_and_shredded() {
        let dir = std::env::temp_dir().join(format!(
            "zhuxia-clip-test-migrate-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let mut e = entry(Kind::Text, "legacy-plaintext", "Notepad");
        e.id = 7;
        let disk = Disk {
            version: 1,
            next_id: 8,
            entries: vec![e],
            tags: vec!["旧标签".into()],
        };
        std::fs::write(dir.join("history.json"), serde_json::to_vec(&disk).unwrap()).unwrap();
        let legacy_tags = serde_json::to_vec(&vec!["另一个标签".to_string()]).unwrap();
        std::fs::write(dir.join("tags.json"), legacy_tags).unwrap();

        let s = Store::open_at(dir.clone());
        assert!(s.get(7).is_some());
        assert!(s.tags().iter().any(|(t, _)| t == "旧标签"));
        assert!(s.tags().iter().any(|(t, _)| t == "另一个标签"));
        assert!(!dir.join("history.json").exists(), "明文历史应被删除");
        assert!(!dir.join("tags.json").exists(), "明文标签应被删除");
        let raw = std::fs::read(dir.join("history.dat")).unwrap();
        assert!(secure::is_protected(&raw));
    }
}
