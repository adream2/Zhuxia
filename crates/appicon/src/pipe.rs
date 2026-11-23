//! 常驻进程的收件管道：Explorer 多选时每个文件起一支短命进程，把活投进来就退出。
//!
//! 协议沿用 imgconv 的控制字符分隔（Windows 文件名不允许 < 0x20）：
//! `模式 \x1f 路径 \x1e` 重复。

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

use crate::output::Mode;

const PIPE_NAME: &str = r"\\.\pipe\zhuxia-appicon";
const SEP_FIELD: char = '\x1f';
const SEP_REC: char = '\x1e';
const MAX_BYTES: usize = 1 << 20;

pub fn send(jobs: &[(Mode, PathBuf)]) -> bool {
    if jobs.is_empty() {
        return false;
    }
    let mut buf = String::new();
    for (m, p) in jobs {
        buf.push_str(m.id());
        buf.push(SEP_FIELD);
        buf.push_str(&p.to_string_lossy());
        buf.push(SEP_REC);
    }
    let bytes = buf.into_bytes();
    let name = zx_core::win::wide(PIPE_NAME);
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

pub fn serve(on_job: impl Fn(Mode, PathBuf) + Send + 'static) {
    std::thread::Builder::new()
        .name("zx-appicon-pipe".into())
        .spawn(move || {
            let name = zx_core::win::wide(PIPE_NAME);
            loop {
                unsafe {
                    let h: HANDLE = CreateNamedPipeW(
                        PCWSTR(name.as_ptr()),
                        PIPE_ACCESS_INBOUND,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        1,
                        MAX_BYTES as u32,
                        MAX_BYTES as u32,
                        0,
                        None,
                    );
                    if h.is_invalid() {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    let _ = ConnectNamedPipe(h, None);
                    let mut buf = vec![0u8; MAX_BYTES];
                    let mut read = 0u32;
                    if ReadFile(h, Some(&mut buf), Some(&mut read), None).is_ok() && read > 0 {
                        buf.truncate(read as usize);
                        let text = String::from_utf8_lossy(&buf);
                        for rec in text.split(SEP_REC) {
                            let mut it = rec.split(SEP_FIELD);
                            let (Some(mode), Some(path)) = (it.next(), it.next()) else { continue };
                            let Some(mode) = Mode::parse(mode) else { continue };
                            if !path.is_empty() {
                                on_job(mode, PathBuf::from(path));
                            }
                        }
                    }
                    let _ = DisconnectNamedPipe(h);
                    let _ = CloseHandle(h);
                }
            }
        })
        .ok();
}
