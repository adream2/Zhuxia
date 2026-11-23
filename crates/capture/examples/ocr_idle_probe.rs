//! OCR 引擎「空闲自动释放」自检：识别一次 → 空转 → 自动释放，全程打印私有内存。
//!
//! `cargo run --release --example ocr_idle_probe`
//!
//! 前提：example 同目录下有 `ocr/{det.onnx,rec.onnx,keys.txt}`（把 `target/release/ocr`
//! 整个复制到 `target/release/examples/` 即可），否则会退到「模型未安装」。
//!
//! 这台机器上的预期：识别后 +200 MB 上下，释放后回到 20 MB 以内。

fn priv_mb() -> f64 {
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS::default();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let _ = K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
        c.PagefileUsage as f64 / (1024.0 * 1024.0)
    }
}

fn main() {
    // 3 秒就释放，省得等两分钟。
    std::env::set_var("ZHUXIA_OCR_IDLE_SECS", "3");

    println!("初始私有内存 {:.1} MB", priv_mb());
    let canvas = zx_ui::Canvas::new(320, 120, 96);
    let started = std::time::Instant::now();
    let r = capture::ocr::recognize(&canvas);
    match &r {
        Ok(t) => println!("识别完成：{} 字，耗时 {:?}", t.chars().count(), started.elapsed()),
        Err(e) => println!("识别失败：{e}"),
    }
    let peak = priv_mb();
    println!("识别后私有内存 {:.1} MB", peak);

    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if capture::ppocr::unload_if_idle() {
            println!("引擎已释放，私有内存 {:.1} MB（峰值 {:.1} MB）", priv_mb(), peak);
            return;
        }
    }
    println!("10 秒内没释放，私有内存 {:.1} MB", priv_mb());
}
