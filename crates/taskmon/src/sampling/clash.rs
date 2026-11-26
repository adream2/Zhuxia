//! Clash / Mihomo 集成：轮询 external-controller 的 `/connections`，
//! 按进程聚合代理流量，作为带 `Clash` 标签的独立行并入网络榜单。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::snapshot::ProcRow;

pub const DEFAULT_ADDRESS: &str = "127.0.0.1:9090";

pub struct ClashPoller {
    rows: Arc<Mutex<Vec<ProcRow>>>,
    endpoint: Arc<Mutex<Option<(String, String)>>>,
}

impl ClashPoller {
    pub fn start() -> ClashPoller {
        let rows = Arc::new(Mutex::new(Vec::new()));
        let endpoint: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
        let r = rows.clone();
        let e = endpoint.clone();
        std::thread::Builder::new().name("zx-clash".into()).spawn(move || loop {
            let target = e.lock().ok().and_then(|g| g.clone());
            match target {
                None => std::thread::sleep(std::time::Duration::from_millis(2000)),
                Some((addr, secret)) => {
                    let url = if addr.starts_with("http") {
                        format!("{}/connections", addr.trim_end_matches('/'))
                    } else {
                        format!("http://{}/connections", addr.trim_end_matches('/'))
                    };
                    let body = if secret.is_empty() {
                        crate::http::get_text(&url, 2000)
                    } else {
                        let auth = format!("Bearer {secret}");
                        crate::http::get_with_headers(&url, 2000, &[("Authorization", auth.as_str())])
                    };
                    if let Some(body) = body {
                        let rows_now = parse(&body);
                        if let Ok(mut g) = r.lock() { *g = rows_now; }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2000));
                }
            }
        }).ok();
        ClashPoller { rows, endpoint }
    }

    pub fn set_endpoint(&self, addr: Option<String>, secret: Option<String>) {
        if let Ok(mut g) = self.endpoint.lock() {
            *g = addr.map(|a| (a, secret.unwrap_or_default()));
        }
    }

    pub fn latest(&self) -> Vec<ProcRow> { self.rows.lock().map(|g| g.clone()).unwrap_or_default() }
}

fn parse(body: &str) -> Vec<ProcRow> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new() };
    let Some(conns) = v.get("connections").and_then(|c| c.as_array()) else { return Vec::new() };
    let mut agg: HashMap<String, (i64, i64)> = HashMap::new();
    for c in conns {
        let path = c.get("metadata").and_then(|m| m.get("processPath")).and_then(|p| p.as_str()).unwrap_or("");
        let up = c.get("upload").and_then(|x| x.as_i64()).unwrap_or(0);
        let down = c.get("download").and_then(|x| x.as_i64()).unwrap_or(0);
        if path.is_empty() { continue; }
        let e = agg.entry(path.to_string()).or_insert((0, 0));
        e.0 += up;
        e.1 += down;
    }
    let mut rows: Vec<ProcRow> = agg.into_iter().map(|(path, (up, down))| {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
        ProcRow {
            name, pid: 0, up, down, path: Some(path), count: 1,
            via_clash: true, tag: Some("Clash".into()), ..Default::default()
        }
    }).collect();
    rows.sort_by_key(|r| -(r.up + r.down));
    rows.truncate(8);
    rows
}
