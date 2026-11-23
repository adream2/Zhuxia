//! 驻匣内核：配置、日志、格式化、Windows 基础工具、进程内事件总线。
//!
//! 与 UI 无关，可被任意模块复用。

pub mod cfg;
pub mod fmt;
pub mod hotkey;
pub mod log;
pub mod monitor;
pub mod win;

pub use monitor::{displays, DisplayInfo};
pub use cfg::{
    CaptureCfg, CaptureSettings, CaptureStyleCfg,
    RecordAudio, ClipAppPolicy, ClipboardCfg, ClipboardSettings, DisplaySel,
    GestureActionCfg, GestureAppCfg, GestureCfg, GestureGroupCfg, GestureRuleCfg, GestureSettings, GestureTrailCfg,
    AppIconCfg, AppIconSettings,
    ImgBedBrowserCfg, ImgBedCfg, ImgBedCopyCfg, ImgBedSettings, ImgBedUploadCfg,
    ImgConvCfg, ImgConvSettings,
    GpuTaskbarContent, GpuTempUnit, MetricDisplayMode, RecordCamera, RecordCameraCfg, RecordCfg, RecordFormat, RecordLayout,
    RecordLayoutCfg, RecordSettings, Settings, Theme,
};
