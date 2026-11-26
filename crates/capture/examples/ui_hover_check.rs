//! 覆盖层元素高亮的自检：把鼠标放到一个控件上 → 开覆盖层 → 抓屏存证 → 关掉。
//!
//! ```text
//! cargo run --release --example ui_hover_check
//! ```
//!
//! 产物：`ui-hover-check.png`（覆盖层自己也是分层窗口，`CAPTUREBLT` 会把它一起拍进来，
//! 所以这张图就是"用户看到的画面"）。用来核对悬停框是否落在控件上、气泡里的
//! 类型和名称对不对。
//!
//! 注意：覆盖层要靠消息循环才会处理"快照到位后重画"的通知，这里自己跑一段泵。
//!
//! 加 `--click x,y` 会在存证之前往那个屏幕坐标点一下左键，用来验证悬停气泡上的
//! 「复制文本 / 复制信息 / 存图标」是不是真的点得动（成功与否看日志）。

use std::time::{Duration, Instant};

use capture::{output, overlay, pin, screen, uielem};
use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, PostMessageW, SetCursorPos, TranslateMessage, MSG,
    PM_REMOVE, WM_CLOSE, WM_LBUTTONDOWN, WM_LBUTTONUP,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // --at x,y 把鼠标摆到指定位置再开覆盖层（测任务栏这类必须的）
    let at = args
        .iter()
        .position(|a| a == "--at")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.split_once(','))
        .and_then(|(x, y)| Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?)));
    let click = args
        .iter()
        .position(|a| a == "--click")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.split_once(','))
        .and_then(|(x, y)| Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?)));
    let _ = &args;

    // 1. 先探一下前台窗口，挑一个有名字的按钮当靶子
    let mem_before = priv_mb();
    let elems = uielem::snapshot_capture(Duration::from_millis(1500));
    println!(
        "UIA 通道净增 {:.1} MB（含 uiautomationcore 映射；{} 个元素）",
        priv_mb() - mem_before,
        elems.len()
    );
    let target = elems
        .iter()
        .find(|e| e.kind == uielem::Kind::Button && e.rect.w >= 40 && e.rect.h >= 20 && !e.name.is_empty())
        .or_else(|| elems.iter().find(|e| !e.name.is_empty()));
    let Some(t) = target else {
        eprintln!("没探到元素：前台窗口可能没有 a11y 实现");
        return;
    };
    let (cx, cy) = at.unwrap_or_else(|| t.rect.center());
    println!("目标元素：{} @ {},{}  {}x{}", t.label(), t.rect.x, t.rect.y, t.rect.w, t.rect.h);

    // 2. 把鼠标摆到控件中心，开覆盖层（窗口类要自己注册，正常路径由 capture::start 做）
    unsafe { let _ = SetCursorPos(cx, cy); }
    std::thread::sleep(Duration::from_millis(120));
    overlay::register();
    pin::register();
    if !overlay::begin() {
        eprintln!("覆盖层没起来（抓不到屏幕？）");
        return;
    }

    // 3. 跑一段消息循环：快照线程完成时会 PostMessage 让覆盖层重画
    pump(Duration::from_millis(2000));

    // 3.4 覆盖层已经显示出来了：此刻 ElementFromPoint 还看不看得穿我们？
    {
        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt); }
        let path = uielem::path_at(pt.x, pt.y);
        println!("
覆盖层已显示时，光标 {},{ } 的实时父链 {} 层：", pt.x, pt.y, path.len());
        for (i, e) in path.iter().enumerate() {
            println!(
                "  {i:>2}. {:<8} {:<24} {}x{} @{},{}",
                e.kind.label(),
                trunc(&e.name, 22),
                e.rect.w,
                e.rect.h,
                e.rect.x,
                e.rect.y
            );
        }
        if path.is_empty() {
            println!("  （空 —— 说明被覆盖层挡住了）");
        }
    }

    // 3.5 可选：点一下气泡上的动作按钮（放在抓屏之前，截图里能看到「已复制」）
    if let Some((x, y)) = click {
        if let Some(h) = overlay::hwnd() {
            let lp = LPARAM(((y as isize) & 0xFFFF) << 16 | ((x as isize) & 0xFFFF));
            unsafe {
                let _ = PostMessageW(h, WM_LBUTTONDOWN, WPARAM(1), lp);
                let _ = PostMessageW(h, WM_LBUTTONUP, WPARAM(0), lp);
            }
            pump(Duration::from_millis(400));
            println!("已模拟点击 {x},{y}");
        }
    }

    // 4. 抓屏存证
    if let Some(f) = screen::Frame::grab() {
        let full = RECT { left: f.x, top: f.y, right: f.x + f.w, bottom: f.y + f.h };
        if let Some(canvas) = output::crop(&f, full) {
            if let Some(png) = canvas.png_bytes() {
                let path = zx_core::cfg::run_dir().join("ui-hover-check.png");
                match std::fs::write(&path, png) {
                    Ok(()) => println!("已存证：{}", path.display()),
                    Err(e) => eprintln!("写文件失败：{e}"),
                }
            }
        }
    }

    // 5. 关掉覆盖层
    if let Some(h) = overlay::hwnd() {
        unsafe { let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)); }
        pump(Duration::from_millis(300));
    }
}

fn priv_mb() -> f64 {
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let _ = K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
        c.PagefileUsage as f64 / (1024.0 * 1024.0)
    }
}

fn pump(d: Duration) {
    let end = Instant::now() + d;
    unsafe {
        while Instant::now() < end {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}

fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}…") } else { t }
}
