//! W 形判定探针：锐角/陡斜的 ↙↘↙ 是否稳定命中，↓→↓ 是否保持不误判。

use gesture::recognizer::{Mode, Recognizer};

fn line(from: (i32, i32), to: (i32, i32), steps: i32) -> Vec<(i32, i32)> {
    (0..=steps)
        .map(|i| (from.0 + (to.0 - from.0) * i / steps, from.1 + (to.1 - from.1) * i / steps))
        .collect()
}

fn main() {
    let r = Recognizer::new(100);
    for ratio in [0.4, 0.6, 0.8, 1.0, 1.4, 2.0, 3.0] {
        let dx = (60.0 * ratio) as i32;
        let dy = 120;
        let mut pts = line((300, 100), (300 - dx, 100 + dy), 12);
        pts.extend(line((300 - dx, 100 + dy), (300 + dx, 100 + 2 * dy), 12).into_iter().skip(1));
        pts.extend(line((300 + dx, 100 + 2 * dy), (300 - dx, 100 + 3 * dy), 12).into_iter().skip(1));
        let exact: String = r.recognize_in(&pts, Mode::Exact).iter().map(|d| d.arrow()).collect();
        let card: String = r.recognize_in(&pts, Mode::Cardinal).iter().map(|d| d.arrow()).collect();
        let zig = r.diagonal_zigzag(&pts).map(|v| v.iter().map(|d| d.arrow()).collect::<String>());
        println!("W ratio={ratio:.1} Exact={exact:6} Card={card:5} zigzag={zig:?}");
    }

    // 真正的 ↓→↓ 不应被判成 W
    let mut hosts = line((300, 100), (300, 220), 12);
    hosts.extend(line((300, 220), (420, 220), 12).into_iter().skip(1));
    hosts.extend(line((420, 220), (420, 340), 12).into_iter().skip(1));
    println!("hosts ↓→↓ zigzag={:?}", r.diagonal_zigzag(&hosts));
}
