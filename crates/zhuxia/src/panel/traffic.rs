//! 设置面板：按进程流量与联网控制（数据来自常驻进程写的 runtime.json）。

use serde_json::{json, Value};

use crate::panel::state::read_runtime;

/// 流量页的数据：常驻进程每 5 秒写进 `runtime.json` 的 `net` 段。
///
/// 面板是独立进程，自己没有采样线程，所以这里只做"读缓存 + 报告状态"。
#[tauri::command]
pub(crate) fn net_state() -> Value {
    let rt = read_runtime();
    json!({
        "net": rt.get("net").cloned().unwrap_or_else(|| json!({ "flows": [] })),
        // 常驻没在跑的时候，数据是旧的，界面要说清楚
        "alive": taskmon::runtime::instance_alive(),
        "admin": taskmon::win_admin(),
    })
}

/// 加/解除某个程序的出站阻止。netsh 要几百毫秒，前端应当先置"处理中"。
#[tauri::command]
pub(crate) fn set_firewall(path: String, block: bool) -> Value {
    // 只下规则：面板进程没有采样数据，走完整版会把 netstat.json 覆盖成空
    let ok = taskmon::netstat::set_blocked_rule(&path, block);
    json!({ "ok": ok, "admin": taskmon::win_admin() })
}

/// 清空历史统计：面板进程清不了常驻进程的内存，写哨兵让它自己清。
#[tauri::command]
pub(crate) fn reset_net_stats() -> Value {
    taskmon::netstat::request_reset();
    json!({ "ok": true })
}
