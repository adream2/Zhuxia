//! CPU 温度来源探针：ACPI 热区 + LibreHardwareMonitor / OpenHardwareMonitor 的 WMI 命名空间。
//!
//! 用法：cargo run --release --example temp_probe -p taskmon

use windows::core::BSTR;
use windows::Win32::System::Com::*;
use windows::Win32::System::Wmi::*;

/// VARIANT 的 vt 与数据区（offset 8）—— 只读标量，不走 union 字段名更省事。
unsafe fn vt_of(v: &windows::core::VARIANT) -> u16 {
    *(v as *const _ as *const u16)
}
unsafe fn v_f64(v: &windows::core::VARIANT) -> Option<f64> {
    let p = v as *const _ as *const u8;
    match vt_of(v) {
        3 => Some(*(p.add(8) as *const i32) as f64),   // VT_I4
        2 => Some(*(p.add(8) as *const i16) as f64),   // VT_I2
        4 => Some(*(p.add(8) as *const f32) as f64),   // VT_R4
        5 => Some(*(p.add(8) as *const f64)),          // VT_R8
        1 => Some(*(p.add(8)) as f64),                 // VT_UI1
        19 => Some(*(p.add(8) as *const u32) as f64),  // VT_UI4
        _ => None,
    }
}

unsafe fn v_str(v: &windows::core::VARIANT) -> String {
    if vt_of(v) != 8 {
        return String::new();
    }
    let b = &*(v as *const _ as *const BSTR);
    if b.is_empty() { String::new() } else { b.to_string() }
}

fn dump(ns: &str, wql: &str, name_props: &[&str], value_prop: &str) {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        println!("-- {ns} : {wql}");
        println!("   CoInitializeEx -> {hr:?}");

        let locator: IWbemLocator = match CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) {
            Ok(l) => l,
            Err(e) => { println!("   CoCreateInstance 失败 {e:?}"); return; }
        };
        let svc: IWbemServices = match locator.ConnectServer::<_, _, _, _, _, Option<&IWbemContext>>(
            &BSTR::from(ns), &BSTR::new(), &BSTR::new(), &BSTR::new(),
            0, &BSTR::new(), None) {
            Ok(s) => s,
            Err(e) => { println!("   ConnectServer 失败 {e:?}"); return; }
        };
        let _ = CoSetProxyBlanket(&svc, 10 /*RPC_C_AUTHN_WINNT*/, 0 /*RPC_C_AUTHZ_NONE*/, None,
                                  RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE);

        let en: IEnumWbemClassObject = match svc.ExecQuery(
            &BSTR::from(wql), &BSTR::from("WQL"),
            WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0),
            None::<&IWbemContext>) {
            Ok(e) => e,
            Err(e) => { println!("   ExecQuery 失败 {e:?}"); return; }
        };

        let mut objs: [Option<IWbemClassObject>; 8] = Default::default();
        let mut ret = 0u32;
        let hr = en.Next(WBEM_INFINITE, &mut objs, &mut ret);
        println!("   Next -> {hr:?} returned={ret}");
        for o in objs.iter().take(ret as usize).flatten() {
            let mut label = String::new();
            for p in name_props {
                let mut v = windows::core::VARIANT::default();
                if o.Get(&BSTR::from(*p), 0, &mut v, None, None).is_ok() {
                    let s = v_str(&v);
                    if !s.is_empty() { label = format!("{}{}={} ", label, p, s); }
                }
            }
            let mut v = windows::core::VARIANT::default();
            let val = if o.Get(&BSTR::from(value_prop), 0, &mut v, None, None).is_ok() {
                v_f64(&v)
            } else { None };
            println!("   [{label}] {value_prop} = {val:?}");
        }
    }
}

fn main() {
    dump(r"ROOT\WMI", "SELECT * FROM MSAcpi_ThermalZoneTemperature",
         &["InstanceName"], "CurrentTemperature");
    dump(r"ROOT\LibreHardwareMonitor", "SELECT * FROM Sensor",
         &["Name", "SensorType"], "Value");
    dump(r"ROOT\OpenHardwareMonitor", "SELECT * FROM Sensor",
         &["Name", "SensorType"], "Value");
}
