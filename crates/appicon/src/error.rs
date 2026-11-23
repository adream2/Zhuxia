//! 失败原因分类。
//!
//! 图标提取的失败比图片转换碎得多：可能是"格式根本不支持"，也可能是"格式没错但里面
//! 没有图标"、"图标是矢量的解得开但画不出"、"DMG 是 APFS 分区"。这些必须分开，
//! 否则用户只会看到一句 `invalid data`，不知道下一步该做什么。

use std::path::Path;

#[derive(Debug, Clone)]
pub enum IconError {
    /// 文件不见了 / 不是文件。
    NotFound(String),
    /// 扩展名或容器不在支持列表里。
    Unsupported(String),
    /// 认不出这是什么（magic 与扩展名都对不上）。
    Unknown(String),
    /// 容器解析成功，但里面没有可用的图标资源。
    NoIcon(String),
    /// 结构损坏：截断、偏移越界、压缩流错误。
    Corrupt(String),
    /// 加密或需要口令（加密 zip、受保护的 IPA）。
    Encrypted(String),
    /// 图标尺寸/解压体积超过保护上限。
    TooLarge(String),
    /// 目标路径写不进去。
    WriteFailed(String),
    /// 需要本机没有的能力（APFS、JPEG 2000、Android 矢量图）。
    NeedExternal(String),
}

impl IconError {
    /// 给用户看的一句话（不含文件名）。
    pub fn reason(&self) -> String {
        match self {
            IconError::NotFound(_) => "文件不存在或已被移动".into(),
            IconError::Unsupported(what) => {
                if !what.is_empty() && what.chars().all(|c| c.is_ascii_alphanumeric()) {
                    format!("不支持提取 .{what} 的图标")
                } else {
                    format!("不支持提取：{what}")
                }
            }
            IconError::Unknown(_) => "认不出文件类型（扩展名与内容都不匹配）".into(),
            IconError::NoIcon(_) => "文件里没有找到图标资源".into(),
            IconError::Corrupt(_) => "文件损坏或结构不完整".into(),
            IconError::Encrypted(_) => "文件已加密，无法读取图标".into(),
            IconError::TooLarge(_) => "图标或解压体积过大，已拒绝处理".into(),
            IconError::WriteFailed(_) => "无法写入目标文件（权限不足、只读或磁盘已满）".into(),
            IconError::NeedExternal(d) => format!("需要系统不支持的能力：{d}"),
        }
    }

    /// 日志/排查用的细节（可以含解析器原文）。
    pub fn detail(&self) -> Option<&str> {
        match self {
            IconError::NotFound(d)
            | IconError::Unsupported(d)
            | IconError::Unknown(d)
            | IconError::NoIcon(d)
            | IconError::Corrupt(d)
            | IconError::Encrypted(d)
            | IconError::TooLarge(d)
            | IconError::WriteFailed(d)
            | IconError::NeedExternal(d) => Some(d),
        }
    }

    /// 批量汇总里的短标签。
    pub fn tag(&self) -> &'static str {
        match self {
            IconError::NotFound(_) => "源文件不存在",
            IconError::Unsupported(_) => "格式不支持",
            IconError::Unknown(_) => "类型未知",
            IconError::NoIcon(_) => "没有图标",
            IconError::Corrupt(_) => "文件损坏",
            IconError::Encrypted(_) => "已加密",
            IconError::TooLarge(_) => "体积过大",
            IconError::WriteFailed(_) => "写入失败",
            IconError::NeedExternal(_) => "缺少能力",
        }
    }
}

/// `io::Error` 折成分类，顺手带系统原文。
pub fn io_error(e: &std::io::Error, what: &Path) -> IconError {
    let detail = format!("{}: {}", what.display(), e);
    match e.kind() {
        std::io::ErrorKind::NotFound => IconError::NotFound(detail),
        std::io::ErrorKind::PermissionDenied => IconError::WriteFailed(detail),
        _ => {
            if matches!(e.raw_os_error(), Some(112) | Some(39)) {
                IconError::WriteFailed(format!("磁盘已满：{detail}"))
            } else {
                IconError::WriteFailed(detail)
            }
        }
    }
}
