//! 失败原因分类。
//!
//! 参考 Filee 的做法（`error.no_route` / `error.over_size_limit` 这类错误码 + 本地化文案）：
//! **错误必须能分类**，不能把 `io::Error` 直接甩给用户——用户看到 "os error 5" 什么也做不了。
//! 这里每个分支都带一条能照做的中文提示，批量转换时还能按类别汇总。

use std::path::Path;

#[derive(Debug, Clone)]
pub enum ConvertError {
    /// 源文件不见了 / 不是文件。
    NotFound(String),
    /// 扩展名不在支持列表里（HEIC、AVIF、PSD……）。
    Unsupported(String),
    /// 源文件本身就是目标格式，转了也是白转。
    AlreadyThat(String),
    /// 解码失败：文件损坏、截断、伪装成图片的文本。
    Corrupt(String),
    /// 像素太多，拒绝加载（防止一张 5 万 × 5 万的 PNG 把内存吃光）。
    TooLarge { w: u32, h: u32 },
    /// 目标路径写不进去：权限、只读、磁盘满。
    WriteFailed(String),
    /// 编码器自己失败（格式限制、参数越界）。
    Encode(String),
}

impl ConvertError {
    /// 给用户看的一句话（不含文件名，文件名由调用方拼）。
    pub fn reason(&self) -> String {
        match self {
            ConvertError::NotFound(_) => "文件不存在或已被移动".into(),
            ConvertError::Unsupported(ext) => format!("不支持读取 .{ext} 格式"),
            ConvertError::AlreadyThat(ext) => format!("已经是 .{ext} 格式，无需转换"),
            ConvertError::Corrupt(_) => "文件损坏或不是有效的图片".into(),
            ConvertError::TooLarge { w, h } => {
                format!("图片尺寸过大（{w}×{h}），已拒绝转换以免占满内存")
            }
            ConvertError::WriteFailed(_) => "无法写入目标文件（权限不足、只读或磁盘已满）".into(),
            ConvertError::Encode(d) => format!("编码失败：{d}"),
        }
    }

    /// 日志/悬停用的细节（可能含系统原文）。
    pub fn detail(&self) -> Option<&str> {
        match self {
            ConvertError::NotFound(d)
            | ConvertError::Unsupported(d)
            | ConvertError::AlreadyThat(d)
            | ConvertError::Corrupt(d)
            | ConvertError::WriteFailed(d)
            | ConvertError::Encode(d) => Some(d),
            ConvertError::TooLarge { .. } => None,
        }
    }

    /// 批量汇总里用的短标签。
    pub fn tag(&self) -> &'static str {
        match self {
            ConvertError::NotFound(_) => "源文件不存在",
            ConvertError::Unsupported(_) => "格式不支持",
            ConvertError::AlreadyThat(_) => "已是目标格式",
            ConvertError::Corrupt(_) => "文件损坏",
            ConvertError::TooLarge { .. } => "尺寸过大",
            ConvertError::WriteFailed(_) => "写入失败",
            ConvertError::Encode(_) => "编码失败",
        }
    }
}

/// 把 `io::Error` 折成我们的分类，顺手带上系统原文。
pub fn io_error(e: &std::io::Error, what: &Path) -> ConvertError {
    let detail = format!("{}: {}", what.display(), e);
    match e.kind() {
        std::io::ErrorKind::NotFound => ConvertError::NotFound(detail),
        std::io::ErrorKind::PermissionDenied => ConvertError::WriteFailed(detail),
        _ => {
            // ERROR_DISK_FULL(112) / ERROR_HANDLE_DISK_FULL(39)
            if matches!(e.raw_os_error(), Some(112) | Some(39)) {
                ConvertError::WriteFailed(format!("磁盘已满：{detail}"))
            } else {
                ConvertError::WriteFailed(detail)
            }
        }
    }
}
