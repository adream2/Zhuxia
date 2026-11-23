//! 美化矩阵探针：把 6 种边框、几种背景、水印各渲染一张，肉眼核对排版。
//!
//! `cargo run --release --example style_probe`
//! 输出到 exe 目录（`target/release/probe-*.png`）。

use capture::style::{self, Aspect, Bg, Frame, Style, WmFont, WmPlace, WmWeight, Watermark};
use zx_ui::Canvas;

fn sample_doc() -> Canvas {
    let mut c = Canvas::new(420, 260, 96);
    c.clear(0xFFF7F8FA);
    c.fill_round(16.0, 16.0, 388.0, 40.0, 8.0, 0xFFFFFFFF);
    c.text_ex("驻匣 · 美化模板", 30.0, 16.0, 360.0, 40.0, 18.0, 0xFF111827,
              zx_ui::TextAlign::Near, zx_ui::VAlign::Center, true, false);
    for (i, col) in [0xFFFF4D4F, 0xFF22C55E, 0xFF2E90FA].iter().enumerate() {
        c.fill_round(24.0 + i as f32 * 92.0, 72.0, 80.0, 56.0, 10.0, *col);
    }
    c.text_ex("这是一段用来检查圆角、阴影与水印排版的示例文字。",
              24.0, 150.0, 380.0, 26.0, 12.0, 0xFF374151,
              zx_ui::TextAlign::Near, zx_ui::VAlign::Center, false, false);
    c.text_ex("second_line 0123456789", 24.0, 180.0, 380.0, 26.0, 12.0, 0xFF6B7280,
              zx_ui::TextAlign::Near, zx_ui::VAlign::Center, false, false);
    c
}

fn save(name: &str, c: &Canvas) {
    if let Some(png) = c.png_bytes() {
        let path = zx_core::cfg::run_dir().join(format!("probe-{name}.png"));
        let _ = std::fs::write(&path, png);
        println!("{}  {}x{}", path.display(), c.w, c.h);
    }
}

fn main() {
    let _ = zx_core::cfg::load();
    let doc = sample_doc();
    let base = Style { enabled: true, padding: 56, radius: 12, shadow: 60, ..Default::default() };

    for f in Frame::ALL {
        let st = Style { frame: f, ..base.clone() };
        save(&format!("frame-{}", f.key()), &style::render(&doc, &[], &st, 1.0));
    }
    for (name, bg) in [
        ("bg-aurora", Bg::Gradient(1)),
        ("bg-mint", Bg::Gradient(5)),
        ("bg-cloud", Bg::Gradient(11)),
        ("bg-solid-dark", Bg::Solid(0)),
        ("bg-solid-custom", Bg::SolidCustom(0xFF2F3542)),
        ("bg-blur", Bg::Blur),
    ] {
        let st = Style { bg, ..base.clone() };
        save(name, &style::render(&doc, &[], &st, 1.0));
    }
    // 水印：6 种字体各来一张（核对"换字体有没有变化"）
    for font in WmFont::ALL {
        let st = Style {
            wm: Watermark {
                enabled: true,
                place: WmPlace::Margin,
                text: "@驻匣 Studio 水印".into(),
                color: None,
                opacity: 62,
                font,
                weight: WmWeight::Regular,
                size: 130,
            },
            ..base.clone()
        };
        save(&format!("wm-font-{}", font.key()), &style::render(&doc, &[], &st, 1.0));
    }
    for (name, place, font, weight, color) in [
        ("wm-image", WmPlace::Image, WmFont::System, WmWeight::Regular, None),
        ("wm-margin", WmPlace::Margin, WmFont::Serif, WmWeight::Light, None),
        ("wm-margin-bold", WmPlace::Margin, WmFont::Geometric, WmWeight::Bold, Some(0xFFFF4D4F)),
        // 新增：平铺（图上按砖缝重复，自动配色）
        ("wm-tile", WmPlace::Tile, WmFont::System, WmWeight::Regular, None),
        ("wm-tile-color", WmPlace::Tile, WmFont::System, WmWeight::Bold, Some(0xFFFF4D4F)),
    ] {
        let st = Style {
            wm: Watermark {
                enabled: true,
                place,
                text: "@驻匣 Studio".into(),
                color,
                opacity: 60,
                font,
                weight,
                size: 110,
            },
            ..base.clone()
        };
        save(name, &style::render(&doc, &[], &st, 1.0));
    }
    // 比例与水印边距（水印放不下时应自动退回图上）
    let st = Style { aspect: Aspect::Square, ..base.clone() };
    save("aspect-square", &style::render(&doc, &[], &st, 1.0));
}
