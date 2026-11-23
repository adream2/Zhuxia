//! DXCore 温度探针：确认适配器枚举、InstanceLuid、IsQueryStateSupported、QueryState 的实际返回。
//!
//! 用法：cargo run --release --example dxcore_probe -p taskmon

use std::mem::size_of;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::DXCore::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

const STATE_TEMP: u32 = 6;
const PROP_INSTANCE_LUID: u32 = 0;
const PROP_DRIVER_DESCRIPTION: u32 = 2;
const PROP_IS_INTEGRATED: u32 = 12;

unsafe fn get_str(ad: &IDXCoreAdapter, prop: u32) -> String {
    let size = match ad.GetPropertySize(DXCoreAdapterProperty(prop)) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if size == 0 { return String::new(); }
    let mut buf = vec![0u8; size];
    if ad.GetProperty(DXCoreAdapterProperty(prop), size, buf.as_mut_ptr() as *mut _).is_err() {
        return String::new();
    }
    let mut s = String::new();
    let mut i = 0;
    while i + 1 < buf.len() {
        let c = u16::from_le_bytes([buf[i], buf[i + 1]]);
        if c == 0 { break; }
        s.push(char::from_u32(c as u32).unwrap_or('?'));
        i += 2;
    }
    s
}

unsafe fn dump(list: &IDXCoreAdapterList, tag: &str) {
    let n = list.GetAdapterCount();
    println!("== {tag}: adapter count = {n} ==");
    for i in 0..n {
        let ad: IDXCoreAdapter = match list.GetAdapter(i) {
            Ok(a) => a,
            Err(e) => { println!("  [{i}] GetAdapter FAILED: {e:?}"); continue; }
        };
        let desc = get_str(&ad, PROP_DRIVER_DESCRIPTION);
        let mut luid = LUID::default();
        let r = ad.GetProperty(DXCoreAdapterProperty(PROP_INSTANCE_LUID), size_of::<LUID>(),
                               &mut luid as *mut _ as *mut _);
        let mut integ: u8 = 0;
        let _ = ad.GetProperty(DXCoreAdapterProperty(PROP_IS_INTEGRATED), 1, &mut integ as *mut u8 as *mut _);
        println!("  [{i}] {desc:?}  luid get={:?} 0x{:08X}_0x{:08X} integrated={integ}",
                 r.is_ok(), luid.HighPart as u32, luid.LowPart);

        // 温度：上游传 input = {physIndex:u32} (4B)，output = f32 (4B)
        if ad.IsQueryStateSupported(DXCoreAdapterState(STATE_TEMP)) {
            let mut inbuf = 0u32.to_le_bytes();
            for phys in 0u32..2 {
                inbuf.copy_from_slice(&phys.to_le_bytes());
                let mut out = [0u8; 4];
                let r = ad.QueryState(DXCoreAdapterState(STATE_TEMP), 4,
                                      Some(inbuf.as_ptr() as *const _), 4,
                                      out.as_mut_ptr() as *mut _);
                println!("       TEMP phys={phys}: ok={:?} val={:.1}", r.is_ok(),
                         f32::from_le_bytes(out));
            }
        } else {
            println!("       TEMP: 不支持");
        }
    }
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let factory: IDXCoreAdapterFactory = match DXCoreCreateAdapterFactory() {
            Ok(f) => f,
            Err(e) => { println!("DXCoreCreateAdapterFactory FAILED: {e:?}"); return; }
        };
        println!("factory ok");

        // 1) 无过滤属性
        match factory.CreateAdapterList::<IDXCoreAdapterList>(&[]) {
            Ok(l) => dump(&l, "no-filter"),
            Err(e) => println!("no-filter CreateAdapterList FAILED: {e:?}"),
        }

        // 2) D3D11 / D3D12 Graphics 运行时属性（Win10 也认）
        let attrs = [
            windows::core::GUID::from_u128(0x8c47866b_7583_450d_f0f0_6bada895af4b), // D3D11_GRAPHICS
            windows::core::GUID::from_u128(0x0c9ece4d_2f6e_4f01_8c96_e89e331b47b1), // D3D12_GRAPHICS
            windows::core::GUID::from_u128(0xb69eb219_3ded_4464_979f_a00bd4687006), // Win11 GPU
        ];
        for (i, a) in attrs.iter().enumerate() {
            match factory.CreateAdapterList::<IDXCoreAdapterList>(std::slice::from_ref(a)) {
                Ok(l) => dump(&l, &format!("attr{i}")),
                Err(e) => println!("attr{i} CreateAdapterList FAILED: {e:?}"),
            }
        }
    }
}
