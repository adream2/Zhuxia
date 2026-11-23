//! 合成性能基线：分离"清屏 / 缩放拷贝 / 整体合成"三段耗时。
//!
//! `cargo run --release --example compose_bench`
//!
//! 存在的理由：设置页有「输出宽度」这一项，如果缩放比 1:1 慢十倍，
//! 那这个选项就是"越省越卡"，必须能反复量。

use capture::record::compose::{self, Corner, Mode, Shape, Spec};
use capture::record::source::{FrameBuf, Rect};
use std::time::Instant;

fn fill(f: &mut FrameBuf, seed: u32) {
    let stride = f.stride();
    for y in 0..f.h as usize {
        for x in 0..f.w as usize {
            let o = y * stride + x * 4;
            f.buf[o] = ((x + seed as usize) % 256) as u8;
            f.buf[o + 1] = ((y * 3 + seed as usize) % 256) as u8;
            f.buf[o + 2] = 128;
            f.buf[o + 3] = 255;
        }
    }
}

fn bench(label: &str, iters: u32, mut f: impl FnMut()) -> f64 {
    // 预热
    for _ in 0..3 {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / iters as f64;
    println!("{label:<34} {ms:6.2} ms/次");
    ms
}

fn main() {
    let spec = |mode: Mode, max_width: i32| Spec {
        mode,
        corner: Corner::BottomRight,
        size_pct: 24,
        shape: Shape::Rounded,
        mirror: true,
        border: true,
        shadow: true,
        margin: 16,
        max_width,
    };

    for (sw, sh, cap) in [(1280, 720, 0), (1920, 1080, 1280), (2560, 1440, 1280)] {
        println!("--- 源 {sw}×{sh}，输出宽度上限 {} ---", if cap == 0 { "不限".to_string() } else { cap.to_string() });
        let mut src = FrameBuf::new(sw, sh);
        fill(&mut src, 1);
        let p = compose::plan(&spec(Mode::Screen, cap), (sw, sh), None);
        let mut out = FrameBuf::new(p.w, p.h);
        println!("    输出 {}×{}", p.w, p.h);

        bench("清屏 clear()", 30, || out.clear(0xFF101010));
        let r: Rect = p.screen.expect("screen layer");
        if r.w == src.w && r.h == src.h {
            println!("    （1:1，走 memcpy 快路径）");
        }
        bench("只做屏幕层 blit_scaled()", 30, || {
            compose::blit_scaled(&mut out, &src, r, None, false);
        });
        bench("整帧 render()", 30, || {
            compose::render(&p, Some(&src), None, &mut out, 0xFF101010);
        });
    }
}
