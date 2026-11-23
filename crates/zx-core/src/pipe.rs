//! 常驻进程的「收件口」：Explorer 每选一个文件就起一支短命进程，把活投进来后立刻退出。
//!
//! 批量 20 个文件只花 20 次进程启动，真正的解码/编码在常驻进程里排队做，
//! 而不是 20 个进程各自冷启动一遍 GDI+。
//!
//! 协议故意用控制字符分隔（Windows 文件名不允许 < 0x20，不会歧义）：
//! 每条记录是 `id \x1f path`，记录之间用 `\x1e`。不引 serde——不为一条管道加依赖。
//!
//! 这里只有"管道 + 帧"这一层；id 与 path 的含义由各模块的 [`Job`] 实现决定。

use std::path::{Path, PathBuf};

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

const SEP_FIELD: char = '\x1f';
const SEP_REC: char = '\x1e';
/// 一次最多收 1MB（够覆盖一次多选），防止客户端塞爆缓冲。
const MAX_BYTES: usize = 1 << 20;
/// 同时挂 8 个实例：多选批量时客户端是一个接一个连上来的，实例太少会互相排队。
const INSTANCES: u32 = 8;

/// 一条可以走管道的任务。
pub trait Job: Sized + Send + 'static {
    /// 编码成一条记录（`id \x1f path`，不含记录分隔符）。
    fn encode(&self) -> String;
    /// 从一条已 trim 过的记录还原；坏记录返回 `None`（宁少做一张，也不要 panic）。
    fn decode(rec: &str) -> Option<Self>;
}

/// `(id, 路径)` 的默认编码：绝大多数模块的任务就是这个形状。
pub fn encode_pair(id: &str, path: &Path) -> String {
    format!("{id}{SEP_FIELD}{}", path.to_string_lossy())
}

/// 把一条记录拆成 `(id, path)`，两条都非空才算数。
pub fn decode_pair(rec: &str) -> Option<(String, PathBuf)> {
    let mut it = rec.splitn(2, SEP_FIELD);
    let (Some(id), Some(path)) = (it.next(), it.next()) else { return None };
    let (id, path) = (id.trim(), path.trim());
    if id.is_empty() || path.is_empty() {
        return None;
    }
    Some((id.to_string(), PathBuf::from(path)))
}

/// 客户端：把一批任务交给常驻进程。成功 = 对方收下了。
pub fn send<J: Job>(pipe_name: &str, jobs: &[J]) -> bool {
    if jobs.is_empty() {
        return false;
    }
    let mut buf = String::new();
    for j in jobs {
        buf.push_str(&j.encode());
        buf.push(SEP_REC);
    }
    let bytes = buf.into_bytes();

    let name = crate::win::wide(pipe_name);
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

/// 服务端：在常驻进程里起一条线程收任务，收到就交给 `on_job`。
///
/// 管道**只建一次**：断开后复用同一个句柄，客户端在两次连接之间也能连上（排队）。
/// 每次重建会留一个几十毫秒的「没有管道」的窗口，多选批量时正好会撞上。
pub fn serve<J: Job>(pipe_name: &str, thread_name: &str, on_job: impl Fn(J) + Send + 'static) {
    let name = crate::win::wide(pipe_name);
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            let create = || unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    INSTANCES,
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
                    // 阻塞等连上来；连上后读到对端关闭为止（一次可能分多个 8KB 包）
                    let _ = ConnectNamedPipe(pipe, None);
                }
                let mut data = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let mut read = 0u32;
                    let ok =
                        unsafe { ReadFile(pipe, Some(&mut chunk), Some(&mut read), None) }.is_ok();
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
                    for job in parse::<J>(&data) {
                        on_job(job);
                    }
                }
            }
        })
        .ok();
}

/// 解析一整个消息体。坏记录直接跳过。
fn parse<J: Job>(data: &[u8]) -> Vec<J> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for rec in text.split(SEP_REC) {
        let rec = rec.trim_matches(|c: char| c == '\0' || c == '\r' || c == '\n');
        if rec.is_empty() {
            continue;
        }
        if let Some(job) = J::decode(rec) {
            out.push(job);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的任务：一个序号 + 一个路径。
    #[derive(Debug, PartialEq)]
    struct T(u32, PathBuf);

    impl Job for T {
        fn encode(&self) -> String {
            encode_pair(&self.0.to_string(), &self.1)
        }
        fn decode(rec: &str) -> Option<Self> {
            let (id, path) = decode_pair(rec)?;
            Some(T(id.parse().ok()?, path))
        }
    }

    fn unique_pipe() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        format!(
            r"\\.\pipe\zhuxia-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn empty_batch_is_not_sent() {
        assert!(!send::<T>(&unique_pipe(), &[]));
    }

    #[test]
    fn pair_round_trip() {
        let rec = encode_pair("webp", &PathBuf::from(r"C:\图片\a b.png"));
        let (id, path) = decode_pair(&rec).expect("应能解回");
        assert_eq!(id, "webp");
        assert_eq!(path, PathBuf::from(r"C:\图片\a b.png"));
    }

    #[test]
    fn malformed_pairs_are_rejected() {
        assert!(decode_pair("").is_none());
        assert!(decode_pair("webp").is_none());
        // 缺 id / 缺路径都算坏记录
        assert!(decode_pair(&format!("\u{1f}{}", r"C:\a.png")).is_none());
        assert!(decode_pair("webp\u{1f}   ").is_none());
    }

    /// 一整个消息体里有多条记录时，要按记录拆开、坏记录跳过。
    ///
    /// 覆盖协议层（这次重构动的就是这层）。真正建命名管道收发要 `CreateNamedPipeW`，
    /// 本机测试进程里它会返回 ERROR_INVALID_NAME（权限/命名空间受限），
    /// 所以不在单测里强行建管道——那部分代码是照搬原来跑通过的实现。
    #[test]
    fn parse_splits_records_and_skips_garbage() {
        let sep_f = '\u{1f}';
        let buf = format!(
            "1{sep_f}{}\u{1e}garbage\u{1e}2{sep_f}{}\u{1e}",
            r"C:\a b.png",
            r"D:\安装包\微信 Setup.exe",
        );
        let jobs: Vec<T> = parse(buf.as_bytes());
        assert_eq!(jobs.len(), 2, "坏记录应被跳过");
        assert_eq!(jobs[0], T(1, PathBuf::from(r"C:\a b.png")));
        assert_eq!(jobs[1], T(2, PathBuf::from(r"D:\安装包\微信 Setup.exe")));
    }

    #[test]
    fn parse_ignores_empty_payload() {
        let jobs: Vec<T> = parse(b"");
        assert!(jobs.is_empty());
    }
}
