//! 磁盘：每个物理盘一次 IOCTL_DISK_PERFORMANCE，增量算 利用率/读写/响应时间
//! （默认 96 分。
//! 与 Task Manager 的公式一致：util = 1 - idleΔ/queryΔ；响应时间除以读次数）。

use std::collections::{HashMap, VecDeque};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::DeviceIoControl;

const MAX_HIST: usize = 60;
const REENUM_TICKS: u32 = 30;
const MAX_FAILS: u32 = 3;
const MAX_PROBE: u32 = 15;

const IOCTL_DISK_PERFORMANCE: u32 = 0x0007_0020;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
/// 分区表布局（DRIVE_LAYOUT_INFORMATION_EX.PartitionStyle）
const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DiskPerf {
    bytes_read: i64,
    bytes_written: i64,
    read_time: i64,
    write_time: i64,
    idle_time: i64,
    read_count: u32,
    write_count: u32,
    queue_depth: u32,
    split_count: u32,
    query_time: i64,
    device_number: u32,
    storage_manager: [u16; 8],
}

pub struct DiskSample {
    pub headline: f64,
    pub hist: Vec<f64>,
    pub disks: Vec<crate::snapshot::DiskInfo>,
}

struct Entry {
    info: crate::snapshot::DiskInfo,
    prev: DiskPerf,
    has_base: bool,
    fails: u32,
}

pub struct DiskSampler {
    disks: HashMap<u32, Entry>,
    hist: VecDeque<f64>,
    ticks: u32,
    last_mode: (u8, usize),
}

impl DiskSampler {
    pub fn new() -> DiskSampler {
        DiskSampler {
            disks: HashMap::new(),
            hist: VecDeque::with_capacity(MAX_HIST),
            ticks: REENUM_TICKS,
            last_mode: (0, 0),
        }
    }

    pub fn sample(&mut self, mode: u8, specific: usize) -> DiskSample {
        if (mode, specific) != self.last_mode { self.hist.clear(); self.last_mode = (mode, specific); }

        self.ticks += 1;
        if self.ticks >= REENUM_TICKS { self.reenumerate(); self.ticks = 0; }

        let mut sum = 0.0;
        let mut max = 0.0;
        let mut specific_util = 0.0;
        let mut specific_found = false;
        let mut count = 0;
        for e in self.disks.values_mut() {
            if query_disk(e) {
                sum += e.info.util;
                if e.info.util > max { max = e.info.util; }
                if e.info.index as usize == specific { specific_util = e.info.util; specific_found = true; }
                count += 1;
            }
        }
        let dead: Vec<u32> = self.disks.iter().filter(|(_, e)| e.fails >= MAX_FAILS).map(|(k, _)| *k).collect();
        for k in dead { self.disks.remove(&k); }

        // 0=Average 1=Max 2=Specific（pinned 盘不在时回落平均）
        let headline = match mode {
            1 => max,
            2 if specific_found => { specific_util },
            _ => if count > 0 { sum / count as f64 } else { 0.0 },
        };
        if self.hist.len() >= MAX_HIST { self.hist.pop_front(); }
        self.hist.push_back(headline);

        let mut keys: Vec<u32> = self.disks.keys().copied().collect();
        keys.sort();
        let disks = keys.iter().filter_map(|k| self.disks.get(k).map(|e| e.info.clone())).collect();

        DiskSample { headline, hist: self.hist.iter().copied().collect(), disks }
    }

    fn reenumerate(&mut self) {
        let mut seen = Vec::new();
        for i in 0..=MAX_PROBE {
            let Some(h) = open_disk(i) else { continue };
            if self.disks.contains_key(&i) {
                unsafe { let _ = CloseHandle(h); }
                seen.push(i);
                continue;
            }
            let classified = unsafe { classify(h, i) };
            let style = unsafe { part_style(h) };
            unsafe { let _ = CloseHandle(h); }
            let Some((name, kind)) = classified else { continue };
            self.disks.insert(i, Entry {
                info: crate::snapshot::DiskInfo { index: i, name, kind, part_style: style,
                                                  ..Default::default() },
                prev: DiskPerf::default(),
                has_base: false,
                fails: 0,
            });
            seen.push(i);
        }
        self.disks.retain(|k, _| seen.contains(k));
        self.update_letters();
    }

    fn update_letters(&mut self) {
        let mut by_disk: HashMap<u32, Vec<String>> = HashMap::new();
        let mut size_by_disk: HashMap<u32, u64> = HashMap::new();
        let mask = unsafe { GetLogicalDrives() };
        for b in 0..26u32 {
            if mask & (1 << b) == 0 { continue; }
            let letter = format!("{}:", (b'A' + b as u8) as char);
            let path = zx_core::win::wide(&format!(r"\\.\{letter}"));
            let Ok(h) = (unsafe {
                CreateFileW(PCWSTR(path.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE,
                            None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None)
            }) else { continue };
            let mut buf = vec![0u8; 8 + 8 * 24];
            let mut got = 0u32;
            let ok = unsafe {
                DeviceIoControl(h, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, 0,
                                Some(buf.as_mut_ptr() as *mut _), buf.len() as u32, Some(&mut got), None).is_ok()
            };
            unsafe { let _ = CloseHandle(h); }
            if !ok { continue; }
            // 该卷的总/剩余容量（普通权限可用）
            let mut total = 0u64;
            let mut free = 0u64;
            let mut avail = 0u64;
            let root = zx_core::win::wide(&format!("{letter}\\"));
            let sized = unsafe {
                GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), Some(&mut avail),
                                    Some(&mut total), Some(&mut free)).is_ok()
            };
            let n = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]).clamp(0, 8);
            for e in 0..n as usize {
                let off = 8 + e * 24;
                let disk = i32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) as u32;
                by_disk.entry(disk).or_default().push(letter.clone());
                if sized && total > 0 {
                    // 一块盘可能有多个分区，容量相加才是整盘
                    *size_by_disk.entry(disk).or_insert(0) += total;
                }
            }
        }
        for (k, e) in self.disks.iter_mut() {
            let mut l = by_disk.get(k).cloned().unwrap_or_default();
            l.sort();
            // 文件系统按第一个盘符读（同一块盘上多个分区一般同格式）
            if let Some(first) = l.first() {
                e.info.fs = volume_fs(first);
            }
            if let Some(sz) = size_by_disk.get(k) {
                e.info.total_bytes = *sz;
            }
            e.info.letters = l.join(" ");
        }
    }
}

impl Default for DiskSampler {
    fn default() -> Self {
        Self::new()
    }
}

fn query_disk(e: &mut Entry) -> bool {
    let Some(h) = open_disk(e.info.index) else {
        e.fails += 1;
        zero(&mut e.info);
        return false;
    };
    let mut perf = DiskPerf::default();
    let mut got = 0u32;
    let ok = unsafe {
        DeviceIoControl(h, IOCTL_DISK_PERFORMANCE, None, 0,
                        Some(&mut perf as *mut _ as *mut _), std::mem::size_of::<DiskPerf>() as u32,
                        Some(&mut got), None).is_ok()
    };
    unsafe { let _ = CloseHandle(h); }
    if !ok { e.fails += 1; zero(&mut e.info); return false; }
    e.fails = 0;

    if !e.has_base {
        e.prev = perf;
        e.has_base = true;
        zero(&mut e.info);
        return true;
    }
    let dt = perf.query_time - e.prev.query_time;
    if dt <= 0 {
        zero(&mut e.info);
    } else {
        let idle = perf.idle_time - e.prev.idle_time;
        let util = if idle < dt { (1.0 - idle as f64 / dt as f64) * 100.0 } else { 0.0 };
        let secs = dt as f64 / 10_000_000.0;
        let read = (((perf.bytes_read - e.prev.bytes_read) as f64) / secs) as i64;
        let write = (((perf.bytes_written - e.prev.bytes_written) as f64) / secs) as i64;
        let mut resp = ((perf.write_time - e.prev.write_time) + (perf.read_time - e.prev.read_time)) as f64 / 10_000.0;
        let reads = perf.read_count as i64 - e.prev.read_count as i64;
        if reads != 0 { resp /= reads as f64; }
        e.info.util = util.clamp(0.0, 100.0);
        e.info.read = read.max(0);
        e.info.write = write.max(0);
        e.info.response_ms = resp.max(0.0);
    }
    e.prev = perf;
    true
}

fn zero(i: &mut crate::snapshot::DiskInfo) {
    i.util = 0.0; i.read = 0; i.write = 0; i.response_ms = 0.0;
}

fn open_disk(i: u32) -> Option<HANDLE> {
    let path = zx_core::win::wide(&format!(r"\\.\PhysicalDrive{i}"));
    unsafe {
        CreateFileW(PCWSTR(path.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None).ok()
    }
}

unsafe fn classify(h: HANDLE, index: u32) -> Option<(String, u8)> {
    #[repr(C)]
    struct Query { property_id: u32, query_type: u32, extra: [u8; 16] }
    let q = Query { property_id: 0, query_type: 0, extra: [0; 16] };
    let mut buf = [0u8; 512];
    let mut got = 0u32;
    if DeviceIoControl(h, IOCTL_STORAGE_QUERY_PROPERTY, Some(&q as *const _ as *const _),
                       std::mem::size_of::<Query>() as u32, Some(buf.as_mut_ptr() as *mut _),
                       buf.len() as u32, Some(&mut got), None).is_err() {
        return None;
    }
    let bus_type = i32::from_le_bytes([buf[0x1C], buf[0x1D], buf[0x1E], buf[0x1F]]);
    if bus_type == 7 { return None; } // BusTypeFileBackedVirtual（VHD）—Taskmgr 同样排除
    let vendor = read_ascii(&buf, u32::from_le_bytes([buf[0x0C], buf[0x0D], buf[0x0E], buf[0x0F]]) as usize);
    let product = read_ascii(&buf, u32::from_le_bytes([buf[0x10], buf[0x11], buf[0x12], buf[0x13]]) as usize);
    let mut name = format!("{vendor} {product}").trim().to_string();
    if name.is_empty() { name = format!("PhysicalDrive{index}"); }

    let kind = match bus_type {
        13 => 3, // SCM
        7 => return None,
        8 => 4,  // USB
        12 => 5, // SD
        _ => if seek_penalty(h) == Some(false) { 1 } else { 2 },
    };
    Some((name, kind))
}

/// 分区表样式（MBR / GPT）。
///
/// 读 `DRIVE_LAYOUT_INFORMATION_EX` 的头 8 个字节：DWORD PartitionStyle, DWORD PartitionCount。
/// 后面的 union（MBR 8 字节 / GPT 32 字节）不同，只读这两个 DWORD 最稳。
///
/// 注意这里**不查容量**：`IOCTL_DISK_GET_LENGTH_INFO` 需要读权限，普通进程打开
/// `\.\PhysicalDriveN` 时 `dwDesiredAccess = 0`，IOCTL 直接失败。容量改由
/// `update_letters` 按卷累加（`GetDiskFreeSpaceEx` 普通权限就能用）。
unsafe fn part_style(h: HANDLE) -> u8 {
    let mut got = 0u32;
    let mut buf = [0u8; 4096];
    if DeviceIoControl(h, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, None, 0,
                       Some(buf.as_mut_ptr() as *mut _), buf.len() as u32,
                       Some(&mut got), None).is_err() {
        return 0;
    }
    match u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) {
        1 => 2, // PARTITION_STYLE_GPT
        0 => 1, // PARTITION_STYLE_MBR
        _ => 0,
    }
}

/// 卷的文件系统名（"NTFS" 之类）。只读，不需要管理员。
fn volume_fs(letter: &str) -> String {
    let root = zx_core::win::wide(&format!("{letter}\\"));
    let mut fs = [0u16; 32];
    unsafe {
        if GetVolumeInformationW(PCWSTR(root.as_ptr()), None, None, None, None,
                                 Some(&mut fs)).is_err() {
            return String::new();
        }
    }
    let n = fs.iter().position(|c| *c == 0).unwrap_or(fs.len());
    String::from_utf16_lossy(&fs[..n]).trim().to_string()
}

unsafe fn seek_penalty(h: HANDLE) -> Option<bool> {
    #[repr(C)]
    struct Query { property_id: u32, query_type: u32, extra: [u8; 16] }
    let q = Query { property_id: 7, query_type: 0, extra: [0; 16] };
    let mut buf = [0u8; 0x0C];
    let mut got = 0u32;
    if DeviceIoControl(h, IOCTL_STORAGE_QUERY_PROPERTY, Some(&q as *const _ as *const _),
                       std::mem::size_of::<Query>() as u32, Some(buf.as_mut_ptr() as *mut _),
                       buf.len() as u32, Some(&mut got), None).is_err() {
        return None;
    }
    Some(buf[0x08] != 0)
}

fn read_ascii(buf: &[u8], off: usize) -> String {
    if off == 0 || off >= buf.len() { return String::new(); }
    let mut end = off;
    while end < buf.len() && buf[end] != 0 { end += 1; }
    String::from_utf8_lossy(&buf[off..end]).trim().to_string()
}
