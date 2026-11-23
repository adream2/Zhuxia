//! 数值格式化：网络速率、内存大小、时间、温度。
//! 逐条对齐上游 NetRateFormatter / MemorySizeFormatter / GpuTempText 的输出。

/// 网络速率（完整形式）：`0 B/s`、`912 B/s`、`1.2 KB/s`、`1192.1 MB/s`。
pub fn net_rate(bps: i64) -> String {
    let v = bps.max(0) as f64;
    if v < 1024.0 { return format!("{:.0} B/s", v); }
    if v < 1024.0 * 1024.0 { return format!("{:.1} KB/s", v / 1024.0); }
    if v < 1024.0 * 1024.0 * 1024.0 { return format!("{:.1} MB/s", v / (1024.0 * 1024.0)); }
    format!("{:.2} GB/s", v / (1024.0 * 1024.0 * 1024.0))
}

/// 网络速率（紧凑形式，侧边任务栏的窄条带用）：省掉单位前的空格与小数尾巴。
pub fn net_rate_compact(bps: i64) -> String {
    let v = bps.max(0) as f64;
    if v < 1024.0 { return format!("{:.0}B", v); }
    if v < 1024.0 * 1024.0 { return format!("{:.0}K", v / 1024.0); }
    if v < 1024.0 * 1024.0 * 1024.0 { return format!("{:.1}M", v / (1024.0 * 1024.0)); }
    format!("{:.1}G", v / (1024.0 * 1024.0 * 1024.0))
}

/// 字节量（进程内存 / 内存明细）：`512 MB`、`3.4 GB`。
pub fn bytes(n: i64) -> String {
    let v = n.max(0) as f64;
    const K: f64 = 1024.0;
    if v < K { return format!("{v:.0} B"); }
    if v < K * K { return format!("{:.1} KB", v / K); }
    if v < K * K * K { return format!("{:.1} MB", v / (K * K)); }
    if v < K * K * K * K { return format!("{:.2} GB", v / (K * K * K)); }
    format!("{:.2} TB", v / (K * K * K * K))
}

/// GiB 数（内存总量）：`31.9 GB`。
pub fn gb(n: f64) -> String { format!("{n:.1} GB") }

/// 运行时间：`3 天 04:15:22` / `04:15:22`。
pub fn uptime(ms: u64) -> String {
    let s = ms / 1000;
    let (d, h, m, sec) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60, s % 60);
    if d > 0 { format!("{d} 天 {h:02}:{m:02}:{sec:02}") } else { format!("{h:02}:{m:02}:{sec:02}") }
}

/// 频率：`4.2 GHz` / `3200 MHz`。
pub fn mhz(v: u32) -> String {
    if v >= 1000 { format!("{:.2} GHz", v as f64 / 1000.0) } else { format!("{v} MHz") }
}

/// GPU 温度文本（任务栏与详情共用）。NaN = 驱动不报温度。
pub fn temp(celsius: f64, fahrenheit: bool) -> String {
    if !celsius.is_finite() { return "--".into(); }
    if fahrenheit { format!("{:.0}°F", celsius * 9.0 / 5.0 + 32.0) } else { format!("{:.0}°C", celsius) }
}

/// 百分比：整数显示（任务栏），`42%`。
pub fn pct(v: f64) -> String { format!("{:.0}%", v.max(0.0)) }

/// 协商链路速率（bits/s）：`2.5 Gbps` / `100 Mbps`。
pub fn link_rate(bps: i64) -> String {
    if bps <= 0 { return "—".into(); }
    let b = bps as f64;
    if b >= 1e9 { format!("{:.1} Gbps", b / 1e9) }
    else if b >= 1e6 { format!("{:.0} Mbps", b / 1e6) }
    else if b >= 1e3 { format!("{:.0} Kbps", b / 1e3) }
    else { format!("{bps} bps") }
}

/// 磁盘类型（详情页"类型："一栏）。
pub fn disk_kind(k: u8) -> &'static str {
    match k { 1 => "SSD", 2 => "HDD", 3 => "SCM", 4 => "USB", 5 => "SD", _ => "未知" }
}
