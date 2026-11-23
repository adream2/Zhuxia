//! 图床热键线程：`Ctrl+Shift+U` 上传剪贴板图片。
//!
//! 没有窗口，只有 `RegisterHotKey` + 消息循环。热键绑定线程，所以注册与处理必须在
//! 同一条线程上；工作线程（上传）是另一条，热键回调只负责把任务丢进队列。
//!
//! 实际生效的热键写到 `<exe>/imgbed-hotkey.txt`：设置面板是独立进程，读文件比
//! 跨进程共享内存省事（和截图/手势一个套路）。

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, MSG, TranslateMessage, WM_HOTKEY, WM_TIMER,
};

const ID_UPLOAD: i32 = 1;
const WM_RELOAD: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 63;
const WM_UPLOAD_CLIPBOARD: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 64;
const FALLBACKS: [&str; 2] = ["Ctrl+Alt+U", "Alt+Shift+U"];

static TID: AtomicU32 = AtomicU32::new(0);

pub fn start() {
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    std::thread::Builder::new()
        .name("zx-imgbed-hotkey".into())
        .spawn(move || unsafe {
            let tid = windows::Win32::System::Threading::GetCurrentThreadId();
            TID.store(tid, Ordering::SeqCst);
            register_all();
            let _ = tx.send(tid);
            let mut msg = MSG::default();
            loop {
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == -1 || r.0 == 0 {
                    break;
                }
                match msg.message {
                    WM_HOTKEY if msg.wParam.0 as i32 == ID_UPLOAD => {
                        crate::queue::submit(crate::queue::Job::Clipboard);
                    }
                    WM_UPLOAD_CLIPBOARD => {
                        crate::queue::submit(crate::queue::Job::Clipboard);
                    }
                    WM_RELOAD => register_all(),
                    WM_TIMER => {}
                    _ => {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        })
        .ok();
    if rx.recv_timeout(std::time::Duration::from_secs(3)).is_err() {
        zx_core::log::warn("图床热键线程 3 秒内没起来");
    }
}

/// 从任意线程触发"上传剪贴板图片"（托盘菜单）。
pub fn trigger() {
    let tid = TID.load(Ordering::SeqCst);
    if tid == 0 {
        zx_core::log::warn("图床热键线程还没起来");
        return;
    }
    unsafe {
        let _ = PostThreadMessageW(tid, WM_UPLOAD_CLIPBOARD, WPARAM(0), LPARAM(0));
    }
}

pub fn reload() {
    let tid = TID.load(Ordering::SeqCst);
    if tid == 0 {
        return;
    }
    unsafe {
        let _ = PostThreadMessageW(tid, WM_RELOAD, WPARAM(0), LPARAM(0));
    }
}

pub fn publish(active: &str, wanted: &str, error: &str) {
    let path = zx_core::cfg::run_dir().join("imgbed-hotkey.txt");
    let _ = std::fs::write(path, format!("active={active}\nwanted={wanted}\nerror={error}\n"));
}

fn register_all() {
    unsafe {
        let _ = UnregisterHotKey(None, ID_UPLOAD);
        let cfg = crate::cfg();
        if !cfg.enabled {
            publish("", "", "模块已禁用");
            return;
        }
        let wanted = cfg.hotkey.trim().to_string();
        if wanted.is_empty() {
            publish("", "", "");
            zx_core::log::info("图床热键未配置（可在设置里填，例如 Ctrl+Shift+U）");
            return;
        }
        let mut tried: Vec<String> = Vec::new();
        let mut last_err = String::new();
        for cand in std::iter::once(wanted.clone()).chain(FALLBACKS.iter().map(|s| s.to_string())) {
            let cand = cand.trim().to_string();
            if cand.is_empty() || tried.contains(&cand) {
                continue;
            }
            tried.push(cand.clone());
            let Some((mods, vk)) = zx_core::hotkey::parse(&cand) else {
                last_err = format!("{cand} 格式不合法");
                continue;
            };
            match RegisterHotKey(None, ID_UPLOAD, mods | MOD_NOREPEAT, vk as u32) {
                Ok(()) => {
                    if cand == wanted {
                        zx_core::log::info(format!("图床热键已注册：{cand}"));
                        publish(&cand, &wanted, "");
                    } else {
                        let why = format!("{wanted} 已被别的程序占用");
                        zx_core::log::warn(format!("{why}，已临时改用 {cand}"));
                        publish(&cand, &wanted, &why);
                    }
                    return;
                }
                Err(e) => last_err = format!("{cand} 注册失败（{e}）"),
            }
        }
        zx_core::log::warn(format!("图床热键全部注册失败（{last_err}）"));
        publish("", &wanted, &last_err);
    }
}

/// 给设置页读状态用。
pub fn state() -> (String, String, String) {
    let path = zx_core::cfg::run_dir().join("imgbed-hotkey.txt");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (String::new(), String::new(), String::new());
    };
    let mut active = String::new();
    let mut wanted = String::new();
    let mut error = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("active=") {
            active = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("wanted=") {
            wanted = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("error=") {
            error = v.trim().to_string();
        }
    }
    (active, wanted, error)
}
