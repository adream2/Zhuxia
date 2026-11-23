//! 按进程网络流量：ETW `Microsoft-Windows-Kernel-Network` 实时会话。
//!
//! 为什么不用 SRUM：本机（Win11 26200）上 `SruRegisterRealTimeStats` 即使提权也
//! 恒定返回 `0x1F0` 且拿不到句柄，实测各种 provider class / flags 组合都一样。
//! ETW 这条路是任务管理器之外唯一稳定可用的内核级来源，事件里自带 ProcessId。
//!
//! 事件语义由 TDH 解出的 Task/Opcode 实测标定（examples/etw_probe.rs）：
//!   10 = TCPIP "Data sent"        → 上传
//!   11 = TCPIP "Data received"    → 下载
//!   42 = UDPIP "Data sent over UDP protocol"
//!   43 = UDPIP "Data received over UDP protocol"
//!   18 = "Protocol copied data on behalf of user"（内核代拷，会重复计数）→ 忽略
//!   12/13 = 连接建立/断开，载荷不是字节数 → 忽略
//! 四个目标事件的 UserData 前 4 字节就是本次传输的字节数。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use windows::core::GUID;
use windows::Win32::System::Diagnostics::Etw::*;

pub const PROVIDER_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
const SESSION_NAME: &str = "Zhuxia-Network";

type Acc = Arc<Mutex<HashMap<u32, (u64, u64)>>>; // pid -> (up, down) 累计字节

unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
    if rec.is_null() { return; }
    let r = &*rec;
    let id = r.EventHeader.EventDescriptor.Id;
    let (up, down) = match id {
        10 => (true, false),
        11 => (false, true),
        42 => (true, false),
        43 => (false, true),
        _ => return,
    };
    if r.UserData.is_null() || r.UserDataLength < 4 { return; }
    let size = *(r.UserData as *const u32) as u64;
    let pid = r.EventHeader.ProcessId;
    if pid == 0 { return; }
    if r.EventHeader.Flags & 0x2 != 0 { return; } // EVENT_HEADER_FLAG_32_BIT... 保留位，不影响
    if let Some(acc) = (r.UserContext as *const Mutex<HashMap<u32, (u64, u64)>>).as_ref() {
        if let Ok(mut g) = acc.lock() {
            let e = g.entry(pid).or_insert((0, 0));
            if up { e.0 += size; } else if down { e.1 += size; }
            let _ = up;
            let _ = down;
        }
    }
}

pub struct EtwNet {
    session: CONTROLTRACE_HANDLE,
    running: bool,
    acc: Acc,
    prev: HashMap<u32, (u64, u64)>,
    rates: HashMap<u32, (i64, i64)>,
    last_rates: HashMap<u32, (i64, i64)>,
    last_active: HashMap<u32, std::time::Instant>,
    last: Option<std::time::Instant>,
    trace: Option<PROCESSTRACE_HANDLE>,
}

impl EtwNet {
    pub fn start() -> EtwNet {
        let acc: Acc = Arc::new(Mutex::new(HashMap::new()));
        let mut me = EtwNet {
            session: CONTROLTRACE_HANDLE { Value: 0 },
            running: false,
            acc: acc.clone(),
            prev: HashMap::new(),
            rates: HashMap::new(),
            last_rates: HashMap::new(),
            last_active: HashMap::new(),
            last: None,
            trace: None,
        };
        if !zx_core::win::is_admin() {
            zx_core::log::info("ETW 按进程网速需要管理员权限，跳过");
            return me;
        }
        unsafe {
            let name_w = zx_core::win::wide(SESSION_NAME);
            // 清掉上次崩溃留下的同名会话
            let mut p = props(SESSION_NAME);
            let _ = ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, windows::core::PCWSTR(name_w.as_ptr()),
                p.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);

            let mut p = props(SESSION_NAME);
            let mut session = CONTROLTRACE_HANDLE { Value: 0 };
            let rc = StartTraceW(&mut session, windows::core::PCWSTR(name_w.as_ptr()),
                                 p.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES);
            if rc.0 != 0 {
                zx_core::log::warn(format!("ETW StartTrace 失败 rc={}——按进程网速不可用", rc.0));
                return me;
            }
            let rc = EnableTraceEx2(session, &PROVIDER_NETWORK,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, TRACE_LEVEL_INFORMATION as u8,
                u64::MAX, 0, 0, None);
            if rc.0 != 0 {
                zx_core::log::warn(format!("ETW EnableTraceEx2 失败 rc={}", rc.0));
                let _ = ControlTraceW(session, windows::core::PCWSTR::null(),
                    props(SESSION_NAME).as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);
                return me;
            }

            let ctx = Arc::into_raw(acc.clone()) as *mut c_void;
            let name_w2 = zx_core::win::wide(SESSION_NAME);
            // LoggerName 必须指向一个**长期有效**的缓冲区（name_w2 活到函数结束）
            let mut logfile = EVENT_TRACE_LOGFILEW {
                LoggerName: windows::core::PWSTR(name_w2.as_ptr() as *mut u16),
                ..Default::default()
            };
            logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
            logfile.Anonymous2.EventRecordCallback = Some(on_event);
            logfile.Context = ctx;

            let trace = OpenTraceW(&mut logfile);
            if trace.Value == 0 || trace.Value == u64::MAX {
                zx_core::log::warn("ETW OpenTrace 失败——按进程网速不可用");
                let _ = ControlTraceW(session, windows::core::PCWSTR::null(),
                    props(SESSION_NAME).as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);
                let _ = Arc::from_raw(ctx as *const Mutex<HashMap<u32, (u64, u64)>>);
                return me;
            }
            me.session = session;
            me.trace = Some(trace);
            me.running = true;

            // ProcessTrace 会阻塞，放独立线程；会话停止后它会返回。
            std::thread::Builder::new().name("zx-etw".into()).spawn(move || {
                let h = [trace];
                let _ = ProcessTrace(&h, None, None);
                zx_core::log::info("ETW 会话已结束");
            }).ok();
            zx_core::log::info("ETW 按进程网速已启动（Microsoft-Windows-Kernel-Network）");
        }
        me
    }

    pub fn available(&self) -> bool { self.running }

    /// 本 tick 的 (pid → (up, down) 字节/秒)。
    pub fn rates(&mut self) -> HashMap<u32, (i64, i64)> {
        if !self.running { return HashMap::new(); }
        let now = std::time::Instant::now();
        let dt = self.last.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        self.last = Some(now);
        let cur = match self.acc.lock() {
            Ok(g) => g.clone(),
            Err(_) => return self.rates.clone(),
        };
        if dt <= 0.0 { return self.rates.clone(); }
        let mut out = HashMap::new();
        for (pid, (up, down)) in cur.iter() {
            let (pu, pd) = self.prev.get(pid).copied().unwrap_or((0, 0));
            let du = up.saturating_sub(pu) as f64 / dt;
            let dd = down.saturating_sub(pd) as f64 / dt;
            if du > 0.0 || dd > 0.0 {
                out.insert(*pid, (du as i64, dd as i64));
            }
        }
        self.prev = cur;

        // 刚活跃过的进程在 10 秒内保持在榜（0 B/s），与任务管理器一致
        for (pid, v) in out.iter() {
            self.last_active.insert(*pid, now);
            self.last_rates.insert(*pid, *v);
        }
        for (pid, t) in self.last_active.iter() {
            if now.duration_since(*t).as_secs_f64() < 10.0 && !out.contains_key(pid) {
                out.insert(*pid, (0, 0));
            }
        }
        let dead: Vec<u32> = self.last_active.iter()
            .filter(|(_, t)| now.duration_since(**t).as_secs_f64() >= 10.0)
            .map(|(p, _)| *p).collect();
        for p in dead { self.last_active.remove(&p); self.last_rates.remove(&p); }

        self.rates = out;
        self.rates.clone()
    }
}

impl Drop for EtwNet {
    fn drop(&mut self) {
        if self.session.Value != 0 {
            unsafe {
                let _ = ControlTraceW(self.session, windows::core::PCWSTR::null(),
                    props(SESSION_NAME).as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);
            }
        }
        // 回调上下文由 Arc::into_raw 泄漏给 ETW；进程退出时由系统回收，
        // 这里不再 from_raw（会话可能仍在投递最后一帧）。
    }
}

fn props(name: &str) -> Vec<u8> {
    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let base = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    let mut buf = vec![0u8; base + name_w.len() * 2 + 8];
    unsafe {
        let p = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        (*p).Wnode.BufferSize = buf.len() as u32;
        (*p).Wnode.Flags = 0x0002_0000; // WNODE_FLAG_TRACED_GUID
        (*p).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        (*p).LoggerNameOffset = base as u32;
        (*p).FlushTimer = 1;
        let dst = buf.as_mut_ptr().add(base) as *mut u16;
        std::ptr::copy_nonoverlapping(name_w.as_ptr(), dst, name_w.len());
    }
    buf
}
