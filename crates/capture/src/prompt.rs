//! 一个极小的单行输入弹窗（原生 EDIT + 自己的消息循环）。
//!
//! 用途：设置水印文字。为什么不把 EDIT 放进分层窗口当子控件——`UpdateLayeredWindow`
//! 的分层表面里子窗口的绘制顺序不可靠（会闪、会被盖）。独立的 topmost popup 最稳。

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ZhuxiaCapturePrompt";
const WM_DONE: u32 = WM_APP + 91;

thread_local! {
    static RESULT: Cell<bool> = const { Cell::new(false) };
}

static OLD_EDIT: AtomicIsize = AtomicIsize::new(0);

fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        unsafe {
            let name = zx_core::win::wide(CLASS);
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: HINSTANCE(hinst.0),
                hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            let _ = RegisterClassExW(&wc);
        }
    });
}

/// 弹出输入框，回车/点别处 = 确定，Esc = 取消。
pub fn text(title: &str, initial: &str, near: POINT) -> Option<String> {
    register();
    RESULT.with(|c| c.set(false));
    let (w, h) = (360, 44);
    let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
    let t = zx_core::win::wide(title);
    let cls = zx_core::win::wide(CLASS);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(cls.as_ptr()),
            PCWSTR(t.as_ptr()),
            WS_POPUP | WS_BORDER,
            near.x, near.y, w, h,
            HWND::default(), HMENU::default(), HINSTANCE(hinst.0), None,
        )
    };
    let Ok(hwnd) = hwnd else { return None };
    let edit = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("EDIT"),
            PCWSTR(zx_core::win::wide(initial).as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            6, 8, w - 12, h - 16,
            hwnd, HMENU::default(), HINSTANCE(hinst.0), None,
        )
    };
    let Ok(edit) = edit else {
        unsafe { let _ = DestroyWindow(hwnd); }
        return None;
    };
    let font = unsafe {
        let face = zx_core::win::wide("Microsoft YaHei UI");
        CreateFontW(-16, 0, 0, 0, 400, 0, 0, 0,
                    DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
                    CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32,
                    DEFAULT_PITCH.0 as u32, PCWSTR(face.as_ptr()))
    };
    unsafe {
        SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        // 全选，方便直接覆盖
        SendMessageW(edit, 0x00B1, WPARAM(0), LPARAM(-1));
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, near.x, near.y, w, h, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(edit);
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as usize as isize);
        OLD_EDIT.store(old, Ordering::SeqCst);
    }

    // 局部消息循环：只等这个弹窗结束（WM_DONE 由 wndproc / edit 子类化投递）
    unsafe {
        // 同 picker：清掉上一次可能残留的完成消息，否则新弹窗会立刻返回空值
        let mut stale = MSG::default();
        while PeekMessageW(&mut stale, None, WM_DONE, WM_DONE, PM_REMOVE).as_bool() {}

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_DONE {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let ok = RESULT.with(|c| c.get());
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(edit, &mut buf) };
    let value = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() };
    unsafe {
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
    if ok { Some(value) } else { None }
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { edit_proc_inner(hwnd, msg, w, l) })
}

unsafe fn edit_proc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    if msg == WM_KEYDOWN {
        let key = w.0 as u16;
        if key == 0x0D {
            RESULT.with(|c| c.set(true));
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(parent, WM_DONE, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        if key == 0x1B {
            RESULT.with(|c| c.set(false));
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(parent, WM_DONE, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
    }
    let old = OLD_EDIT.load(Ordering::SeqCst);
    if old == 0 {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    type Proc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    let old: Proc = std::mem::transmute(old);
    CallWindowProcW(Some(old), hwnd, msg, w, l)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    match msg {
        WM_ACTIVATE => {
            if (w.0 & 0xFFFF) == 0 {
                // 点别处 = 确定（和截图里的文字标注一致）
                RESULT.with(|c| c.set(true));
                let _ = PostMessageW(hwnd, WM_DONE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
