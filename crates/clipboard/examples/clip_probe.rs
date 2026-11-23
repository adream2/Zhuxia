//! 剪贴板模块的离线探针（不启动宿主、不抢单实例）：
//!
//! ```text
//! cargo run -p clipboard --example clip_probe -- capture 20   # 监听 20 秒并打印记录
//! cargo run -p clipboard --example clip_probe -- shot out.png # 截一张主屏（验证界面）
//! cargo run -p clipboard --example clip_probe -- panel 30     # 直接打开面板并跑 30 秒消息循环
//! ```

unsafe extern "system" fn caret_target_proc(
    h: windows::Win32::Foundation::HWND,
    m: u32,
    w: windows::Win32::Foundation::WPARAM,
    l: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(h, m, w, l)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("capture");
    zx_ui::window::init_dpi();
    let _ = zx_core::cfg::load();

    match mode {
        "shot" => {
            let path = args.get(2).cloned().unwrap_or_else(|| "shot.png".into());
            shot(&path);
        }
        "sendwinv" => {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            let send = |vk: u16, up: bool| {
                let input = INPUT {
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
                };
                unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            };
            send(VK_LWIN.0, false);
            std::thread::sleep(std::time::Duration::from_millis(40));
            send(b'V' as u16, false);
            send(b'V' as u16, true);
            send(VK_LWIN.0, true);
            println!("sent win+v");
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
        "winv" => {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetClassNameW, FindWindowW};
            use windows::Win32::UI::WindowsAndMessaging::*;
            clipboard::panel::init();
            clipboard::winv::set_enabled(true);
            std::thread::sleep(std::time::Duration::from_millis(400));
            println!("hook active={}", clipboard::winv::active());
            let send = |vk: u16, up: bool| {
                let input = INPUT {
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
                };
                let n = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
                if n == 0 {
                    println!("SendInput failed err={:?}", unsafe { windows::Win32::Foundation::GetLastError() });
                }
            };
            send(VK_LWIN.0, false);
            std::thread::sleep(std::time::Duration::from_millis(30));
            send(b'V' as u16, false);
            send(b'V' as u16, true);
            send(VK_LWIN.0, true);
            // 面板由调度窗口的消息驱动，探针这里得自己泵一会儿消息
            unsafe {
                let end = std::time::Instant::now() + std::time::Duration::from_millis(900);
                let mut msg = MSG::default();
                while std::time::Instant::now() < end {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
            }
            println!("panel visible={}", clipboard::panel::is_visible());
            unsafe {
                let fg = GetForegroundWindow();
                let mut cls = [0u16; 128];
                let n = GetClassNameW(fg, &mut cls);
                println!("foreground class={:?}", String::from_utf16_lossy(&cls[..n as usize]));
                for name in ["XamlExplorerHostIslandWindow", "Windows.UI.Core.CoreWindow", "Shell_TrayWnd"] {
                    let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                    let h = FindWindowW(windows::core::PCWSTR(w.as_ptr()), windows::core::PCWSTR::null());
                    println!("{name} -> {:?}", h.map(|h| !h.is_invalid()));
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(15));
        }
        "altc" => {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            let send = |vk: u16, up: bool| {
                let input = INPUT {
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
                };
                unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            };
            send(VK_MENU.0, false);
            std::thread::sleep(std::time::Duration::from_millis(30));
            send(b'C' as u16, false);
            send(b'C' as u16, true);
            send(VK_MENU.0, true);
            println!("sent alt+c");
        }
        "editwin" => {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::Graphics::Gdi::CreateSolidBrush;
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::*;
            unsafe {
                let hinst = GetModuleHandleW(None).unwrap();
                let cls: Vec<u16> = "ZXCaretTarget\0".encode_utf16().collect();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(caret_target_proc),
                    hInstance: hinst.into(),
                    lpszClassName: windows::core::PCWSTR(cls.as_ptr()),
                    hbrBackground: CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00FFFFFF)),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                let h = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::PCWSTR(cls.as_ptr()),
                    windows::core::w!("caret target"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    120, 120, 520, 150,
                    HWND::default(), HMENU::default(), hinst, None,
                ).unwrap();
                let edit = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    windows::core::w!("hello caret target text"),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(0x0080) | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                    14, 14, 470, 28,
                    h, HMENU(std::ptr::dangling_mut()), hinst, None,
                ).unwrap();
                let _ = ShowWindow(h, SW_SHOW);
                let _ = SetWindowPos(h, HWND_TOPMOST, 120, 120, 520, 150, SWP_SHOWWINDOW);
                let _ = SetForegroundWindow(h);
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(edit);
                SendMessageW(edit, 0x00B1, WPARAM(4), LPARAM(4));
                println!("editwin ready");
                let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
                let mut msg = MSG::default();
                while std::time::Instant::now() < end {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        "carettarget" => {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::Graphics::Gdi::ClientToScreen;
            use windows::Win32::UI::WindowsAndMessaging::*;
            let cls: Vec<u16> = "ZXCaretTarget\0".encode_utf16().collect();
            unsafe {
                let Ok(found) = FindWindowW(windows::core::PCWSTR(cls.as_ptr()), windows::core::PCWSTR::null()) else {
                    println!("target not found");
                    return;
                };
                unsafe fn find_edit(parent: HWND, depth: usize) -> HWND {
                    if depth > 4 || parent.is_invalid() { return HWND::default(); }
                    let mut child = GetWindow(parent, GW_CHILD).unwrap_or_default();
                    while !child.is_invalid() {
                        let mut c = [0u16; 64];
                        let n = GetClassNameW(child, &mut c);
                        if String::from_utf16_lossy(&c[..n as usize]) == "Edit" { return child; }
                        let r = find_edit(child, depth + 1);
                        if !r.is_invalid() { return r; }
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                    HWND::default()
                }
                let edit = find_edit(found, 0);
                if edit.is_invalid() { println!("edit not found"); return; }
                // 用真实鼠标点击把焦点交给目标编辑框（跨进程 SetFocus 不可靠）
                {
                    use windows::Win32::UI::Input::KeyboardAndMouse::*;
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(edit, &mut r);
                    let _ = SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                    let mk = |flags: MOUSE_EVENT_FLAGS| INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: 0,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let events = [mk(MOUSEEVENTF_LEFTDOWN), mk(MOUSEEVENTF_LEFTUP)];
                    SendInput(&events, std::mem::size_of::<INPUT>() as i32);
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
                SendMessageW(edit, 0x00B1, WPARAM(6), LPARAM(6));
                let mut pid = 0u32;
                let tid = GetWindowThreadProcessId(found, Some(&mut pid));
                {
                    let mut er = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(edit, &mut er);
                    let mut cur = windows::Win32::Foundation::POINT::default();
                    let _ = GetCursorPos(&mut cur);
                    println!("edit rect {},{} {}x{}, cursor {},{}", er.left, er.top, er.right-er.left, er.bottom-er.top, cur.x, cur.y);
                    let fg = GetForegroundWindow();
                    let mut t = [0u16; 128];
                    let n = GetWindowTextW(fg, &mut t);
                    println!("foreground is target: {} title={:?}", fg == found, String::from_utf16_lossy(&t[..n as usize]));
                }
                let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
                if GetGUIThreadInfo(tid, &mut gti).is_ok() && !gti.hwndCaret.is_invalid() {
                    let mut a = windows::Win32::Foundation::POINT { x: gti.rcCaret.left, y: gti.rcCaret.top };
                    let mut b = windows::Win32::Foundation::POINT { x: gti.rcCaret.left, y: gti.rcCaret.bottom };
                    let _ = ClientToScreen(gti.hwndCaret, &mut a);
                    let _ = ClientToScreen(gti.hwndCaret, &mut b);
                    println!("caret {},{},{},{}", a.x, a.y, b.x, b.y);
                } else {
                    println!("no caret");
                }
                clipboard::panel::init();
                clipboard::panel::show(true);
                let end = std::time::Instant::now() + std::time::Duration::from_millis(900);
                let mut msg = MSG::default();
                while std::time::Instant::now() < end {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    println!("panel rect {},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top);
                }
                std::thread::sleep(std::time::Duration::from_secs(12));
            }
        }
        "caret" => {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::*;
            use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
            use windows::Win32::Graphics::Gdi::ClientToScreen;
            let _ = std::process::Command::new("notepad.exe").spawn();
            std::thread::sleep(std::time::Duration::from_millis(3000));
            unsafe {
                let mut found = HWND::default();
                for cls in ["Notepad", "ApplicationFrameWindow"] {
                    let c: Vec<u16> = cls.encode_utf16().chain(std::iter::once(0)).collect();
                    if let Ok(h) = FindWindowW(windows::core::PCWSTR(c.as_ptr()), windows::core::PCWSTR::null()) {
                        found = h;
                        break;
                    }
                }
                if found.is_invalid() {
                    println!("notepad not found");
                    return;
                }
                let _ = SetForegroundWindow(found);
                std::thread::sleep(std::time::Duration::from_millis(400));
                unsafe fn find_edit(parent: HWND, depth: usize) -> HWND {
                    if depth > 5 || parent.is_invalid() {
                        return HWND::default();
                    }
                    let mut child = GetWindow(parent, GW_CHILD).unwrap_or_default();
                    while !child.is_invalid() {
                        let mut cls = [0u16; 64];
                        let n = GetClassNameW(child, &mut cls);
                        let c = String::from_utf16_lossy(&cls[..n as usize]);
                        if matches!(c.as_str(), "RichEditD2DPT" | "Edit" | "RICHEDIT50W" | "RichEdit20W") {
                            return child;
                        }
                        let r = find_edit(child, depth + 1);
                        if !r.is_invalid() {
                            return r;
                        }
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                    HWND::default()
                }
                let edit = find_edit(found, 0);
                if edit.is_invalid() {
                    println!("edit not found in {:?}", found.0);
                    return;
                }
                // 跨线程 SetFocus 要先 attach 目标线程的输入队列
                let mut npid = 0u32;
                let ntid = GetWindowThreadProcessId(found, Some(&mut npid));
                let self_tid = windows::Win32::System::Threading::GetCurrentThreadId();
                let attached = windows::Win32::System::Threading::AttachThreadInput(self_tid, ntid, true).as_bool();
                let _ = SetFocus(edit);
                SendMessageW(edit, 0x00B1, WPARAM(5), LPARAM(5));
                std::thread::sleep(std::time::Duration::from_millis(300));
                {
                    let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
                    if GetGUIThreadInfo(ntid, &mut gti).is_ok() && !gti.hwndCaret.is_invalid() {
                        let mut a = windows::Win32::Foundation::POINT { x: gti.rcCaret.left, y: gti.rcCaret.top };
                        let mut b = windows::Win32::Foundation::POINT { x: gti.rcCaret.left, y: gti.rcCaret.bottom };
                        let _ = ClientToScreen(gti.hwndCaret, &mut a);
                        let _ = ClientToScreen(gti.hwndCaret, &mut b);
                        println!("caret before show: ({},{})..({},{})", a.x, a.y, b.x, b.y);
                    } else {
                        println!("no caret before show");
                    }
                    println!("fg is notepad: {}", GetForegroundWindow() == found);
                }
                if attached {
                    let _ = windows::Win32::System::Threading::AttachThreadInput(self_tid, ntid, false);
                }
                clipboard::panel::init();
                clipboard::panel::show(true);
                // 泵一会儿消息，让面板画出来
                let end = std::time::Instant::now() + std::time::Duration::from_millis(900);
                let mut msg = MSG::default();
                while std::time::Instant::now() < end {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                // 插入符屏幕坐标（独立算一遍方便对照）
                let mut pid = 0u32;
                let tid = GetWindowThreadProcessId(found, Some(&mut pid));
                let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
                if GetGUIThreadInfo(tid, &mut gti).is_ok() && !gti.hwndCaret.is_invalid() {
                    let mut pt = windows::Win32::Foundation::POINT { x: gti.rcCaret.left, y: gti.rcCaret.bottom };
                    let _ = ClientToScreen(gti.hwndCaret, &mut pt);
                    println!("caret screen {},{},{}x{}", pt.x, pt.y, gti.rcCaret.right - gti.rcCaret.left, gti.rcCaret.bottom - gti.rcCaret.top);
                } else {
                    println!("no caret info");
                }
                let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    println!("panel rect {},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top);
                }
                std::thread::sleep(std::time::Duration::from_secs(12));
            }
        }
        "inspect" => {
            let path = args.get(2).cloned().unwrap_or_default();
            match std::fs::read(&path) {
                Ok(bytes) => {
                    println!("len={} magic={}", bytes.len(), clipboard::secure::is_protected(&bytes));
                    match clipboard::secure::unprotect(&bytes) {
                        Some(plain) => {
                            println!("plain len={} head={:?}", plain.len(), String::from_utf8_lossy(&plain[..plain.len().min(120)]));
                            if let Some(out) = args.get(3) {
                                let _ = std::fs::write(out, &plain);
                                println!("written to {out}");
                            }
                        }
                        None => println!("decrypt failed"),
                    }
                }
                Err(e) => println!("read error: {e}"),
            }
        }
        "dirstats" => {
            let dir = std::path::PathBuf::from(
                args.get(2).cloned().unwrap_or_else(|| "target/debug/examples/clipboard".into()),
            );
            let st = clipboard::store::read_disk_stats(&dir);
            println!(
                "total={} pinned={} secret={} images={} files={} sensitive={} bytes={}",
                st.total, st.pinned, st.secret, st.images, st.files, st.sensitive, st.bytes
            );
        }
        "rect" => {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let h = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr()));
                if let Ok(h) = h {
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    println!("rect {},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top);
                    let mut cr = windows::Win32::Foundation::RECT::default();
                    let _ = GetClientRect(h, &mut cr);
                    println!("client {}x{}", cr.right, cr.bottom);
                } else {
                    println!("not found");
                }
            }
        }
        "seed" => {
            let mut st = clipboard::store::store().lock().unwrap();
            let now = clipboard::model::now_ms();
            let mk = |kind, content: &str, html: Option<&str>, src: &str, ago: i64| {
                let mut e = clipboard::model::Entry {
                    kind,
                    content: content.to_string(),
                    html: html.map(|h| h.to_string()),
                    source_app: src.to_string(),
                    created_at: now - ago,
                    sort_at: now - ago,
                    ..Default::default()
                };
                e.preview = clipboard::rules::build_preview(kind, content, html);
                e.hash = clipboard::rules::text_hash(content);
                e
            };
            st.add(mk(clipboard::model::Kind::Text, "这是一条很长的文本，用来检查换行与详情页的排版。The quick brown fox jumps over the lazy dog. 0123456789 中文混排 ABC def。再补一段让预览进入第二行。", None, "WeChat", 30_000));
            st.add(mk(clipboard::model::Kind::Url, "https://github.com/example/repo", None, "Chrome", 120_000));
            st.add(mk(clipboard::model::Kind::Code, "fn main() {
    println!(\"hello\");
}", None, "Code", 300_000));
            st.add(mk(clipboard::model::Kind::RichText, "加粗的富文本内容", Some("<p><b>加粗的</b>富文本内容</p>"), "Word", 600_000));
            // 图片走附件：现在附件是 DPAPI 密文，正好验证解密显示/粘贴。
            let png = std::fs::read("assets/icons/png/zhuxia-128.png").unwrap_or_default();
            if let Some(path) = st.save_attachment(&png, "png") {
                let mut img = mk(clipboard::model::Kind::Image, &path, None, "Snipping Tool", 900_000);
                img.ext = Some("png".into());
                st.add(img);
            }
            st.add(mk(clipboard::model::Kind::File, "C:/Windows/notepad.exe
C:/Windows/explorer.exe", None, "Explorer", 1_200_000));
            // 密钥样例：只有 content 是「会粘贴」的东西，标题/描述只是元数据。
            let mut k = mk(clipboard::model::Kind::Text, "ghp_SECRET_ABC123456789xyz", None, "1Password", 1_500_000);
            let kid = st.add(k).unwrap();
            st.set_secret(kid, true, "GitHub Token");
            st.set_note(kid, "工作账号，2027-01 过期；只给 CI 用");
            k = mk(clipboard::model::Kind::Text, "P@ssw0rd!2026", None, "KeePass", 1_800_000);
            let kid2 = st.add(k).unwrap();
            st.set_secret(kid2, true, "内网 VPN 账号");
            println!("seeded -> {}", st.len());
        }
        "paste" => {
            let id: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
            let entry = clipboard::store::store().lock().unwrap().get(id).expect("entry");
            println!("write={}", clipboard::paste::write_entry(&entry, false));
            println!("text={:?}", clipboard::win_clip::read_text().map(|t| t.chars().take(40).collect::<String>()));
            println!("html={:?}", clipboard::win_clip::read_html().map(|t| t.chars().take(40).collect::<String>()));
            println!("native={:?}", clipboard::win_clip::read_native_image().map(|(b, e, _)| (b.len(), e)));
            println!("files={:?}", clipboard::win_clip::read_files());
        }
        "text" => {
            let txt = args.get(2).cloned().unwrap_or_else(|| "hello".into());
            println!("write={}", clipboard::win_clip::write_text(&txt));
            println!("read={:?}", clipboard::win_clip::read_text());
        }
        "image" => {
            let path = args.get(2).cloned().unwrap_or_default();
            println!("write={}", clipboard::win_clip::write_image_file(&path));
            println!("native={:?}", clipboard::win_clip::read_native_image().map(|(b, e, _)| (b.len(), e)));
        }
        "rclick" => {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::*;
            let x: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
            let y: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(160);
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    let _ = SetCursorPos(r.left + x, r.top + y);
                    let lp = LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
                    // 菜单是模态的：这条消息要等菜单关掉才返回。
                    let _ = SendMessageW(h, WM_RBUTTONUP, WPARAM(0), lp);
                    println!("menu closed");
                }
            }
        }
        "hover" => {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::*;
            let x: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
            let y: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(160);
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    let _ = SetCursorPos(r.left + x, r.top + y);
                    let lp = LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
                    let _ = SendMessageW(h, WM_MOUSEMOVE, WPARAM(0), lp);
                    println!("hover at {},{}", r.left + x, r.top + y);
                } else {
                    println!("panel not found");
                }
            }
        }
        "click" => {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::*;
            let x: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
            let y: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(160);
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    let lp = LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
                    let _ = SendMessageW(h, WM_LBUTTONDOWN, WPARAM(1), lp);
                    let _ = SendMessageW(h, WM_LBUTTONUP, WPARAM(0), lp);
                    println!("click {x},{y}");
                }
            }
        }
        "ctrlkey" => {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            let vk: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(b'N' as u16);
            let mk = |vk: u16, up: bool| INPUT {
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
            };
            unsafe {
                SendInput(&[mk(VK_CONTROL.0, false)], std::mem::size_of::<INPUT>() as i32);
                std::thread::sleep(std::time::Duration::from_millis(30));
                SendInput(&[mk(vk, false), mk(vk, true)], std::mem::size_of::<INPUT>() as i32);
                std::thread::sleep(std::time::Duration::from_millis(30));
                SendInput(&[mk(VK_CONTROL.0, true)], std::mem::size_of::<INPUT>() as i32);
            }
            println!("sent ctrl+{vk:#x}");
        }
        "formdump" => {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let title = args.get(2).cloned().unwrap_or_else(|| "密钥信息".into());
            let tw: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(tw.as_ptr())) {
                    let mut child = GetWindow(h, GW_CHILD).unwrap_or_default();
                    let mut i = 0usize;
                    while !child.is_invalid() {
                        let mut cls = [0u16; 32];
                        let n = GetClassNameW(child, &mut cls);
                        if String::from_utf16_lossy(&cls[..n as usize]) == "Edit" {
                            let len = GetWindowTextLengthW(child);
                            let mut buf = vec![0u16; len as usize + 1];
                            let n = GetWindowTextW(child, &mut buf);
                            println!("edit{i} = {:?}", String::from_utf16_lossy(&buf[..n as usize]));
                            i += 1;
                        }
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                } else {
                    println!("dialog not found");
                }
            }
        }
        "formset" => {
            // 只填字段，不按确定（复现“填完标题点描述”的问题）
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::*;
            let title = args.get(2).cloned().unwrap_or_else(|| "密钥信息".into());
            let text = args.get(3).cloned().unwrap_or_default();
            let tw: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(tw.as_ptr())) {
                    let mut child = GetWindow(h, GW_CHILD).unwrap_or_default();
                    let mut i = 0usize;
                    while !child.is_invalid() {
                        let mut cls = [0u16; 32];
                        let n = GetClassNameW(child, &mut cls);
                        if String::from_utf16_lossy(&cls[..n as usize]) == "Edit" && i == 0 {
                            let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                            SendMessageW(child, WM_SETTEXT, WPARAM(0), LPARAM(w.as_ptr() as isize));
                            i += 1;
                        }
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                    println!("filled title only");
                } else {
                    println!("dialog not found");
                }
            }
        }
        "formclick" => {
            // 真鼠标点第 n 个 EDIT（0 起），用来看“点描述”会不会让程序退出
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            use windows::Win32::UI::WindowsAndMessaging::*;
            let index: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let title = args.get(2).cloned().unwrap_or_else(|| "密钥信息".into());
            let tw: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(tw.as_ptr())) {
                    let mut child: HWND = GetWindow(h, GW_CHILD).unwrap_or_default();
                    let mut edits = Vec::new();
                    while !child.is_invalid() {
                        let mut cls = [0u16; 32];
                        let n = GetClassNameW(child, &mut cls);
                        if String::from_utf16_lossy(&cls[..n as usize]) == "Edit" {
                            edits.push(child);
                        }
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                    if let Some(edit) = edits.get(index) {
                        let mut r = windows::Win32::Foundation::RECT::default();
                        let _ = GetWindowRect(*edit, &mut r);
                        let _ = SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                        let mk = |flags: MOUSE_EVENT_FLAGS| INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dx: 0,
                                    dy: 0,
                                    mouseData: 0,
                                    dwFlags: flags,
                                    time: 0,
                                    dwExtraInfo: 0,
                                },
                            },
                        };
                        let events = [mk(MOUSEEVENTF_LEFTDOWN), mk(MOUSEEVENTF_LEFTUP)];
                        SendInput(&events, std::mem::size_of::<INPUT>() as i32);
                        println!("clicked edit {} at {},{}", index, (r.left + r.right) / 2, (r.top + r.bottom) / 2);
                    } else {
                        println!("edit {} not found (only {})", index, edits.len());
                    }
                } else {
                    println!("dialog not found");
                }
            }
        }
        "fillform" => {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::*;
            let title = args.get(2).cloned().unwrap_or_else(|| "备注".into());
            let text = args.get(3).cloned().unwrap_or_default();
            let tw: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(tw.as_ptr())) {
                    // 按顺序给每个 EDIT 子窗口填一个值
                    let mut values = vec![text.clone()];
                    values.extend(args.iter().skip(4).cloned());
                    let mut child = GetWindow(h, GW_CHILD).unwrap_or_default();
                    let mut i = 0usize;
                    while !child.is_invalid() {
                        let mut cls = [0u16; 32];
                        let n = GetClassNameW(child, &mut cls);
                        if String::from_utf16_lossy(&cls[..n as usize]) == "Edit" {
                            let v = values.get(i).cloned().unwrap_or_default();
                            let w: Vec<u16> = v.encode_utf16().chain(std::iter::once(0)).collect();
                            SendMessageW(child, WM_SETTEXT, WPARAM(0), LPARAM(w.as_ptr() as isize));
                            i += 1;
                        }
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                    SendMessageW(h, WM_COMMAND, WPARAM(102), LPARAM(0));
                    println!("filled {i} field(s) and confirmed");
                } else {
                    println!("dialog not found");
                }
            }
        }
        "sendvk" => {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            let vk: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0x0D);
            let mk = |up: bool| INPUT {
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
            };
            unsafe { SendInput(&[mk(false), mk(true)], std::mem::size_of::<INPUT>() as i32) };
            println!("sent vk {vk:#x}");
        }
        "ctrlk" => {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            let send = |vk: u16, up: bool| {
                let input = INPUT {
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
                };
                unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            };
            send(VK_CONTROL.0, false);
            std::thread::sleep(std::time::Duration::from_millis(40));
            send(b'K' as u16, false);
            send(b'K' as u16, true);
            send(VK_CONTROL.0, true);
            println!("sent ctrl+k");
        }
        "typename" => {
            // 往当前前台窗口逐字符发送（用于表单对话框）
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            let text = args.get(2).cloned().unwrap_or_default();
            for ch in text.chars() {
                let vk = match ch {
                    'a'..='z' => ch.to_ascii_uppercase() as u16,
                    'A'..='Z' => ch as u16,
                    '0'..='9' => ch as u16,
                    ' ' => VK_SPACE.0,
                    '-' => VK_OEM_MINUS.0,
                    _ => continue,
                };
                let shift = ch.is_ascii_uppercase();
                let mk = |vk: u16, up: bool| INPUT {
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
                };
                unsafe {
                    if shift {
                        SendInput(&[mk(VK_SHIFT.0, false)], std::mem::size_of::<INPUT>() as i32);
                    }
                    SendInput(&[mk(vk, false), mk(vk, true)], std::mem::size_of::<INPUT>() as i32);
                    if shift {
                        SendInput(&[mk(VK_SHIFT.0, true)], std::mem::size_of::<INPUT>() as i32);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            println!("typed");
        }
        "key" => {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let vk: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0x28);
            let times: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    for _ in 0..times {
                        let r = SendMessageW(h, WM_KEYDOWN, windows::Win32::Foundation::WPARAM(vk as usize), windows::Win32::Foundation::LPARAM(0));
                        println!("key ret={:?}", r.0);
                        std::thread::sleep(std::time::Duration::from_millis(30));
                    }
                }
            }
            println!("sent {times} x {vk}");
        }
        "type" => {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let text = args.get(2).cloned().unwrap_or_else(|| "test".into());
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    if let Ok(edit) = GetWindow(h, GW_CHILD) {
                        let mut w: Vec<u16> = text.encode_utf16().collect();
                        w.push(0);
                        SendMessageW(
                            edit,
                            WM_SETTEXT,
                            windows::Win32::Foundation::WPARAM(0),
                            windows::Win32::Foundation::LPARAM(w.as_ptr() as isize),
                        );
                        println!("set text ok");
                    }
                }
            }
        }
        "dump" => {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let title: Vec<u16> = "驻匣 · 剪贴板".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), windows::core::PCWSTR(title.as_ptr())) {
                    println!("panel visible={} enabled={}", IsWindowVisible(h).as_bool(), IsWindowVisible(h).as_bool());
                    let mut child = GetWindow(h, GW_CHILD).unwrap_or_default();
                    loop {
                        if child.is_invalid() { break; }
                        let mut r = windows::Win32::Foundation::RECT::default();
                        let _ = GetWindowRect(child, &mut r);
                        let mut cls = [0u16; 64];
                        let n = GetClassNameW(child, &mut cls);
                        println!("child {:?} visible={} rect={}x{}", String::from_utf16_lossy(&cls[..n as usize]), IsWindowVisible(child).as_bool(), r.right-r.left, r.bottom-r.top);
                        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
                    }
                }
            }
        }
        "panel2" => {
            use windows::Win32::UI::WindowsAndMessaging::*;
            clipboard::panel::init();
            clipboard::panel::show(true);
            let pump = |ms: u64| unsafe {
                let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
                let mut msg = MSG::default();
                while std::time::Instant::now() < end {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
            };
            pump(400);
            clipboard::panel::hide();
            pump(300);
            // 第二次 show：走 show_now 复用分支（占位提示不能被全选）
            clipboard::panel::show(true);
            pump(400);
            println!("panel2 ready");
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
        "panel" => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            clipboard::start();
            clipboard::panel::show(true);
            let hwnd = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(secs));
                // 到点自己退出
                std::process::exit(0);
            });
            let _ = hwnd;
            zx_ui::window::run_loop();
        }
        _ => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
            clipboard::start();
            std::thread::sleep(std::time::Duration::from_secs(secs));
            let s = clipboard::store::store().lock().unwrap();
            let list = s.list(&clipboard::store::Query::default());
            println!("共 {} 条", list.len());
            for e in list.iter().take(10) {
                println!("#{} [{:?}] {} ← {}", e.id, e.kind, e.preview, e.source_app);
            }
        }
    }
}

fn shot(path: &str) {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    use windows::Win32::Graphics::GdiPlus::*;
    zx_ui::painter::init_gdiplus();
    unsafe {
        let screen = GetDC(None);
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        let _ = BitBlt(mem, 0, 0, w, h, screen, 0, 0, SRCCOPY);
        let mut img: *mut GpBitmap = std::ptr::null_mut();
        GdipCreateBitmapFromHBITMAP(bmp, HPALETTE::default(), &mut img);
        let mut num = 0u32;
        let mut size = 0u32;
        GdipGetImageEncodersSize(&mut num, &mut size);
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let enc = buf.as_mut_ptr() as *mut ImageCodecInfo;
        GdipGetImageEncoders(num, size, enc);
        let mut clsid = None;
        for i in 0..num as usize {
            let info = &*enc.add(i);
            let mut len = 0usize;
            if info.MimeType.is_null() {
                continue;
            }
            while *info.MimeType.0.add(len) != 0 {
                len += 1;
            }
            let mime = String::from_utf16_lossy(std::slice::from_raw_parts(info.MimeType.0, len));
            if mime == "image/png" {
                clsid = Some(info.Clsid);
            }
        }
        if let Some(clsid) = clsid {
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let st = GdipSaveImageToFile(img as *mut GpImage, windows::core::PCWSTR(wide.as_ptr()), &clsid, std::ptr::null());
            println!("save status {}", st.0);
        }
        GdipDisposeImage(img as *mut GpImage);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}
