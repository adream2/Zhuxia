//! 这里曾经是本仓库的第一份 WinHTTP 实现。
//!
//! 现在真身统一在 [`zx_core::http`]（`taskmon` 也用它）。这个模块只做转发，
//! 保留 `imgbed::http::request` 这个路径，调用点一行不用改。

pub use zx_core::http::*;
