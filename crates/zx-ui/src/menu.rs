//! 原生弹出菜单（右键菜单）。原生菜单跟随系统主题，内存零增量。

use std::borrow::Cow;

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Item {
    pub id: u32,
    /// 文案支持借用与拥有两种形态：绝大多数调用点传 `&'static str` 零成本，
    /// 托盘那种“已启用/已暂停”状态文案可以临时 `format!` 出来。
    pub label: Cow<'static, str>,
    pub checked: bool,
    pub enabled: bool,
    pub separator: bool,
}

impl Item {
    pub fn new(id: u32, label: impl Into<Cow<'static, str>>) -> Item {
        Item { id, label: label.into(), checked: false, enabled: true, separator: false }
    }
    pub fn checked(mut self, on: bool) -> Item { self.checked = on; self }
    pub fn enabled(mut self, on: bool) -> Item { self.enabled = on; self }
    pub fn sep() -> Item {
        Item { id: 0, label: Cow::Borrowed(""), checked: false, enabled: false, separator: true }
    }
}

/// 弹出菜单并返回被点中的 id（0 = 取消）。`x, y` 是**屏幕坐标**。
///
/// 坑：调用方都是从 `GetCursorPos` 拿的屏幕坐标；早期这里又做了一次
/// `ClientToScreen`，窗口不在原点时菜单会整体偏移一个窗口位置。
pub fn popup(hwnd: HWND, x: i32, y: i32, items: &[Item]) -> u32 {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        for it in items {
            if it.separator {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                continue;
            }
            let mut flags = MF_STRING;
            if it.checked { flags |= MF_CHECKED; }
            if !it.enabled { flags |= MF_GRAYED; }
            // 文案可能是临时拼的，转成 UTF-16 后必须在本次调用期间存活；
            // `AppendMenuW` 会自己拷贝字符串，这里用局部变量即可。
            let w: Vec<u16> = it.label.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = AppendMenuW(menu, flags, it.id as usize, windows::core::PCWSTR(w.as_ptr()));
        }
        // TrackPopupMenu 要求窗口在前台，否则点菜单外面菜单不会消失
        let _ = SetForegroundWindow(hwnd);
        let pt = POINT { x, y };
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, pt.x, pt.y, 0, hwnd, None);
        // MSDN 明写的 workaround：不给窗口投一条消息，菜单可能"粘"在屏幕上
        let _ = PostMessageW(hwnd, WM_NULL, windows::Win32::Foundation::WPARAM(0),
                             windows::Win32::Foundation::LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as u32
    }
}
