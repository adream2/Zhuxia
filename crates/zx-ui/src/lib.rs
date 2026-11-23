//! 驻匣原生 UI 工具箱（GDI+ 离屏 + 分层窗口）。
//!
//! 为什么不是 WPF / WebView：常驻工具的内存必须按 MB 计。这里一块 400×48 的
//! 32bpp 位图 + 一个 GDI+ Graphics，1Hz 重绘，常驻内存不到 2MB，也没有
//! 任何 UI 框架的托管堆/渲染进程。

pub mod menu;
pub mod painter;
pub mod theme;
pub mod window;

pub use painter::{font_available, luma, Canvas, TextAlign, VAlign};
pub use theme::Palette;

/// 给一个 ARGB 颜色换透明度（0–255）。
pub const fn with_alpha(c: u32, a: u8) -> u32 { (c & 0x00FF_FFFF) | ((a as u32) << 24) }
