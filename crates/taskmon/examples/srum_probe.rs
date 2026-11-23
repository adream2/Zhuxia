//! SRUM 实时 API 探针 v2：把参数形状、provider class、错误码都摊开看。
//!
//! `cargo run --release -p taskmon --example srum_probe`

use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SystemTime {
    year: u16, month: u16, dow: u16, day: u16,
    hour: u16, minute: u16, second: u16, ms: u16,
}

type Cb = unsafe extern "system" fn(*mut c_void, *mut c_void);
type Register = unsafe extern "system" fn(
    i32, *const SystemTime, u32, *mut c_void, Cb, *mut *mut c_void, *mut *mut c_void) -> i32;
type Unregister = unsafe extern "system" fn(*mut c_void);

static HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "system" fn cb(ctx: *mut c_void, set: *mut c_void) {
    HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if set.is_null() {
        println!("  [cb] ctx={ctx:?} set=NULL");
        return;
    }
    let count = *(set as *const i32);
    let records = *((set as *const u8).add(8) as *const *const u8);
    println!("  [cb] ctx={ctx:?} count={count} records={records:?}");
    if records.is_null() { return; }
    for i in 0..count.min(4) as usize {
        let rec = records.add(i * 0x40);
        let payload = *(rec.add(0x28) as *const *const u8);
        let fields = *(rec.add(0x30) as *const u16);
        println!("    rec[{i}] payload={payload:?} fields={fields}");
    }
}

fn now() -> SystemTime {
    let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTime() };
    SystemTime { year: t.wYear, month: t.wMonth, dow: t.wDayOfWeek, day: t.wDay,
        hour: t.wHour, minute: t.wMinute, second: t.wSecond, ms: t.wMilliseconds }
}

fn msg(code: u32) -> String {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    unsafe {
        let mut buf: *mut u16 = std::ptr::null_mut();
        let n = windows::Win32::System::Diagnostics::Debug::FormatMessageW(
            windows::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_OPTIONS(0x1000 | 0x200),
            None, code, 0,
            windows::core::PWSTR(&mut buf as *mut _ as *mut u16), 0, None);
        if n == 0 || buf.is_null() { return String::new(); }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(buf, n as usize)).trim().to_string();
        let _ = LocalFree(HLOCAL(buf as *mut _));
        s
    }
}

fn main() {
    println!("admin={}", zx_core::win::is_admin());
    let mut dummy = 12345u64;
    unsafe {
        let lib = match libloading::Library::new("srumapi.dll") {
            Ok(l) => l,
            Err(e) => { println!("load: {e}"); return; }
        };
        let reg: libloading::Symbol<Register> = lib.get(b"SruRegisterRealTimeStats\0").unwrap();
        let unreg: libloading::Symbol<Unregister> = lib.get(b"SruUnregisterRealTimeStats\0").unwrap();

        // 1) provider class × flags × ctx
        for class in [0i32, 1, 2, 3] {
            for flags in [0x102u32, 0x101, 0x2, 0x1] {
                let st = now();
                let mut h: *mut c_void = std::ptr::null_mut();
                let mut init: *mut c_void = std::ptr::null_mut();
                let rc = reg(class, &st, flags, &mut dummy as *mut _ as *mut c_void, cb, &mut h, &mut init);
                if rc != 496 || !h.is_null() || !init.is_null() {
                    println!("class={class} flags=0x{flags:03X} rc={rc} h={h:?} init={init:?}");
                }
                if !h.is_null() { unreg(h); }
            }
        }

        // 2) 起始时间为 NULL（部分 build 允许省略）
        for flags in [0x102u32, 0x101] {
            let mut h: *mut c_void = std::ptr::null_mut();
            let mut init: *mut c_void = std::ptr::null_mut();
            let rc = reg(0, std::ptr::null(), flags, &mut dummy as *mut _ as *mut c_void, cb, &mut h, &mut init);
            println!("null-time flags=0x{flags:03X} rc={rc} h={h:?} init={init:?}");
        }

        // 3) 错误码释义
        println!("rc 496 => {:?}", msg(496));
        println!("last error => {:?}", windows::Win32::Foundation::GetLastError());

        // 4) 长等：看回调是否异步到来
        println!("--- 等 6 秒 ---");
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            println!("  hits={}", HITS.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
}
