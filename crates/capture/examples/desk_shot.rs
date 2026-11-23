//! 桌面截图小工具：把当前虚拟桌面抓成一张 PNG（UI 核对用）。
//!
//! `cargo run --release --example desk_shot -- <输出路径>`
//!
//! 用来在截图覆盖层还显示着的时候从另一个进程抓一张，核对工具条/色板的真实观感
//! （`--capture-selftest-ui` 只能看，不方便并存）。

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "desk.png".into());
    let Some(frame) = capture::screen::Frame::grab() else {
        eprintln!("抓屏失败");
        std::process::exit(1);
    };
    let b = frame.bounds();
    let mut cv = zx_ui::Canvas::new(b.right - b.left, b.bottom - b.top, 96);
    frame.blit_into(&mut cv);
    let Some(png) = cv.png_bytes() else {
        eprintln!("PNG 编码失败");
        std::process::exit(1);
    };
    match std::fs::write(&out, png) {
        Ok(_) => println!("已保存 {out}（{}x{}）", cv.w, cv.h),
        Err(e) => eprintln!("写文件失败：{e}"),
    }
}
