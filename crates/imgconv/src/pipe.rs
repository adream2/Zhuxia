//! 常驻进程的「收件口」：Explorer 每选一个文件就起一支 `zhuxia.exe --img-convert`，
//! 这些短命进程把活交给常驻进程排队，自己立刻退出——批量 20 张就只花 20 次
//! 进程启动 + 一次解码/编码，而不是 20 个 GDI+ 实例各自冷启动。
//!
//! 协议故意用控制字符分隔（Windows 文件名不允许 < 0x20，不会歧义）：
//! `格式 \x1f 路径 \x1e` 重复。不引 serde，不为一条管道加依赖。

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};

use crate::fmt::Target;

const PIPE_NAME: &str = r"\\.\pipe\zhuxia-imgconv";
const SEP_FIELD: char = '\x1f';
const SEP_REC: char = '\x1e';
/// 一次最多收 400 条（够覆盖一次多选），防止客户端塞爆缓冲。
const MAX_BYTES: usize = 1 << 20;

/// 客户端：把一批任务交给常驻进程。成功 = 对方收下了。
pub fn send(jobs: &[(Target, PathBuf)]) -> bool {
    if jobs.is_empty() {
        return false;
    }
    let mut buf = String::new();
    for (t, p) in jobs {
        buf.push_str(t.id());
        buf.push(SEP_FIELD);
        buf.push_str(&p.to_string_lossy());
        buf.push(SEP_REC);
    }
    let bytes = buf.into_bytes();

    let name = zx_core::win::wide(PIPE_NAME);
    // 常驻进程可能刚起来还没建好管道：重试几次，仍失败就让调用方走本进程兜底。
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        unsafe {
            let h = CreateFileW(
                PCWSTR(name.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            );
            let Ok(h) = h else { continue };
            let mut written = 0u32;
            let ok = WriteFile(h, Some(&bytes), Some(&mut written), None).is_ok()
                && written as usize == bytes.len();
            let _ = CloseHandle(h);
            if ok {
                return true;
            }
        }
    }
    false
}

/// 服务端：常驻进程里跑一条线程，收到任务就交给 `on_job`。
///
/// 管道**只建一次**：断开后复用同一个句柄，客户端在两次连接之间也能连上（排队）。
/// 每次重建会留一个几十毫秒的"没有管道"的窗口，多选批量时正好会撞上。
pub fn serve(on_job: impl Fn(Target, PathBuf) + Send + 'static) {
    std::thread::Builder::new()
        .name("zx-imgconv-pipe".into())
        .spawn(move || {
            let name = zx_core::win::wide(PIPE_NAME);
            let create = || unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    8,
                    0,
                    8192,
                    0,
                    None,
                )
            };

            let mut pipe: HANDLE = create();
            loop {
                if pipe.is_invalid() {
                    // 名字被占/资源不足：歇一会儿再建，别把线程打死。
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    pipe = create();
                    continue;
                }
                unsafe {
                    // 阻塞等连上来；连上后一次读完整个消息
                    let _ = ConnectNamedPipe(pipe, None);
                }
                let mut data = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let mut read = 0u32;
                    let ok = unsafe { ReadFile(pipe, Some(&mut chunk), Some(&mut read), None) }.is_ok();
                    if !ok || read == 0 {
                        break;
                    }
                    data.extend_from_slice(&chunk[..read as usize]);
                    if data.len() > MAX_BYTES {
                        break;
                    }
                }
                let disconnected = unsafe { DisconnectNamedPipe(pipe) }.is_ok();
                if !disconnected {
                    // 句柄状态坏了：关掉重来。
                    unsafe { let _ = CloseHandle(pipe); }
                    pipe = create();
                }
                if !data.is_empty() {
                    for (t, p) in parse(&data) {
                        on_job(t, p);
                    }
                }
            }
        })
        .ok();
}

/// 解析协议。坏记录直接跳过（宁少转一张，也不要 panic 掉常驻进程）。
fn parse(data: &[u8]) -> Vec<(Target, PathBuf)> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for rec in text.split(SEP_REC) {
        let rec = rec.trim_matches(|c: char| c == '\0' || c == '\r' || c == '\n');
        if rec.is_empty() {
            continue;
        }
        let mut it = rec.splitn(2, SEP_FIELD);
        let (Some(fmt), Some(path)) = (it.next(), it.next()) else { continue };
        let (Some(t), false) = (Target::parse(fmt), path.trim().is_empty()) else { continue };
        out.push((t, PathBuf::from(path.trim().to_string())));
    }
    out
}


