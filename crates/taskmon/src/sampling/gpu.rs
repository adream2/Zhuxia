//! GPU：PDH `\GPU Engine(*)\Utilization Percentage`（任务管理器 Processes 页 GPU 列的同源计数器）
//! 同时给出每个适配器的利用率与每个进程的利用率；`\GPU Adapter Memory(*)` 给显存占用。
//!
//! 温度走 DXCore 的 `AdapterTemperatureCelsius`（与任务管理器 `WdcGpuMonitor` 同源），
//! 按 LUID 与 PDH 实例名对齐，顺带用 `DriverDescription` 修正适配器显示名。
//! 驱动不支持时 `temp_valid = false`，界面显示 `--`。

use std::collections::{HashMap, VecDeque};
use windows::core::PCWSTR;
use windows::Win32::Graphics::DXCore::{
    DXCoreAdapterProperty, DXCoreAdapterState, DXCoreCreateAdapterFactory, IDXCoreAdapter,
    IDXCoreAdapterFactory, IDXCoreAdapterList,
};
use windows::Win32::System::Performance::*;

const MAX_HIST: usize = 60;
const NOCAP: PDH_FMT = PDH_FMT(0x8000);
const FMT: PDH_FMT = PDH_FMT(PDH_FMT_DOUBLE.0 | NOCAP.0);

#[derive(Clone, Default)]
struct AdapterAcc {
    luid: String,
    util: f64,
    engine: String,
    dedicated: i64,
    shared: i64,
}

pub struct GpuSample {
    pub ok: bool,
    pub pct: f64,
    pub hist: Vec<f64>,
    pub temp: f64,
    pub temp_valid: bool,
    pub gpus: Vec<crate::snapshot::GpuInfo>,
    pub per_proc: HashMap<u32, (f64, String)>,
}

pub struct GpuSampler {
    engine: isize,
    mem_ded: isize,
    mem_shr: isize,
    query: isize,
    hist: VecDeque<f64>,
    names: Vec<String>,
    last_mode: (u8, usize),
    // DXCore 适配器（LUID → 温度 / 名称）只在第一 tick 与适配器数变化时重查，
    // 温度本身每 tick 读一次，都是一次 COM 调用，开销可忽略。
    dx: Vec<DxAdapter>,
    dx_tick: u32,
}

impl GpuSampler {
    pub fn new() -> GpuSampler {
        let mut query = 0isize;
        let mut engine = 0isize;
        let mut mem_ded = 0isize;
        let mut mem_shr = 0isize;
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) == 0 {
                let e = zx_core::win::wide(r"\GPU Engine(*)\Utilization Percentage");
                // 专用/共享是两个**不同的计数器**，实例名里不会写 dedicated/shared。
                // 之前只加了一个 Dedicated，却拿实例名去猜类型：结果专用值只在第一次
                // 采样时写进去、之后再也不更新（共享值还被写进了同一个字段）。
                let md = zx_core::win::wide(r"\GPU Adapter Memory(*)\Dedicated Usage");
                let ms = zx_core::win::wide(r"\GPU Adapter Memory(*)\Shared Usage");
                if PdhAddEnglishCounterW(query, PCWSTR(e.as_ptr()), 0, &mut engine) != 0 { engine = 0; }
                if PdhAddEnglishCounterW(query, PCWSTR(md.as_ptr()), 0, &mut mem_ded) != 0 { mem_ded = 0; }
                if PdhAddEnglishCounterW(query, PCWSTR(ms.as_ptr()), 0, &mut mem_shr) != 0 { mem_shr = 0; }
                let _ = PdhCollectQueryData(query);
            }
        }
        GpuSampler {
            engine, mem_ded, mem_shr, query,
            hist: VecDeque::with_capacity(MAX_HIST),
            names: adapter_names(),
            last_mode: (255, 0),
            dx: Vec::new(),
            dx_tick: u32::MAX,
        }
    }

    pub fn available(&self) -> bool { self.engine != 0 }

    pub fn sample(&mut self, mode: u8, specific: usize, temp_index: i32) -> GpuSample {
        if (mode, specific) != self.last_mode { self.hist.clear(); self.last_mode = (mode, specific); }
        if self.query == 0 || self.engine == 0 {
            return GpuSample { ok: false, pct: 0.0, hist: Vec::new(),
                temp: f64::NAN, temp_valid: false, gpus: Vec::new(), per_proc: HashMap::new() };
        }
        unsafe { let _ = PdhCollectQueryData(self.query); }

        let items = read_array(self.engine, FMT);
        let mut adapters: HashMap<String, AdapterAcc> = HashMap::new();
        let mut per_proc: HashMap<u32, (f64, String)> = HashMap::new();

        for (inst, val) in &items {
            let p = parse_instance(inst);
            let Some(p) = p else { continue };
            let util = val.clamp(0.0, 100.0);
            let key = format!("{}_{}", p.luid, p.phys);
            let e = adapters.entry(key).or_default();
            e.luid = p.luid.clone();
            if util > e.util {
                e.util = util;
                // 该适配器当前负载最重的引擎（3D / Copy / VideoDecode…），详情页的"负载类型"
                e.engine = p.engtype.clone();
            }

            if let Some(pid) = p.pid {
                if pid > 0 {
                    let slot = per_proc.entry(pid).or_insert((0.0, String::new()));
                    slot.0 += util;
                    if util >= slot.0 - util && !p.engtype.is_empty() { slot.1 = p.engtype.clone(); }
                }
            }
        }

        if self.mem_ded != 0 {
            for (inst, val) in read_array(self.mem_ded, FMT) {
                if let Some(p) = parse_instance(&inst) {
                    let key = format!("{}_{}", p.luid, p.phys);
                    let e = adapters.entry(key).or_default();
                    e.luid = p.luid.clone();
                    e.dedicated = val as i64;
                }
            }
        }
        if self.mem_shr != 0 {
            for (inst, val) in read_array(self.mem_shr, FMT) {
                if let Some(p) = parse_instance(&inst) {
                    let key = format!("{}_{}", p.luid, p.phys);
                    let e = adapters.entry(key).or_default();
                    e.luid = p.luid.clone();
                    e.shared = val as i64;
                }
            }
        }

        if adapters.is_empty() {
            return GpuSample { ok: false, pct: 0.0, hist: Vec::new(),
                temp: f64::NAN, temp_valid: false, gpus: Vec::new(), per_proc };
        }

        let mut keys: Vec<String> = adapters.keys().cloned().collect();
        keys.sort();
        // DXCore 适配器清单：首次 + 每 60 tick（30 秒）重查一次，显卡热插拔/驱动重装能跟上
        if self.dx.is_empty() || self.dx_tick.is_multiple_of(60) { self.dx = dx_adapters(); }
        self.dx_tick = self.dx_tick.wrapping_add(1);

        let mut gpus = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            let a = &adapters[k];
            let nk = norm_luid(&a.luid);
            let dx = self.dx.iter().find(|d| d.key == nk);
            // 名字优先用驱动自报的（和任务管理器一致），没有才回落到注册表顺序
            let name = dx
                .map(|d| d.name.clone())
                .filter(|n| !n.is_empty())
                .or_else(|| self.names.get(i).cloned())
                .unwrap_or_else(|| format!("GPU {i}"));
            let t = dx.and_then(query_temp);
            gpus.push(crate::snapshot::GpuInfo {
                index: i,
                name,
                util: a.util,
                temp: t.map(|v| v as f64).unwrap_or(f64::NAN),
                temp_valid: t.is_some(),
                engine: a.engine.clone(),
                dedicated: a.dedicated,
                shared: a.shared,
                dedicated_total: dx.map(|d| d.ded_total).unwrap_or(0),
                shared_total: dx.map(|d| d.shr_total).unwrap_or(0),
            });
        }

        // 去掉软件渲染适配器（Microsoft Basic Render Driver）——
        // 它在任务管理器里都不算一块 GPU，出现在列表里只会多出一个恒 0% 的条目。
        gpus.retain(|g| !g.name.contains("Basic Render"));
        for (i, g) in gpus.iter_mut().enumerate() { g.index = i; }

        let mut sum = 0.0;
        let mut max = 0.0;
        let mut spec = 0.0;
        let mut spec_found = false;
        for g in &gpus {
            sum += g.util;
            if g.util > max { max = g.util; }
            if g.index == specific { spec = g.util; spec_found = true; }
        }
        let pct = match mode {
            1 => max,
            2 => if spec_found { spec } else { max },
            _ => if gpus.is_empty() { 0.0 } else { sum / gpus.len() as f64 },
        };
        if self.hist.len() >= MAX_HIST { self.hist.pop_front(); }
        self.hist.push_back(pct);

        // 温度来源：-1 = 最热的适配器，否则指定 GPU N（取不到就回落最高）
        let pick = |gs: &Vec<crate::snapshot::GpuInfo>| -> (f64, bool) {
            let mut best = (f64::NAN, false);
            for g in gs.iter().filter(|g| g.temp_valid) {
                if !best.1 || g.temp > best.0 { best = (g.temp, true); }
            }
            best
        };
        let (temp, temp_valid) = if temp_index >= 0 {
            match gpus.iter().find(|g| g.index as i32 == temp_index && g.temp_valid) {
                Some(g) => (g.temp, true),
                None => pick(&gpus),
            }
        } else {
            pick(&gpus)
        };

        GpuSample { ok: true, pct, hist: self.hist.iter().copied().collect(),
                    temp, temp_valid, gpus, per_proc }
    }
}

impl Default for GpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GpuSampler {
    fn drop(&mut self) { if self.query != 0 { unsafe { PdhCloseQuery(self.query); } } }
}

struct Inst { pid: Option<u32>, luid: String, phys: String, engtype: String }

fn parse_instance(name: &str) -> Option<Inst> {
    let mut pid = None;
    let mut luid = String::new();
    let mut phys = String::new();
    let mut engtype = String::new();
    let tokens: Vec<&str> = name.split('_').collect();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "pid" if i + 1 < tokens.len() => { pid = tokens[i + 1].parse().ok(); i += 2; }
            "luid" if i + 2 < tokens.len() => { luid = format!("{}_{}", tokens[i + 1], tokens[i + 2]); i += 3; }
            "phys" if i + 1 < tokens.len() => { phys = tokens[i + 1].to_string(); i += 2; }
            "eng" if i + 1 < tokens.len() => { i += 2; }
            "engtype" if i + 1 < tokens.len() => { engtype = tokens[i + 1..].join("_"); break; }
            _ => i += 1,
        }
    }
    if luid.is_empty() { return None; }
    Some(Inst { pid, luid, phys, engtype })
}

fn read_array(counter: isize, fmt: PDH_FMT) -> Vec<(String, f64)> {
    if counter == 0 { return Vec::new(); }
    let mut size = 0u32;
    let mut count = 0u32;
    let rc = unsafe { PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, None) };
    if rc != 0x8000_07D2 { return Vec::new(); } // PDH_MORE_DATA
    if size == 0 || count == 0 { return Vec::new(); }
    let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let mut buf = vec![0u8; size as usize + item * 4];
    let rc = unsafe {
        PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count,
            Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W))
    };
    if rc != 0 { return Vec::new(); }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let it = unsafe { &*(buf.as_ptr().add(i * item) as *const PDH_FMT_COUNTERVALUE_ITEM_W) };
        let name = if it.szName.is_null() {
            String::new()
        } else {
            unsafe {
                let mut len = 0usize;
                while *it.szName.0.add(len) != 0 && len < 512 { len += 1; }
                String::from_utf16_lossy(std::slice::from_raw_parts(it.szName.0, len))
            }
        };
        let v = unsafe { it.FmtValue.Anonymous.doubleValue };
        if it.FmtValue.CStatus == 0 { out.push((name, v)); }
    }
    out
}

/// 把 PDH 实例名里的 luid 折叠成可比较的规范形式。
///
/// PDH 给的是 `luid_0x00000000_0x000145AB` 两段原文，DXCore 给的是 LUID 结构体，
/// 大小写/前导零都可能不一致，两边都过一遍这个函数就不会错配。
fn norm_luid(s: &str) -> String {
    let mut out = String::new();
    for part in s.split('_') {
        if !out.is_empty() { out.push('_'); }
        let h = part.trim_start_matches("0x").trim_start_matches("0X");
        let v = u64::from_str_radix(h, 16).unwrap_or(0);
        out.push_str(&format!("{v:x}"));
    }
    out
}

/// 一个 DXCore 适配器：LUID（规范形式）+ 驱动自报名字 + 接口本身（每 tick 读温度）。
struct DxAdapter {
    key: String,
    name: String,
    ad: IDXCoreAdapter,
    /// 专用显存总量（7 = DedicatedAdapterMemory）
    ded_total: u64,
    /// 共享显存上限（9 = SharedSystemMemory）
    shr_total: u64,
}

/// 枚举全部图形适配器。
///
/// 坑：`CreateAdapterList(&[])`（numAttributes = 0）会返回 **E_INVALIDARG**，拿到空列表——
/// 必须至少给一个过滤属性。这里用 Win11 的硬件类型 GPU 属性，再补 D3D11 / D3D12 Graphics
/// 两个运行时属性（Win10 与虚拟显卡只认后两个），三趟按 LUID 去重。
fn dx_adapters() -> Vec<DxAdapter> {
    use windows::Win32::Foundation::LUID;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    let mut out: Vec<DxAdapter> = Vec::new();
    unsafe {
        // dxcore 对象是 COM；采样线程是一路裸线程，必须自己进套间。
        // S_OK / S_FALSE / RPC_E_CHANGED_MODE 都算成功，绝不 CoUninitialize。
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let Ok(factory) = DXCoreCreateAdapterFactory::<IDXCoreAdapterFactory>() else { return out };
        let attrs: [windows::core::GUID; 3] = [
            // Win11: 硬件类型 = GPU
            windows::core::GUID::from_u128(0xb69eb219_3ded_4464_979f_a00bd4687006),
            // D3D12_GRAPHICS / D3D11_GRAPHICS：两个操作系统族都认的运行时属性
            windows::core::GUID::from_u128(0x0c9ece4d_2f6e_4f01_8c96_e89e331b47b1),
            windows::core::GUID::from_u128(0x8c47866b_7583_450d_f0f0_6bada895af4b),
        ];
        for a in attrs {
            let Ok(list) = factory.CreateAdapterList::<IDXCoreAdapterList>(std::slice::from_ref(&a))
            else { continue };
            for i in 0..list.GetAdapterCount() {
                let Ok(ad) = list.GetAdapter::<IDXCoreAdapter>(i) else { continue };
                if !ad.IsValid() { continue; }
                // 0 = InstanceLuid
                let mut luid = LUID::default();
                if ad.GetProperty(DXCoreAdapterProperty(0), std::mem::size_of::<LUID>(),
                                  &mut luid as *mut _ as *mut _).is_err() {
                    continue;
                }
                let key = format!("{:x}_{:x}", luid.HighPart as u32, luid.LowPart);
                if out.iter().any(|d| d.key == key) { continue; }
                // 2 = DriverDescription（注意返回的是**窄字符串**，不是 UTF-16）
                let name = dx_string(&ad, 2);
                // 7 = DedicatedAdapterMemory / 9 = SharedSystemMemory，都是 u64 字节数
                let ded_total = dx_u64(&ad, 7);
                let shr_total = dx_u64(&ad, 9);
                out.push(DxAdapter { key, name, ad, ded_total, shr_total });
            }
        }
    }
    out
}

/// 读一个 u64 属性（显存容量）。
fn dx_u64(ad: &IDXCoreAdapter, prop: u32) -> u64 {
    let mut v = 0u64;
    unsafe {
        if ad.GetProperty(DXCoreAdapterProperty(prop), 8,
                          &mut v as *mut u64 as *mut _).is_err() {
            return 0;
        }
    }
    v
}

/// 读 DXCore 的字符串属性。驱动描述是 ANSI/UTF-8 窄串，按 UTF-16 读会得到一堆方块字。
unsafe fn dx_string(ad: &IDXCoreAdapter, prop: u32) -> String {
    let size = match ad.GetPropertySize(DXCoreAdapterProperty(prop)) {
        Ok(s) if s > 1 => s,
        _ => return String::new(),
    };
    let mut buf = vec![0u8; size];
    if ad.GetProperty(DXCoreAdapterProperty(prop), size, buf.as_mut_ptr() as *mut _).is_err() {
        return String::new();
    }
    if let Some(z) = buf.iter().position(|b| *b == 0) { buf.truncate(z); }
    String::from_utf8_lossy(&buf).trim().to_string()
}

/// 读一个适配器的摄氏温度。
///
/// 坑：`AdapterTemperatureCelsius` 需要 **inputStateDetails = {physIndex: u32}**（4 字节），
/// 并且输出缓冲恰好 4 字节。传 inputSize = 0 / NULL 会直接 E_INVALIDARG——
/// 这就是之前"`IsQueryStateSupported` 为真却永远读不到值"的原因。
fn query_temp(d: &DxAdapter) -> Option<f32> {
    unsafe {
        // 6 = AdapterTemperatureCelsius
        if !d.ad.IsQueryStateSupported(DXCoreAdapterState(6)) { return None; }
        let phys = 0u32.to_le_bytes();
        let mut out = [0u8; 4];
        let r = d.ad.QueryState(DXCoreAdapterState(6), 4, Some(phys.as_ptr() as *const _), 4,
                                out.as_mut_ptr() as *mut _);
        if r.is_err() { return None; }
        let t = f32::from_le_bytes(out);
        if t > 0.0 && t < 150.0 { Some(t) } else { None }
    }
}

fn adapter_names() -> Vec<String> {
    use windows::Win32::System::Registry::*;
    let mut out = Vec::new();
    let base = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    for i in 0..8 {
        let sub = zx_core::win::wide(&format!("{base}\\{i:04}"));
        let mut hkey = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut hkey) }.is_err() {
            continue;
        }
        let mut buf = [0u16; 256];
        let mut cb = (buf.len() * 2) as u32;
        let ok = unsafe {
            RegQueryValueExW(hkey, windows::core::w!("DriverDesc"), None, None,
                Some(buf.as_mut_ptr() as *mut u8), Some(&mut cb)).is_ok()
        };
        unsafe { let _ = RegCloseKey(hkey); }
        if ok {
            let n = (cb as usize / 2).min(buf.len());
            let s = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').trim().to_string();
            if !s.is_empty() { out.push(s); }
        }
    }
    out
}
