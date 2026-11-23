//! 图标探针：把工具条用到的图标按实际尺寸与 4 倍放大各画一遍，存成 PNG 供核对。
use capture::design::{self, Icon};
use zx_ui::Canvas;

const ICONS: [(&str, Icon); 27] = [
    ("Pointer", Icon::Pointer),
    ("Square", Icon::Square),
    ("Circle", Icon::Circle),
    ("ArrowUpRight", Icon::ArrowUpRight),
    ("Slash", Icon::Slash),
    ("Pencil", Icon::Pencil),
    ("Highlighter", Icon::Highlighter),
    ("Pixelate", Icon::Pixelate),
    ("Type", Icon::Type),
    ("Counter", Icon::Counter),
    ("Droplet", Icon::Droplet),
    ("Undo", Icon::Undo),
    ("Redo", Icon::Redo),
    ("Sparkles", Icon::Sparkles),
    ("ImagePlus", Icon::ImagePlus),
    ("ScanText", Icon::ScanText),
    ("UnfoldVertical", Icon::UnfoldVertical),
    ("Record", Icon::Record),
    ("Gif", Icon::Gif),
    ("Copy", Icon::Copy),
    ("Download", Icon::Download),
    ("FolderDown", Icon::FolderDown),
    ("Pin", Icon::Pin),
    ("Close", Icon::Close),
    ("Check", Icon::Check),
    ("Plus", Icon::Plus),
    ("Upload", Icon::Upload),
];

fn main() {
    zx_ui::painter::init_gdiplus();
    let bg = 0xFF1F1F1F;
    let fg = 0xFFE8E8EA;
    let cell = 56.0;
    let zoom = 4.0;
    let cols = 7usize;
    let rows = ICONS.len().div_ceil(cols);
    let w = (cols as f32 * cell) as i32;
    let h = (rows as f32 * cell + rows as f32 * cell * zoom + 280.0) as i32;

    let mut cv = Canvas::new(w.max(400), h.max(400), 96);
    cv.clear(bg);

    // 第一段：实际尺寸 16px（工具条里的样子），格子 56
    for (i, (_, icon)) in ICONS.iter().enumerate() {
        let cx = (i % cols) as f32 * cell + cell / 2.0;
        let cy = (i / cols) as f32 * cell + cell / 2.0;
        cv.stroke_round(cx - 14.0, cy - 14.0, 28.0, 28.0, 7.0, 1.0, 0x33FFFFFF);
        design::draw_icon(&mut cv, *icon, cx, cy, 16.0, fg, 1.0);
    }

    // 第二段：4 倍放大，看清形状
    let y0 = rows as f32 * cell + 10.0;
    for (i, (_, icon)) in ICONS.iter().enumerate() {
        let cx = (i % cols) as f32 * cell + cell / 2.0;
        let cy = y0 + (i / cols) as f32 * cell * zoom + cell * zoom / 2.0;
        design::draw_icon(&mut cv, *icon, cx, cy, 16.0 * zoom, fg, 1.0);
    }

    // 第三段：Pointer 按工具条真实尺寸（16px）画到 32×32，再逐像素放大 8 倍，
    // 用来确认像素级的完整性（有没有被裁边、缺笔画）。
    {
        let mut tiny = Canvas::new(32, 32, 96);
        tiny.clear(bg);
        design::draw_icon(&mut tiny, Icon::Pointer, 16.0, 16.0, 16.0, fg, 1.0);
        let px: Vec<u8> = tiny.pixels().to_vec();
        let z = 8.0f32;
        let by = y0 + rows as f32 * cell * zoom + 12.0;
        for y in 0..32 {
            for x in 0..32 {
                let o = y * 32 * 4 + x * 4;
                let (b, g, r, a) = (px[o], px[o + 1], px[o + 2], px[o + 3]);
                if a == 0 {
                    continue;
                }
                // 画布是预乘 ARGB：这里把 alpha 合到纯色上就够看了
                let col = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                cv.fill_rect(x as f32 * z, by + y as f32 * z, z, z, col);
            }
        }
    }

    let out = std::env::args().nth(1).unwrap_or_else(|| "icon_probe.png".into());
    let png = cv.png_bytes().expect("png encode");
    std::fs::write(&out, png).expect("write png");
    println!("{out}");
}
