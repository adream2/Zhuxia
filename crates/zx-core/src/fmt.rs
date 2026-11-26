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

#[cfg(test)]
mod tests {
    use super::*;

    // 这些函数直接决定任务栏上显示的数字。算错不会崩，只会悄悄误导人，
    // 所以边界值（1023/1024 这种进位点、0、负数、NaN）比典型值更重要。

    #[test]
    fn net_rate_steps_at_each_unit() {
        assert_eq!(net_rate(0), "0 B/s");
        assert_eq!(net_rate(1023), "1023 B/s");
        assert_eq!(net_rate(1024), "1.0 KB/s");
        assert_eq!(net_rate(1024 * 1024 - 1), "1024.0 KB/s");
        assert_eq!(net_rate(1024 * 1024), "1.0 MB/s");
        assert_eq!(net_rate(1024 * 1024 * 1024), "1.00 GB/s");
    }

    #[test]
    fn net_rate_never_shows_negative() {
        // 计数器回绕 / 换网卡时可能出现负数，显示成 -3 MB/s 只会让人以为坏了
        assert_eq!(net_rate(-12345), "0 B/s");
    }

    #[test]
    fn net_rate_compact_drops_space_and_decimals() {
        assert_eq!(net_rate_compact(0), "0B");
        assert_eq!(net_rate_compact(1023), "1023B");
        assert_eq!(net_rate_compact(1024), "1K");
        assert_eq!(net_rate_compact(1024 * 1024), "1.0M");
        assert_eq!(net_rate_compact(1024 * 1024 * 1024 * 2), "2.0G");
    }

    #[test]
    fn bytes_picks_tenths_up_to_gb() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KB");
        assert_eq!(bytes(1024 * 1024), "1.0 MB");
        assert_eq!(bytes(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(bytes(1024i64 * 1024 * 1024 * 1024), "1.00 TB");
        assert_eq!(bytes(-1), "0 B");
    }

    #[test]
    fn uptime_switches_to_days_only_when_there_are_some() {
        assert_eq!(uptime(0), "00:00:00");
        assert_eq!(uptime(86_399_000), "23:59:59", "差一秒还在当天");
        assert_eq!(uptime(86_400_000), "1 天 00:00:00");
        let three_days = ((3 * 86400 + 4 * 3600 + 15 * 60 + 22) * 1000) as u64;
        assert_eq!(uptime(three_days), "3 天 04:15:22");
    }

    #[test]
    fn mhz_switches_to_ghz_at_one_thousand() {
        assert_eq!(mhz(0), "0 MHz");
        assert_eq!(mhz(999), "999 MHz");
        assert_eq!(mhz(1000), "1.00 GHz");
        assert_eq!(mhz(4600), "4.60 GHz");
    }

    #[test]
    fn temp_hides_when_driver_reports_nothing() {
        // GPU 驱动不报温度时给 NaN，界面上要显示 --，不能显示 "NaN°C"
        assert_eq!(temp(f64::NAN, false), "--");
        assert_eq!(temp(f64::INFINITY, false), "--");
        assert_eq!(temp(61.4, false), "61°C");
        assert_eq!(temp(100.0, true), "212°F");
        assert_eq!(temp(-40.0, true), "-40°F", "两个刻度在这里相交");
    }

    #[test]
    fn pct_clamps_and_rounds() {
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(42.4), "42%");
        assert_eq!(pct(42.6), "43%");
        assert_eq!(pct(-5.0), "0%", "负占比（采样竞态）不该显示成 -5%");
        assert_eq!(pct(f64::NAN), "0%");
    }

    #[test]
    fn link_rate_covers_all_three_scales() {
        assert_eq!(link_rate(0), "—");
        assert_eq!(link_rate(-1), "—");
        assert_eq!(link_rate(100), "100 bps");
        assert_eq!(link_rate(1000), "1 Kbps");
        assert_eq!(link_rate(100_000_000), "100 Mbps");
        assert_eq!(link_rate(2_500_000_000), "2.5 Gbps");
    }

    #[test]
    fn disk_kind_falls_back_to_unknown() {
        assert_eq!(disk_kind(1), "SSD");
        assert_eq!(disk_kind(2), "HDD");
        assert_eq!(disk_kind(4), "USB");
        assert_eq!(disk_kind(0), "未知");
        assert_eq!(disk_kind(99), "未知", "真遇到新类型也不能显示乱码");
    }

    #[test]
    fn gb_keeps_one_decimal() {
        assert_eq!(gb(31.94), "31.9 GB");
        assert_eq!(gb(0.0), "0.0 GB");
    }
}
