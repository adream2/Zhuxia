//! 进程：一次 NtQuerySystemInformation(SystemProcessInformation) 走完，
//! 同时产出 CPU / 内存 / 磁盘三张榜单 + 进程/线程/句柄总数（Task Manager 同源）。
//!
//! CPU% = 该进程 CycleTime 增量 ÷ 全部进程（含 Idle）增量，与任务管理器的
//! "整机 100%" 口径一致。

use std::collections::HashMap;
use windows::Win32::Foundation::{CloseHandle, UNICODE_STRING};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::snapshot::ProcRow;

const TOP_N: usize = 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct Spi {
    next: u32,
    nthreads: u32,
    ws_private: i64,
    hard_fault: u32,
    nthreads_hw: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UNICODE_STRING,
    base_priority: i32,
    _pad: i32,
    pid: *mut core::ffi::c_void,
    inherited: *mut core::ffi::c_void,
    handles: u32,
    session: u32,
    unique_key: *mut core::ffi::c_void,
    peak_virtual: usize,
    virtual_size: usize,
    page_faults: u32,
    _pad2: u32,
    peak_ws: usize,
    ws: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_nonpaged: usize,
    quota_nonpaged: usize,
    pagefile: usize,
    peak_pagefile: usize,
    private_pages: usize,
    read_ops: i64,
    write_ops: i64,
    other_ops: i64,
    read_bytes: i64,
    write_bytes: i64,
    other_bytes: i64,
}

const THREAD_SIZE: usize = 0x50;

pub struct ProcSample {
    pub top_cpu: Vec<ProcRow>,
    pub top_mem: Vec<ProcRow>,
    pub top_disk: Vec<ProcRow>,
    pub pid_to_name: HashMap<u32, String>,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub compressed: i64,
}

struct Raw {
    pid: u32,
    name: String,
    cycles: u64,
    ws: i64,
    threads: u32,
    handles: u32,
    disk_read: i64,
    disk_write: i64,
    delta: u64,
    read_rate: i64,
    write_rate: i64,
}

pub struct ProcSampler {
    prev_cycles: HashMap<u32, u64>,
    prev_read: HashMap<u32, i64>,
    prev_write: HashMap<u32, i64>,
    prev_tick_ms: u64,
    exe_cache: HashMap<u32, Option<String>>,
    buf: Vec<u8>,
}

impl ProcSampler {
    pub fn new() -> ProcSampler {
        ProcSampler {
            prev_cycles: HashMap::new(),
            prev_read: HashMap::new(),
            prev_write: HashMap::new(),
            prev_tick_ms: 0,
            exe_cache: HashMap::new(),
            buf: Vec::new(),
        }
    }

    pub fn sample(&mut self, merge_by_path: bool) -> ProcSample {
        let mut out = ProcSample {
            top_cpu: Vec::new(), top_mem: Vec::new(), top_disk: Vec::new(),
            pid_to_name: HashMap::new(), process_count: 0, thread_count: 0, handle_count: 0, compressed: 0,
        };
        let Some(mut list) = enumerate(&mut self.buf) else { return out };

        for r in &list {
            out.pid_to_name.insert(r.pid, r.name.clone());
        }

        let mut total = 0u64;
        for r in list.iter_mut() {
            let prev = self.prev_cycles.get(&r.pid).copied();
            r.delta = match prev {
                Some(p) if r.cycles >= p => r.cycles - p,
                _ => 0,
            };
            total += r.delta;
        }

        let mut threads = 0u32;
        let mut handles = 0u32;
        let mut procs = 0u32;
        let mut compressed = 0i64;
        for r in &list {
            threads += r.threads;
            handles += r.handles;
            if r.pid != 0 { procs += 1; }
            if compressed == 0 && r.name.eq_ignore_ascii_case("Memory Compression") {
                compressed = r.ws.max(0);
            }
        }
        out.thread_count = threads;
        out.handle_count = handles;
        out.process_count = procs;
        out.compressed = compressed;

        self.prev_cycles.clear();
        for r in &list { self.prev_cycles.insert(r.pid, r.cycles); }

        // 磁盘读写速率：PROCESS_DISK_COUNTERS 尾块的字节增量 / 真实间隔。
        let now = tick_ms();
        let secs = if self.prev_tick_ms != 0 { (now.saturating_sub(self.prev_tick_ms)) as f64 / 1000.0 } else { 0.0 };
        if secs > 0.0 {
            for r in list.iter_mut() {
                if let Some(&pr) = self.prev_read.get(&r.pid) {
                    if r.disk_read >= pr { r.read_rate = ((r.disk_read - pr) as f64 / secs) as i64; }
                }
                if let Some(&pw) = self.prev_write.get(&r.pid) {
                    if r.disk_write >= pw { r.write_rate = ((r.disk_write - pw) as f64 / secs) as i64; }
                }
            }
        }
        self.prev_read.clear();
        self.prev_write.clear();
        for r in &list {
            self.prev_read.insert(r.pid, r.disk_read);
            self.prev_write.insert(r.pid, r.disk_write);
        }
        self.prev_tick_ms = now;

        // ---- 按 CPU% ----
        list.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.name.cmp(&b.name)));
        let mut cpu_rows = Vec::new();
        for r in &list {
            if r.pid == 0 || r.name.eq_ignore_ascii_case("Memory Compression") { continue; }
            let pct = if total > 0 { r.delta as f64 / total as f64 * 100.0 } else { 0.0 };
            cpu_rows.push(ProcRow {
                name: r.name.clone(), pid: r.pid, cpu: pct.clamp(0.0, 100.0),
                count: 1, path: self.exe_path(r.pid, &r.name),
                ..Default::default()
            });
            if !merge_by_path && cpu_rows.len() >= TOP_N { break; }
        }
        out.top_cpu = if merge_by_path { merge_by_path_rows(cpu_rows, |p| p.cpu) } else { cpu_rows };

        // ---- 按内存 ----
        list.sort_by(|a, b| b.ws.cmp(&a.ws).then_with(|| a.name.cmp(&b.name)));
        let mut mem_rows = Vec::new();
        for r in &list {
            if r.pid == 0 || r.name.eq_ignore_ascii_case("Memory Compression") { continue; }
            mem_rows.push(ProcRow {
                name: r.name.clone(), pid: r.pid, working_set: r.ws.max(0),
                count: 1, path: self.exe_path(r.pid, &r.name),
                ..Default::default()
            });
            if !merge_by_path && mem_rows.len() >= TOP_N { break; }
        }
        out.top_mem = if merge_by_path { merge_by_path_rows(mem_rows, |p| p.working_set as f64) } else { mem_rows };

        // ---- 按磁盘读写速率 ----
        list.sort_by(|a, b| (b.read_rate + b.write_rate).cmp(&(a.read_rate + a.write_rate))
            .then_with(|| a.name.cmp(&b.name)));
        let mut disk_rows = Vec::new();
        for r in &list {
            if r.pid == 0 || r.name.eq_ignore_ascii_case("Memory Compression") { continue; }
            disk_rows.push(ProcRow {
                name: r.name.clone(), pid: r.pid, disk_read: r.read_rate, disk_write: r.write_rate,
                count: 1, path: self.exe_path(r.pid, &r.name),
                ..Default::default()
            });
            if !merge_by_path && disk_rows.len() >= TOP_N { break; }
        }
        out.top_disk = if merge_by_path {
            merge_by_path_rows(disk_rows, |p| (p.disk_read + p.disk_write) as f64)
        } else { disk_rows };

        out
    }

    fn exe_path(&mut self, pid: u32, image: &str) -> Option<String> {
        if let Some(v) = self.exe_cache.get(&pid) {
            if let Some(p) = v {
                if image.is_empty() || p.to_lowercase().ends_with(&image.to_lowercase()) { return v.clone(); }
            } else {
                return None;
            }
        }
        let path = query_image_path(pid);
        self.exe_cache.insert(pid, path.clone());
        path
    }
}

impl Default for ProcSampler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn query_image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if ok { Some(String::from_utf16_lossy(&buf[..len as usize])) } else { None }
    }
}

fn enumerate(reuse: &mut Vec<u8>) -> Option<Vec<Raw>> {
    const CLASS: u32 = 5; // SystemProcessInformation
    let mut size: usize = 1 << 20;
    for _ in 0..6 {
        if reuse.len() < size { reuse.resize(size, 0); }
        let buf = &mut reuse[..size];
        let mut ret = 0u32;
        let st = unsafe {
            windows::Wdk::System::SystemInformation::NtQuerySystemInformation(
                windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS(CLASS as i32),
                buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut ret)
        };
        if st.0 == 0 {
            let n = (ret as usize).min(size);
            return Some(parse(&reuse[..n], n));
        }
        if st.0 as u32 != 0xC000_0004 { return None; } // STATUS_INFO_LENGTH_MISMATCH
        size <<= 1;
    }
    None
}

fn parse(buf: &[u8], bytes: usize) -> Vec<Raw> {
    let stride = std::mem::size_of::<Spi>();
    let mut out = Vec::with_capacity(256);
    let mut off = 0usize;
    loop {
        if off + stride > bytes { break; }
        let spi = unsafe { &*(buf.as_ptr().add(off) as *const Spi) };
        let pid = spi.pid as usize as u32;
        let name = unsafe {
            if !spi.image_name.Buffer.is_null() && spi.image_name.Length > 0 {
                let s = std::slice::from_raw_parts(spi.image_name.Buffer.0 as *const u16,
                                                   (spi.image_name.Length / 2) as usize);
                String::from_utf16_lossy(s)
            } else if pid == 0 {
                "System Idle Process".to_string()
            } else {
                format!("PID {pid}")
            }
        };

        // 24H2+ 每个条目在线程数组之后跟一个 PROCESS_DISK_COUNTERS 尾块
        // {BytesRead@+0, BytesWritten@+8, ...}（任务管理器磁盘列的同一来源）。
        let trailer = off + stride + spi.nthreads as usize * THREAD_SIZE;
        // avail 一定要夹到缓冲区末尾：spi.next / nthreads 都是内核给的值，
        // 万一异常（或缓冲区被截断），trailer+16 会越过 buf 直接读越界内存。
        let avail = if spi.next != 0 {
            (off + spi.next as usize).min(bytes)
        } else {
            bytes
        };
        let (read, write) = if trailer + 16 <= avail {
            let r = unsafe { *(buf.as_ptr().add(trailer) as *const i64) };
            let w = unsafe { *(buf.as_ptr().add(trailer + 8) as *const i64) };
            (r, w)
        } else {
            (spi.read_bytes, spi.write_bytes)
        };

        out.push(Raw {
            pid, name, cycles: spi.cycle_time, ws: spi.ws_private,
            threads: spi.nthreads, handles: spi.handles,
            disk_read: read, disk_write: write,
            delta: 0, read_rate: 0, write_rate: 0,
        });
        if spi.next == 0 { break; }
        let next = spi.next as usize;
        // next 必须是正的且不越界，否则下一轮的 off + stride 检查就白做了
        if next == 0 || next > bytes - off { break; }
        off += next;
    }
    out
}

/// 合并相同 exe 路径的行（svchost 与 Clash 行单独保留，见上游 ProcessListMerger）。
pub fn merge_by_path_rows(rows: Vec<ProcRow>, rank: impl Fn(&ProcRow) -> f64) -> Vec<ProcRow> {
    let mut groups: HashMap<String, ProcRow> = HashMap::new();
    let mut solo: Vec<ProcRow> = Vec::new();
    for row in rows {
        if row.via_clash || row.name.eq_ignore_ascii_case("svchost.exe") {
            solo.push(row);
            continue;
        }
        let key = match &row.path {
            Some(p) if !p.is_empty() => p.to_lowercase(),
            _ => format!("\0{}", row.name),
        };
        match groups.get_mut(&key) {
            None => {
                let mut r = row;
                r.count = 1;
                r.pids = vec![r.pid];
                groups.insert(key, r);
            }
            Some(g) => {
                g.count += 1;
                g.pids.push(row.pid);
                g.cpu += row.cpu;
                g.working_set += row.working_set;
                g.up += row.up;
                g.down += row.down;
                g.disk_read += row.disk_read;
                g.disk_write += row.disk_write;
                if row.gpu > g.gpu { g.gpu = row.gpu; g.gpu_engine = row.gpu_engine.clone(); }
            }
        }
    }
    let mut merged: Vec<ProcRow> = groups.into_values().collect();
    for m in merged.iter_mut() {
        if m.count > 1 {
            m.tag = Some(format!("×{}", m.count));
        } else {
            m.pids.clear();
        }
    }
    merged.extend(solo);
    merged.sort_by(|a, b| rank(b).partial_cmp(&rank(a)).unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.name.cmp(&b.name)));
    merged.truncate(TOP_N);
    merged
}

pub fn tick_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}
