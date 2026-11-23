//! 调色板：深浅两套，全部按 ARGB 存。
//!
//! 任务栏形态的标注色：文字纯色、标签 70%、选中 15%、悬停 7%、分割线 12%；
//! 卡片底色 0xF3F3F3（浅）/ 0x202020（深）。这些比例是和文字对比度一起调出来的，
//! 单独改某一项会让小字号失去可读性。

#[derive(Clone, Copy)]
pub struct Palette {
    pub light: bool,
    pub text: u32,
    pub label: u32,
    pub highlight: u32,
    pub hover: u32,
    pub separator: u32,
    pub card: u32,
    pub card_border: u32,
    pub card_solid: u32,
    pub subtle: u32,
    pub accent: u32,
    pub danger: u32,
    pub track: u32,
    pub chart_fill_a: u32,
    pub chart_fill_b: u32,
}

pub fn for_window(light: bool) -> Palette {
    if light {
        Palette {
            light,
            text: 0xFF1A1A1A,
            label: 0xB31A1A1A,
            highlight: 0x261A1A1A,
            hover: 0x121A1A1A,
            separator: 0x1F1A1A1A,
            card: 0xFFF3F3F3,
            card_border: 0x33000000,
            card_solid: 0xFFF9F9F9,
            subtle: 0xFF6B6B6B,
            accent: 0xFF0F6CBD,
            danger: 0xFFC42B1C,
            track: 0x1A000000,
            chart_fill_a: 0x590F6CBD,
            chart_fill_b: 0x1A0F6CBD,
        }
    } else {
        Palette {
            light,
            text: 0xFFFFFFFF,
            label: 0xB3FFFFFF,
            highlight: 0x26FFFFFF,
            hover: 0x12FFFFFF,
            separator: 0x1FFFFFFF,
            card: 0xFF202020,
            card_border: 0x59FFFFFF,
            card_solid: 0xFF2B2B2B,
            subtle: 0xFF9A9A9A,
            accent: 0xFF60CDFF,
            danger: 0xFFFF99A4,
            track: 0x33FFFFFF,
            chart_fill_a: 0x9960CDFF,
            chart_fill_b: 0x2260CDFF,
        }
    }
}

/// 任务栏形态专用：叠在真实任务栏上，只画文字与交互层。
pub fn for_taskbar(light: bool) -> Palette {
    let c = if light { 0x000000u32 } else { 0xFFFFFFu32 };
    let mut p = for_window(light);
    p.text = with_a(c, 0xFF);
    p.label = with_a(c, 0xB3);
    p.highlight = with_a(c, 0x26);
    p.hover = with_a(c, 0x12);
    p.separator = with_a(c, 0x1F);
    p.card = if light { 0xFFF3F3F3 } else { 0xFF202020 };
    p
}

const fn with_a(c: u32, a: u8) -> u32 { (c & 0x00FF_FFFF) | ((a as u32) << 24) }
