//! 剪贴板记录模型。
//!
//! 一条记录的生命周期里时间字段有三个，别混用：
//! - `created_at` 首次采集（此后不变）
//! - `sort_at` 列表排序键（再次复制、粘贴后置顶会刷新）
//! - `last_used_at` 最近一次显式使用（粘贴 / 打开详情）
//!
//! `use_count` 统计被复制或粘贴的次数；`pinned_order` 是收藏区内的手工顺序，
//! 数值越大越靠前。

use serde::{Deserialize, Serialize};

/// 内容类型。决定列表图标、预览规则和能否套用文本清理规则。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Text,
    Code,
    Url,
    RichText,
    Image,
    File,
    Video,
}

impl Kind {
    /// 一个类型的四个静态属性：序列化名、中文名、角标、是否算「文本」。
    ///
    /// 放在一张表里而不是四个 `match`，是为了加类型时只改一处、不会漏掉某个方法。
    fn spec(self) -> (&'static str, &'static str, &'static str, bool) {
        match self {
            Kind::Text => ("text", "文本", "T", true),
            Kind::Code => ("code", "代码", "</>", true),
            Kind::Url => ("url", "链接", "↗", true),
            Kind::RichText => ("rich_text", "富文本", "¶", true),
            Kind::Image => ("image", "图片", "▣", false),
            Kind::File => ("file", "文件", "F", false),
            Kind::Video => ("video", "视频", "▶", false),
        }
    }

    pub fn as_str(self) -> &'static str {
        self.spec().0
    }

    pub fn label(self) -> &'static str {
        self.spec().1
    }

    pub fn badge(self) -> &'static str {
        self.spec().2
    }

    /// 文本类内容才适用清理规则与脱敏预览。
    pub fn is_text(self) -> bool {
        self.spec().3
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Entry {
    pub id: i64,
    pub kind: Kind,
    /// 文本正文；图片 / 文件 / 视频存磁盘路径，多文件按行分隔。
    pub content: String,
    /// 富文本的 HTML 片段。内联图片以 HTML 注释形式的占位标记出现，
    /// 写回剪贴板时再还原成真正的 `<img>`。
    pub html: Option<String>,
    pub source_app: String,
    pub source_path: Option<String>,
    pub created_at: i64,
    pub last_used_at: i64,
    pub sort_at: i64,
    pub preview: String,
    pub pinned: bool,
    pub pinned_order: i64,
    pub tags: Vec<String>,
    pub note: String,
    pub use_count: i32,
    /// 去重指纹（文本按归一化后的字节，图片按原始字节）。
    pub hash: u64,
    /// 图片格式（png/gif/jpg/webp），写回剪贴板时需要。
    pub ext: Option<String>,
    /// 这一条绑定的全局快捷键，空串表示没设。
    pub hotkey: String,
    /// 「密钥」分类标记。只影响列表归类与显示，粘贴时写进剪贴板的仍然只有 `content`。
    pub secret: bool,
    /// 密钥的用途标题（列表与悬浮卡显示，同样不会进剪贴板）。
    pub title: String,
}

/// 取路径的最后一段；拿不到就用回退值。
fn file_name_or(path: &str, fallback: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| fallback.to_string())
}

impl Entry {
    /// 内容指向磁盘上的文件（而不是内联 data: 或纯文本）。
    pub fn is_external(&self) -> bool {
        matches!(self.kind, Kind::Image | Kind::File | Kind::Video)
            && !self.content.starts_with("data:")
    }

    /// 多文件记录拆成路径列表，空行忽略。
    pub fn file_paths(&self) -> Vec<String> {
        self.content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// 列表里显示的一行概要。
    pub fn display_line(&self) -> String {
        match self.kind {
            Kind::Image => {
                if self.content.starts_with("data:") {
                    "[图片]".to_string()
                } else {
                    file_name_or(&self.content, "[图片]")
                }
            }
            Kind::File | Kind::Video => {
                let paths = self.file_paths();
                match paths.len() {
                    0 => self.preview.clone(),
                    1 => file_name_or(&paths[0], &self.preview),
                    n => format!("{} 等 {n} 项", file_name_or(&paths[0], &self.preview)),
                }
            }
            _ => self.preview.clone(),
        }
    }

    /// 密钥标题；没填时给占位，避免列表出现空行。
    pub fn secret_title(&self) -> String {
        let t = self.title.trim();
        if t.is_empty() {
            "未命名密钥".to_string()
        } else {
            t.to_string()
        }
    }

    /// 密钥的遮蔽显示：只暴露长度，绝不出现原文。
    pub fn masked_content(&self) -> String {
        let n = self.content.chars().count();
        // 圆点数量限制在 6–16：太短看不出「有内容」，太长会把行撑开。
        let dots = n.clamp(6, 16);
        format!("{}  （{n} 个字符）", "•".repeat(dots))
    }
}

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 列表时间列："刚刚" / "3 分钟前" / "昨天" / "3月5日" / "2023-03-05"。
pub fn relative_time(ts_ms: i64) -> String {
    if ts_ms <= 0 {
        return String::new();
    }
    let now = now_ms();
    let secs = (now - ts_ms).max(0) / 1000;
    match secs {
        s if s < 60 => "刚刚".to_string(),
        s if s < 3_600 => format!("{} 分钟前", s / 60),
        s if s < 86_400 => format!("{} 小时前", s / 3_600),
        s if s < 172_800 => "昨天".to_string(),
        s if s < 604_800 => format!("{} 天前", s / 86_400),
        _ => {
            let (y, m, d) = civil_from_ms(ts_ms);
            let (this_year, _, _) = civil_from_ms(now);
            if y == this_year {
                format!("{m}月{d}日")
            } else {
                format!("{y}-{m:02}-{d:02}")
            }
        }
    }
}

fn is_leap_year(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_year(y: i64) -> i64 {
    if is_leap_year(y) {
        366
    } else {
        365
    }
}

/// 毫秒时间戳 → (年, 月, 日)，按 UTC+8 的本地日历。
///
/// 从 1970 起按整年推进：调用点只有列表的时间列，量级几十年，逐年后移比
/// 位运算版本更好读，也不会在闰年边界上算错。
pub fn civil_from_ms(ms: i64) -> (i32, u32, u32) {
    let local = ms / 1000 + 8 * 3_600;
    let mut days = local.div_euclid(86_400);
    let mut year = 1970i64;

    while days >= days_in_year(year) {
        days -= days_in_year(year);
        year += 1;
    }
    // 1970 之前的时间戳：往回退，保证 days 落在 [0, 当年天数)。
    while days < 0 {
        year -= 1;
        days += days_in_year(year);
    }

    const LAYOUT: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0usize;
    while month < 11 {
        let len = if month == 1 && is_leap_year(year) {
            29
        } else {
            LAYOUT[month]
        };
        if days < len as i64 {
            break;
        }
        days -= len as i64;
        month += 1;
    }
    (year as i32, month as u32 + 1, days as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_metadata_is_consistent() {
        assert_eq!(Kind::RichText.as_str(), "rich_text");
        assert_eq!(Kind::Code.label(), "代码");
        assert!(Kind::RichText.is_text());
        assert!(!Kind::Video.is_text());
    }

    #[test]
    fn civil_handles_leap_day_and_year_end() {
        assert_eq!(civil_from_ms(0), (1970, 1, 1));
        assert_eq!(civil_from_ms(951_782_400_000), (2000, 2, 29));
        assert_eq!(civil_from_ms(946_656_000_000), (2000, 1, 1));
        assert_eq!(civil_from_ms(1_764_460_799_000), (2025, 11, 30));
    }

    #[test]
    fn display_line_prefers_file_names() {
        let e = Entry {
            kind: Kind::File,
            content: "C:/a/报告.pdf\nC:/a/2.pdf\n".into(),
            ..Default::default()
        };
        assert_eq!(e.display_line(), "报告.pdf 等 2 项");
        assert_eq!(e.file_paths().len(), 2);
    }

    #[test]
    fn masked_content_hides_original() {
        let e = Entry { content: "abc".into(), ..Default::default() };
        assert_eq!(e.masked_content(), "••••••  （3 个字符）");
        assert!(!e.masked_content().contains("abc"));
    }
}
