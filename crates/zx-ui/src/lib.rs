//! 驻匣原生 UI 工具箱（GDI+ 离屏 + 分层窗口）。
//!
//! 为什么不是 WPF / WebView：常驻工具的内存必须按 MB 计。这里一块 400×48 的
//! 32bpp 位图 + 一个 GDI+ Graphics，1Hz 重绘，常驻内存不到 2MB，也没有
//! 任何 UI 框架的托管堆/渲染进程。

pub mod menu;
pub mod painter;
pub mod theme;
pub mod toast;
pub mod window;

pub use painter::{font_available, luma, Canvas, TextAlign, VAlign};
pub use theme::Palette;

/// 给一个 ARGB 颜色换透明度（0–255）。
pub const fn with_alpha(c: u32, a: u8) -> u32 { (c & 0x00FF_FFFF) | ((a as u32) << 24) }

/// 窗口过程 / 低级钩子的 panic 屏障。
///
/// `extern "system"` 不允许 unwind：回调里任何 panic（切片越界、`unwrap`、
/// 状态机不变量失手）都会直接终止整个进程——对常驻工具来说等于"点一下就没了"。
/// 用这个包住回调体，panic 收敛成一行日志 + 走系统默认处理。
///
/// ```ignore
/// unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
///     zx_ui::guard_proc!(DefWindowProcW(h, m, w, l), { wndproc_inner(h, m, w, l) })
/// }
/// unsafe fn wndproc_inner(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT { /* 原函数体 */ }
/// ```
///
/// 兜底表达式要选"假装这个窗口不认识这条消息"的那一个：窗口过程用
/// `DefWindowProcW`，子类化过程用 `DefSubclassProc`，钩子用 `CallNextHookEx`。
#[macro_export]
macro_rules! guard_proc {
    ($fallback:expr, $body:block) => {
        match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| $body)) {
            ::std::result::Result::Ok(v) => v,
            ::std::result::Result::Err(e) => {
                $crate::log_panic(&e);
                $fallback
            }
        }
    };
}

/// 记录一次发生在回调里的 panic（`guard_proc!` 内部用）。
pub fn log_panic(e: &(dyn std::any::Any + Send)) {
    let msg = if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知 panic".to_string()
    };
    zx_core::log::error(format!("窗口过程/钩子内 panic，已走系统默认处理：{msg}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::LRESULT;

    /// `guard_proc!` 必须真的能兜住 panic。
    ///
    /// 注意要连 release 一起跑（`cargo test --release`）：release 若把 panic 设成
    /// `abort`，这个测试会直接把测试进程干掉——那正是我们要防的"整个程序消失"。
    #[test]
    fn guard_proc_catches_panic() {
        let v: LRESULT = guard_proc!(LRESULT(-1), {
            if std::env::var_os("ZX_TEST_NO_PANIC").is_none() {
                panic!("故意触发的 panic");
            }
            LRESULT(7)
        });
        assert_eq!(v.0, -1, "panic 之后应返回兜底值");
    }

    #[test]
    fn guard_proc_passes_value_through() {
        let v: LRESULT = guard_proc!(LRESULT(-1), {
            let _ = std::env::var_os("ZX_TEST_NO_PANIC");
            LRESULT(42)
        });
        assert_eq!(v.0, 42);
        let _ = with_alpha(0x0012_3456, 0x80);
    }
}
