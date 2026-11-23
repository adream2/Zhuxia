//! 托盘图标：常驻入口，也是"任务栏条万一不见了"时的兜底。
//!
//! 左键单击 = 立即截图；右键 = 固定五项菜单：
//! 打开设置 / 启停鼠标手势 / 启停按键回显 / 图床 / 退出。
//! 监听 explorer 的 `TaskbarCreated`，explorer 重启后自动补上图标。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 20;
const CLASS: &str = "ZhuxiaTray";
const ID: u32 = 1;

// 托盘回调里的原始鼠标消息（lParam 低 16 位）。
const WM_LBUTTONUP_RAW: u32 = 0x0202;
const WM_LBUTTONDBLCLK_RAW: u32 = 0x0203;
const WM_RBUTTONUP_RAW: u32 = 0x0205;
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;

// 菜单项 id（固定五项，分隔线不算项）。
const ID_SETTINGS: u32 = 300;
const ID_GESTURE: u32 = 301;
const ID_KEYECHO: u32 = 302;
const ID_IMGBED: u32 = 303;
const ID_QUIT: u32 = 304;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
/// 是否成功切到 NOTIFYICON_VERSION_4。失败时退回传统消息语义。
static V4: AtomicBool = AtomicBool::new(false);

thread_local! {
    static TASKBAR_CREATED: RefCell<u32> = const { RefCell::new(0) };
}

pub fn create() -> Option<HWND> {
    zx_ui::window::register_class(CLASS, wndproc, None);
    let hwnd = zx_ui::window::create_window(CLASS, "ZhuxiaTray", WS_POPUP, WS_EX_TOOLWINDOW, 0, 0)?;
    TRAY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    let msg = unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) };
    TASKBAR_CREATED.with(|m| *m.borrow_mut() = msg);
    add_icon(hwnd);
    Some(hwnd)
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    // 托盘图标必须按 SM_CXSMICON 取：LR_DEFAULTSIZE 给的是 32px，
    // shell 再缩到 16px 会糊，而且视觉上比旁边的图标小一圈。
    unsafe {
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        let icon = zx_ui::window::load_app_icon(cx, cy);
        if icon != 0 { d.hIcon = HICON(icon as *mut _); }
    }
    // 左键语义变了，提示语跟着改：不写清楚，老用户会以为左键还是设置。
    let tip: Vec<u16> = "驻匣 Zhuxia · 左键截图，右键菜单".encode_utf16().collect();
    d.szTip[..tip.len().min(127)].copy_from_slice(&tip[..tip.len().min(127)]);
    d
}

fn add_icon(hwnd: HWND) {
    unsafe {
        let d = nid(hwnd);
        if d.hIcon.is_invalid() {
            zx_core::log::warn("托盘图标资源加载失败（exe 资源 ID 1 取不到 16px 图标）");
        }
        if !Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
            // 常见于 explorer 刚起来还没准备好，稍后由 TaskbarCreated 补
            zx_core::log::warn("托盘图标添加失败（explorer 可能未就绪）");
        }
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);

        // v4：单击只发 NIN_SELECT，右键发 WM_CONTEXTMENU，坐标在 wParam。
        // 传统消息会把单击和双击都投成 WM_LBUTTONUP，双击等于触发两次截图。
        let mut ver = d;
        // 这个字段在 windows crate 里是 uTimeout/uVersion 的联合体。
        ver.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let ok = Shell_NotifyIconW(NIM_SETVERSION, &ver).as_bool();
        V4.store(ok, Ordering::Relaxed);
        if !ok {
            zx_core::log::warn("托盘消息未切到 V4，退回传统单击语义");
        }
    }
}

pub fn destroy() {
    let h = TRAY_HWND.swap(0, Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let d = nid(HWND(h as *mut _));
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            let _ = DestroyWindow(HWND(h as *mut _));
        }
    }
}

/// 气泡提示。用于"截图模块被禁用"这种点了却没反应的情况——
/// 只写日志用户看不到，等于静默失败。
fn balloon(hwnd: HWND, title: &str, text: &str) {
    unsafe {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ID,
            uFlags: NIF_INFO,
            dwInfoFlags: NIIF_INFO,
            ..Default::default()
        };
        let t: Vec<u16> = title.encode_utf16().collect();
        let m: Vec<u16> = text.encode_utf16().collect();
        let n = t.len().min(d.szInfoTitle.len() - 1);
        d.szInfoTitle[..n].copy_from_slice(&t[..n]);
        let n = m.len().min(d.szInfo.len() - 1);
        d.szInfo[..n].copy_from_slice(&m[..n]);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

fn on_left_click(hwnd: HWND) {
    if crate::tray_state().capture_enabled {
        crate::run_action("capture");
    } else {
        balloon(hwnd, "截图模块已禁用", "去「设置 → 工具 → 截图工具」里启用，或重启驻匣");
        zx_core::log::info("托盘左键：截图模块已禁用，未触发");
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    let created = TASKBAR_CREATED.with(|m| *m.borrow());
    if created != 0 && msg == created {
        // explorer 重启：重新挂图标，并把覆盖层重建（旧 HWND 已失效）
        add_icon(hwnd);
        crate::app::rebuild_overlays();
        return LRESULT(0);
    }
    match msg {
        WM_CREATE => {
            SetTimer(hwnd, 1, 3000, None);
            LRESULT(0)
        }
        WM_TIMER => {
            // 每 3 秒自检：覆盖层丢了就重建，还在就把 z 序顶回去。
            // 顺带盯一下数量——插拔显示器后每屏一个覆盖层，少了就补齐。
            let have = crate::overlay::count();
            let want = if zx_core::cfg::cfg().floating() { 1 } else { crate::overlay::expected_taskbar_count() };
            if have == 0 || have < want {
                zx_core::log::warn(format!("覆盖层数量不足（{have}/{want}），自动重建"));
                crate::app::rebuild_overlays();
            } else {
                crate::overlay::reassert_topmost();
            }
            LRESULT(0)
        }
        WM_TRAY => {
            // v4 与传统消息的事件号都在 lParam 低 16 位。
            let ev = (l.0 & 0xFFFF) as u32;
            let (mut x, mut y) = (0, 0);
            if V4.load(Ordering::Relaxed) {
                // v4：wParam 低 16 位 = x，高 16 位 = y（有符号，兼容负坐标副屏）。
                x = (w.0 & 0xFFFF) as u16 as i16 as i32;
                y = ((w.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            }
            match ev {
                NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP_RAW => {
                    on_left_click(hwnd);
                }
                WM_LBUTTONDBLCLK_RAW => {
                    // v4 下不会走到这里；传统语义下双击会再补一条 UP，
                    // 这里不额外处理，交给 overlay 的 is_active() 去重。
                }
                WM_CONTEXTMENU => {
                    if x == 0 && y == 0 {
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        x = pt.x;
                        y = pt.y;
                    }
                    show_menu(hwnd, x, y);
                }
                WM_RBUTTONUP_RAW => {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    show_menu(hwnd, pt.x, pt.y);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    use zx_ui::menu::{popup, Item};
    let st = crate::tray_state();

    let (gesture_label, gesture_checked) = if !st.gesture_enabled {
        ("鼠标手势（未加载，需重启）".to_string(), false)
    } else if st.gesture_paused {
        ("鼠标手势（已暂停）".to_string(), false)
    } else {
        ("鼠标手势（已启用）".to_string(), true)
    };
    let keyecho_label = format!("按键回显（{}）", if st.key_echo { "已开启" } else { "已关闭" });
    let imgbed_label = if !st.imgbed_enabled {
        "图床（已停用）".to_string()
    } else if st.imgbed_configured {
        "图床（已启用）".to_string()
    } else {
        "图床（未配置）".to_string()
    };

    let items = vec![
        Item::new(ID_SETTINGS, "打开设置"),
        Item::sep(),
        Item::new(ID_GESTURE, gesture_label)
            .checked(gesture_checked)
            .enabled(st.gesture_enabled),
        Item::new(ID_KEYECHO, keyecho_label).checked(st.key_echo),
        // 图床是“打开设置里那一页”，不是开关；用文字表达状态，不画勾选，
        // 免得看起来像点了会切换。
        Item::new(ID_IMGBED, imgbed_label),
        Item::sep(),
        Item::new(ID_QUIT, "退出"),
    ];
    match popup(hwnd, x, y, &items) {
        ID_SETTINGS => crate::open_settings(),
        ID_GESTURE => crate::run_action("toggle-gesture"),
        ID_KEYECHO => crate::run_action("toggle-keyecho"),
        ID_IMGBED => crate::run_action("imgbed"),
        ID_QUIT => crate::app::exit(),
        _ => {}
    }
}
