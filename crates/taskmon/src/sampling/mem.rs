//! 内存：GlobalMemoryStatusEx + NtQuerySystemInformation(class 2 / 80) 的 Task Manager 明细。

use std::collections::VecDeque;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use crate::snapshot::MemDetail;

const MAX_HIST: usize = 60;
const PAGE: i64 = 4096;

pub struct MemSample {
    pub pct: f64,
    pub total_gb: f64,
    pub used_gb: f64,
    pub hist: Vec<f64>,
    pub detail: MemDetail,
}

pub struct MemSampler {
    hist: VecDeque<f64>,
}

impl MemSampler {
    pub fn new() -> MemSampler { MemSampler { hist: VecDeque::with_capacity(MAX_HIST) } }

    pub fn sample(&mut self) -> MemSample {
        let mut st = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        unsafe { let _ = GlobalMemoryStatusEx(&mut st); }

        let total = st.ullTotalPhys as f64;
        let avail = st.ullAvailPhys as f64;
        let pct = st.dwMemoryLoad as f64;
        if self.hist.len() >= MAX_HIST { self.hist.pop_front(); }
        self.hist.push_back(pct);

        let mut d = MemDetail {
            available: st.ullAvailPhys as i64,
            in_use: (st.ullTotalPhys - st.ullAvailPhys) as i64,
            committed: (st.ullTotalPageFile - st.ullAvailPageFile) as i64,
            commit_limit: st.ullTotalPageFile as i64,
            ..Default::default()
        };

        // class 2 = SystemPerformanceInformation：分页/非分页池页数 @0x70/0x74。
        let mut buf = vec![0u8; 0x178];
        let mut ret = 0u32;
        let st2 = unsafe {
            windows::Wdk::System::SystemInformation::NtQuerySystemInformation(
                windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS(2),
                buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut ret)
        };
        if st2.0 == 0 {
            let paged = unsafe { *(buf.as_ptr().add(0x70) as *const i32) } as i64;
            let nonpaged = unsafe { *(buf.as_ptr().add(0x74) as *const i32) } as i64;
            d.paged_pool = paged * PAGE;
            d.nonpaged_pool = nonpaged * PAGE;
        }

        // class 80 = SystemMemoryListInformation：standby 各优先级 @0x28..0x60，modified @0x10。
        let mut ml = vec![0u8; 0x400];
        let st3 = unsafe {
            windows::Wdk::System::SystemInformation::NtQuerySystemInformation(
                windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS(80),
                ml.as_mut_ptr() as *mut _, ml.len() as u32, &mut ret)
        };
        if st3.0 == 0 {
            let mut pages = 0i64;
            let mut off = 0x28;
            while off <= 0x60 {
                pages += unsafe { *(ml.as_ptr().add(off) as *const i64) };
                off += 8;
            }
            d.cached = pages * PAGE;
            d.modified = unsafe { *(ml.as_ptr().add(0x10) as *const i64) } * PAGE;
        }

        MemSample {
            pct: pct.clamp(0.0, 100.0),
            total_gb: total / (1024.0 * 1024.0 * 1024.0),
            used_gb: (total - avail) / (1024.0 * 1024.0 * 1024.0),
            hist: self.hist.iter().copied().collect(),
            detail: d,
        }
    }
}

impl Default for MemSampler {
    fn default() -> Self {
        Self::new()
    }
}
