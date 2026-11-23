//! 文件日志：`logs/zhuxia-yyyy-MM-dd.log`，永不抛异常。
//!
//! 只在真的出问题时写，正常 tick 不产生任何 I/O（避免把低占用工具变成磁盘噪音源）。

use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub enum Level { Info, Warn, Error }

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());
static WARNED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn path() -> &'static PathBuf {
    LOG_PATH.get_or_init(|| {
        let dir = crate::cfg::run_dir().join("logs");
        let _ = create_dir_all(&dir);
        // 日期取自本地时间：够用，不引入 chrono。
        let d = crate::win::local_date_string();
        dir.join(format!("zhuxia-{d}.log"))
    })
}

pub fn info(msg: impl AsRef<str>) { write(Level::Info, msg.as_ref()) }
pub fn warn(msg: impl AsRef<str>) { write(Level::Warn, msg.as_ref()) }
pub fn error(msg: impl AsRef<str>) { write(Level::Error, msg.as_ref()) }

/// 同一条告警只记一次（每个 key）——避免设备/网卡异常时刷屏。
pub fn warn_once(key: &'static str, msg: impl AsRef<str>) {
    let mut w = WARNED.lock().unwrap_or_else(|p| p.into_inner());
    if w.contains(&key) { return; }
    w.push(key);
    drop(w);
    write(Level::Warn, msg.as_ref());
}

fn write(level: Level, msg: &str) {
    let _g = LOCK.lock();
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path()) else { return };
    let tag = match level { Level::Info => "INFO", Level::Warn => "WARN", Level::Error => "ERR " };
    let ts = crate::win::local_time_string();
    let _ = writeln!(f, "{ts} [{tag}] {msg}");
}
