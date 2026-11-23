//! 合成一次右键拖动，用来在没有真鼠标（或远程调试）时驱动手势链路。
//!
//! 钩子只忽略带"ZXGS"魔数的自身注入，所以这里的合成事件会被正常处理。
//! 建议同时开 `ZHUXIA_GESTURE_TRACE=1` 看会话日志。
//!
//! 注意：`SetCursorPos` 不产生低级钩子事件，移动必须用 `SendInput` 绝对移动；
//! 抬手后也不要立刻把光标挪回去，否则抬手事件会带上起点坐标，凭空多出一个方向。
//!
//! ```bash
//! # 终端 A（调试版常驻，跳过提权）
//! $env:ZHUXIA_NO_ELEVATE=1; $env:ZHUXIA_GESTURE_TRACE=1
//! .\target\debug\zhuxia.exe
//! # 终端 B
//! cargo run -p gesture --example mouse_drag -- "←"
//! cargo run -p gesture --example mouse_drag -- "↙↘↙" --len 150
//! ```
//!
//! 松手会真的执行命中的动作（`←` 是 Alt+←，基本无害；`↙↘↙` 会打开截图），
//! 所以别在重要窗口里跑。

use std::time::Duration;

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut input = String::new();
    let mut len = 120.0f64;
    let mut interval = 7u64;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--len" => len = args.next().and_then(|v| v.parse().ok()).unwrap_or(len),
            "--interval" => interval = args.next().and_then(|v| v.parse().ok()).unwrap_or(interval),
            other => {
                if !input.is_empty() {
                    input.push(' ');
                }
                input.push_str(other);
            }
        }
    }
    let dirs = parse_pattern(&input);
    if dirs.is_empty() {
        eprintln!("用法: mouse_drag \"←\" [--len 120]");
        std::process::exit(2);
    }

    unsafe {
        let mut origin = POINT::default();
        let _ = GetCursorPos(&mut origin);
        // 从鼠标当前位置开始，把路径塞进屏幕里（留 10% 边距）
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let mut cursor = (origin.x as f64, origin.y as f64);
        let margin = 0.12;
        let min_x = vx as f64 + vw as f64 * margin;
        let max_x = vx as f64 + vw as f64 * (1.0 - margin);
        let min_y = vy as f64 + vh as f64 * margin;
        let max_y = vy as f64 + vh as f64 * (1.0 - margin);
        // 先按整条路径的总位移把起点挪到能容纳它的位置
        let (mut dx_sum, mut dy_sum) = (0.0, 0.0);
        for d in &dirs {
            let rad = d.angle().to_radians();
            dx_sum += rad.cos() * len;
            dy_sum += rad.sin() * len;
        }
        cursor.0 = (cursor.0 - dx_sum.min(0.0)).clamp(min_x, max_x);
        cursor.1 = (cursor.1 - dy_sum.min(0.0)).clamp(min_y, max_y);
        // 绝对移动是注入事件；钩子只忽略自己打的魔数，所以这里能被正常处理。
        let (nx, ny) = normalize(cursor.0, cursor.1);
        send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, nx, ny);
        let _ = SetCursorPos(cursor.0 as i32, cursor.1 as i32);
        std::thread::sleep(Duration::from_millis(60));

        // 右键按下 → 沿方向串移动 → 抬起
        send_mouse(MOUSEEVENTF_RIGHTDOWN, 0, 0);
        std::thread::sleep(Duration::from_millis(40));
        for d in &dirs {
            let rad = d.angle().to_radians();
            let steps = 14;
            let step = len / steps as f64;
            for _ in 0..steps {
                cursor.0 += rad.cos() * step;
                cursor.1 += rad.sin() * step;
                // 必须用 SendInput 绝对移动：SetCursorPos 不产生低级钩子事件
                let (nx, ny) = normalize(cursor.0, cursor.1);
                send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, nx, ny);
                std::thread::sleep(Duration::from_millis(interval));
            }
        }
        std::thread::sleep(Duration::from_millis(30));
        send_mouse(MOUSEEVENTF_RIGHTUP, 0, 0);
        // 抬起事件的坐标由系统在"处理时"的指针位置决定；立刻把光标挪回去会让抬起
        // 带着起点坐标到达钩子，凭空多出一个方向。等它落地再恢复。
        std::thread::sleep(Duration::from_millis(400));
        let _ = SetCursorPos(origin.x, origin.y);
    }

    let arrows: String = dirs.iter().map(|d| d.arrow()).collect();
    println!("已合成 {arrows}（{} 段，步长 {len}px，间隔 {interval}ms）", dirs.len());
}

fn normalize(x: f64, y: f64) -> (i32, i32) {
    unsafe {
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let nx = ((x - vx as f64) * 65535.0 / (vw.max(1) - 1) as f64).round() as i32;
        let ny = ((y - vy as f64) * 65535.0 / (vh.max(1) - 1) as f64).round() as i32;
        (nx.clamp(0, 65535), ny.clamp(0, 65535))
    }
}

fn send_mouse(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, dx: i32, dy: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn parse_pattern(text: &str) -> Vec<gesture::model::Direction> {
    use gesture::model::Direction;
    let mut out: Vec<Direction> = Vec::new();
    let mut token = String::new();
    let push = |d: Direction, out: &mut Vec<Direction>| {
        if out.last() != Some(&d) {
            out.push(d);
        }
    };
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            token.push(ch);
            continue;
        }
        if !token.is_empty() {
            if let Some(d) = Direction::parse(&token) {
                push(d, &mut out);
            }
            token.clear();
        }
        if let Some(d) = Direction::parse(&ch.to_string()) {
            push(d, &mut out);
        }
    }
    if !token.is_empty() {
        if let Some(d) = Direction::parse(&token) {
            push(d, &mut out);
        }
    }
    out
}
