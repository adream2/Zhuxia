//! CPU 温度。
//!
//! Windows 没有通用的用户态 CPU 温度 API —— 这是硬事实，不是没实现。任务管理器自己也不显示。
//! 能拿到温度的路径只有三条，前两条不需要装驱动：
//!
//! 1. **ACPI 热区**（`ROOT\WMI:MSAcpi_ThermalZoneTemperature`）：BIOS 把 `_TZ_` 暴露出来才有。
//!    笔记本普遍有；桌面主板（尤其是 AMD）经常整个 thermal zone 都不导出，
//!    查询直接返回 `WBEM_E_NOT_SUPPORTED (0x80041018)` —— 本机就是这种。
//! 2. **LibreHardwareMonitor / OpenHardwareMonitor 的 WMI 命名空间**：装了这两个软件
//!    （并在其中勾选 "Options → WMI Provider"）就能读到真正的 package 温度（含 Tctl/Tdie）。
//!    它们是靠自带的内核驱动读 MSR/SMN 的，用户态自己做不到。
//! 3. 自带上签名的内核驱动 —— 不现实。
//!
//! 所以这里的策略是"尽力而为"：按 1 → 2 的顺序探测，拿到就发布，拿不到就
//! `valid = false`，界面显示 `--`。没有来源时每 60 秒重探一次（用户可能刚装 LHM）。

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use windows::core::BSTR;
use windows::Win32::System::Com::*;
use windows::Win32::System::Wmi::*;

/// 来源枚举（0 = 无，1 = ACPI，2 = LHM，3 = OHM）
static SOURCE: AtomicU8 = AtomicU8::new(0);
/// 最近一次读数（f32 的位模式；NaN 表示没有值）
static CELSIUS: AtomicU32 = AtomicU32::new(0x7FC0_0000);

const REFRESH_MS: u64 = 5000;
const RETRY_MS: u64 = 60_000;

pub struct ThermalSample {
    pub celsius: f64,
    pub valid: bool,
    pub source: &'static str,
}

pub fn latest() -> ThermalSample {
    let bits = CELSIUS.load(Ordering::Relaxed);
    let celsius = f32::from_bits(bits) as f64;
    ThermalSample {
        celsius,
        valid: celsius.is_finite() && celsius > 0.0,
        source: match SOURCE.load(Ordering::Relaxed) {
            1 => "ACPI",
            2 => "LibreHardwareMonitor",
            3 => "OpenHardwareMonitor",
            _ => "",
        },
    }
}

/// 起一个后台线程（WMI 首次连接要几十毫秒，绝不能挂在采样线程上）。
pub fn spawn() {
    std::thread::Builder::new().name("zx-thermal".into()).spawn(|| {
        // WMI 是 COM：这个线程自己进套间，进程内其它线程不受影响
        unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); }
        loop {
            let (src, temp) = probe();
            SOURCE.store(src, Ordering::Relaxed);
            CELSIUS.store(temp.unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
            let wait = if temp.is_some() { REFRESH_MS } else { RETRY_MS };
            std::thread::sleep(std::time::Duration::from_millis(wait));
        }
    }).ok();
}

/// 依次尝试各来源，返回 (来源编号, 摄氏温度)。
fn probe() -> (u8, Option<f32>) {
    if let Some(t) = query_acpi() {
        return (1, Some(t));
    }
    if let Some(t) = query_sensors(r"ROOT\LibreHardwareMonitor") {
        return (2, Some(t));
    }
    if let Some(t) = query_sensors(r"ROOT\OpenHardwareMonitor") {
        return (3, Some(t));
    }
    (0, None)
}

/// ACPI 热区：`CurrentTemperature` 是 0.1 开尔文。
fn query_acpi() -> Option<f32> {
    let rows = wmi_query("ROOT\\WMI",
                         "SELECT CurrentTemperature FROM MSAcpi_ThermalZoneTemperature",
                         &["CurrentTemperature"])?;
    let mut best: Option<f32> = None;
    for row in rows {
        let Some(raw) = row.nums.first().and_then(|v| *v) else { continue };
        let c = (raw / 10.0 - 273.15) as f32;
        if c > 5.0 && c < 125.0 && best.is_none_or(|b| c > b) {
            best = Some(c);
        }
    }
    best
}

/// LHM / OHM 的 Sensor 表：挑 CPU 的 package 温度。
fn query_sensors(ns: &str) -> Option<f32> {
    let rows = wmi_query(ns, "SELECT Name, SensorType, Value FROM Sensor",
                         &["Name", "SensorType", "Value"])?;
    let mut best: Option<f32> = None;
    for row in rows {
        let name = string_at(&row, 0).unwrap_or_default().to_lowercase();
        let kind = string_at(&row, 1).unwrap_or_default().to_lowercase();
        if kind != "temperature" { continue; }
        // 只要 CPU 相关的：Package / Tctl / Tdie / Core / CPU
        if !(name.contains("package") || name.contains("tctl") || name.contains("tdie")
             || name.contains("core") || name.contains("cpu")) {
            continue;
        }
        let Some(v) = row.nums.first().and_then(|v| *v) else { continue };
        let c = v as f32;
        if c > 5.0 && c < 125.0 && best.is_none_or(|b| c > b) {
            best = Some(c);
        }
    }
    best
}

/// 执行一次 WQL，每行返回"按属性顺序的数值列 + 文本列"。
///
/// 文本列在数值列之后追加：`row[0..n]` 是数值，`string_at` 取文本。
struct Row {
    nums: Vec<Option<f64>>,
    strs: Vec<String>,
}

fn string_at(row: &Row, i: usize) -> Option<String> {
    row.strs.get(i).cloned().filter(|s| !s.is_empty())
}

/// 一次查询里"前一半属性当数值、后一半当文本"太绕，这里简化：
/// 传入的属性名里，能转成数字的进 nums，其余进 strs（顺序各自保持）。
fn wmi_query(ns: &str, wql: &str, props: &[&str]) -> Option<Vec<Row>> {
    unsafe {
        let locator: IWbemLocator =
            CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
        let svc: IWbemServices = locator
            .ConnectServer::<_, _, _, _, _, Option<&IWbemContext>>(
                &BSTR::from(ns), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
            .ok()?;
        let _ = CoSetProxyBlanket(&svc, 10 /*RPC_C_AUTHN_WINNT*/, 0 /*RPC_C_AUTHZ_NONE*/, None,
                                  RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
                                  None, EOAC_NONE);
        let en: IEnumWbemClassObject = svc
            .ExecQuery(&BSTR::from(wql), &BSTR::from("WQL"),
                       WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0),
                       None::<&IWbemContext>)
            .ok()?;

        let mut out = Vec::new();
        loop {
            let mut objs: [Option<IWbemClassObject>; 8] = Default::default();
            let mut ret = 0u32;
            if en.Next(WBEM_INFINITE, &mut objs, &mut ret).is_err() || ret == 0 { break; }
            for o in objs.iter().take(ret as usize).flatten() {
                let mut row = Row { nums: Vec::new(), strs: Vec::new() };
                for p in props {
                    let mut v = windows::core::VARIANT::default();
                    if o.Get(&BSTR::from(*p), 0, &mut v, None, None).is_err() { continue; }
                    match v_f64(&v) {
                        Some(n) => row.nums.push(Some(n)),
                        None => {
                            let s = v_str(&v);
                            if !s.is_empty() { row.strs.push(s); }
                        }
                    }
                }
                out.push(row);
            }
            if (ret as usize) < 8 { break; }
        }
        Some(out)
    }
}

/// VARIANT 的标量取值。直接按 offset 读，不依赖 union 的字段名（0.58 里嵌套三层）。
unsafe fn v_f64(v: &windows::core::VARIANT) -> Option<f64> {
    let p = v as *const _ as *const u8;
    let vt = *(p as *const u16);
    match vt {
        2 => Some(*(p.add(8) as *const i16) as f64),        // VT_I2
        3 => Some(*(p.add(8) as *const i32) as f64),        // VT_I4
        4 => Some(*(p.add(8) as *const f32) as f64),        // VT_R4
        5 => Some(*(p.add(8) as *const f64)),               // VT_R8
        11 => Some(*(p.add(8) as *const i16) as f64),       // VT_BOOL
        17 => Some(*(p.add(8)) as f64),                    // VT_UI1
        18 => Some(*(p.add(8) as *const u16) as f64),       // VT_UI2
        19 => Some(*(p.add(8) as *const u32) as f64),       // VT_UI4
        _ => None,
    }
}

unsafe fn v_str(v: &windows::core::VARIANT) -> String {
    let p = v as *const _ as *const u8;
    if *(p as *const u16) != 8 { return String::new(); }   // VT_BSTR
    let b = &*(p.add(8) as *const BSTR);
    if b.is_empty() { String::new() } else { b.to_string() }
}
