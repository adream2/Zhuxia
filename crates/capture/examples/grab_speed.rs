//! 抓屏 / 合成 / 编码的速度探针。
//!
//! 用来回答一个问题：**这台机器能录到多少帧**。
//! 分别测三段耗时——`BitBlt` 抓帧、逐像素合成、GIF 量化+LZW——因为它们决定了
//! 录屏（走硬件 H.264）与录 GIF（纯 CPU）能跑到的上限完全不同。
//!
//! ```text
//! cargo run --release --example grab_speed
//! ```

use std::time::Instant;

use windows::Win32::Foundation::RECT;

use capture::record::source::{self, FrameBuf};
use capture::screen::Frame;

fn pct(mut v: Vec<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((v.len() - 1) as f64 * p) as usize;
    v[i]
}

fn bench_grab(w: i32, h: i32) -> Vec<f64> {
    let rect = RECT { left: 100, top: 100, right: 100 + w, bottom: 100 + h };
    // 预热：DIB 创建 + 首次 BitBlt 有一次性成本
    for _ in 0..3 {
        let _ = Frame::grab_rect(rect);
    }
    let mut out = FrameBuf::new(w, h);
    let mut times = Vec::new();
    for _ in 0..60 {
        let t = Instant::now();
        let ok = source::grab_screen(rect, &mut out);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ok {
            times.push(ms);
        }
    }
    times
}

fn bench_compose(w: i32, h: i32) -> Vec<f64> {
    use capture::record::compose::{self, Mode, Shape, Spec};
    let mut screen = FrameBuf::new(w, h);
    screen.clear(0xFF203040);
    let mut cam = FrameBuf::new(640, 480);
    cam.clear(0xFF805040);
    let spec = Spec {
        mode: Mode::Pip,
        corner: compose::Corner::BottomRight,
        size_pct: 24,
        shape: Shape::Rounded,
        mirror: true,
        border: true,
        shadow: true,
        margin: 16,
        max_width: 0,
    };
    let plan = compose::plan(&spec, (w, h), Some((640, 480)));
    let mut out = FrameBuf::new(plan.w, plan.h);
    let mut times = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        compose::render(&plan, Some(&screen), Some(&cam), &mut out, 0xFF101010);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times
}

fn bench_gif(w: i32, h: i32) -> Vec<f64> {
    use capture::record::gifenc::GifWriter;
    let path = std::env::temp_dir().join("zx-grab-speed.gif");
    let mut wr = match GifWriter::create(&path, w, h, 25) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("GIF 探针建文件失败：{e}");
            return Vec::new();
        }
    };
    // 每帧都改一块区域，逼脏矩形与量化真的干活
    let mut frame = FrameBuf::new(w, h);
    frame.clear(0xFF202020);
    let mut times = Vec::new();
    for i in 0..20 {
        let x = (i * 37) % (w - 80).max(1);
        for y in 40..(40 + 80).min(h) {
            for xx in x..(x + 80).min(w) {
                let o = y as usize * frame.stride() + xx as usize * 4;
                frame.buf[o] = 60;
                frame.buf[o + 1] = 200;
                frame.buf[o + 2] = 240;
            }
        }
        let t = Instant::now();
        let _ = wr.push(&frame);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let _ = wr.finish();
    let _ = std::fs::remove_file(&path);
    times
}

fn report(name: &str, times: &[f64], budget_ms: f64) {
    if times.is_empty() {
        println!("{name:<34} 无数据");
        return;
    }
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "{name:<34} avg {:>6.2}ms  p95 {:>6.2}ms  max {:>6.2}ms   → 上限 {:>5.1} fps（预算 {budget_ms}ms）",
        avg,
        pct(times.to_vec(), 0.95),
        times.iter().cloned().fold(0.0, f64::max),
        budget_ms / avg.max(0.001) * (budget_ms / pct(times.to_vec(), 0.95).max(0.001)).min(1.0),
    );
}

/// 只测 BitBlt 本身（复用 memdc + DIB，不每帧新建）。
///
/// 用来区分两种成本：**创建 DIB**（每帧 8~15MB 分配 + 清零）与 **BitBlt 抓屏**。
/// 如果这一项仍然贴着 16.7 / 33.3ms，说明 BitBlt 是与 DWM present 同步的，
/// 再怎么优化内存也没用——60fps 只能换 WGC/DXGI。
fn bench_raw_bitblt(w: i32, h: i32) -> Vec<f64> {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let screen = GetDC(None);
        let memdc = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        let old = SelectObject(memdc, dib);
        let rop = ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0);
        for _ in 0..3 {
            let _ = BitBlt(memdc, 0, 0, w, h, screen, 100, 100, rop);
        }
        let mut times = Vec::new();
        for _ in 0..50 {
            let t = Instant::now();
            let _ = BitBlt(memdc, 0, 0, w, h, screen, 100, 100, rop);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        SelectObject(memdc, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(memdc);
        ReleaseDC(None, screen);
        times
    }
}

fn main() {
    let _ = zx_core::cfg::load();
    for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440)] {
        println!("\n─── {w}×{h} ───");
        let raw = bench_raw_bitblt(w, h);
        report("BitBlt 本身（复用 DIB）", &raw, 33.3);
        let grab = bench_grab(w, h);
        report(&"BitBlt 抓帧".to_string(), &grab, 33.3);
        let comp = bench_compose(w, h);
        report("合成（画中画 + 遮罩 + 阴影）", &comp, 33.3);
        let gif = bench_gif(w, h);
        report("GIF 量化 + LZW", &gif, 40.0);
        if let (Some(g), Some(c)) = (grab.first(), comp.first()) {
            println!(
                "    抓帧+合成 ≈ {:.2}ms（30fps 预算 33.3ms，60fps 预算 16.7ms）",
                g + c
            );
        }
    }
    println!("\n注：GIF 的“上限 fps”按 25fps（40ms 预算）算；录视频走硬件 MFT，编码不占这些时间。");
}
