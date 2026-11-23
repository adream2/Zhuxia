//! 全局快捷键。
//!
//! 对照参考实现的 `register_hotkey` / `globalShortcut`：
//! - 主界面（默认 `Alt+C`）
//! - 纯文本粘贴（默认未绑定，`Alt+Shift+V` 是上游的 Mac 默认值，Windows 上留空）
//! - 顺序粘贴（默认 `Alt+V`）
//! - 面板内搜索（默认 `Alt+F`，打开面板并直接聚焦搜索框）
//! - 单条记录的快捷键（右键「设置这条的快捷键」）
//!
//! 所有注册都在同一个线程里完成（`RegisterHotKey` 绑定线程），配置改动后
//! 用 `PostThreadMessage` 让该线程整体重注册。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_HOTKEY,
};

/// 解析规则只有一份，放在 `zx-core`（截图模块用同一套）。
pub use zx_core::hotkey::parse;

const WM_RELOAD_HOTKEYS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 41;
const ITEM_BASE: i32 = 100;
const ITEM_MAX: i32 = 220;

static HOTKEY_TID: AtomicU32 = AtomicU32::new(0);
static ITEM_MAP: Mutex<Vec<(i32, i64)>> = Mutex::new(Vec::new());

pub fn start() {
    std::thread::Builder::new()
        .name("zx-clip-hotkey".into())
        .spawn(|| {
            unsafe {
                let tid = windows::Win32::System::Threading::GetCurrentThreadId();
                HOTKEY_TID.store(tid, Ordering::SeqCst);
                register_all();
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if msg.message == WM_HOTKEY {
                        dispatch(msg.wParam.0 as i32);
                        continue;
                    }
                    if msg.message == WM_RELOAD_HOTKEYS {
                        register_all();
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .ok();
}

/// 配置或条目快捷键变了：让热键线程重注册。
pub fn reload() {
    let tid = HOTKEY_TID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_RELOAD_HOTKEYS, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
        }
    }
}

pub fn reload_item_hotkeys() {
    reload();
}

fn register_all() {
    unsafe {
        for id in 1..=4 {
            let _ = UnregisterHotKey(None, id);
        }
        for id in ITEM_BASE..ITEM_BASE + ITEM_MAX {
            let _ = UnregisterHotKey(None, id);
        }
        let cfg = crate::cfg();
        bind(1, &cfg.hotkey);
        bind(2, &cfg.plain_paste_hotkey);
        bind(3, &cfg.sequential_hotkey);
        bind(4, &cfg.search_hotkey);

        let mut map = Vec::new();
        let entries = crate::store::store()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hotkey_entries();
        for (i, (id, text)) in entries.into_iter().enumerate().take(ITEM_MAX as usize) {
            if bind(ITEM_BASE + i as i32, &text) {
                map.push((ITEM_BASE + i as i32, id));
            }
        }
        *ITEM_MAP.lock().unwrap_or_else(|p| p.into_inner()) = map;
    }
}

fn bind(id: i32, text: &str) -> bool {
    let Some((mods, vk)) = parse(text) else { return false };
    unsafe {
        match RegisterHotKey(None, id, mods | MOD_NOREPEAT, vk as u32) {
            Ok(()) => true,
            Err(e) => {
                zx_core::log::warn(format!("热键 {text} 注册失败（可能被占用）：{e}"));
                false
            }
        }
    }
}

fn dispatch(id: i32) {
    match id {
        1 => crate::panel::toggle(),
        2 => crate::paste::paste_latest(true),
        3 => crate::paste::paste_next(),
        4 => crate::panel::show_search(),
        _ => {
            let item = ITEM_MAP
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .find(|(h, _)| *h == id)
                .map(|(_, e)| *e);
            if let Some(entry_id) = item {
                let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
                crate::paste::paste_entry(entry_id, false, true, Some(fg));
            }
        }
    }
}
