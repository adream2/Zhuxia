//! 一次采样的完整快照。UI 层只读它，不自己采样。
//!
//! 分三层组织：
//! - 头部标量：任务栏直接显示（CPU / 内存 / 磁盘 / GPU / 网速）；
//! - 五个进程榜：CPU、内存、磁盘、网络、GPU 各一份，详情弹窗直接换着读；
//! - 明细结构：内存分项、每块磁盘、每块显卡、网络链路。

/// 进程表的一行（也可能是一组合并后的同名程序）。
#[derive(Clone, Default, Debug)]
pub struct ProcRow {
    pub name: String,
    pub pid: u32,
    pub cpu: f64,
    pub working_set: i64,
    pub up: i64,
    pub down: i64,
    pub disk_read: i64,
    pub disk_write: i64,
    pub gpu: f64,
    pub gpu_engine: String,
    pub path: Option<String>,
    /// 合并相同程序后的成员数（1 = 普通行）。
    pub count: u32,
    /// 合并行的全部成员 PID（结束进程时要全杀）。
    pub pids: Vec<u32>,
    /// 行标签：`Clash` / `服务` / `服务组` / `×N`。
    pub tag: Option<String>,
    pub via_clash: bool,
    /// svchost 承载的服务名（tooltip 用）。
    pub services: Option<String>,
}

#[derive(Clone, Default, Debug)]
pub struct DiskInfo {
    pub index: u32,
    pub name: String,
    /// 1=SSD 2=HDD 3=SCM 4=USB 5=SD
    pub kind: u8,
    /// 这块盘上的盘符（多分区时连写）
    pub letters: String,
    /// 物理盘容量（字节，0 = 未知）
    pub total_bytes: u64,
    /// 分区表样式：0=未知 1=MBR 2=GPT
    pub part_style: u8,
    /// 文件系统（NTFS / exFAT / FAT32…），按第一个盘符读
    pub fs: String,
    pub util: f64,
    pub read: i64,
    pub write: i64,
    pub response_ms: f64,
}

#[derive(Clone, Default, Debug)]
pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub util: f64,
    pub temp: f64,
    pub temp_valid: bool,
    pub engine: String,
    pub dedicated: i64,
    pub shared: i64,
    /// 专用显存总量（DXCore DedicatedAdapterMemory，0 = 驱动不报）
    pub dedicated_total: u64,
    /// 共享显存上限（DXCore SharedSystemMemory，0 = 驱动不报）
    pub shared_total: u64,
}

/// 内存分项，详情弹窗用。
#[derive(Clone, Default, Debug)]
pub struct MemDetail {
    pub in_use: i64,
    pub compressed: i64,
    pub available: i64,
    pub committed: i64,
    pub commit_limit: i64,
    pub cached: i64,
    pub paged_pool: i64,
    pub nonpaged_pool: i64,
    pub modified: i64,
}

/// 链路信息（无线才有 SSID / 频段，有线只有速率）。
#[derive(Clone, Default, Debug)]
pub struct NetInfo {
    pub connection_type: String,
    pub ssid: Option<String>,
    pub standard: Option<String>,
    pub band: Option<String>,
    pub width: Option<String>,
    pub link_rx_bps: i64,
    pub link_tx_bps: i64,
    pub local_ip: Option<String>,
    pub public_ip: Option<String>,
    pub public_ip_v6: Option<String>,
    pub gateway_rtt_ms: i64,
    pub public_rtt_ms: i64,
}

#[derive(Clone, Default, Debug)]
pub struct Snapshot {
    pub interval_ms: u32,

    // ---- CPU
    pub cpu: f64,
    pub per_core: Vec<f64>,
    pub cpu_hist: Vec<f64>,
    pub cpu_name: String,
    pub cpu_base_mhz: u32,
    pub cpu_cur_mhz: u32,
    /// CPU 温度（ACPI 热区或 LHM/OHM）；拿不到时 valid=false，界面显示 `--`
    pub cpu_temp: f64,
    pub cpu_temp_valid: bool,
    pub cpu_temp_source: String,
    pub logical_cpus: u32,

    // ---- 系统概况
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub uptime_ms: u64,

    // ---- 五个进程榜
    pub top_cpu: Vec<ProcRow>,
    pub top_mem: Vec<ProcRow>,
    pub top_disk: Vec<ProcRow>,
    pub top_net: Vec<ProcRow>,
    pub top_gpu: Vec<ProcRow>,

    // ---- 内存
    pub ram: f64,
    pub ram_total_gb: f64,
    pub ram_used_gb: f64,
    pub ram_hist: Vec<f64>,
    pub mem: MemDetail,

    // ---- 网络
    pub net_up: i64,
    pub net_down: i64,
    pub net_name: String,
    pub net_up_hist: Vec<i64>,
    pub net_down_hist: Vec<i64>,
    pub net_info: NetInfo,
    /// 按进程的历史流量（净网详情页的「累计」视图）
    pub net_flows: Vec<crate::netstat::Flow>,
    pub net_total_up: u64,
    pub net_flow_count: usize,
    pub net_total_down: u64,

    // ---- 磁盘
    pub disk_pct: f64,
    pub disk_hist: Vec<f64>,
    pub disks: Vec<DiskInfo>,

    // ---- 显卡
    pub gpu_ok: bool,
    pub gpu_pct: f64,
    pub gpu_hist: Vec<f64>,
    pub gpu_temp: f64,
    pub gpu_temp_valid: bool,
    pub gpus: Vec<GpuInfo>,
}

/// 全局最新快照：采样线程发布，UI 线程读取。
///
/// 写少读多，用 `RwLock<Arc<_>>` 而不是消息传递：读者只拿 Arc 的克隆，
/// 不会在渲染期间被采样线程卡住。
static SLOT: std::sync::OnceLock<std::sync::RwLock<std::sync::Arc<Snapshot>>> =
    std::sync::OnceLock::new();

fn slot() -> &'static std::sync::RwLock<std::sync::Arc<Snapshot>> {
    SLOT.get_or_init(|| std::sync::RwLock::new(std::sync::Arc::new(Snapshot::default())))
}

pub fn publish(s: std::sync::Arc<Snapshot>) {
    *slot().write().unwrap_or_else(|p| p.into_inner()) = s;
}

pub fn latest() -> std::sync::Arc<Snapshot> {
    slot().read().unwrap_or_else(|p| p.into_inner()).clone()
}

// 任务栏分组位：与设置里的「显示哪些项」共用同一套位序。
pub const MASK_CPU: u32 = 1;
pub const MASK_RAM: u32 = 2;
pub const MASK_DISK: u32 = 4;
pub const MASK_GPU: u32 = 8;
pub const MASK_NET: u32 = 16;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn publish_replaces_latest_snapshot() {
        let s = Snapshot { cpu: 42.0, ..Default::default() };
        publish(Arc::new(s));
        assert_eq!(latest().cpu, 42.0);

        let s = Snapshot { cpu: 7.5, ..Default::default() };
        publish(Arc::new(s));
        assert_eq!(latest().cpu, 7.5);
    }

    #[test]
    fn masks_are_one_bit_per_group() {
        let all = MASK_CPU | MASK_RAM | MASK_DISK | MASK_GPU | MASK_NET;
        assert_eq!(all, 0b11111);
        assert_eq!(all.count_ones(), 5);
    }
}
