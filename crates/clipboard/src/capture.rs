//! 剪贴板监听与采集流水线。
//!
//! 结构对照 `services/clipboard_listener.rs` + `services/clipboard/{mod,pipeline}.rs`：
//!
//! ```text
//! WM_CLIPBOARDUPDATE ──▶ 监听窗口（只置位） ──▶ 采集工作线程（合并抖动）
//!                                                    │
//!          读 CF_HDROP / HTML / 图片原始格式 / 文本 ◀─┘
//!                                                    ▼
//!   Discovery → Transformation → Dedup → Persistence → Notify
//! ```
//!
//! 采集放在工作线程：某些程序（Office/WPS/截图工具）会在通知后几十毫秒才把
//! 富文本/文件格式写好，监听线程里 sleep 会把消息循环堵死。

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::model::{now_ms, Entry, Kind};
use crate::rules;
use crate::store;
use crate::win_clip;

static LAST_HASH: AtomicU64 = AtomicU64::new(0);
static LAST_AT: AtomicI64 = AtomicI64::new(0);
static LAST_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn start() {
    // 监听窗口：一个只处理 WM_CLIPBOARDUPDATE 的消息窗。
    std::thread::Builder::new()
        .name("zx-clip-listen".into())
        .spawn(listen_thread)
        .ok();
}

fn listen_thread() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW;

    let pending = Arc::new((Mutex::new(false), Condvar::new()));
    {
        let pending = pending.clone();
        std::thread::Builder::new()
            .name("zx-clip-capture".into())
            .spawn(move || loop {
                {
                    let (lock, cv) = &*pending;
                    let mut changed = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while !*changed {
                        changed = cv.wait(changed).unwrap_or_else(|p| p.into_inner());
                    }
                    *changed = false;
                }
                if let Err(e) = std::panic::catch_unwind(capture_once) {
                    zx_core::log::warn(format!("采集线程 panic（已忽略）：{e:?}"));
                }
            })
            .ok();
    }

    unsafe {
        let Ok(inst) = GetModuleHandleW(None) else { return };
        let class_name: Vec<u16> = "ZhuxiaClipboardListener\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(inst.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            HINSTANCE(inst.0),
            None,
        ) else {
            zx_core::log::warn("剪贴板监听窗口创建失败");
            return;
        };
        // 把通知函数指针塞进窗口的 user data：一个 Arc 的裸指针。
        let notify: Box<Arc<dyn Fn() + Send + Sync>> = Box::new(Arc::new({
            let pending = pending.clone();
            move || {
                let (lock, cv) = &*pending;
                *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
                cv.notify_one();
            }
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(notify) as isize);
        if AddClipboardFormatListener(hwnd).is_err() {
            zx_core::log::warn("注册剪贴板监听失败");
            free_notify(hwnd);
            let _ = DestroyWindow(hwnd);
            return;
        }
        zx_core::log::info("剪贴板监听已启动（WM_CLIPBOARDUPDATE）");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RemoveClipboardFormatListener(hwnd);
        free_notify(hwnd);
        let _ = DestroyWindow(hwnd);
    }
}

/// 释放挂在窗口 user data 上的通知闭包。
unsafe fn free_notify(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWLP_USERDATA,
    };
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if p != 0 {
        drop(Box::from_raw(p as *mut Arc<dyn Fn() + Send + Sync>));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, GetWindowLongPtrW, WM_CLIPBOARDUPDATE};
    if msg == WM_CLIPBOARDUPDATE {
        let p = GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA);
        if p != 0 {
            let f = &*(p as *const Arc<dyn Fn() + Send + Sync>);
            f();
        }
        return windows::Win32::Foundation::LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 一次采集。按参考实现的优先级：文件 → 富文本 → 原生图片格式 → 位图 → 文本。
fn capture_once() {
    if win_clip::paused() {
        return;
    }
    let cfg = crate::cfg();
    if !cfg.enabled || cfg.pause {
        return;
    }

    let seq = win_clip::sequence_number() as u64;
    let last = LAST_SEQ.load(Ordering::Relaxed);
    if seq != 0 && seq == last {
        return;
    }
    LAST_SEQ.store(seq, Ordering::Relaxed);

    if win_clip::is_marked_private() {
        return;
    }

    let source = crate::source::current();

    // ---- 1. 文件（Explorer 的复制是 CF_HDROP，必须先于文本/缩略图）----
    if let Some(files) = win_clip::read_files() {
        if files.len() == 1 && rules::is_image_file(&files[0]) && !cfg.capture_files {
            if let Some((bytes, ext)) = read_image_file(&files[0]) {
                let hash = rules::bytes_hash(&bytes);
                // 文件复制常连发两次通知：和文本/位图一样做抖动合并。
                if win_clip::is_own_write(Some(hash)) || burst(hash) {
                    return;
                }
                commit(Entry {
                    kind: Kind::Image,
                    content: String::new(),
                    ext: Some(ext.to_string()),
                    hash,
                    ..blank(&source)
                }, Some(bytes));
            }
            return;
        }
        if !cfg.capture_files {
            return;
        }
        let kind = if files.len() == 1 {
            rules::classify_path(&files[0])
        } else {
            Kind::File
        };
        let content = files.join("\n");
        let hash = rules::text_hash(&content);
        if win_clip::is_own_write(Some(hash)) || burst(hash) {
            return;
        }
        commit(
            Entry {
                kind,
                content: content.clone(),
                preview: rules::build_preview(kind, &content, None),
                hash,
                ..blank(&source)
            },
            None,
        );
        return;
    }

    // ---- 2. 原始编码图片（GIF 保留动画）----
    if cfg.record_images {
        if let Some((bytes, ext, hash)) = win_clip::read_native_image() {
            if win_clip::is_own_write(Some(hash)) || burst(hash) {
                return;
            }
            commit(
                Entry {
                    kind: Kind::Image,
                    ext: Some(ext.to_string()),
                    hash,
                    ..blank(&source)
                },
                Some(bytes),
            );
            return;
        }
    }

    // ---- 3. 富文本 / 文本 ----
    let text = win_clip::read_text();
    let html = if cfg.capture_rich_text {
        win_clip::read_html()
            .filter(|h| !h.trim().is_empty())
            .filter(|h| !rules::html_is_image_only(h))
    } else {
        None
    };

    let (kind, content, html_out) = if let Some(html) = html.clone() {
        let plain = text
            .as_deref()
            .map(rules::normalize_line_endings)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| rules::derive_rich_text("", Some(&html)));
        (Kind::RichText, plain, Some(html))
    } else if let Some(text) = text.clone().filter(|t| !t.is_empty()) {
        (rules::detect_content_type(&text), text, None)
    } else {
        (Kind::Text, String::new(), None)
    };

    if !content.trim().is_empty() {
        let normalized = rules::normalize_line_endings(&content);
        let hash = rules::text_hash(&normalized);
        if win_clip::is_own_write(Some(hash)) || burst(hash) {
            return;
        }
        commit(
            Entry {
                kind,
                content: normalized.clone(),
                html: html_out,
                preview: rules::build_preview(kind, &normalized, None),
                hash,
                ..blank(&source)
            },
            None,
        );
        return;
    }

    // ---- 4. 位图 ----
    if cfg.record_images {
        if let Some((png, hash)) = win_clip::read_dib_png() {
            if win_clip::is_own_write(Some(hash)) || burst(hash) {
                return;
            }
            commit(
                Entry {
                    kind: Kind::Image,
                    ext: Some("png".into()),
                    hash,
                    ..blank(&source)
                },
                Some(png),
            );
        }
    }
}

fn blank(source: &crate::source::SourceApp) -> Entry {
    let now = now_ms();
    Entry {
        source_app: if source.app.is_empty() {
            "未知".into()
        } else {
            source.app.clone()
        },
        source_path: source.path.clone(),
        created_at: now,
        sort_at: now,
        ..Default::default()
    }
}

fn burst(hash: u64) -> bool {
    let last = LAST_HASH.load(Ordering::Relaxed);
    let at = LAST_AT.load(Ordering::Relaxed);
    store::is_burst(last, hash, at, now_ms())
}

fn read_image_file(path: &str) -> Option<(Vec<u8>, &'static str)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let ext = match bytes.first()? {
        0x89 if bytes.starts_with(&[0x89, b'P', b'N', b'G']) => "png",
        0x47 if bytes.starts_with(b"GIF8") => "gif",
        0xFF if bytes.starts_with(&[0xFF, 0xD8]) => "jpg",
        b'R' if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => "webp",
        b'B' if bytes.starts_with(b"BM") => "bmp",
        _ => return None,
    };
    Some((bytes, ext))
}

/// Discovery + Transformation + Dedup + Persistence + Notify。
fn commit(mut entry: Entry, image_bytes: Option<Vec<u8>>) {
    let cfg = crate::cfg();

    // ---- 来源程序策略 ----
    for policy in &cfg.app_policies {
        if !rules::policy_matches(policy, &entry.source_app, entry.source_path.as_deref(), entry.kind)
        {
            continue;
        }
        if policy.action.eq_ignore_ascii_case("ignore") {
            return;
        }
        if entry.kind.is_text() && !policy.rules.trim().is_empty() {
            let ruleset = rules::parse_cleanup_rules(&policy.rules);
            match rules::apply_cleanup_rules(&entry.content, &ruleset) {
                Some(cleaned) if !cleaned.trim().is_empty() => {
                    entry.content = cleaned.replace("\r\n", "\n");
                    entry.preview = rules::build_preview(entry.kind, &entry.content, entry.html.as_deref());
                    entry.hash = rules::text_hash(&entry.content);
                }
                _ => return,
            }
        }
    }

    // ---- 全局清理规则 ----
    if entry.kind.is_text() && !cfg.cleanup_rules.trim().is_empty() {
        let ruleset = rules::parse_cleanup_rules(&cfg.cleanup_rules);
        if !ruleset.is_empty() {
            match rules::apply_cleanup_rules(&entry.content, &ruleset) {
                Some(cleaned) if !cleaned.trim().is_empty() => {
                    entry.content = cleaned.replace("\r\n", "\n");
                    entry.preview = rules::build_preview(entry.kind, &entry.content, entry.html.as_deref());
                    entry.hash = rules::text_hash(&entry.content);
                }
                _ => return,
            }
        }
    }

    // ---- 超大记录：照常可用，但不进历史文件（原因见 store::MAX_PERSIST_BYTES）----
    {
        let html = entry.html.as_ref().map_or(0, |h| h.len());
        if entry.content.len() + html > store::MAX_PERSIST_BYTES
            && !entry.tags.iter().any(|t| t == "不保存")
        {
            entry.tags.push("不保存".into());
            // 只在这一条第一次入库时打一行，别每次复制都刷屏
            zx_core::log::info(format!(
                "这条剪贴板内容 {:.1} MB，超过 {} MB 上限：本次会话可用，但不写历史文件",
                (entry.content.len() + html) as f64 / 1048576.0,
                store::MAX_PERSIST_BYTES / 1024 / 1024
            ));
        }
    }

    // ---- 隐私识别：打 sensitive 标签（预览脱敏在面板里做）----
    if cfg.privacy
        && entry.kind.is_text()
        && rules::contains_sensitive(&entry.content, &cfg.privacy_kinds, &cfg.privacy_custom)
        && !entry.tags.iter().any(|t| t == "sensitive")
    {
        entry.tags.push("sensitive".into());
    }

    // ---- 图片落盘 ----
    if entry.kind == Kind::Image {
        if let Some(bytes) = image_bytes {
            let ext = entry.ext.clone().unwrap_or_else(|| "png".into());
            let store = store::store().lock().unwrap_or_else(|p| p.into_inner());
            let Some(path) = store.save_attachment(&bytes, &ext) else { return };
            entry.content = path;
            entry.preview = "[图片]".into();
        } else if entry.content.is_empty() {
            return;
        }
    }

    let hash = entry.hash;
    LAST_HASH.store(hash, Ordering::Relaxed);
    LAST_AT.store(now_ms(), Ordering::Relaxed);

    let existed = {
        let s = store::store().lock().unwrap_or_else(|p| p.into_inner());
        s.find_hash(hash).is_some()
    };

    let id = {
        let mut s = store::store().lock().unwrap_or_else(|p| p.into_inner());
        s.add(entry.clone())
    };
    let Some(id) = id else { return };

    // 参考实现把「复制音」放在落库之前，为的是手感；这里同样不等 I/O 完成。
    if cfg.sound {
        crate::sound::play("copy", cfg.sound_volume);
    }
    if cfg.sequential_mode && !existed {
        crate::paste::enqueue(id);
    }
    crate::panel::refresh();
    zx_core::log::info(format!(
        "采集 #{} {} {}（来源 {}）",
        id,
        entry.kind.as_str(),
        entry.preview.chars().take(30).collect::<String>(),
        entry.source_app
    ));
}
