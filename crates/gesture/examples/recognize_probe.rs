//! 手势识别探针：给一条方向串，打印识别器在两种保真度下的结果与命中的默认规则。
//!
//! 用途：排查"我明明划的是 ↙↘↙，为什么没反应"这类问题——先看它被认成了什么。
//!
//! ```bash
//! cargo run -p gesture --example recognize_probe -- "↙↘↙"
//! cargo run -p gesture --example recognize_probe -- "down,right"
//! cargo run -p gesture --example recognize_probe -- "ld dr dl" --len 160
//! ```
//!
//! 输出里的 `Exact` 是"每一笔 8 方向"，`Cardinal` 是上游口径（多笔画只认上下左右）。
//! 匹配顺序是 **Exact 优先、Cardinal 兜底**，与 `engine::match_best` 一致。

use gesture::matcher;
use gesture::model::{self, Direction, MouseButton, ScopeContext};
use gesture::recognizer::{path_length, Mode, Recognizer};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut input = String::new();
    let mut len = 120.0f64;
    let mut sensitivity = 100i32;
    let mut app: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--len" => len = args.next().and_then(|v| v.parse().ok()).unwrap_or(len),
            "--sensitivity" => sensitivity = args.next().and_then(|v| v.parse().ok()).unwrap_or(sensitivity),
            "--app" => app = args.next().filter(|v| !v.is_empty()),
            other => {
                if !input.is_empty() {
                    input.push(' ');
                }
                input.push_str(other);
            }
        }
    }
    if input.trim().is_empty() {
        eprintln!("用法: recognize_probe \"↙↘↙\" [--len 120] [--sensitivity 100] [--app chrome]");
        std::process::exit(2);
    }

    let dirs = match parse_pattern(&input) {
        Some(d) if !d.is_empty() => d,
        _ => {
            eprintln!("方向串解析为空：{input}");
            std::process::exit(2);
        }
    };

    let points = build_path(&dirs, len);
    let rec = Recognizer::new(sensitivity);
    let exact = rec.recognize_in(&points, Mode::Exact);
    let cardinal = rec.recognize_in(&points, Mode::Cardinal);

    let app_label = app.clone();
    println!("输入      : {}{}", arrows(&dirs), app_label.map(|a| format!("（程序={a}）")).unwrap_or_default());
    println!("路径      : {} 段，{} 点，{:.0}px", dirs.len(), points.len(), path_length(&points));
    println!("Exact     : {}", arrows(&exact));
    println!("Cardinal  : {}", arrows(&cardinal));

    matcher::set_config(model::defaults(), model::default_groups());
    let ctx = ScopeContext { app: app.clone(), categories: Vec::new() };
    let hit = if let Some(rule) = matcher::match_rule(&exact, &ctx, MouseButton::Right) {
        Some(("Exact", rule))
    } else {
        matcher::match_rule(&cardinal, &ctx, MouseButton::Right).map(|rule| ("Cardinal", rule))
    };
    match hit {
        Some((pass, rule)) => println!("命中      : 「{}」（{}，{}）", rule.name, rule.action.kind(), pass),
        None => println!("命中      : 无（默认规则里没有这个方向串）"),
    }
}

fn arrows(dirs: &[Direction]) -> String {
    if dirs.is_empty() {
        return "（空）".into();
    }
    dirs.iter().map(|d| d.arrow()).collect()
}

/// 逐字符/逐词解析：支持 `← ↘`、`left,down`、`ld`（连写单字母）。
fn parse_pattern(text: &str) -> Option<Vec<Direction>> {
    let mut out = Vec::new();
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            token.push(ch);
            continue;
        }
        flush(&mut token, &mut out);
        if let Some(d) = Direction::parse(&ch.to_string()) {
            push(&mut out, d);
        }
    }
    flush(&mut token, &mut out);
    Some(out)
}

fn flush(token: &mut String, out: &mut Vec<Direction>) {
    if token.is_empty() {
        return;
    }
    if let Some(d) = Direction::parse(token) {
        push(out, d);
    } else if token.chars().count() > 1
        && token.chars().all(|c| matches!(c.to_ascii_lowercase(), 'u' | 'r' | 'd' | 'l'))
    {
        for c in token.chars() {
            if let Some(d) = Direction::parse(&c.to_string()) {
                push(out, d);
            }
        }
    }
    token.clear();
}

fn push(out: &mut Vec<Direction>, d: Direction) {
    if out.last() != Some(&d) {
        out.push(d);
    }
}

/// 按方向串画一条合成轨迹：每段按该角度的单位向量前进 `len` 像素，段内 12 个采样点。
fn build_path(dirs: &[Direction], len: f64) -> Vec<(i32, i32)> {
    let mut points = vec![(2000, 2000)];
    let mut cursor = (2000.0f64, 2000.0f64);
    for d in dirs {
        let rad = d.angle().to_radians();
        let step = (len / 12.0) as f32 as f64;
        for _ in 0..12 {
            cursor.0 += rad.cos() * step;
            cursor.1 += rad.sin() * step;
            points.push((cursor.0.round() as i32, cursor.1.round() as i32));
        }
    }
    points
}
