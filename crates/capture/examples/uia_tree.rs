//! UIA 层级探针：光标下元素的**真实父链**（ControlView vs RawView）。
//!
//! ```text
//! uia_tree.exe            # 光标位置，RawView（默认）
//! uia_tree.exe --control  # 对照 ControlView
//! ```
//!
//! 这个探针回答的是整个方案的核心问题：
//!
//! 这决定战略方向：
//! * 如果父链里有"卡片行/面板"这一层（List/Pane，矩形和视觉块对得上）→ UIA 能做到；
//! * 如果卡片直接挂在 Document 下（中间什么都没有）→ UIA 拿不到布局块，只能上视觉。
//!
//! 用法：`cargo run --example _tree_probe -- <x,y>`（光标位置由参数给，避免动鼠标）

use std::time::Duration;

use capture::uielem;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationElement,
    IUIAutomationTreeWalker};
use windows::Win32::UI::Accessibility::{
    UIA_DocumentControlTypeId, UIA_GroupControlTypeId, UIA_ListControlTypeId,
    UIA_PaneControlTypeId, UIA_WindowControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows::Win32::Foundation::{POINT, RECT};

fn main() {
    let mut pt = POINT::default();
    unsafe { let _ = GetCursorPos(&mut pt); }
    if let Some(a) = std::env::args().nth(1) {
        if let Some((x, y)) = a.split_once(',') {
            if let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) {
                pt = POINT { x, y };
            }
        }
    }
    if std::env::args().any(|a| a == "--list") {
        for (cls, title, x, y, w, h) in list_windows() {
            println!("{cls:<28} {:<34} @{x},{y} {w}x{h}", trunc(&title, 32));
        }
        return;
    }
    println!("探测点：{},{}", pt.x, pt.y);
    if std::env::args().any(|a| a == "--chain") {
        for t in capture::target::chain_at(pt.x, pt.y, std::process::id()) {
            println!(
                "  {:<44} {}x{} @{},{}",
                trunc(&t.label, 42),
                t.rect.right - t.rect.left,
                t.rect.bottom - t.rect.top,
                t.rect.left,
                t.rect.top
            );
        }
        return;
    }
    probe(pt);
}

fn probe(pt: POINT) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let uia: IUIAutomation = match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("创建 UIA 失败：{e}");
                return;
            }
        };
        let el = match uia.ElementFromPoint(pt) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("ElementFromPoint 失败：{e}");
                return;
            }
        };
        // 默认就是 RawView：布局层级只在 RawView 里，ControlView 只剩元素 → Document。
        // 要对照 ControlView 就加 `--control`。
        let raw = !std::env::args().any(|a| a == "--control");
        let walker: IUIAutomationTreeWalker = match if raw {
            uia.RawViewWalker()
        } else {
            uia.ControlViewWalker()
        } {
            Ok(w) => w,
            Err(e) => {
                eprintln!("取 walker 失败：{e}");
                return;
            }
        };
        let mut cur: Option<IUIAutomationElement> = Some(el);
        let mut depth = 0;
        while let Some(e) = cur {
            let ct = e.CurrentControlType().map(|c| c.0).unwrap_or(-1);
            let name = e.CurrentName().map(|b| b.to_string()).unwrap_or_default();
            let r: RECT = e.CurrentBoundingRectangle().unwrap_or_default();
            println!(
                "{:>2}. {:<10} {:<28} @{},{} {}x{}",
                depth,
                type_name(ct),
                trunc(&name, 26),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top
            );
            if ct == UIA_WindowControlTypeId.0 {
                break;
            }
            cur = walker.GetParentElement(&e).ok();
            depth += 1;
            if depth > 12 {
                break;
            }
        }
        // 顺便报一下"几何祖先"会选出什么，验证它和真实父链是否一致
        let elems = uielem::snapshot_targets(&uielem::capture_targets(), Duration::from_millis(1500));
        let chain = uielem::stack_at(&elems, pt.x, pt.y);
        println!("\n几何包含链（我们实际用的）：");
        for (i, e) in chain.iter().enumerate() {
            println!(
                "{i:>2}. {:<10} {:<28} {}x{}",
                e.kind.label(),
                trunc(&e.name, 26),
                e.rect.w,
                e.rect.h
            );
        }
    }
}

fn type_name(ct: i32) -> &'static str {
    if ct == UIA_ListControlTypeId.0 {
        "List"
    } else if ct == UIA_PaneControlTypeId.0 {
        "Pane"
    } else if ct == UIA_GroupControlTypeId.0 {
        "Group"
    } else if ct == UIA_DocumentControlTypeId.0 {
        "Document"
    } else if ct == UIA_WindowControlTypeId.0 {
        "Window"
    } else {
        "ctrl"
    }
}

fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}…") } else { t }
}

/// 列出可见的顶层窗口（类名/标题/矩形），找不到浏览器时用来定位。
pub fn list_windows() -> Vec<(String, String, i32, i32, i32, i32)> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, IsWindowVisible,
    };
    unsafe extern "system" fn cb(hwnd: HWND, l: LPARAM) -> BOOL {
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        let out = &mut *(l.0 as *mut Vec<(String, String, i32, i32, i32, i32)>);
        let mut b1 = [0u16; 96];
        let n1 = GetClassNameW(hwnd, &mut b1).max(0) as usize;
        let cls = String::from_utf16_lossy(&b1[..n1.min(96)]);
        let mut b2 = [0u16; 160];
        let n2 = GetWindowTextW(hwnd, &mut b2).max(0) as usize;
        let title = String::from_utf16_lossy(&b2[..n2.min(160)]);
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        if r.right - r.left > 200 && r.bottom - r.top > 100 {
            out.push((cls, title, r.left, r.top, r.right - r.left, r.bottom - r.top));
        }
        TRUE
    }
    let mut v: Vec<(String, String, i32, i32, i32, i32)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}
