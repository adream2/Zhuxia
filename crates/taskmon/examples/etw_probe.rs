//! ETW 探针 v2：用 TDH 解出每个 EventID 的 Task/Opcode 名，顺便打印载荷前 4 字节。
//! 目的：标定哪个 id 是 TCP/UDP 的收/发，不靠猜。
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::System::Diagnostics::Etw::*;

static HITS: [AtomicU64; 128] = [const { AtomicU64::new(0) }; 128];
static BYTES: [AtomicU64; 128] = [const { AtomicU64::new(0) }; 128];
static SEEN: Mutex<[bool; 128]> = Mutex::new([false; 128]);

const PROVIDER: windows::core::GUID =
    windows::core::GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);

fn wide_at(base: *const u8, off: u32) -> String {
    if off == 0 { return String::new(); }
    unsafe {
        let p = base.add(off as usize) as *const u16;
        let mut n = 0usize;
        while *p.add(n) != 0 && n < 128 { n += 1; }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
    if rec.is_null() { return; }
    let r = &*rec;
    let id = r.EventHeader.EventDescriptor.Id as usize;
    if id >= 128 { return; }
    HITS[id].fetch_add(1, Ordering::Relaxed);
    if !r.UserData.is_null() && r.UserDataLength >= 4 {
        BYTES[id].fetch_add(*(r.UserData as *const u32) as u64, Ordering::Relaxed);
    }
    let first = {
        let mut s = SEEN.lock().unwrap();
        if s[id] { false } else { s[id] = true; true }
    };
    if !first { return; }

    let Ok(tdh) = libloading::Library::new("tdh.dll") else { return };
    type GetInfo = unsafe extern "system" fn(*const EVENT_RECORD, u32, *const c_void, *mut u8, *mut u32) -> u32;
    let Ok(f) = tdh.get::<GetInfo>(b"TdhGetEventInformation\0") else { return };
    let mut size: u32 = 0;
    let mut rc = f(rec, 0, std::ptr::null(), std::ptr::null_mut(), &mut size);
    if rc != 122 {
        println!("id={id} TdhGetEventInformation rc={rc}");
        return;
    }
    let mut buf = vec![0u8; size as usize];
    rc = f(rec, 0, std::ptr::null(), buf.as_mut_ptr(), &mut size);
    if rc != 0 { println!("id={id} second rc={rc}"); return; }
    // TRACE_EVENT_INFO：GUID(16)+GUID(16)+EVENT_DESCRIPTOR(16)+DecodingSource(4)=52
    let base = buf.as_ptr();
    let off = |i: usize| unsafe { *(base.add(52 + i * 4) as *const u32) };
    let provider = wide_at(base, off(0));
    let level = wide_at(base, off(1));
    let task = wide_at(base, off(4));
    let opcode = wide_at(base, off(5));
    let payload = if r.UserData.is_null() || r.UserDataLength < 4 {
        0
    } else {
        unsafe { *(r.UserData as *const u32) }
    };
    println!("id={id:>3} provider={provider} level={level} task={task} opcode={opcode} firstU32={payload} len={}", r.UserDataLength);
}

fn props(name: &str) -> Vec<u8> {
    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let base = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    let mut buf = vec![0u8; base + name_w.len() * 2 + 8];
    unsafe {
        let p = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        (*p).Wnode.BufferSize = buf.len() as u32;
        (*p).Wnode.Flags = 0x0002_0000;
        (*p).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        (*p).LoggerNameOffset = base as u32;
        (*p).FlushTimer = 1;
        let dst = buf.as_mut_ptr().add(base) as *mut u16;
        std::ptr::copy_nonoverlapping(name_w.as_ptr(), dst, name_w.len());
    }
    buf
}

fn main() {
    println!("admin={}", zx_core::win::is_admin());
    let name = "ZhuxiaEtwProbe";
    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut p = props(name);
        let _ = ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, windows::core::PCWSTR(name_w.as_ptr()),
            p.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);

        let mut p = props(name);
        let mut session = CONTROLTRACE_HANDLE { Value: 0 };
        let rc = StartTraceW(&mut session, windows::core::PCWSTR(name_w.as_ptr()),
                             p.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES);
        println!("StartTrace rc={}", rc.0);
        if rc.0 != 0 { return; }
        let rc = EnableTraceEx2(session, &PROVIDER, 1, 5, u64::MAX, 0, 0, None);
        println!("EnableTraceEx2 rc={}", rc.0);

        let name_w2: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut logfile = EVENT_TRACE_LOGFILEW {
            LoggerName: windows::core::PWSTR(name_w2.as_ptr() as *mut u16),
            ..Default::default()
        };
        logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = Some(on_event);
        let th = OpenTraceW(&mut logfile);
        if th.Value == 0 || th.Value == u64::MAX { println!("OpenTrace failed"); return; }
        let h = [th];
        let t = std::thread::spawn(move || { let rc = ProcessTrace(&h, None, None); println!("ProcessTrace rc={}", rc.0); });
        std::thread::sleep(std::time::Duration::from_secs(14));
        let _ = ControlTraceW(session, windows::core::PCWSTR::null(),
            props(name).as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);
        let _ = t.join();
        let mut rows: Vec<(usize, u64, u64)> = (0..128)
            .map(|i| (i, HITS[i].load(Ordering::Relaxed), BYTES[i].load(Ordering::Relaxed)))
            .filter(|(_, n, _)| *n > 0).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        for (id, n, b) in rows { println!("SUM id={id:>3} hits={n:<7} size={b}"); }
    }
}
