//! 美化模板：背景、边框、阴影、水印与画布布局。
//!
//! 输出画布由四层组成：
//!
//! ```text
//! 背景（渐变/纯色/模糊/透明） + 投影 + 边框（标题栏/内边距） + 图片 + 水印 + 标注
//! ```
//!
//! 边框的圆角与图片圆角同心；内容在画布中做轻微光学上抬，避免下方阴影显得过重。
//! 水印支持边距、图上、平铺三种位置，颜色可自动按底图亮度选择。

use std::sync::{Mutex, OnceLock};

use zx_core::CaptureStyleCfg;
use zx_ui::Canvas;

use crate::annotate::{self, Anno};

// ---------------------------------------------------------------- 背景

/// 径向色斑：中心与半径都按画布比例定义。
#[derive(Clone, Copy)]
pub struct Blob {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub color: u32,
}

pub struct Gradient {
    pub id: &'static str,
    pub name: &'static str,
    pub angle: f32,
    pub stops: &'static [u32],
    pub blobs: &'static [Blob],
    pub light: bool,
}

const fn blob(x: f32, y: f32, r: f32, color: u32) -> Blob {
    Blob { x, y, r, color }
}

/// 12 组渐变背景。
pub static GRADIENTS: [Gradient; 12] = [
    Gradient {
        id: "sunset",
        name: "落日",
        angle: 135.0,
        light: false,
        stops: &[0xFFFA709A, 0xFFFEE140],
        blobs: &[blob(0.12, 0.10, 0.55, 0x38FFFFFF), blob(0.92, 0.95, 0.50, 0x47FF7850)],
    },
    Gradient {
        id: "aurora",
        name: "极光",
        angle: 160.0,
        light: false,
        stops: &[0xFF0B1A2F, 0xFF123B52, 0xFF0F2027],
        blobs: &[
            blob(0.20, 0.15, 0.55, 0x5738F9D7),
            blob(0.85, 0.25, 0.50, 0x5C7AA2FF),
            blob(0.55, 0.95, 0.55, 0x42D696FF),
        ],
    },
    Gradient {
        id: "ocean",
        name: "海洋",
        angle: 135.0,
        light: false,
        stops: &[0xFF4FACFE, 0xFF00F2FE],
        blobs: &[blob(0.90, 0.10, 0.50, 0x33FFFFFF), blob(0.10, 0.90, 0.55, 0x4D2C5AFF)],
    },
    Gradient {
        id: "grape",
        name: "葡萄",
        angle: 135.0,
        light: false,
        stops: &[0xFF667EEA, 0xFF764BA2],
        blobs: &[blob(0.85, 0.15, 0.50, 0x4DFF8CDC), blob(0.10, 0.85, 0.50, 0x3850C8FF)],
    },
    Gradient {
        id: "peach",
        name: "蜜桃",
        angle: 135.0,
        light: true,
        stops: &[0xFFFFECD2, 0xFFFCB69F],
        blobs: &[blob(0.90, 0.90, 0.55, 0x52FF8A80), blob(0.10, 0.10, 0.45, 0x73FFFFFF)],
    },
    Gradient {
        id: "mint",
        name: "薄荷",
        angle: 135.0,
        light: true,
        stops: &[0xFF43E97B, 0xFF38F9D7],
        blobs: &[blob(0.90, 0.90, 0.55, 0x3D1E78FF), blob(0.10, 0.10, 0.45, 0x4DFFFFFF)],
    },
    Gradient {
        id: "lavender",
        name: "薰衣草",
        angle: 135.0,
        light: true,
        stops: &[0xFFA18CD1, 0xFFFBC2EB],
        blobs: &[blob(0.15, 0.90, 0.50, 0x42786EFF), blob(0.90, 0.10, 0.45, 0x52FFFFFF)],
    },
    Gradient {
        id: "midnight",
        name: "午夜",
        angle: 145.0,
        light: false,
        stops: &[0xFF30CFD0, 0xFF330867],
        blobs: &[blob(0.85, 0.90, 0.55, 0x3DFF50B4), blob(0.10, 0.10, 0.40, 0x24FFFFFF)],
    },
    Gradient {
        id: "rose",
        name: "玫瑰",
        angle: 135.0,
        light: false,
        stops: &[0xFFF093FB, 0xFFF5576C],
        blobs: &[blob(0.10, 0.10, 0.50, 0x38FFFFFF), blob(0.90, 0.90, 0.50, 0x42FFA03C)],
    },
    Gradient {
        id: "ember",
        name: "余烬",
        angle: 125.0,
        light: false,
        stops: &[0xFFFF9A8B, 0xFFFF6A88, 0xFFFF99AC],
        blobs: &[blob(0.85, 0.10, 0.50, 0x57FFD278), blob(0.10, 0.95, 0.55, 0x38823CFF)],
    },
    Gradient {
        id: "graphite",
        name: "石墨",
        angle: 160.0,
        light: false,
        stops: &[0xFF2B2D33, 0xFF101114],
        blobs: &[blob(0.20, 0.10, 0.60, 0x297896FF), blob(0.90, 0.90, 0.50, 0x0FFFFFFF)],
    },
    Gradient {
        id: "cloud",
        name: "云白",
        angle: 160.0,
        light: true,
        stops: &[0xFFF5F7FA, 0xFFDDE4EE],
        blobs: &[blob(0.15, 0.10, 0.55, 0x4DA0BEFF), blob(0.90, 0.90, 0.50, 0x47FFC8DC)],
    },
];

/// 8 个纯色预设。
pub static SOLIDS: [(&str, u32); 8] = [
    ("深空", 0xFF0F172A),
    ("墨黑", 0xFF111113),
    ("石板", 0xFF1E293B),
    ("靛蓝", 0xFF312E81),
    ("米白", 0xFFF5F5F0),
    ("纯白", 0xFFFFFFFF),
    ("鹅黄", 0xFFFDE68A),
    ("薄荷", 0xFFDCFCE7),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bg {
    Gradient(usize),
    Solid(usize),
    SolidCustom(u32),
    Blur,
    Transparent,
}

impl Bg {
    pub fn key(self) -> String {
        match self {
            Bg::Gradient(i) => {
                format!("gradient:{}", GRADIENTS.get(i).map(|g| g.id).unwrap_or("sunset"))
            }
            Bg::Solid(i) => {
                let c = SOLIDS.get(i).map(|s| s.1).unwrap_or(0xFF000000);
                format!(
                    "solid:#{:02X}{:02X}{:02X}",
                    (c >> 16) & 0xFF,
                    (c >> 8) & 0xFF,
                    c & 0xFF
                )
            }
            Bg::SolidCustom(c) => {
                format!(
                    "solid:#{:02X}{:02X}{:02X}",
                    (c >> 16) & 0xFF,
                    (c >> 8) & 0xFF,
                    c & 0xFF
                )
            }
            Bg::Blur => "blur".into(),
            Bg::Transparent => "transparent".into(),
        }
    }

    fn light(self) -> bool {
        match self {
            Bg::Gradient(i) => GRADIENTS.get(i).map(|g| g.light).unwrap_or(false),
            Bg::Solid(i) => SOLIDS.get(i).map(|s| zx_ui::luma(s.1) > 0.6).unwrap_or(false),
            Bg::SolidCustom(c) => zx_ui::luma(c) > 0.6,
            Bg::Blur | Bg::Transparent => false,
        }
    }
}

fn parse_bg(raw: Option<&str>) -> Option<Bg> {
    let s = raw?.trim();
    if s.eq_ignore_ascii_case("blur") {
        return Some(Bg::Blur);
    }
    if s.eq_ignore_ascii_case("transparent") {
        return Some(Bg::Transparent);
    }

    let (kind, value) = s.split_once(':')?;
    let value = value.trim();
    match kind {
        "solid" => {
            if let Some(hex) = value.strip_prefix('#') {
                if hex.len() == 6 {
                    if let Ok(n) = u32::from_str_radix(hex, 16) {
                        return Some(match SOLIDS.iter().position(|s| s.1 & 0xFFFFFF == n) {
                            Some(i) => Bg::Solid(i),
                            None => Bg::SolidCustom(0xFF00_0000 | n),
                        });
                    }
                }
            }
            value
                .parse::<usize>()
                .ok()
                .filter(|i| *i < SOLIDS.len())
                .map(Bg::Solid)
        }
        "gradient" => {
            if let Some(i) = GRADIENTS.iter().position(|g| g.id == value) {
                return Some(Bg::Gradient(i));
            }
            value
                .parse::<usize>()
                .ok()
                .filter(|i| *i < GRADIENTS.len())
                .map(Bg::Gradient)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- 边框

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Frame {
    None,
    Classic,
    Glass,
    MacosDark,
    MacosLight,
    Browser,
}

impl Frame {
    pub fn name(self) -> &'static str {
        match self {
            Frame::None => "无",
            Frame::Classic => "经典",
            Frame::Glass => "玻璃",
            Frame::MacosDark => "macOS 深",
            Frame::MacosLight => "macOS 浅",
            Frame::Browser => "浏览器",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Frame::None => "none",
            Frame::Classic => "classic",
            Frame::Glass => "glass",
            Frame::MacosDark => "macos-dark",
            Frame::MacosLight => "macos-light",
            Frame::Browser => "browser",
        }
    }

    pub fn parse(s: Option<&str>) -> Option<Frame> {
        Some(match s?.trim() {
            "none" => Frame::None,
            "classic" => Frame::Classic,
            "glass" => Frame::Glass,
            "macos-dark" => Frame::MacosDark,
            "macos-light" => Frame::MacosLight,
            "browser" => Frame::Browser,
            _ => return None,
        })
    }

    pub const ALL: [Frame; 6] = [
        Frame::None,
        Frame::Classic,
        Frame::Glass,
        Frame::MacosDark,
        Frame::MacosLight,
        Frame::Browser,
    ];
}

struct Metrics {
    title_bar: f32,
    inset: f32,
    flush_top: bool,
    min_radius: f32,
}

fn metrics(f: Frame) -> Metrics {
    match f {
        Frame::None => Metrics { title_bar: 0.0, inset: 0.0, flush_top: false, min_radius: 0.0 },
        Frame::Classic => {
            Metrics { title_bar: 30.0, inset: 12.0, flush_top: false, min_radius: 0.0 }
        }
        Frame::Glass => Metrics { title_bar: 0.0, inset: 14.0, flush_top: false, min_radius: 0.0 },
        Frame::MacosDark | Frame::MacosLight => {
            Metrics { title_bar: 28.0, inset: 0.0, flush_top: true, min_radius: 10.0 }
        }
        Frame::Browser => {
            Metrics { title_bar: 40.0, inset: 0.0, flush_top: true, min_radius: 10.0 }
        }
    }
}

// ---------------------------------------------------------------- 比例

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aspect {
    Auto,
    Square,
    FourThree,
    SixteenNine,
    ThreeFour,
    NineSixteen,
}

impl Aspect {
    pub fn ratio(&self) -> Option<f32> {
        Some(match self {
            Aspect::Auto => return None,
            Aspect::Square => 1.0,
            Aspect::FourThree => 4.0 / 3.0,
            Aspect::SixteenNine => 16.0 / 9.0,
            Aspect::ThreeFour => 3.0 / 4.0,
            Aspect::NineSixteen => 9.0 / 16.0,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Aspect::Auto => "自动",
            Aspect::Square => "1:1",
            Aspect::FourThree => "4:3",
            Aspect::SixteenNine => "16:9",
            Aspect::ThreeFour => "3:4",
            Aspect::NineSixteen => "9:16",
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Aspect::Auto => "auto",
            Aspect::Square => "1:1",
            Aspect::FourThree => "4:3",
            Aspect::SixteenNine => "16:9",
            Aspect::ThreeFour => "3:4",
            Aspect::NineSixteen => "9:16",
        }
    }

    pub fn parse(s: Option<&str>) -> Option<Aspect> {
        Some(match s?.trim() {
            "auto" => Aspect::Auto,
            "1:1" => Aspect::Square,
            "4:3" => Aspect::FourThree,
            "16:9" => Aspect::SixteenNine,
            "3:4" => Aspect::ThreeFour,
            "9:16" => Aspect::NineSixteen,
            _ => return None,
        })
    }

    pub const ALL: [Aspect; 6] = [
        Aspect::Auto,
        Aspect::Square,
        Aspect::FourThree,
        Aspect::SixteenNine,
        Aspect::ThreeFour,
        Aspect::NineSixteen,
    ];
}

// ---------------------------------------------------------------- 水印

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WmFont {
    System,
    Serif,
    Geometric,
    Fangsong,
    Script,
    Mono,
}

impl WmFont {
    pub fn name(self) -> &'static str {
        match self {
            WmFont::System => "系统",
            WmFont::Serif => "宋体",
            WmFont::Geometric => "等线",
            WmFont::Fangsong => "仿宋",
            WmFont::Script => "楷体",
            WmFont::Mono => "等宽",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            WmFont::System => "system",
            WmFont::Serif => "serif",
            WmFont::Geometric => "geometric",
            WmFont::Fangsong => "fangsong",
            WmFont::Script => "script",
            WmFont::Mono => "mono",
        }
    }

    fn candidates(self) -> &'static [&'static str] {
        match self {
            WmFont::System => &["Microsoft YaHei UI", "Microsoft YaHei", "SimHei", "Segoe UI"],
            WmFont::Serif => &["SimSun", "NSimSun", "Songti SC", "STSong"],
            WmFont::Geometric => &["DengXian", "Microsoft YaHei UI", "SimHei"],
            WmFont::Fangsong => &["FangSong", "STFangsong", "SimSun"],
            WmFont::Script => &["KaiTi", "STKaiti", "KaiTi_GB2312", "SimSun"],
            WmFont::Mono => &["Consolas", "Cascadia Mono", "Courier New"],
        }
    }

    pub fn family(self) -> &'static str {
        static CACHE: std::sync::OnceLock<[&'static str; 6]> = std::sync::OnceLock::new();
        let table = CACHE.get_or_init(|| {
            let pick = |list: &'static [&'static str]| -> &'static str {
                list.iter()
                    .copied()
                    .find(|n| zx_ui::font_available(n))
                    .unwrap_or(list[0])
            };
            [
                pick(WmFont::System.candidates()),
                pick(WmFont::Serif.candidates()),
                pick(WmFont::Geometric.candidates()),
                pick(WmFont::Fangsong.candidates()),
                pick(WmFont::Script.candidates()),
                pick(WmFont::Mono.candidates()),
            ]
        });
        table[match self {
            WmFont::System => 0,
            WmFont::Serif => 1,
            WmFont::Geometric => 2,
            WmFont::Fangsong => 3,
            WmFont::Script => 4,
            WmFont::Mono => 5,
        }]
    }

    pub fn parse(s: Option<&str>) -> Option<WmFont> {
        Some(match s?.trim() {
            "system" | "default" | "yahei" => WmFont::System,
            "serif" | "song" | "simsun" => WmFont::Serif,
            "geometric" | "sans" | "deng" => WmFont::Geometric,
            "fangsong" | "fangsongti" => WmFont::Fangsong,
            "script" | "kaiti" | "kai" => WmFont::Script,
            "mono" | "consolas" => WmFont::Mono,
            _ => return None,
        })
    }

    pub const ALL: [WmFont; 6] = [
        WmFont::System,
        WmFont::Serif,
        WmFont::Geometric,
        WmFont::Fangsong,
        WmFont::Script,
        WmFont::Mono,
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WmWeight {
    Light,
    Regular,
    Bold,
}

impl WmWeight {
    pub fn name(self) -> &'static str {
        match self {
            WmWeight::Light => "细",
            WmWeight::Regular => "常规",
            WmWeight::Bold => "粗",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            WmWeight::Light => "light",
            WmWeight::Regular => "regular",
            WmWeight::Bold => "bold",
        }
    }

    fn gdi(self) -> i32 {
        match self {
            WmWeight::Light => 300,
            WmWeight::Regular => 500,
            WmWeight::Bold => 700,
        }
    }

    pub fn parse(s: Option<&str>) -> Option<WmWeight> {
        Some(match s?.trim() {
            "light" => WmWeight::Light,
            "regular" => WmWeight::Regular,
            "bold" => WmWeight::Bold,
            _ => return None,
        })
    }

    pub const ALL: [WmWeight; 3] = [WmWeight::Light, WmWeight::Regular, WmWeight::Bold];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WmPlace {
    Margin,
    Image,
    Tile,
}

impl WmPlace {
    pub fn name(self) -> &'static str {
        match self {
            WmPlace::Margin => "边距",
            WmPlace::Image => "图上",
            WmPlace::Tile => "平铺",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            WmPlace::Margin => "margin",
            WmPlace::Image => "image",
            WmPlace::Tile => "tile",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Watermark {
    pub enabled: bool,
    pub place: WmPlace,
    pub text: String,
    pub color: Option<u32>,
    pub opacity: i32,
    pub font: WmFont,
    pub weight: WmWeight,
    pub size: i32,
}

impl Default for Watermark {
    fn default() -> Self {
        Watermark {
            enabled: false,
            place: WmPlace::Margin,
            text: String::new(),
            color: None,
            opacity: 42,
            font: WmFont::System,
            weight: WmWeight::Regular,
            size: 100,
        }
    }
}

pub use crate::annotate::PALETTE as WM_COLORS;

// ---------------------------------------------------------------- 样式

#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub enabled: bool,
    pub bg: Bg,
    pub padding: i32,
    pub radius: i32,
    pub shadow: i32,
    pub frame: Frame,
    pub aspect: Aspect,
    pub wm: Watermark,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            enabled: false,
            bg: Bg::Gradient(0),
            padding: 56,
            radius: 12,
            shadow: 60,
            frame: Frame::Classic,
            aspect: Aspect::Auto,
            wm: Watermark::default(),
        }
    }
}

// ---------------------------------------------------------------- 持久化

static CURRENT: OnceLock<Mutex<Style>> = OnceLock::new();

fn store() -> &'static Mutex<Style> {
    CURRENT.get_or_init(|| Mutex::new(from_cfg(&crate::cfg().style)))
}

pub fn current() -> Style {
    store().lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn reload() {
    let s = from_cfg(&crate::cfg().style);
    *store().lock().unwrap_or_else(|p| p.into_inner()) = s;
}

pub fn set_mem(s: Style) {
    *store().lock().unwrap_or_else(|p| p.into_inner()) = s;
}

pub fn persist() {
    let s = current();
    zx_core::cfg::update_cfg(|c| {
        let cap = c.capture.get_or_insert_with(Default::default);
        cap.style = Some(to_cfg(&s));
    });
}

#[allow(dead_code)]
pub fn set(s: Style) {
    set_mem(s);
    persist();
}

pub fn from_cfg(c: &Option<CaptureStyleCfg>) -> Style {
    let d = Style::default();
    let Some(c) = c else { return d };
    let legacy = c.watermark.clone().unwrap_or_default();
    let text = c.wm_text.clone().unwrap_or(legacy);
    Style {
        enabled: c.enabled.unwrap_or(d.enabled),
        bg: parse_bg(c.background.as_deref()).unwrap_or(d.bg),
        padding: c.padding.unwrap_or(d.padding).clamp(0, 160),
        radius: c.radius.unwrap_or(d.radius).clamp(0, 36),
        shadow: c.shadow.unwrap_or(d.shadow).clamp(0, 100),
        frame: Frame::parse(c.frame.as_deref()).unwrap_or(d.frame),
        aspect: Aspect::parse(c.aspect.as_deref()).unwrap_or(d.aspect),
        wm: Watermark {
            enabled: c.wm_enabled.unwrap_or_else(|| !text.trim().is_empty()),
            place: match c.wm_placement.as_deref() {
                Some("image") => WmPlace::Image,
                Some("tile") => WmPlace::Tile,
                _ => d.wm.place,
            },
            text,
            color: parse_hex_color(c.wm_color.as_deref()),
            opacity: c.wm_opacity.unwrap_or(d.wm.opacity).clamp(24, 80),
            font: WmFont::parse(c.wm_font.as_deref()).unwrap_or(d.wm.font),
            weight: WmWeight::parse(c.wm_weight.as_deref()).unwrap_or(d.wm.weight),
            size: c.wm_size.unwrap_or(d.wm.size).clamp(60, 200),
        },
    }
}

pub fn to_cfg(s: &Style) -> CaptureStyleCfg {
    CaptureStyleCfg {
        enabled: Some(s.enabled),
        background: Some(s.bg.key()),
        padding: Some(s.padding),
        radius: Some(s.radius),
        shadow: Some(s.shadow),
        frame: Some(s.frame.key().to_string()),
        aspect: Some(s.aspect.key().to_string()),
        watermark: Some(s.wm.text.clone()),
        wm_enabled: Some(s.wm.enabled),
        wm_placement: Some(s.wm.place.key().to_string()),
        wm_text: Some(s.wm.text.clone()),
        wm_color: Some(match s.wm.color {
            Some(c) => crate::picker::hex(c),
            None => "auto".into(),
        }),
        wm_opacity: Some(s.wm.opacity),
        wm_font: Some(s.wm.font.key().to_string()),
        wm_weight: Some(s.wm.weight.key().to_string()),
        wm_size: Some(s.wm.size),
    }
}

fn parse_hex_color(s: Option<&str>) -> Option<u32> {
    let s = s?.trim();
    if s.eq_ignore_ascii_case("auto") {
        return None;
    }
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(0xFF00_0000 | n)
}

// ---------------------------------------------------------------- 布局

#[derive(Clone, Copy, Debug, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub unit: f32,
    pub scale: f32,
    pub beautify: bool,
    pub frame: RectF,
    pub frame_radius: f32,
    pub title_bar: f32,
    pub image: RectF,
    pub radii: [f32; 4],
}

const MAX_SIDE: f32 = 8000.0;
const MAX_AREA: f32 = 40_000_000.0;

fn compute_layout(doc_w: i32, doc_h: i32, unit: f32, st: &Style) -> Layout {
    let u = if unit > 0.05 { unit } else { 1.0 };
    let w = doc_w.max(1) as f32;
    let h = doc_h.max(1) as f32;
    if !st.enabled {
        return Layout {
            width: doc_w.max(1),
            height: doc_h.max(1),
            unit: u,
            scale: 1.0,
            beautify: false,
            frame: RectF { x: 0.0, y: 0.0, w, h },
            frame_radius: 0.0,
            title_bar: 0.0,
            image: RectF { x: 0.0, y: 0.0, w, h },
            radii: [0.0; 4],
        };
    }

    let m = metrics(st.frame);
    let pad = st.padding.clamp(0, 160) as f32 * u;
    let radius = st.radius.clamp(0, 36) as f32 * u;
    let inset = m.inset * u;
    let title = m.title_bar * u;
    let frame_radius = (m.min_radius * u).max(radius + inset);

    let mut cw = w + inset * 2.0 + pad * 2.0;
    let mut ch = h + title + inset * 2.0 + pad * 2.0;
    if let Some(ratio) = st.aspect.ratio() {
        if cw / ch < ratio {
            cw = (ch * ratio).round();
        } else {
            ch = (cw / ratio).round();
        }
    }

    let mut k = 1.0f32;
    if cw > MAX_SIDE || ch > MAX_SIDE {
        k = (MAX_SIDE / cw).min(MAX_SIDE / ch);
    }
    if cw * k * ch * k > MAX_AREA {
        k *= (MAX_AREA / (cw * k * ch * k)).sqrt();
    }

    let cw = (cw * k).max(1.0);
    let ch = (ch * k).max(1.0);
    let fw = (w + inset * 2.0) * k;
    let fh = (h + title + inset * 2.0) * k;
    let (radius, inset, title, frame_radius) =
        (radius * k, inset * k, title * k, frame_radius * k);

    let fx = ((cw - fw) / 2.0).round();
    let lift = ((ch - fh) / 2.0 * 0.2).min(14.0 * u)
        * (st.shadow.clamp(0, 100) as f32 / 100.0);
    let fy = ((ch - fh) / 2.0 - lift).max(0.0).round();

    let bottom = if m.flush_top { frame_radius } else { radius };
    let top = if m.flush_top { 0.0 } else { radius };
    Layout {
        width: cw.round() as i32,
        height: ch.round() as i32,
        unit: u,
        scale: k,
        beautify: true,
        frame: RectF { x: fx, y: fy, w: fw, h: fh },
        frame_radius,
        title_bar: title,
        image: RectF { x: fx + inset, y: fy + title + inset, w: w * k, h: h * k },
        radii: [top, top, bottom, bottom],
    }
}

// ---------------------------------------------------------------- 渲染

pub fn render(doc: &Canvas, annos: &[Anno], st: &Style, unit: f32) -> Canvas {
    let l = compute_layout(doc.w, doc.h, unit, st);
    let mut out = Canvas::new(l.width, l.height, 96);

    if l.beautify {
        paint_background(&mut out, st, doc);
        paint_shadow(&mut out, &l, st.shadow);
        paint_frame(&mut out, st, &l);
    }

    let img = l.image;
    out.draw_canvas_rounded4(
        doc,
        img.x,
        img.y,
        img.w,
        img.h,
        l.radii[0],
        l.radii[1],
        l.radii[2],
        l.radii[3],
    );

    let margin_room = l.height as f32 - (l.frame.y + l.frame.h);
    let in_margin = st.wm.enabled
        && st.wm.place == WmPlace::Margin
        && l.beautify
        && margin_room >= 22.0 * l.unit;
    if st.wm.enabled && !in_margin {
        if st.wm.place == WmPlace::Tile {
            draw_watermark_tile(&mut out, &st.wm, &img, l.unit, l.radii);
        } else {
            draw_watermark_image(&mut out, &st.wm, &img, l.unit);
        }
    }
    annotate::draw_all(&mut out, annos, img.x, img.y, None, l.scale);

    if l.beautify && matches!(st.frame, Frame::Classic | Frame::Glass) {
        out.stroke_round4(
            img.x,
            img.y,
            img.w,
            img.h,
            l.radii[0],
            l.radii[1],
            l.radii[2],
            l.radii[3],
            1.0 * l.unit,
            0x1AFFFFFF,
        );
    }
    if st.wm.enabled && in_margin {
        draw_watermark_margin(&mut out, &st.wm, &l);
    }
    out
}

pub fn make_swatch(bg: Bg, w: i32, h: i32) -> Canvas {
    let mut c = Canvas::new(w.max(2), h.max(2), 96);
    match bg {
        Bg::Gradient(i) => {
            let g = &GRADIENTS[i.min(GRADIENTS.len() - 1)];
            paint_gradient(&mut c, g);
        }
        Bg::Solid(i) => {
            let col = SOLIDS.get(i).map(|s| s.1).unwrap_or(0xFF000000);
            c.fill_rect(0.0, 0.0, w as f32, h as f32, col);
        }
        Bg::SolidCustom(col) => c.fill_rect(0.0, 0.0, w as f32, h as f32, col),
        Bg::Blur => {
            c.fill_rect(0.0, 0.0, w as f32, h as f32, 0xFF3A3B40);
            c.gradient_rect(0.0, 0.0, w as f32, h as f32, 0xFF6B7280, 0xFF1F2937, 45.0);
        }
        Bg::Transparent => {}
    }
    c
}

// ---------------- 背景 ----------------

fn rgb(c: u32) -> (f32, f32, f32) {
    (((c >> 16) & 0xFF) as f32, ((c >> 8) & 0xFF) as f32, (c & 0xFF) as f32)
}

fn hash_noise(x: usize, y: usize) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h >> 8) & 0xFF) as f32 / 255.0 - 0.5
}

fn sample_stops(stops: &[u32], t: f32) -> (f32, f32, f32) {
    match stops.len() {
        0 => (0.0, 0.0, 0.0),
        1 => rgb(stops[0]),
        n => {
            let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (pos.floor() as usize).min(n - 2);
            let f = pos - i as f32;
            let a = rgb(stops[i]);
            let b = rgb(stops[i + 1]);
            (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
        }
    }
}

/// 线性渐变 + 径向色斑 + 颗粒，写入 BGRA 缓冲。
fn gradient_into(px: &mut [u8], stride: usize, w: usize, h: usize, g: &Gradient, grain: bool) {
    if px.len() < stride * h {
        return;
    }
    let (wf, hf) = (w as f32, h as f32);
    let rad = (g.angle - 90.0).to_radians();
    let (cos, sin) = (rad.cos(), rad.sin());
    let half = ((wf * cos).abs() + (hf * sin).abs()) / 2.0;
    let (cx, cy) = (wf / 2.0, hf / 2.0);
    let (x0, y0) = (cx - cos * half, cy - sin * half);
    let denom = (2.0 * half).max(1.0);
    let diag = wf.hypot(hf);
    let blobs: Vec<(f32, f32, f32, f32, f32, f32, f32)> = g
        .blobs
        .iter()
        .map(|b| {
            (
                b.x * wf,
                b.y * hf,
                (b.r * diag).max(1.0),
                ((b.color >> 16) & 0xFF) as f32,
                ((b.color >> 8) & 0xFF) as f32,
                (b.color & 0xFF) as f32,
                ((b.color >> 24) & 0xFF) as f32 / 255.0,
            )
        })
        .collect();

    for y in 0..h {
        for x in 0..w {
            let t = ((x as f32 + 0.5 - x0) * cos + (y as f32 + 0.5 - y0) * sin) / denom;
            let (mut r, mut gg, mut b) = sample_stops(g.stops, t);
            for (bx, by, br, cr, cg, cb, ca) in &blobs {
                let dx = x as f32 - bx;
                let dy = y as f32 - by;
                let d2 = dx * dx + dy * dy;
                if d2 >= br * br {
                    continue;
                }
                let a = ca * (1.0 - d2.sqrt() / br);
                r += (cr - r) * a;
                gg += (cg - gg) * a;
                b += (cb - b) * a;
            }
            let n = if grain { hash_noise(x, y) * 0.05 * 255.0 } else { 0.0 };
            let o = y * stride + x * 4;
            px[o] = (b + n).clamp(0.0, 255.0) as u8;
            px[o + 1] = (gg + n).clamp(0.0, 255.0) as u8;
            px[o + 2] = (r + n).clamp(0.0, 255.0) as u8;
            px[o + 3] = 255;
        }
    }
}

/// 大画布先低分辨率算渐变再放大，避免长截图逐像素开方。
fn paint_gradient(canvas: &mut Canvas, g: &Gradient) {
    let (w, h) = (canvas.w.max(1) as usize, canvas.h.max(1) as usize);
    let area = (w * h) as f32;
    const TARGET: f32 = 1_200_000.0;
    if area <= TARGET {
        let stride = canvas.stride();
        gradient_into(canvas.pixels_mut(), stride, w, h, g, true);
        return;
    }
    let k = (TARGET / area).sqrt().max(0.05);
    let sw = ((w as f32 * k) as usize).max(2);
    let sh = ((h as f32 * k) as usize).max(2);
    let mut small = Canvas::new(sw as i32, sh as i32, 96);
    let stride = small.stride();
    gradient_into(small.pixels_mut(), stride, sw, sh, g, false);
    canvas.draw_canvas(&small, 0.0, 0.0, w as f32, h as f32);
}

fn paint_solid(canvas: &mut Canvas, color: u32) {
    let (w, h, stride) = (canvas.w.max(1) as usize, canvas.h.max(1) as usize, canvas.stride());
    let (r, g, b) = rgb(color);
    let px = canvas.pixels_mut();
    for y in 0..h {
        for x in 0..w {
            let n = hash_noise(x, y) * 0.025 * 255.0;
            let o = y * stride + x * 4;
            px[o] = (b + n).clamp(0.0, 255.0) as u8;
            px[o + 1] = (g + n).clamp(0.0, 255.0) as u8;
            px[o + 2] = (r + n).clamp(0.0, 255.0) as u8;
            px[o + 3] = 255;
        }
    }
}

fn paint_background(canvas: &mut Canvas, st: &Style, doc: &Canvas) {
    match st.bg {
        Bg::Transparent => {}
        Bg::Solid(i) => paint_solid(canvas, SOLIDS.get(i).map(|s| s.1).unwrap_or(0xFF000000)),
        Bg::SolidCustom(c) => paint_solid(canvas, c),
        Bg::Gradient(i) => paint_gradient(canvas, &GRADIENTS[i.min(GRADIENTS.len() - 1)]),
        Bg::Blur => {
            let k = 72.0 / doc.w.max(doc.h).max(1) as f32;
            let tw = ((doc.w as f32 * k).round() as i32).max(8);
            let th = ((doc.h as f32 * k).round() as i32).max(8);
            let mut small = Canvas::new(tw, th, 96);
            small.draw_canvas(doc, 0.0, 0.0, doc.w as f32, doc.h as f32);

            let (cw, ch) = (canvas.w as f32, canvas.h as f32);
            let scale = (cw / tw as f32).max(ch / th as f32);
            let (dw, dh) = (tw as f32 * scale, th as f32 * scale);
            canvas.draw_canvas(&small, (cw - dw) / 2.0, (ch - dh) / 2.0, dw, dh);
            canvas.fill_rect(0.0, 0.0, cw, ch, 0x14000000);

            let (w, h, stride) = (canvas.w.max(1) as usize, canvas.h.max(1) as usize, canvas.stride());
            let px = canvas.pixels_mut();
            for y in 0..h {
                for x in 0..w {
                    let o = y * stride + x * 4;
                    let n = hash_noise(x, y) * 0.05 * 255.0;
                    for c in 0..3 {
                        px[o + c] = (px[o + c] as f32 + n).clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
}

// ---------------- 阴影 ----------------

fn paint_shadow(canvas: &mut Canvas, l: &Layout, strength: i32) {
    if strength <= 0 {
        return;
    }
    if canvas.w as f32 * canvas.h as f32 > 1_500_000.0 {
        let s = 0.25;
        let sw = ((canvas.w as f32 * s) as i32).max(8);
        let sh = ((canvas.h as f32 * s) as i32).max(8);
        let mut small = Canvas::new(sw, sh, 96);
        shadow_into(&mut small, l, strength, s);
        canvas.draw_canvas(&small, 0.0, 0.0, canvas.w as f32, canvas.h as f32);
        return;
    }
    shadow_into(canvas, l, strength, 1.0);
}

fn shadow_into(canvas: &mut Canvas, l: &Layout, strength: i32, s: f32) {
    let k = strength.clamp(0, 100) as f32 / 100.0;
    let u = l.unit * l.scale * s;
    let f = RectF {
        x: l.frame.x * s,
        y: l.frame.y * s,
        w: l.frame.w * s,
        h: l.frame.h * s,
    };
    let base_radius = l.frame_radius * s;

    // 接触阴影 / 主阴影 / 环境扩散。
    let layers = [
        (0.22, 3.0 * u, 1.0 * u),
        (0.20, 18.0 * u, 8.0 * u),
        (0.32, 64.0 * u, 30.0 * u),
    ];
    for (alpha, blur, dy) in layers {
        let steps = 14;
        for i in (1..=steps).rev() {
            let t = i as f32 / steps as f32;
            let spread = blur * t;
            let a = (alpha * k * 2.0 / steps as f32 * (1.0 - t)).clamp(0.0, 1.0);
            if a < 0.004 {
                continue;
            }
            canvas.fill_round(
                f.x - spread,
                f.y - spread + dy,
                f.w + spread * 2.0,
                f.h + spread * 2.0,
                base_radius + spread,
                ((a * 255.0) as u32) << 24,
            );
        }
    }
}

// ---------------- 边框 ----------------

const TRAFFIC: [u32; 3] = [0xFFFF5F57, 0xFFFEBC2E, 0xFF28C840];

fn traffic_lights(canvas: &mut Canvas, start_x: f32, cy: f32, unit: f32) {
    let r = 6.0 * unit;
    let gap = 20.0 * unit;
    for (i, color) in TRAFFIC.iter().enumerate() {
        let cx = start_x + i as f32 * gap;
        canvas.fill_ellipse(cx - r, cy - r, r * 2.0, r * 2.0, *color);
        canvas.stroke_ellipse(cx - r, cy - r, r * 2.0, r * 2.0, 0.75 * unit, 0x24000000);
    }
}

fn paint_frame(canvas: &mut Canvas, st: &Style, l: &Layout) {
    let f = l.frame;
    let u = l.unit * l.scale;
    let r = l.frame_radius;
    match st.frame {
        Frame::None => {}
        Frame::Classic => {
            canvas.gradient_rect(f.x, f.y, f.w, f.h, 0xE61E2027, 0xEB101116, 90.0);
            canvas.clip_round4(f.x, f.y, f.w, f.h, r, r, r, r);
            canvas.gradient_rect(
                f.x,
                f.y,
                f.w,
                (l.title_bar * 1.4).max(1.0),
                0x17FFFFFF,
                0x00FFFFFF,
                90.0,
            );
            canvas.reset_clip();

            let inset = l.image.x - f.x;
            traffic_lights(canvas, l.image.x + 10.0 * u, f.y + (l.title_bar + inset) / 2.0, u);
            canvas.stroke_round4(
                f.x + 0.5 * u,
                f.y + 0.5 * u,
                f.w - u,
                f.h - u,
                r,
                r,
                r,
                r,
                1.0 * u,
                0x2EFFFFFF,
            );
        }
        Frame::Glass => {
            canvas.clip_round4(f.x, f.y, f.w, f.h, r, r, r, r);
            canvas.fill_rect(
                f.x,
                f.y,
                f.w,
                f.h,
                if st.bg.light() { 0x57FFFFFF } else { 0x24FFFFFF },
            );
            canvas.gradient_rect(f.x, f.y, f.w, f.h, 0x29FFFFFF, 0x14FFFFFF, 45.0);
            canvas.reset_clip();
            canvas.stroke_round4(
                f.x + 0.6 * u,
                f.y + 0.6 * u,
                f.w - 1.2 * u,
                f.h - 1.2 * u,
                r,
                r,
                r,
                r,
                1.2 * u,
                0x7AFFFFFF,
            );
        }
        Frame::MacosDark => window_frame(canvas, l, true, false),
        Frame::MacosLight => window_frame(canvas, l, false, false),
        Frame::Browser => window_frame(canvas, l, true, true),
    }
}

fn window_frame(canvas: &mut Canvas, l: &Layout, dark: bool, browser: bool) {
    let f = l.frame;
    let u = l.unit * l.scale;
    let r = l.frame_radius;
    let bar = l.title_bar;
    let (body, bar_top, bar_bot, sep, border, control, field) = if dark {
        (0xFF1E1F24, 0xFF35363C, 0xFF2A2B30, 0x8C000000, 0x29FFFFFF, 0x66000000, 0x14FFFFFF)
    } else {
        (0xFFFFFFFF, 0xFFF1F1F3, 0xFFE6E6E9, 0x1F000000, 0x29000000, 0x61000000, 0x0F000000)
    };

    canvas.fill_round4(f.x, f.y, f.w, f.h, r, r, r, r, body);
    canvas.clip_round4(f.x, f.y, f.w, f.h, r, r, r, r);
    canvas.gradient_rect(f.x, f.y, f.w, bar.max(1.0), bar_top, bar_bot, 90.0);
    canvas.fill_rect(f.x, f.y + bar - u, f.w, u, sep);
    canvas.reset_clip();

    let lights_x = f.x + 19.0 * u;
    traffic_lights(canvas, lights_x, f.y + bar / 2.0, u);

    if browser {
        let cy = f.y + bar / 2.0;
        let chevron_x = lights_x + 64.0 * u;
        for (i, dir) in [-1.0f32, 1.0].iter().enumerate() {
            let x = chevron_x + i as f32 * 22.0 * u;
            canvas.polyline(
                &[
                    (x - dir * 3.0 * u, cy - 5.0 * u),
                    (x + dir * 3.0 * u, cy),
                    (x - dir * 3.0 * u, cy + 5.0 * u),
                ],
                1.6 * u,
                control,
            );
        }
        let available = f.w - 2.0 * (chevron_x + 40.0 * u - f.x);
        let field_w = (380.0 * u).min(f.w * 0.36).min(available);
        if field_w > 80.0 * u {
            let fh = 24.0 * u;
            let fx = f.x + (f.w - field_w) / 2.0;
            let fy = cy - fh / 2.0;
            canvas.fill_round4(fx, fy, field_w, fh, 7.0 * u, 7.0 * u, 7.0 * u, 7.0 * u, field);
            let lock_x = fx + 12.0 * u;
            canvas.fill_round4(
                lock_x,
                cy - 1.0 * u,
                8.0 * u,
                6.5 * u,
                1.5 * u,
                1.5 * u,
                1.5 * u,
                1.5 * u,
                control,
            );
            canvas.stroke_ellipse(lock_x + 1.4 * u, cy - 3.6 * u, 5.2 * u, 5.2 * u, 1.2 * u, control);
        }
    }

    canvas.stroke_round4(
        f.x + 0.5 * u,
        f.y + 0.5 * u,
        f.w - u,
        f.h - u,
        r,
        r,
        r,
        r,
        1.0 * u,
        border,
    );
}

// ---------------- 水印 ----------------

fn sample_luma(canvas: &Canvas, x: f32, y: f32, w: f32, h: f32) -> Option<f32> {
    let px = canvas.pixels();
    let stride = canvas.stride();
    let x0 = x.max(0.0) as usize;
    let y0 = y.max(0.0) as usize;
    let x1 = ((x + w).max(0.0) as usize).min(canvas.w.max(1) as usize);
    let y1 = ((y + h).max(0.0) as usize).min(canvas.h.max(1) as usize);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let step_x = ((x1 - x0) / 40).max(1);
    let step_y = ((y1 - y0) / 12).max(1);
    let (mut total, mut n) = (0f32, 0u32);
    let mut yy = y0;
    while yy < y1 {
        let mut xx = x0;
        while xx < x1 {
            let o = yy * stride + xx * 4;
            let (b, g, r) = (px[o] as f32, px[o + 1] as f32, px[o + 2] as f32);
            total += (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
            n += 1;
            xx += step_x;
        }
        yy += step_y;
    }
    if n == 0 { None } else { Some(total / n as f32) }
}

fn sample_avg(canvas: &Canvas, x: f32, y: f32, w: f32, h: f32) -> Option<(f32, f32, f32)> {
    let px = canvas.pixels();
    let stride = canvas.stride();
    let x0 = x.max(0.0) as usize;
    let y0 = y.max(0.0) as usize;
    let x1 = ((x + w).max(0.0) as usize).min(canvas.w.max(1) as usize);
    let y1 = ((y + h).max(0.0) as usize).min(canvas.h.max(1) as usize);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let step_x = ((x1 - x0) / 40).max(1);
    let step_y = ((y1 - y0) / 12).max(1);
    let (mut r, mut g, mut b, mut n) = (0f32, 0f32, 0f32, 0u32);
    let mut yy = y0;
    while yy < y1 {
        let mut xx = x0;
        while xx < x1 {
            let o = yy * stride + xx * 4;
            b += px[o] as f32;
            g += px[o + 1] as f32;
            r += px[o + 2] as f32;
            n += 1;
            xx += step_x;
        }
        yy += step_y;
    }
    if n == 0 { None } else { Some((r / n as f32, g / n as f32, b / n as f32)) }
}

fn tone_on_tone(r: f32, g: f32, b: f32) -> u32 {
    let l = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
    let target = if l >= 0.5 { 0.13 } else { 0.88 };
    let k = if l <= 0.02 { 1.0 } else { target / l };
    let f = |c: f32| (c * k).clamp(0.0, 255.0) as u32;
    0xFF00_0000 | (f(r) << 16) | (f(g) << 8) | f(b)
}

fn fit_text(
    canvas: &mut Canvas,
    text: &str,
    size: f32,
    family: &str,
    weight: i32,
    max_w: f32,
) -> String {
    if canvas.measure_named(text, size, family, weight) <= max_w {
        return text.to_string();
    }
    let mut out = text.to_string();
    while out.chars().count() > 1 {
        out.pop();
        let probe = format!("{out}…");
        if canvas.measure_named(&probe, size, family, weight) <= max_w {
            return probe;
        }
    }
    "…".to_string()
}

fn draw_watermark_image(canvas: &mut Canvas, wm: &Watermark, img: &RectF, unit: f32) {
    let text = wm.text.trim();
    if text.is_empty() {
        return;
    }
    let u = unit;
    let size =
        (img.w * 0.024).clamp(12.0 * u, 26.0 * u) * (wm.size.clamp(60, 200) as f32 / 100.0);
    let right = (img.w * 0.032).clamp(12.0 * u, 36.0 * u);
    let bottom = (img.h * 0.04).clamp(10.0 * u, 30.0 * u);
    let family = wm.font.family();
    let weight = wm.weight.gdi();
    let shown = fit_text(canvas, text, size, family, weight, img.w * 0.42);
    let tw = canvas.measure_named(&shown, size, family, weight);
    let anchor_x = img.x + img.w - right;
    let anchor_y = img.y + img.h - bottom;

    let fill = match wm.color {
        Some(c) => c,
        None => match sample_luma(canvas, anchor_x - tw, anchor_y - size, tw, size * 1.2) {
            Some(v) if v >= 0.58 => 0xFF111827,
            _ => 0xFFF8FAFC,
        },
    };
    let alpha = (wm.opacity.clamp(24, 80) as f32 / 100.0 * 255.0) as u32;
    let solid = (fill & 0x00FF_FFFF) | (alpha << 24);
    let outline = if zx_ui::luma(fill) < 0.5 { 0x3D000000 } else { 0x52FFFFFF };
    let outline_argb = (outline & 0x00FF_FFFF) | ((alpha * 82 / 100) << 24);

    let off = (size * 0.06).clamp(0.8, 1.6 * u);
    for (dx, dy) in [(-off, 0.0), (off, 0.0), (0.0, -off), (0.0, off)] {
        canvas.text_named(
            &shown,
            anchor_x - tw + dx - 2.0,
            anchor_y - size + dy - 1.0,
            tw + 6.0,
            size * 1.5,
            size,
            outline_argb,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            family,
            weight,
        );
    }
    canvas.text_named(
        &shown,
        anchor_x - tw - 2.0,
        anchor_y - size - 1.0,
        tw + 6.0,
        size * 1.5,
        size,
        solid,
        zx_ui::TextAlign::Near,
        zx_ui::VAlign::Center,
        family,
        weight,
    );
}

fn draw_watermark_tile(
    canvas: &mut Canvas,
    wm: &Watermark,
    img: &RectF,
    unit: f32,
    radii: [f32; 4],
) {
    let text = wm.text.trim();
    if text.is_empty() || img.w < 40.0 || img.h < 40.0 {
        return;
    }
    let u = unit;
    let size = ((img.w * 0.016).clamp(9.0 * u, 20.0 * u)
        * (wm.size.clamp(60, 200) as f32 / 100.0))
        .max(6.0);
    let family = wm.font.family();
    let weight = wm.weight.gdi();
    let tw = canvas.measure_named(text, size, family, weight);
    if tw <= 0.0 {
        return;
    }
    let fill = match wm.color {
        Some(c) => c,
        None => match sample_avg(canvas, img.x, img.y, img.w, img.h) {
            Some((r, g, b)) => tone_on_tone(r, g, b),
            None => 0xFFFFFFFF,
        },
    };
    let alpha = ((wm.opacity.clamp(24, 80) as f32 / 100.0) * 255.0 * 0.42) as u32;
    let argb = (fill & 0x00FF_FFFF) | (alpha << 24);

    canvas.clip_round4(img.x, img.y, img.w, img.h, radii[0], radii[1], radii[2], radii[3]);
    let step_x = (tw + size * 5.0).max(48.0 * u);
    let step_y = size * 4.6;
    let mut y = img.y + size * 0.6;
    let mut row = 0usize;
    while y + size < img.y + img.h && row < 400 {
        let off = if row % 2 == 1 { step_x / 2.0 } else { 0.0 };
        let mut x = img.x + off - step_x;
        while x < img.x + img.w {
            if x + tw > img.x + 2.0 && x < img.x + img.w - 2.0 {
                canvas.text_named(
                    text,
                    x,
                    y,
                    tw + 8.0,
                    size * 1.6,
                    size,
                    argb,
                    zx_ui::TextAlign::Near,
                    zx_ui::VAlign::Center,
                    family,
                    weight,
                );
            }
            x += step_x;
        }
        y += step_y;
        row += 1;
    }
    canvas.reset_clip();
}

fn draw_watermark_margin(canvas: &mut Canvas, wm: &Watermark, l: &Layout) {
    let text = wm.text.trim();
    if text.is_empty() {
        return;
    }
    let f = l.frame;
    let u = l.unit;
    let bottom = f.y + f.h;
    let margin = (l.height as f32 - bottom).max(0.0);
    let scale = wm.size.clamp(60, 200) as f32 / 100.0;
    let gap = (margin * 0.24).clamp(9.0 * u, 18.0 * u);
    let size = ((f.w * 0.028).clamp(11.0 * u, 14.0 * u) * scale)
        .min(margin * 0.34 * scale)
        .min((margin - gap) * 0.82)
        .max(1.0);
    let family = wm.font.family();
    let weight = wm.weight.gdi();
    let shown = fit_text(canvas, text, size, family, weight, f.w * 0.6);
    let tw = canvas.measure_named(&shown, size, family, weight);
    let right = f.x + f.w;
    let cy = bottom + gap + size / 2.0;

    let (fill, alpha) = match wm.color {
        Some(c) => (c, (wm.opacity + 18).clamp(40, 95)),
        None => {
            let c = sample_avg(canvas, right - tw, cy - size / 2.0, tw, size)
                .map(|(r, g, b)| tone_on_tone(r, g, b))
                .unwrap_or(0xFFFFFFFF);
            (c, ((wm.opacity as f32 / 100.0 + 0.33) * 100.0).clamp(55.0, 92.0) as i32)
        }
    };
    let argb = (fill & 0x00FF_FFFF) | (((alpha as u32) * 255 / 100) << 24);
    canvas.text_named(
        &shown,
        right - tw - 2.0,
        cy - size,
        tw + 6.0,
        size * 2.0,
        size,
        argb,
        zx_ui::TextAlign::Near,
        zx_ui::VAlign::Center,
        family,
        weight,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_solid_round_trips() {
        let bg = parse_bg(Some("solid:#123456")).expect("应解析成自选纯色");
        assert_eq!(bg, Bg::SolidCustom(0xFF12_3456));
        assert_eq!(bg.key(), "solid:#123456");
        // 预设色仍然走预设下标，不会被当成自选色
        assert_eq!(parse_bg(Some("solid:#111113")), Some(Bg::Solid(1)));
    }

    #[test]
    fn tile_placement_survives_config() {
        assert_eq!(WmPlace::Tile.key(), "tile");
        assert_eq!(WmPlace::Tile.name(), "平铺");
        let cfg = Some(crate::style::CaptureStyleCfg {
            wm_placement: Some("tile".into()),
            ..Default::default()
        });
        assert_eq!(from_cfg(&cfg).wm.place, WmPlace::Tile);
    }

    #[test]
    fn auto_keeps_content_size_plus_margins() {
        let doc = Canvas::new(100, 50, 96);
        let style = Style { enabled: true, padding: 10, shadow: 0, frame: Frame::None, ..Default::default() };
        let out = render(&doc, &[], &style, 1.0);
        assert_eq!((out.w, out.h), (120, 70));
    }

    #[test]
    fn tone_on_tone_keeps_contrast() {
        // 橙色背景：结果必须明显比背景暗
        let c = tone_on_tone(255.0, 166.0, 64.0);
        let lum = |c: u32| {
            let f = |v: u32| v as f32 / 255.0;
            0.299 * f((c >> 16) & 0xFF) + 0.587 * f((c >> 8) & 0xFF) + 0.114 * f(c & 0xFF)
        };
        assert!(lum(c) < 0.25, "亮背景上应压到深色，实际 {:.2}", lum(c));
        // 深蓝背景：结果必须明显比背景亮
        let c = tone_on_tone(18.0, 30.0, 70.0);
        assert!(lum(c) > 0.7, "暗背景上应提到亮色，实际 {:.2}", lum(c));
    }

    #[test]
    fn square_aspect_expands_width() {
        let doc = Canvas::new(100, 100, 96);
        let style = Style {
            enabled: true, padding: 0, shadow: 0, frame: Frame::None,
            aspect: Aspect::SixteenNine, ..Default::default()
        };
        let out = render(&doc, &[], &style, 1.0);
        assert!(out.w > out.h);
    }

    #[test]
    fn disabled_style_is_identity() {
        let mut doc = Canvas::new(30, 20, 96);
        doc.clear(0xFF00FF00);
        let out = render(&doc, &[], &Style::default(), 1.0);
        assert_eq!((out.w, out.h), (30, 20));
    }

    #[test]
    fn oversized_canvas_scales_content_not_crop() {
        let doc = Canvas::new(100, 10000, 96);
        let style = Style { enabled: true, padding: 0, shadow: 0, frame: Frame::None, ..Default::default() };
        let out = render(&doc, &[], &style, 1.0);
        assert_eq!((out.w, out.h), (80, 8000));
    }

    #[test]
    fn classic_frame_adds_title_bar_and_inset() {
        let doc = Canvas::new(200, 100, 96);
        let style = Style {
            enabled: true, padding: 0, shadow: 0, radius: 0,
            frame: Frame::Classic, ..Default::default()
        };
        let out = render(&doc, &[], &style, 1.0);
        // 经典边框：标题栏 30 + 上下 inset 12*2
        assert_eq!((out.w, out.h), (200 + 24, 100 + 30 + 24));
    }

    #[test]
    fn background_and_frame_round_trip_through_config() {
        for bg in [Bg::Gradient(0), Bg::Gradient(11), Bg::Solid(3), Bg::Blur, Bg::Transparent] {
            assert_eq!(parse_bg(Some(&bg.key())), Some(bg), "bg {:?}", bg);
        }
        for f in Frame::ALL {
            assert_eq!(Frame::parse(Some(f.key())), Some(f));
        }
        for w in WmWeight::ALL {
            assert_eq!(WmWeight::parse(Some(w.key())), Some(w));
        }
        for f in WmFont::ALL {
            assert_eq!(WmFont::parse(Some(f.key())), Some(f));
        }
    }
}
