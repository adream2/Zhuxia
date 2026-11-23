//! CPU：GetSystemTimes 增量 + NtQuerySystemInformation(SystemProcessorPerformanceInformation)
//! 每核占用；频率用 PDH `% Processor Performance` × 基频（Task Manager 同源）。

use std::collections::VecDeque;
use windows::core::PCWSTR;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::*;
use windows::Win32::System::Threading::{GetActiveProcessorCount, GetSystemTimes};

const MAX_HIST: usize = 60;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CorePerf {
    idle: i64,
    kernel: i64,
    user: i64,
    dpc: i64,
    interrupt: i64,
    count: u32,
}

pub struct CpuSample {
    pub pct: f64,
    pub per_core: Vec<f64>,
    pub hist: Vec<f64>,
    pub name: String,
    pub base_mhz: u32,
    pub cur_mhz: u32,
    pub logical: u32,
}

pub struct CpuSampler {
    prev_idle: u64,
    prev_kernel: u64,
    prev_user: u64,
    prev_core: Vec<CorePerf>,
    hist: VecDeque<f64>,
    name: String,
    base_mhz: u32,
    logical: u32,
    pdh_query: isize,
    pdh_counter: isize,
}

impl CpuSampler {
    pub fn new() -> CpuSampler {
        let logical = unsafe {
            let n = GetActiveProcessorCount(0xFFFF);
            if n == 0 { 1 } else { n }
        };
        let (name, base_mhz) = unsafe { read_cpu_registry() };
        let base_mhz = if base_mhz > 0 { base_mhz } else { unsafe { power_max_mhz(logical) } };

        let (idle, kernel, user) = system_times();
        let prev_core = core_times(logical);

        let mut pdh_query = 0isize;
        let mut pdh_counter = 0isize;
        unsafe {
            let path = zx_core::win::wide(r"\Processor Information(_Total)\% Processor Performance");
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut pdh_query) == 0
                && PdhAddEnglishCounterW(pdh_query, PCWSTR(path.as_ptr()), 0, &mut pdh_counter) == 0
            {
                PdhCollectQueryData(pdh_query); // 建立基线
            } else {
                if pdh_query != 0 { PdhCloseQuery(pdh_query); }
                pdh_query = 0;
                pdh_counter = 0;
            }
        }

        CpuSampler {
            prev_idle: idle, prev_kernel: kernel, prev_user: user,
            prev_core, hist: VecDeque::with_capacity(MAX_HIST),
            name, base_mhz, logical, pdh_query, pdh_counter,
        }
    }

    pub fn sample(&mut self) -> CpuSample {
        let (idle, kernel, user) = system_times();
        let di = idle.saturating_sub(self.prev_idle);
        let dk = kernel.saturating_sub(self.prev_kernel);
        let du = user.saturating_sub(self.prev_user);
        self.prev_idle = idle; self.prev_kernel = kernel; self.prev_user = user;

        // kernel_time 已含 idle，所以分母是 kernel+user。
        let total = dk + du;
        let pct = if total > 0 { (1.0 - di as f64 / total as f64) * 100.0 } else { 0.0 }.clamp(0.0, 100.0);
        if self.hist.len() >= MAX_HIST { self.hist.pop_front(); }
        self.hist.push_back(pct);

        let cur = core_times(self.logical);
        let mut per_core = Vec::with_capacity(cur.len());
        for (i, c) in cur.iter().enumerate() {
            if let Some(p) = self.prev_core.get(i) {
                let t = (c.kernel - p.kernel) + (c.user - p.user);
                let v = if t > 0 { (1.0 - (c.idle - p.idle) as f64 / t as f64) * 100.0 } else { 0.0 };
                per_core.push(v.clamp(0.0, 100.0));
            }
        }
        self.prev_core = cur;

        CpuSample {
            pct,
            per_core,
            hist: self.hist.iter().copied().collect(),
            name: self.name.clone(),
            base_mhz: self.base_mhz,
            cur_mhz: self.current_mhz(),
            logical: self.logical,
        }
    }

    fn current_mhz(&self) -> u32 {
        if self.pdh_query == 0 || self.pdh_counter == 0 { return self.base_mhz; }
        unsafe {
            if PdhCollectQueryData(self.pdh_query) != 0 { return self.base_mhz; }
            let mut val = PDH_FMT_COUNTERVALUE::default();
            if PdhGetFormattedCounterValue(self.pdh_counter, PDH_FMT_DOUBLE, None, &mut val) != 0 {
                return self.base_mhz;
            }
            let pct = val.Anonymous.doubleValue;
            if val.CStatus == 0 && pct > 0.0 && self.base_mhz > 0 {
                (self.base_mhz as f64 * pct / 100.0).round() as u32
            } else {
                self.base_mhz
            }
        }
    }
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CpuSampler {
    fn drop(&mut self) {
        if self.pdh_query != 0 { unsafe { PdhCloseQuery(self.pdh_query); } }
    }
}

fn ft(v: FILETIME) -> u64 { ((v.dwHighDateTime as u64) << 32) | v.dwLowDateTime as u64 }

fn system_times() -> (u64, u64, u64) {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { let _ = GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)); }
    (ft(idle), ft(kernel), ft(user))
}

fn core_times(n: u32) -> Vec<CorePerf> {
    const CLASS: u32 = 8; // SystemProcessorPerformanceInformation
    let size = std::mem::size_of::<CorePerf>();
    let mut buf = vec![0u8; n as usize * size + 64];
    let mut ret = 0u32;
    let status = unsafe {
        windows::Wdk::System::SystemInformation::NtQuerySystemInformation(
            windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS(CLASS as i32),
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            &mut ret,
        )
    };
    if status.0 != 0 { return Vec::new(); }
    let count = ((ret as usize) / size).min(n as usize);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let p = unsafe { &*(buf.as_ptr().add(i * size) as *const CorePerf) };
        out.push(*p);
    }
    out
}

unsafe fn read_cpu_registry() -> (String, u32) {
    use windows::Win32::System::Registry::*;
    let mut name = String::from("未知 CPU");
    let mut mhz = 0u32;
    let sub = zx_core::win::wide(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0");
    let mut hkey = HKEY::default();
    if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut hkey).is_err() {
        return (name, mhz);
    }
    let mut buf = [0u16; 256];
    let mut cb = (buf.len() * 2) as u32;
    if RegQueryValueExW(hkey, windows::core::w!("ProcessorNameString"), None, None,
                        Some(buf.as_mut_ptr() as *mut u8), Some(&mut cb)).is_ok() {
        let n = (cb as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        name = s.trim_end_matches('\0').trim().to_string();
    }
    let mut dw = 0u32;
    let mut cb2 = 4u32;
    if RegQueryValueExW(hkey, windows::core::w!("~MHz"), None, None,
                        Some(&mut dw as *mut u32 as *mut u8), Some(&mut cb2)).is_ok() {
        mhz = dw;
    }
    let _ = RegCloseKey(hkey);
    (name, mhz)
}

unsafe fn power_max_mhz(logical: u32) -> u32 {
    use windows::Win32::System::Power::*;
    #[repr(C)]
    #[derive(Default)]
    struct ProcPower { number: u32, max_mhz: u32, current_mhz: u32, mhz_limit: u32, max_idle: u32, current_idle: u32 }
    let size = std::mem::size_of::<ProcPower>();
    let mut buf = vec![0u8; logical as usize * size];
    if CallNtPowerInformation(POWER_INFORMATION_LEVEL(11), Some(std::ptr::null()), 0,
                              Some(buf.as_mut_ptr() as *mut _), buf.len() as u32).is_err() {
        return 0;
    }
    let mut sum = 0u64;
    let mut n = 0u32;
    for i in 0..logical as usize {
        let p = &*(buf.as_ptr().add(i * size) as *const ProcPower);
        if p.max_mhz > 0 { sum += p.max_mhz as u64; n += 1; }
    }
    if n > 0 { (sum / n as u64) as u32 } else { 0 }
}
