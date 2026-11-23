//! 端到端测试：像素进、文件出。跑在真 Windows 上，不 mock GDI+/libwebp。

use std::path::PathBuf;

use imgconv::convert::{convert, Options};
use imgconv::fmt::Target;

/// 造一张 64×48 的 PNG：左半红、右半半透明蓝（用来验证 alpha 压平）。
fn make_png(dir: &std::path::Path) -> PathBuf {
    let p = dir.join("src.png");
    let (w, h) = (64u32, 48u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _y in 0..h {
        for x in 0..w {
            if x < w / 2 {
                rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 255, 0]); // 全透明
            }
        }
    }
    let file = std::fs::File::create(&p).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().unwrap();
    wr.write_image_data(&rgba).unwrap();
    wr.finish().unwrap();
    p
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zx-imgconv-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn png_to_webp_and_back() {
    let d = scratch("webp");
    let src = make_png(&d);
    let opts = Options::default();

    let a = convert(&src, Target::WebP, &opts).expect("png → webp");
    assert_eq!(a.output.extension().unwrap(), "webp");
    let head = std::fs::read(&a.output).unwrap();
    assert_eq!(&head[..4], b"RIFF", "WebP 应该是 RIFF 容器");
    assert_eq!(&head[8..12], b"WEBP");

    // WebP 读回来再转 PNG：走 libwebp 解码那条路
    let b = convert(&a.output, Target::Png, &opts).expect("webp → png");
    let bytes = std::fs::read(&b.output).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn jpeg_flattens_alpha_and_drops_alpha_channel() {
    let d = scratch("jpg");
    let src = make_png(&d);
    let out = convert(&src, Target::Jpg, &Options::default()).expect("png → jpg");
    let bytes = std::fs::read(&out.output).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG SOI");
    // 解码后不应再有 alpha，透明区应变成背景（白）
    let img = image::open(&out.output).unwrap().to_rgba8();
    let px = img.get_pixel(60, 20);
    assert_eq!(px[3], 255);
    assert!(px[0] > 230 && px[1] > 230 && px[2] > 230, "透明区应压成白色，实际 {px:?}");
}

#[test]
fn refuses_same_format_and_corrupt_file() {
    let d = scratch("refuse");
    let src = make_png(&d);
    let opts = Options::default();

    let same = convert(&src, Target::Png, &opts);
    assert!(same.is_err(), "同格式应拒绝");

    let bad = d.join("bad.jpg");
    std::fs::write(&bad, b"definitely not a jpeg").unwrap();
    let err = convert(&bad, Target::Png, &opts);
    assert!(err.is_err(), "损坏文件应报错而不是产出空文件");
}

#[test]
fn never_overwrites_source_and_avoids_collision() {
    let d = scratch("name");
    let src = make_png(&d);
    let opts = Options::default();

    let first = convert(&src, Target::WebP, &opts).unwrap();
    let second = convert(&src, Target::WebP, &opts).unwrap();
    assert_ne!(first.output, second.output, "第二次必须换名");
    assert!(second.output.to_string_lossy().contains("(2)"), "命名应为 名字 (2).webp");
    assert!(src.exists(), "源文件必须还在");
}

#[test]
fn pipe_protocol_roundtrip() {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    imgconv::pipe::serve(move |t, p| {
        let _ = tx.send((t, p));
    });
    std::thread::sleep(std::time::Duration::from_millis(300));

    let path = PathBuf::from(r"C:\图片 目录\带 空格.jpg");
    assert!(imgconv::pipe::send(&[(Target::WebP, path.clone())]), "客户端应能写入管道");
    let (t, p) = rx.recv_timeout(std::time::Duration::from_secs(3)).expect("服务端应收到任务");
    assert_eq!(t, Target::WebP);
    assert_eq!(p, path);
}

/// 矢量源：长边不足 256 要放大到 256，颜色不能变。
#[test]
fn svg_renders_at_least_256_long_edge() {
    let d = scratch("svg");
    let svg = d.join("logo.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32" viewBox="0 0 64 32">
              <rect width="64" height="32" fill="#ff0000"/></svg>"##,
    )
    .unwrap();

    let out = convert(&svg, Target::Png, &Options::default()).expect("svg → png");
    assert_eq!((out.w, out.h), (256, 128), "64×32 的 SVG 长边应放大到 256");
    let img = image::open(&out.output).unwrap().to_rgba8();
    let px = img.get_pixel(10, 10);
    assert_eq!((px[0], px[1], px[2]), (255, 0, 0), "填充色必须保留，实际 {px:?}");
}

/// ICO：容器头、条目数量、宽高字节、每帧 PNG 签名。
#[test]
fn ico_contains_requested_sizes() {
    let d = scratch("ico");
    let src = make_png(&d);
    let opts = Options { ico_sizes: vec![16, 32, 48], ..Options::default() };
    let out = convert(&src, Target::Ico, &opts).expect("png → ico");
    let b = std::fs::read(&out.output).unwrap();

    assert_eq!(&b[0..4], &[0, 0, 1, 0], "ICONDIR: reserved=0, type=1");
    let count = u16::from_le_bytes([b[4], b[5]]);
    assert_eq!(count, 3, "三个规格");

    let mut sizes = Vec::new();
    for i in 0..count as usize {
        let e = 6 + i * 16;
        let w = if b[e] == 0 { 256 } else { b[e] as u32 };
        let h = if b[e + 1] == 0 { 256 } else { b[e + 1] as u32 };
        assert_eq!(w, h, "图标必须正方形");
        sizes.push(w);
        let len = u32::from_le_bytes([b[e + 8], b[e + 9], b[e + 10], b[e + 11]]) as usize;
        let off = u32::from_le_bytes([b[e + 12], b[e + 13], b[e + 14], b[e + 15]]) as usize;
        assert_eq!(&b[off..off + 8], b"\x89PNG\r\n\x1a\n", "第 {i} 帧应是 PNG");
        assert!(len > 8);
    }
    assert_eq!(sizes, vec![48, 32, 16], "按从大到小写");
}

/// 非正方形源要补成正方形，否则图标会被拉扁。
#[test]
fn ico_pads_non_square_sources() {
    let d = scratch("ico-pad");
    let src = make_png(&d); // 64×48
    let opts = Options { ico_sizes: vec![32], ..Options::default() };
    let out = convert(&src, Target::Ico, &opts).unwrap();
    let b = std::fs::read(&out.output).unwrap();
    assert_eq!((b[6], b[7]), (32, 32));
}

/// 矢量直接出 ICO：走“解码到最大规格再逐级缩”那条路。
#[test]
fn svg_to_ico() {
    let d = scratch("svg-ico");
    let svg = d.join("icon.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256">
              <circle cx="128" cy="128" r="120" fill="#0a84ff"/></svg>"##,
    )
    .unwrap();
    let opts = Options { ico_sizes: vec![16, 48, 256], ..Options::default() };
    let out = convert(&svg, Target::Ico, &opts).expect("svg → ico");
    let b = std::fs::read(&out.output).unwrap();
    assert_eq!(u16::from_le_bytes([b[4], b[5]]), 3);
    // 256 的条目宽度字节写 0
    assert_eq!(b[6], 0);
}
