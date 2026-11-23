//! 采样编排：一个线程、一个 tick，产出整份 Snapshot 并广播给所有窗口。

pub mod clash;
pub mod etw;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod mem;
pub mod net;
pub mod proc;
pub mod srum;
pub mod thermal;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::snapshot::{self, Snapshot, MASK_CPU, MASK_DISK, MASK_GPU, MASK_NET, MASK_RAM};

pub struct Sampler {
    cpu: cpu::CpuSampler,
    mem: mem::MemSampler,
    disk: disk::DiskSampler,
    net: net::NetSampler,
    proc: proc::ProcSampler,
    gpu: gpu::GpuSampler,
    srum: srum::Srum,
    etw: etw::EtwNet,
    netinfo: net::NetInfoPoller,
    clash: clash::ClashPoller,
    last_mask: u32,
}

impl Sampler {
    pub fn new() -> Sampler {
        Sampler {
            cpu: cpu::CpuSampler::new(),
            mem: mem::MemSampler::new(),
            disk: disk::DiskSampler::new(),
            net: net::NetSampler::new(),
            proc: proc::ProcSampler::new(),
            gpu: gpu::GpuSampler::new(),
            srum: srum::Srum::new(),
            etw: { let e = etw::EtwNet::start(); crate::set_etw_ok(e.available()); e },
            netinfo: net::NetInfoPoller::start(),
            clash: clash::ClashPoller::start(),
            last_mask: 0,
        }
    }

    pub fn sample(&mut self) -> Snapshot {
        let cfg = zx_core::cfg::cfg();
        let mask = cfg.mask();
        let prev = self.last_mask;
        self.last_mask = mask;
        let mut s = Snapshot { interval_ms: cfg.sample_interval_ms(), ..Default::default() };

        // 一次进程枚举喂三张榜单 + 计数（最贵的一步，全关时跳过）。
        let proc_sample = if mask != 0 { Some(self.proc.sample(cfg.merge_by_path())) } else { None };

        if mask & MASK_CPU != 0 {
            let c = self.cpu.sample();
            if prev & MASK_CPU == 0 {
                // 刚打开：只建立基线，避免把关闭期间的空档平均成一个假的尖峰。
            } else {
                s.cpu = c.pct;
                s.per_core = c.per_core;
                s.cpu_hist = c.hist;
                s.cpu_name = c.name;
                s.cpu_base_mhz = c.base_mhz;
                s.cpu_cur_mhz = c.cur_mhz;
                s.logical_cpus = c.logical;
                s.uptime_ms = proc::tick_ms();
                // 温度来自另一个后台线程（WMI 慢），这里只读缓存值
                let th = thermal::latest();
                s.cpu_temp = th.celsius;
                s.cpu_temp_valid = th.valid;
                s.cpu_temp_source = th.source.to_string();
            }
            if let Some(p) = &proc_sample {
                s.process_count = p.process_count;
                s.thread_count = p.thread_count;
                s.handle_count = p.handle_count;
                s.top_cpu = p.top_cpu.clone();
            }
        }

        if mask & MASK_RAM != 0 {
            let m = self.mem.sample();
            if prev & MASK_RAM != 0 {
                s.ram = m.pct;
                s.ram_total_gb = m.total_gb;
                s.ram_used_gb = m.used_gb;
                s.ram_hist = m.hist;
                s.mem = m.detail;
                if let Some(p) = &proc_sample {
                    s.mem.compressed = p.compressed;
                    s.top_mem = p.top_mem.clone();
                }
            }
        }

        if mask & MASK_NET != 0 {
            let n = self.net.sample(cfg.net_adapter_id.clone());
            self.clash.set_endpoint(
                if cfg.clash() {
                    Some(cfg.clash_api_address.clone().filter(|a| !a.trim().is_empty())
                        .unwrap_or_else(|| clash::DEFAULT_ADDRESS.to_string()))
                } else { None },
                cfg.clash_api_secret.clone(),
            );
            if prev & MASK_NET != 0 {
                s.net_up = n.up;
                s.net_down = n.down;
                s.net_name = n.name.clone();
                s.net_up_hist = n.up_hist.clone();
                s.net_down_hist = n.down_hist.clone();
                if cfg.public_ip() { s.net_info = self.netinfo.get(); }
                else {
                    let mut i = self.netinfo.get();
                    i.public_ip = None; i.public_ip_v6 = None; i.public_rtt_ms = -1;
                    s.net_info = i;
                }
                // 按进程网速：SRUM 优先，SRUM 不可用则用 ETW（都需管理员）
                let rates = if self.srum.available() { self.srum.rates() } else { self.etw.rates() };
                // 把"字节/秒"积分成累计字节（按 exe 路径聚合，落盘后重启接着算）
                if !rates.is_empty() {
                    let dt = cfg.sample_interval_ms() as f64 / 1000.0;
                    let names = proc_sample.as_ref().map(|p| &p.pid_to_name);
                    let empty = std::collections::HashMap::new();
                    crate::netstat::accumulate(&rates, names.unwrap_or(&empty), dt, cfg.merge_by_path());
                }
                let mut rows: Vec<snapshot::ProcRow> = Vec::new();
                for (pid, (up, down)) in rates.iter() {
                    if *up <= 0 && *down <= 0 && !self.srum.active_recently(*pid) { continue; }
                    let name = proc_sample.as_ref().and_then(|p| p.pid_to_name.get(pid)).cloned()
                        .unwrap_or_else(|| format!("PID {pid}"));
                    if name.eq_ignore_ascii_case("Memory Compression") { continue; }
                    rows.push(snapshot::ProcRow {
                        name, pid: *pid, up: *up, down: *down, count: 1,
                        path: proc::query_image_path(*pid), ..Default::default()
                    });
                }
                if cfg.merge_by_path() {
                    rows = proc::merge_by_path_rows(rows, |r| (r.up + r.down) as f64);
                } else {
                    rows.sort_by_key(|r| -(r.up + r.down));
                    rows.truncate(8);
                }
                rows.extend(self.clash.latest());
                rows.sort_by_key(|r| -(r.up + r.down));
                rows.truncate(8);
                if !rows.is_empty() && !NET_LOGGED.load(Ordering::Relaxed) {
                    NET_LOGGED.store(true, Ordering::Relaxed);
                    let top: Vec<String> = rows.iter().take(3)
                        .map(|r| format!("{}({}B/s)", r.name, r.up + r.down)).collect();
                    zx_core::log::info(format!("按进程网速首个非空榜单：{}", top.join(", ")));
                }
                s.top_net = rows;
                s.net_flows = crate::netstat::top(24);
                let (tu, td) = crate::netstat::totals();
                s.net_total_up = tu;
                s.net_total_down = td;
                s.net_flow_count = crate::netstat::flow_count();

            }
        } else {
            self.clash.set_endpoint(None, None);
        }

        if mask & MASK_DISK != 0 {
            let d = self.disk.sample(cfg.disk_display() as u8, cfg.disk_display_index.unwrap_or(0));
            if prev & MASK_DISK != 0 {
                s.disk_pct = d.headline;
                s.disk_hist = d.hist;
                s.disks = d.disks;
                if let Some(p) = &proc_sample { s.top_disk = p.top_disk.clone(); }
            }
        }

        if mask & MASK_GPU != 0 {
            let g = self.gpu.sample(cfg.gpu_display() as u8, cfg.gpu_display_index.unwrap_or(0),
                cfg.gpu_temp_index.map(|v| v as i32).unwrap_or(-1));
            if prev & MASK_GPU != 0 {
                s.gpu_ok = g.ok;
                s.gpu_pct = g.pct;
                s.gpu_hist = g.hist;
                s.gpu_temp = g.temp;
                s.gpu_temp_valid = g.temp_valid;
                s.gpus = g.gpus;
                let mut rows: Vec<snapshot::ProcRow> = g.per_proc.iter()
                    .filter(|(pid, (v, _))| **pid > 0 && *v > 0.0)
                    .map(|(pid, (v, eng))| snapshot::ProcRow {
                        name: proc_sample.as_ref().and_then(|p| p.pid_to_name.get(pid)).cloned()
                            .unwrap_or_else(|| format!("PID {pid}")),
                        pid: *pid, gpu: *v, gpu_engine: eng.clone(), count: 1,
                        path: proc::query_image_path(*pid), ..Default::default()
                    }).collect();
                if cfg.merge_by_path() {
                    rows = proc::merge_by_path_rows(rows, |r| r.gpu);
                } else {
                    rows.sort_by(|a, b| b.gpu.partial_cmp(&a.gpu).unwrap_or(std::cmp::Ordering::Equal));
                    rows.truncate(8);
                }
                s.top_gpu = rows;
            }
        }

        s
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// 采样线程的跳数（看门狗用）。
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// 退出请求：置位后采样线程跳出循环，`Sampler` 随之析构。
///
/// 这一步很关键：ETW 的 trace session、SRUM 的实时注册都在 `Drop` 里清理，
/// 进程直接结束的话它们会残留在系统里（SRUM 的实时注册有数量上限，反复启动会耗尽）。
static STOP: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

/// 等采样线程收尾，最多 `timeout_ms`。返回是否确认已退出。
pub fn shutdown(timeout_ms: u64) -> bool {
    STOP.store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if DONE.load(Ordering::Relaxed) { return true; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    DONE.load(Ordering::Relaxed)
}

/// 分段睡眠：每 25ms 看一眼退出标志，别让 shutdown 白等一整个采样周期。
fn sleep_interruptible(ms: u64) {
    let mut left = ms;
    while left > 0 && !STOP.load(Ordering::Relaxed) {
        let d = 25.min(left);
        std::thread::sleep(std::time::Duration::from_millis(d));
        left -= d;
    }
}
static NET_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static LAST_TICK_MS: AtomicU64 = AtomicU64::new(0);

pub fn spawn() {
    std::thread::Builder::new().name("zx-sampler".into()).spawn(|| {
        thermal::spawn();
        let mut sampler = Sampler::new();
        loop {
            if STOP.load(Ordering::Relaxed) { break; }
            let t0 = std::time::Instant::now();
            LAST_TICK_MS.store(proc::tick_ms(), Ordering::Relaxed);
            let snap = std::sync::Arc::new(sampler.sample());
            snapshot::publish(snap.clone());
            TICKS.fetch_add(1, Ordering::Relaxed);
            crate::runtime::notify_snapshot();
            // 每 5 秒把设备清单/能力写一份缓存，设置面板（独立进程）读它
            if TICKS.load(Ordering::Relaxed).is_multiple_of(5) { crate::runtime::write_cache(&snap); }
            // 累计流量有变化时每 60 tick 落一次盘（退出时还会强写一次）
            if TICKS.load(Ordering::Relaxed).is_multiple_of(60) { crate::netstat::save(false); }
            // 每 30 tick 与系统防火墙规则对账（面板进程或 wf.msc 都可能改过）
            if TICKS.load(Ordering::Relaxed).is_multiple_of(30) { crate::netstat::reconcile(); }
            let interval = zx_core::cfg::cfg().sample_interval_ms();
            let spent = t0.elapsed().as_millis() as u32;
            sleep_interruptible(interval.saturating_sub(spent).max(50) as u64);
        }
        // 跳出循环 → sampler 在这里析构：ETW session / SRUM 注册都会在 Drop 里清理
        drop(sampler);
        DONE.store(true, Ordering::Relaxed);
    }).ok();
}
