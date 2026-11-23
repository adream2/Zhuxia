//! 粘贴回填：写剪贴板 → 还焦点 → 发按键 → 善后（计数 / 置顶 / 删除 / 恢复剪贴板）。
//!
//! 对照 `services/clipboard_ops.rs`：粘贴方式 `auto / ctrl_v / shift_insert /
//! ctrl_shift_v` 的选择规则、先松开修饰键、瞬时粘贴后恢复原剪贴板、粘贴后删除，
//! 全部保留；顺序粘贴（`Alt+V` 依次粘贴复制过的 N 条）用 `queue` 实现。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_INSERT, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};

use crate::model::{Entry, Kind};
use crate::store;
use crate::win_clip;

// ---------------------------------------------------------------- 顺序粘贴队列

#[derive(Default)]
struct Queue {
    items: VecDeque<i64>,
    /// 上一次剪贴板写入是队列粘贴：下一次复制要重新排队。
    last_was_paste: bool,
}

fn queue() -> &'static Mutex<Queue> {
    static Q: OnceLock<Mutex<Queue>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(Queue::default()))
}

/// 每次正常复制后入队（仅在开启顺序模式时）。
pub fn enqueue(id: i64) {
    let mut q = queue().lock().unwrap_or_else(|p| p.into_inner());
    if q.last_was_paste {
        q.items.clear();
        q.last_was_paste = false;
    }
    if !q.items.contains(&id) {
        q.items.push_back(id);
    }
}

/// 顺序粘贴按下时：取队首。
pub fn next_in_queue() -> Option<i64> {
    let mut q = queue().lock().unwrap_or_else(|p| p.into_inner());
    let id = q.items.pop_front()?;
    q.last_was_paste = true;
    Some(id)
}

fn mark_pasted() {
    let mut q = queue().lock().unwrap_or_else(|p| p.into_inner());
    q.last_was_paste = true;
}

// ---------------------------------------------------------------- 写剪贴板

pub fn write_entry(entry: &Entry, plain: bool) -> bool {
    match entry.kind {
        Kind::Image => {
            if entry.content.starts_with("data:") {
                return win_clip::write_text(&entry.content);
            }
            // 附件是加密存放的：解密到内存再写剪贴板，不经过临时明文文件。
            match store::Store::read_attachment(&entry.content) {
                Some(bytes) => {
                    let ext = entry
                        .ext
                        .clone()
                        .unwrap_or_else(|| "png".into());
                    let files = if store::Store::attachment_is_protected(&entry.content) {
                        Vec::new()
                    } else {
                        vec![entry.content.clone()]
                    };
                    win_clip::write_image_bytes(&bytes, &ext, &files)
                }
                None => win_clip::write_image_file(&entry.content),
            }
        }
        Kind::File | Kind::Video => {
            let paths = entry.file_paths();
            if paths.is_empty() {
                false
            } else {
                win_clip::write_files(&paths)
            }
        }
        _ => {
            if !plain {
                if let Some(html) = entry.html.as_deref().filter(|h| !h.trim().is_empty()) {
                    return win_clip::write_text_html(&entry.content, html);
                }
            }
            win_clip::write_text(&entry.content)
        }
    }
}

// ---------------------------------------------------------------- 按键

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn chord(mods: &[u16], vk: u16) {
    // 按住修饰键之前，先把可能还按着的 Shift/Ctrl/Alt/Win 松开——
    // 热键触发时用户的手还压在键上，不松开会变成 Ctrl+Alt+V 之类。
    let releases = [VK_LWIN.0, VK_RWIN.0, VK_MENU.0, VK_SHIFT.0, VK_CONTROL.0];
    let up: Vec<INPUT> = releases.iter().map(|&v| key(v, true)).collect();
    unsafe {
        SendInput(&up, std::mem::size_of::<INPUT>() as i32);
    }
    std::thread::sleep(std::time::Duration::from_millis(12));
    let mut seq: Vec<INPUT> = mods.iter().map(|&m| key(m, false)).collect();
    seq.push(key(vk, false));
    seq.push(key(vk, true));
    for &m in mods.iter().rev() {
        seq.push(key(m, true));
    }
    unsafe {
        SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
    }
}

/// `auto`：按目标程序选键。终端用 Ctrl+Shift+V，Chromium / 微信 / QQ / Telegram 用
/// Ctrl+V，其余（Office、记事本、WPS…）用 Shift+Insert——与上游同一张表。
fn resolve_method(method: &str) -> String {
    if method != "auto" {
        return method.to_string();
    }
    unsafe {
        let fg = GetForegroundWindow();
        let mut buf = [0u16; 256];
        let n = GetClassNameW(fg, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]).to_ascii_lowercase();
        let app = crate::source::current().app.to_ascii_lowercase();
        if class.contains("consolewindowclass")
            || class.contains("cascadia_hosting_window_class")
            || app.contains("windowsterminal")
        {
            return "ctrl_shift_v".into();
        }
        if class.contains("chrome_widgetwin")
            || ["qq", "telegram", "wechat", "weixin", "txplatform"]
                .iter()
                .any(|k| app.contains(k))
        {
            return "ctrl_v".into();
        }
    }
    "shift_insert".into()
}

pub fn send_paste(method: &str) {
    match resolve_method(method).as_str() {
        "ctrl_v" => chord(&[VK_CONTROL.0], VK_V.0),
        "ctrl_shift_v" => chord(&[VK_CONTROL.0, VK_SHIFT.0], VK_V.0),
        _ => chord(&[VK_SHIFT.0], VK_INSERT.0),
    }
}

// ---------------------------------------------------------------- 主流程

/// 把某条记录粘贴到当前前台窗口。
///
/// - `plain`：只用纯文本（富文本也退化成 text）
/// - `transient`：粘贴前保存用户原剪贴板，粘贴后放回（热键纯文本粘贴用）
/// - `target`：面板显示前记下的目标窗口；面板隐藏后如果焦点没还回去就手动还
pub fn paste_entry(id: i64, plain: bool, transient: bool, target: Option<HWND>) -> bool {
    let Some(entry) = store::store().lock().unwrap_or_else(|p| p.into_inner()).get(id) else {
        return false;
    };
    let cfg = crate::cfg();

    let snapshot = if transient { Some(win_clip::snapshot()) } else { None };

    // 先发按键再写剪贴板会粘出旧内容：写 → 等系统把数据送达 → 再发键。
    if !write_entry(&entry, plain) {
        return false;
    }
    mark_pasted();

    if let Some(h) = target {
        crate::source::set_foreground(h);
    }
    std::thread::sleep(std::time::Duration::from_millis(45));
    send_paste(&cfg.paste_method);

    if cfg.paste_sound {
        crate::sound::play("paste", cfg.sound_volume);
    }

    {
        let mut s = store::store().lock().unwrap_or_else(|p| p.into_inner());
        s.touch(id);
    }

    if cfg.delete_after_paste {
        // 等目标程序真正取走数据再删，否则粘贴出来是空的。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            store::store()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .delete(id);
            crate::panel::refresh();
        });
    }

    if let Some(snap) = snapshot {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            win_clip::restore(snap);
        });
    }
    true
}

/// 纯文本粘贴热键：不打开面板，直接粘最新（或正在选中）的一条。
pub fn paste_latest(plain: bool) {
    let id = {
        let s = store::store().lock().unwrap_or_else(|p| p.into_inner());
        s.list(&store::Query::default()).first().map(|e| e.id)
    };
    if let Some(id) = id {
        if crate::panel::is_visible() {
            crate::panel::hide_for_paste();
            let target = crate::panel::target_window();
            std::thread::sleep(std::time::Duration::from_millis(60));
            paste_entry(id, plain, false, target);
        } else {
            let fg = unsafe { GetForegroundWindow() };
            paste_entry(id, plain, true, Some(fg));
        }
    }
}

/// 顺序粘贴：队首那条。
pub fn paste_next() {
    let Some(id) = next_in_queue() else {
        crate::panel::toast("顺序队列是空的");
        return;
    };
    if crate::panel::is_visible() {
        crate::panel::hide_for_paste();
        let target = crate::panel::target_window();
        std::thread::sleep(std::time::Duration::from_millis(60));
        paste_entry(id, false, false, target);
    } else {
        let fg = unsafe { GetForegroundWindow() };
        paste_entry(id, false, true, Some(fg));
    }
}
