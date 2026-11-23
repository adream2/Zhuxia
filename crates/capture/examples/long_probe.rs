//! 长截图探针：不开 UI，直接跑「start → 滚轮 → tick」循环，把拼接过程打出来。
//!
//! `cargo run --release --example long_probe -- <left> <top> <w> <h> [steps]`
//!
//! 需要目标区域真的能滚（先开一个长文档 / 网页），光标会被移到区域中心再用滚轮。
//! 每一步打印：总高度、找到的位移 dy、匹配失败的行数。

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

/// 抓一块区域返回 (宽, 高, 像素)。仅探针内部用，随便拷一份省得依赖私有 API。
fn grab(r: &RECT) -> Option<(i32, i32, Vec<u8>)> {
    let f = capture::screen::Frame::grab_rect(*r)?;
    let bytes = unsafe { std::slice::from_raw_parts(f.pixels_ptr(), f.pixels_len()) };
    Some((f.w, f.h, bytes.to_vec()))
}

fn wheel(notches: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: (-120 * notches) as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// 按标题子串找一个可见窗口，返回它内部的区域（跳过标题栏与边框）。
fn find_window_rect(needle: &str) -> Option<RECT> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowTextW, IsWindowVisible,
    };
    struct Ctx<'a> {
        needle: String,
        found: Option<RECT>,
        _p: std::marker::PhantomData<&'a ()>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        if title.contains(&ctx.needle) {
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_ok() {
                ctx.found = Some(RECT {
                    left: r.left + 20,
                    top: r.top + 70,
                    right: r.right - 20,
                    bottom: r.bottom - 40,
                });
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut ctx = Ctx { needle: needle.to_string(), found: None, _p: std::marker::PhantomData };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.found
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // `long_probe ui <窗口标题>`：走真实控制条（run + 定时器 + 收尾），
    // 后台线程代替人滚轮，2.5 秒后自动完成。
    if raw.first().map(|v| v.as_str()) == Some("ui") {
        let title = raw.get(1).cloned().unwrap_or_else(|| "long-test".into());
        let Some(rect) = find_window_rect(&title) else {
            eprintln!("没找到窗口「{title}」");
            return;
        };
        unsafe {
            let _ = SetCursorPos(rect.left + (rect.right - rect.left) / 2,
                                 rect.top + (rect.bottom - rect.top) / 2);
        }
        std::thread::spawn(move || {
            for _ in 0..4 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                wheel(3);
            }
        });
        std::env::set_var("ZHUXIA_CAPTURE_LONG_AUTO", "1");
        match capture::longshot::run(rect) {
            Some(c) => {
                let path = std::env::current_exe().unwrap().parent().unwrap().join("long-ui.png");
                if let Some(png) = c.png_bytes() {
                    let _ = std::fs::write(&path, png);
                }
                println!("控制条路径成品 {}x{} → {}", c.w, c.h, path.display());
            }
            None => println!("取消或失败"),
        }
        return;
    }
    let (rect, steps) = match raw.first() {
        Some(first) if first.parse::<i32>().is_ok() => {
            let a: Vec<i32> = raw.iter().filter_map(|v| v.parse().ok()).collect();
            if a.len() < 4 {
                eprintln!("用法：long_probe <left> <top> <w> <h> [steps]");
                return;
            }
            (
                RECT { left: a[0], top: a[1], right: a[0] + a[2], bottom: a[1] + a[3] },
                a.get(4).copied().unwrap_or(6),
            )
        }
        Some(title) => {
            let Some(r) = find_window_rect(title) else {
                eprintln!("没找到标题包含「{title}」的窗口");
                return;
            };
            (r, raw.get(1).and_then(|v| v.parse().ok()).unwrap_or(6))
        }
        None => {
            eprintln!("用法：long_probe <left> <top> <w> <h> [steps] | <窗口标题> [steps]");
            return;
        }
    };
    if !capture::longshot::start(rect) {
        eprintln!("start 失败（区域太小或抓不到）");
        return;
    }
    println!("初始 {}x{}，每步滚 3 格", rect.right - rect.left, rect.bottom - rect.top);

    // 先单独确认「滚轮到底有没有生效」：抓两帧，按 dy 统计逐行逐字节相等的行数。
    // 全等 → 滚轮没进到窗口；某个 dy 突然一堆行全等 → 滚动正常，问题在匹配。
    if std::env::var("ZHUXIA_LONG_TRACE").is_ok() {
        let first = grab(&rect);
        wheel(3);
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let (Some(a), Some(b)) = (first, grab(&rect)) {
            // 上面已经滚动过一次，这里比较的是「滚动前 vs 滚动后」
            println!("两帧尺寸 {}x{} / {}x{}", a.0, a.1, b.0, b.1);
            let rows = |f: &(i32, i32, Vec<u8>)| f.1 as usize;
            let stride = a.0 as usize * 4;
            let h = rows(&a).min(rows(&b));
            let row_eq = |i: usize, j: usize| a.2[i * stride..(i + 1) * stride]
                == b.2[j * stride..(j + 1) * stride];
            let identical = (0..h).filter(|i| row_eq(*i, *i)).count();
            println!("同一位置相等的行：{identical}/{h}");
            let mut best = (0usize, 0usize);
            for dy in 1..h.min(300) {
                let n = (0..h - dy).filter(|i| row_eq(*i, *i + dy)).count();
                if n > best.0 {
                    best = (n, dy);
                }
            }
            println!("最佳位移 dy={} 命中 {} 行（共 {}）", best.1, best.0, h - best.1);
        }
    }
    unsafe {
        let _ = SetCursorPos(rect.left + (rect.right - rect.left) / 2,
                             rect.top + (rect.bottom - rect.top) / 2);
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    for i in 0..steps {
        wheel(3);
        std::thread::sleep(std::time::Duration::from_millis(350));
        let h = capture::longshot::tick();
        println!("第 {} 步：累计 {h} px", i + 1);
    }
    match capture::longshot::finish() {
        Some(c) => {
            let path = std::env::current_exe().unwrap().parent().unwrap().join("long-probe.png");
            if let Some(png) = c.png_bytes() {
                let _ = std::fs::write(&path, png);
            }
            println!("成品 {}x{} → {}", c.w, c.h, path.display());
        }
        None => println!("没有成品"),
    }
}
