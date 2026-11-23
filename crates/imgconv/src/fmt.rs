//! 目标格式表：一份枚举同时管扩展名、菜单文案、格式上限。
//!
//! 参考 Filee 的 `ImageEncoder.Writable` / `MaxSide`：**能读的不等于能写的**，
//! 每个格式还各有自己的物理上限（JPEG 65500、WebP 16383），超了编码器直接失败。
//! 这里把这些约束集中在一处，转换主流程只查表。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Jpg,
    Png,
    WebP,
    Bmp,
    Gif,
    Tiff,
    Ico,
}

/// 右键菜单里的顺序（也是子菜单顺序）：常用的排前面。
pub const ALL: [Target; 7] = [
    Target::Png,
    Target::Jpg,
    Target::WebP,
    Target::Gif,
    Target::Bmp,
    Target::Tiff,
    Target::Ico,
];

impl Target {
    pub fn parse(s: &str) -> Option<Target> {
        match s.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "jpe" => Some(Target::Jpg),
            "png" => Some(Target::Png),
            "webp" => Some(Target::WebP),
            "bmp" => Some(Target::Bmp),
            "gif" => Some(Target::Gif),
            "tif" | "tiff" => Some(Target::Tiff),
            "ico" => Some(Target::Ico),
            _ => None,
        }
    }

    /// 命令行参数用（`--img-convert png`）。
    pub fn id(self) -> &'static str {
        match self {
            Target::Jpg => "jpg",
            Target::Png => "png",
            Target::WebP => "webp",
            Target::Bmp => "bmp",
            Target::Gif => "gif",
            Target::Tiff => "tiff",
            Target::Ico => "ico",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Target::Jpg => "jpg",
            Target::Png => "png",
            Target::WebP => "webp",
            Target::Bmp => "bmp",
            Target::Gif => "gif",
            Target::Tiff => "tiff",
            Target::Ico => "ico",
        }
    }

    /// 菜单文案。
    pub fn label(self) -> &'static str {
        match self {
            Target::Jpg => "JPG",
            Target::Png => "PNG",
            Target::WebP => "WebP",
            Target::Bmp => "BMP",
            Target::Gif => "GIF",
            Target::Tiff => "TIFF",
            Target::Ico => "ICO",
        }
    }

    /// 这个格式能存 alpha 吗？不能的（JPG/GIF/BMP）在编码前要把透明压到背景上，
    /// 否则 GDI+ 会把透明像素写成黑块。
    pub fn keeps_alpha(self) -> bool {
        !matches!(self, Target::Jpg | Target::Gif | Target::Bmp)
    }

    /// 格式的物理边上限（0 = 不管）。数值取自 Filee `MaxSide`：
    /// JPEG 65535（留 35px 余量给编码器取整）、WebP 16383。
    pub fn max_side(self) -> u32 {
        match self {
            Target::Jpg => 65_500,
            Target::WebP => 16_383,
            _ => 0,
        }
    }
}

/// 判断两个扩展名是不是同一种格式（`jpg` 与 `jpeg` 视为同一种）。
pub fn same_format(ext_a: &str, t: Target) -> bool {
    match Target::parse(ext_a) {
        Some(t2) => t2 == t,
        None => false,
    }
}

/// GDI+ 能解码的输入扩展名。
///
/// 故意只列真能读的：HEIC/AVIF/JXL 在系统装了对应解码器时 GDI+ 也读不了，
/// 与其让它抛一个含糊的 `GenericError`，不如在入口就告诉用户"这个格式不支持"。
/// WebP 不在表里——它走 libwebp 那条路；SVG 走 resvg（见 `decode::load`）。
pub fn gdiplus_reads(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "gif" | "bmp" | "dib" | "tif" | "tiff" | "ico" | "webp"
    )
}

/// 支持作为输入的全部扩展名（含 WebP / SVG）。用于命令行/菜单的预检查。
pub fn is_input(ext: &str) -> bool {
    ext == "webp" || ext == "svg" || ext == "svgz" || gdiplus_reads(ext)
}

/// 矢量输入（要跑 resvg，尺寸策略与位图不同）。
pub fn is_svg(ext: &str) -> bool {
    ext == "svg" || ext == "svgz"
}
