//! 按进程的历史流量统计 + 防火墙封禁状态。
//!
//! 数据来源沿用采样线程的 `rates()`（ETW / SRUM），这里只做两件事：
//!   1. 把"字节/秒"按采样间隔积分成累计字节，按 **exe 路径**聚合（同一程序多开算一份）；
//!   2. 记住哪些 exe 被加进了防火墙出站阻止规则，供界面切换。
//!
//! 累计值落盘到 `netstat.json`（60 秒一次 + 退出时），重启接着算；
//! 进程被杀不会丢，因为 key 是可执行文件路径而不是 pid。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::json;

/// 一条进程流量记录（对外只读）。
#[derive(Clone, Default, Debug)]
pub struct Flow {
    /// 小写 exe 路径；拿不到路径时是 `name:<进程名>`
    pub key: String,
    pub name: String,
    pub path: String,
    /// 历史累计（含之前的运行）
    pub up: u64,
    pub down: u64,
    /// 是否已被加入防火墙出站阻止
    pub blocked: bool,
}

struct Stat {
    flows: HashMap<String, Flow>,
    /// pid → exe 路径（进程存活期内不变；定期整体清空防泄漏）
    pid_path: HashMap<u32, String>,
    /// 我们加过的规则：小写路径
    blocked: HashSet<String>,
    dirty: bool,
    ticks: u32,
}

static STAT: OnceLock<Mutex<Stat>> = OnceLock::new();

fn stat() -> MutexGuard<'static, Stat> {
    STAT.get_or_init(|| {
        Mutex::new(Stat {
            flows: HashMap::new(),
            pid_path: HashMap::new(),
            blocked: HashSet::new(),
            dirty: false,
            ticks: 0,
        })
    }).lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------- 累计 ----------------

/// 把本 tick 的速率积分进累计表。`dt` 是采样间隔（秒）。
pub fn accumulate(rates: &HashMap<u32, (i64, i64)>, names: &HashMap<u32, String>,
                  dt: f64, merge_by_path: bool) {
    if dt <= 0.0 || rates.is_empty() { return; }
    let mut g = stat();
    g.ticks = g.ticks.wrapping_add(1);
    // 每 300 tick 清一次 pid→path 缓存：进程退出后这些条目就没用了
    if g.ticks.is_multiple_of(300) { g.pid_path.clear(); }

    let mut touched = false;
    for (pid, (up_bps, down_bps)) in rates {
        let (up_bps, down_bps) = (*up_bps, *down_bps);
        if up_bps <= 0 && down_bps <= 0 { continue; }

        // pid → 路径（找不到路径的受保护进程按名字归并）
        let (key, path, name) = {
            let cached = g.pid_path.get(pid).cloned();
            let path = match cached {
                Some(p) => Some(p),
                None => {
                    let p = crate::sampling::proc::query_image_path(*pid);
                    if let Some(q) = &p { g.pid_path.insert(*pid, q.clone()); }
                    p
                }
            };
            let pname = names.get(pid).cloned()
                .or_else(|| path.as_deref().and_then(|p| std::path::Path::new(p).file_name()
                    .map(|f| f.to_string_lossy().to_string())))
                .unwrap_or_else(|| format!("PID {pid}"));
            match &path {
                Some(p) if merge_by_path => (p.to_lowercase(), p.clone(), pname),
                _ => (format!("name:{}", pname.to_lowercase()), String::new(), pname),
            }
        };

        let up_add = (up_bps.max(0) as f64 * dt) as u64;
        let down_add = (down_bps.max(0) as f64 * dt) as u64;
        let f = g.flows.entry(key.clone()).or_insert_with(|| Flow {
            key, name: name.clone(), path, ..Default::default()
        });
        f.up += up_add;
        f.down += down_add;
        if f.name.is_empty() { f.name = name; }
        touched = true;
    }
    if touched { g.dirty = true; }
}

/// 当前榜单：按累计流量从大到小。
pub fn top(limit: usize) -> Vec<Flow> {
    let g = stat();
    let mut v: Vec<Flow> = g.flows.values().cloned().collect();
    for f in &mut v {
        f.blocked = g.blocked.contains(&f.key);
    }
    v.sort_by_key(|f| -(f.up.saturating_add(f.down) as i64));
    v.truncate(limit);
    v
}

/// 全量榜单（设置面板的流量页用，取前 `limit` 条）。
pub fn top_with(limit: usize) -> Vec<Flow> { top(limit) }

/// 全机累计（不分进程）。
pub fn totals() -> (u64, u64) {
    let g = stat();
    g.flows.values().fold((0u64, 0u64), |(u, d), f| (u + f.up, d + f.down))
}

/// 有流量记录的进程总数（按 exe 路径去重）。
pub fn flow_count() -> usize { stat().flows.len() }

// ---------------- 防火墙 ----------------

/// 切换某个可执行文件的出站阻止规则。返回是否成功。
///
/// 实际动作在 `netfw::apply`（netsh，规则按**程序**匹配）；这里只负责同步内存状态
/// 并立刻落盘——规则已经生效了，界面状态不能等下一个 60 秒。
/// 需要管理员：非提权时 netsh 会失败并返回 false。
pub fn set_blocked(path: &str, block: bool) -> bool {
    if path.is_empty() { return false; }
    let key = path.to_lowercase();
    let ok = crate::netfw::apply(path, block);
    if ok {
        let mut g = stat();
        if block { g.blocked.insert(key); } else { g.blocked.remove(&key); }
        g.dirty = true;
        drop(g);
        // 立刻落盘：防火墙规则已经生效，界面状态不能等下一个 60 秒
        save(true);
    } else {
        zx_core::log::warn(format!("防火墙规则{}失败：{}", if block { "添加" } else { "删除" }, path));
    }
    ok
}

pub fn is_blocked(path: &str) -> bool {
    stat().blocked.contains(&path.to_lowercase())
}

/// 规则名带上文件名，方便用户在 `wf.msc` 里认出来。
/// 只改系统防火墙规则，**不碰本进程的累计数据**。
///
/// 设置面板是独立进程：它没有采样线程，内存里 flows 是空的。如果直接走 `set_blocked`，
/// 结尾的 `save()` 会把 netstat.json 写成"空 flows"，把常驻进程的历史记录覆盖掉。
/// 面板只负责下规则，状态交给常驻进程 30 秒一次的对账去同步。
pub fn set_blocked_rule(path: &str, block: bool) -> bool {
    crate::netfw::apply(path, block)
}

/// 当前被封禁的程序数。
pub fn blocked_count() -> usize { stat().blocked.len() }

/// 与系统里的真实规则对账。
///
/// 面板进程是独立进程，它自己也能加/删规则（一样走 netsh），但它那份"封禁集合"
/// 只在自己内存里；用户在 `wf.msc` 里手动删规则同样不会通知我们。
/// 所以常驻进程每 30 秒把集合按系统实际状态重算一遍，避免界面长期说谎。
pub fn reconcile() {
    let cands: Vec<String> = {
        let g = stat();
        let mut v: Vec<String> = g.blocked.iter().cloned().collect();
        for f in g.flows.values() {
            if !f.path.is_empty() { v.push(f.path.to_lowercase()); }
        }
        v.sort();
        v.dedup();
        v
    };
    let real = crate::netfw::filter_blocked(&cands);
    let mut g = stat();
    if real.len() != g.blocked.len() || !real.iter().all(|p| g.blocked.contains(p)) {
        g.blocked = real.into_iter().collect();
        g.dirty = true;
    }
}

/// 请求常驻进程清空统计（设置面板是独立进程，它自己清不动常驻的内存）。
pub fn request_reset() {
    let p = zx_core::cfg::run_dir().join("net-reset.flag");
    let _ = std::fs::write(p, b"1");
}

/// 常驻侧轮询：有人要求清空就消费掉这个哨兵。
pub fn take_reset_request() -> bool {
    let p = zx_core::cfg::run_dir().join("net-reset.flag");
    if p.exists() { let _ = std::fs::remove_file(&p); return true; }
    false
}

/// 清空历史累计（保留封禁规则）。界面上的"清空统计"。
pub fn reset() {
    let mut g = stat();
    g.flows.clear();
    g.pid_path.clear();
    g.dirty = true;
    drop(g);
    save(true);
    zx_core::log::info("已清空历史流量统计");
}

// ---------------- 落盘 ----------------

fn store_path() -> Option<std::path::PathBuf> { Some(zx_core::cfg::run_dir().join("netstat.json")) }

/// 有变化才写，且调用方负责限频（采样线程里 60 秒一次）。
pub fn save(force: bool) {
    let mut g = stat();
    if !g.dirty && !force { return; }
    g.dirty = false;
    let flows: Vec<_> = g.flows.values().map(|f| json!({
        "key": f.key, "name": f.name, "path": f.path, "up": f.up, "down": f.down,
    })).collect();
    let blocked: Vec<_> = g.blocked.iter().cloned().collect();
    let v = json!({ "flows": flows, "blocked": blocked });
    if let Some(p) = store_path() {
        if let Ok(text) = serde_json::to_string(&v) {
            let _ = std::fs::write(p, text);
        }
    }
}

/// 启动时读回（只恢复历史累计与封禁集合，本次运行计数归零）。
pub fn load() {
    let Some(p) = store_path() else { return };
    let Ok(text) = std::fs::read_to_string(&p) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let mut g = stat();
    if let Some(arr) = v.get("flows").and_then(|x| x.as_array()) {
        for f in arr {
            let Some(key) = f.get("key").and_then(|x| x.as_str()) else { continue };
            g.flows.insert(key.to_string(), Flow {
                key: key.to_string(),
                name: f.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                path: f.get("path").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                up: f.get("up").and_then(|x| x.as_u64()).unwrap_or(0),
                down: f.get("down").and_then(|x| x.as_u64()).unwrap_or(0),
                blocked: false,
            });
        }
    }
    if let Some(arr) = v.get("blocked").and_then(|x| x.as_array()) {
        for b in arr {
            if let Some(s) = b.as_str() { g.blocked.insert(s.to_string()); }
        }
    }
    zx_core::log::info(format!("网络流量缓存：{} 条记录，{} 条封禁规则", g.flows.len(), g.blocked.len()));
}
