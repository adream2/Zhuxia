//! SRUM 实时 API（srumapi.dll，未文档化）：任务管理器"进程 → 网络"列的同一数据源。
//!
//! 动态加载 + 每次 tick 对累计值求差；回调在 SRU 自己的线程上跑，累加器加锁。
//! 任何一步失败都只是"没有按进程网速"，绝不影响其它指标。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type SruCb = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void);
type RegisterFn = unsafe extern "system" fn(
    i32, *const SYSTEMTIME, u32, *mut core::ffi::c_void, SruCb,
    *mut *mut core::ffi::c_void, *mut *mut core::ffi::c_void) -> i32;
type UnregisterFn = unsafe extern "system" fn(*mut core::ffi::c_void);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEMTIME {
    pub year: u16, pub month: u16, pub day_of_week: u16, pub day: u16,
    pub hour: u16, pub minute: u16, pub second: u16, pub ms: u16,
}

/// 注册是否成功（供 UI 判断"没数据"到底是权限、接口不可用，还是真的没流量）。
static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn registered() -> bool { REGISTERED.load(std::sync::atomic::Ordering::Relaxed) }

#[derive(Default)]
struct Acc {
    cur: HashMap<u32, (u64, u64)>, // pid -> (down, up)
    version: u64,
}

pub struct Srum {
    _lib: Option<libloading::Library>,
    reg: *mut core::ffi::c_void,
    unregister: Option<UnregisterFn>,
    acc: Arc<Mutex<Acc>>,
    prev: HashMap<u32, (u64, u64)>,
    rates: HashMap<u32, (i64, i64)>,
    last_active: HashMap<u32, std::time::Instant>,
    last_version: u64,
    last_time: Option<std::time::Instant>,
    available: bool,
}

unsafe extern "system" fn on_records(ctx: *mut core::ffi::c_void, record_set: *mut core::ffi::c_void) {
    if ctx.is_null() || record_set.is_null() { return; }
    let acc = &*(ctx as *const Mutex<Acc>);
    let mut g = match acc.lock() { Ok(g) => g, Err(_) => return };

    // _SRU_STATS_RECORD_SET: count @0x00, records ptr @0x08
    let count = *(record_set as *const i32);
    if count <= 0 || count > 4096 { return; }
    let records = *((record_set as *const u8).add(8) as *const *const u8);
    if records.is_null() { return; }

    for i in 0..count as usize {
        let rec = records.add(i * 0x40);
        let payload = *(rec.add(0x28) as *const *const u8);
        if payload.is_null() { continue; } // 全局汇总行
        let field_count = *(rec.add(0x30) as *const u16);
        if field_count == 0 || field_count > 64 { continue; }
        let fields = *(rec.add(0x38) as *const *const u8);
        if fields.is_null() { continue; }

        let mut pid = 0u32;
        let mut down = 0u64;
        let mut up = 0u64;
        for k in 0..field_count as usize {
            let f = fields.add(k * 24);
            let id = *(f as *const i16);
            let val = *(f.add(8) as *const i64) as u64;
            match id {
                3 => up = val,   // 本 build：id3 = 上传
                4 => down = val, // id4 = 下载
                6 => pid = val as u32,
                _ => {}
            }
        }
        if pid != 0 {
            let e = g.cur.entry(pid).or_insert((0, 0));
            e.0 = down;
            e.1 = up;
        }
    }
    g.version += 1;
    if g.version == 1 || g.version % 60 == 0 {
        zx_core::log::info(format!("SRUM 第 {} 帧：{} 个进程记录", g.version, g.cur.len()));
    }
}

impl Srum {
    pub fn new() -> Srum {
        let mut s = Srum {
            _lib: None, reg: std::ptr::null_mut(), unregister: None,
            acc: Arc::new(Mutex::new(Acc::default())),
            prev: HashMap::new(), rates: HashMap::new(), last_active: HashMap::new(),
            last_version: 0, last_time: None, available: false,
        };
        unsafe {
            let Ok(lib) = libloading::Library::new("srumapi.dll") else { return s };
            let register: libloading::Symbol<RegisterFn> = match lib.get(b"SruRegisterRealTimeStats\0") {
                Ok(f) => f, Err(_) => return s,
            };
            let unregister: libloading::Symbol<UnregisterFn> = match lib.get(b"SruUnregisterRealTimeStats\0") {
                Ok(f) => f, Err(_) => return s,
            };
            let st = now_systemtime();
            let ctx = &*s.acc as *const Mutex<Acc> as *mut core::ffi::c_void;
            let mut reg: *mut core::ffi::c_void = std::ptr::null_mut();
            #[allow(unused_assignments)]
            let mut initial: *mut core::ffi::c_void = std::ptr::null_mut();
            // 实测：0x101（实时 + 用户范围）在本机（UAC 关闭）能拿到按进程记录；
            // 0x102（管理员范围）反而恒定 0x1F0。所以按 0x101 → 0x102 的顺序试。
            let mut rc = 0i32;
            for flags in [0x101u32, 0x102u32] {
                reg = std::ptr::null_mut();
                initial = std::ptr::null_mut();
                rc = register(0, &st, flags, ctx, on_records, &mut reg, &mut initial);
                zx_core::log::info(format!("SRUM 注册 flags=0x{flags:X} rc={rc} reg={reg:?}"));
                if rc >= 0 && !reg.is_null() { break; }
            }
            if rc >= 0 && !reg.is_null() {
                s.available = true;
                s.unregister = Some(*unregister);
                REGISTERED.store(true, std::sync::atomic::Ordering::Relaxed);
                zx_core::log::info(format!("SRUM 实时 API 已注册（rc={rc}，管理员={}）", crate::win_admin()));
            } else {
                zx_core::log::warn(format!("SRUM 实时 API 注册失败 rc={rc} reg={reg:?}——按进程网速不可用（需管理员权限）"));
            }
            s._lib = Some(lib);
        }
        s
    }

    pub fn available(&self) -> bool { self.available }

    /// 取本 tick 的 (pid → (up, down) 字节/秒)。没有新帧时沿用上一帧速率。
    pub fn rates(&mut self) -> HashMap<u32, (i64, i64)> {
        if !self.available { return HashMap::new(); }
        let (version, snapshot) = {
            let g = match self.acc.lock() { Ok(g) => g, Err(_) => return self.rates.clone() };
            (g.version, g.cur.clone())
        };
        let now = std::time::Instant::now();
        if version == self.last_version {
            return self.rates.clone();
        }
        let dt = self.last_time.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        self.last_time = Some(now);
        self.last_version = version;
        if dt <= 0.0 { return self.rates.clone(); }

        let mut out = HashMap::new();
        for (pid, (down, up)) in snapshot.iter() {
            let (pd, pu) = self.prev.get(pid).copied().unwrap_or((0, 0));
            let dd = down.saturating_sub(pd) as f64 / dt;
            let du = up.saturating_sub(pu) as f64 / dt;
            if dd > 0.0 || du > 0.0 {
                out.insert(*pid, (du as i64, dd as i64));
                self.last_active.insert(*pid, now);
            }
        }
        self.prev = snapshot;
        self.rates = out;
        self.rates.clone()
    }

    pub fn active_recently(&self, pid: u32) -> bool {
        self.last_active.get(&pid).map(|t| t.elapsed().as_secs_f64() < 10.0).unwrap_or(false)
    }
}

impl Default for Srum {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Srum {
    fn drop(&mut self) {
        if let Some(f) = self.unregister {
            if !self.reg.is_null() { unsafe { f(self.reg); } }
        }
    }
}

fn now_systemtime() -> SYSTEMTIME {
    // 必须是 UTC：SRUM 的 StartTime 按 UTC 解释，用本地时间（UTC+8）会让
    // 注册窗口落在 8 小时之后，回调一帧都不来。
    let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTime() };
    SYSTEMTIME {
        year: t.wYear, month: t.wMonth, day_of_week: t.wDayOfWeek, day: t.wDay,
        hour: t.wHour, minute: t.wMinute, second: t.wSecond, ms: t.wMilliseconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用一条人造的 record set 验证解析偏移（record stride 0x40 / field stride 24 /
    /// id3=↑ id4=↓ id6=PID）——这是整块最脆弱的地方，注册能不能成得靠提权环境验证。
    #[test]
    fn parses_record_set() {
        // 一条记录：3 个字段
        let mut fields = vec![0u8; 3 * 24];
        let put = |buf: &mut Vec<u8>, idx: usize, id: i16, val: i64| {
            let f = idx * 24;
            buf[f..f + 2].copy_from_slice(&id.to_le_bytes());
            buf[f + 8..f + 16].copy_from_slice(&val.to_le_bytes());
        };
        put(&mut fields, 0, 3, 100); // up
        put(&mut fields, 1, 4, 900); // down
        put(&mut fields, 2, 6, 4242); // pid

        let mut rec = [0u8; 0x40];
        rec[0x28..0x30].copy_from_slice(&(1u64).to_le_bytes()); // payload != null
        rec[0x30..0x32].copy_from_slice(&(3u16).to_le_bytes()); // field count
        rec[0x38..0x40].copy_from_slice(&(fields.as_ptr() as u64).to_le_bytes());

        let mut set = vec![0u8; 16];
        set[0..4].copy_from_slice(&(1i32).to_le_bytes());
        set[8..16].copy_from_slice(&(rec.as_ptr() as u64).to_le_bytes());

        let acc = Acc::default();
        let m = std::sync::Mutex::new(acc);
        unsafe { on_records(&m as *const _ as *mut core::ffi::c_void, set.as_mut_ptr() as *mut _) };
        let g = m.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(g.cur.get(&4242), Some(&(900, 100)));
        assert_eq!(g.version, 1);
    }
}
