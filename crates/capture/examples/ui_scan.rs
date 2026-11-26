//! UI 元素识别端到端探针：UIA 元素 + OCR 文本融合，打印成表。
//!
//! ```text
//! cargo run --release --example ui_scan              # 当前窗口：元素 + 文字
//! cargo run --release --example ui_scan -- 800,600   # 只探这一点的元素
//! ```
//!
//! 前提：`ocr/{det.onnx,rec.onnx,keys.txt}` 已就位（和 `ocr_idle_probe` 一样，
//! 把 `target/release/ocr` 复制到 `target/release/examples/`）。没有模型时会跳过
//! 文字识别，只输出元素框——UIA 通道本身是零模型的。
//!
//! 这个 example 就是覆盖层里要跑的完整流程：
//! 先取元素快照（此时还没有覆盖层，UIA 才能命中真实窗口）→ 抓屏 →
//! 按元素框裁切识别（准）→ 区域内 det 补漏（全）→ 融合。

use std::time::{Duration, Instant};

use capture::{output, ppocr, screen, uielem};
use uielem::{Kind, OcrLine, Rect};
use windows::Win32::Foundation::RECT;

/// 单点探测的总预算。超过就放弃，界面不能为它卡住。
const PROBE_BUDGET: Duration = Duration::from_millis(250);
/// 全窗口遍历的预算。浏览器那种 2000+ 节点的窗口走到这里就截断，先出结果。
const SNAP_BUDGET: Duration = Duration::from_millis(1500);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // ---- 单点模式
    if let Some((x, y)) = args.first().and_then(|a| parse_point(a)) {
        let t0 = Instant::now();
        let got = uielem::probe_at(x, y, PROBE_BUDGET);
        let ms = t0.elapsed().as_millis();
        match got {
            Some(e) => println!(
                "{}\t{}x{} @ ({},{})\t来源={:?}\t文本={:?}",
                e.kind.label(),
                e.rect.w,
                e.rect.h,
                e.rect.x,
                e.rect.y,
                e.source,
                e.name
            ),
            None => println!("({x},{y}) 没有拿到元素（自绘 UI、窗口无 a11y 实现，或 UIA 不可用）"),
        }
        println!("耗时 {ms}ms");
        return;
    }

    // ---- 全窗口模式：先取元素，再抓屏（顺序不能反，覆盖层会挡住命中测试）
    let t0 = Instant::now();
    let elems = uielem::snapshot_capture(SNAP_BUDGET);
    let snap_ms = t0.elapsed().as_millis();
    println!("元素快照 {} 个，{snap_ms}ms", elems.len());
    if elems.is_empty() {
        eprintln!("没有拿到元素：这个窗口没有 a11y 实现（自绘/游戏），只能走视觉通道");
    }

    let Some(frame) = screen::Frame::grab() else {
        eprintln!("抓屏失败");
        return;
    };
    let full = RECT {
        left: frame.x,
        top: frame.y,
        right: frame.x + frame.w,
        bottom: frame.y + frame.h,
    };
    let Some(canvas) = output::crop(&frame, full) else {
        eprintln!("裁切失败");
        return;
    };

    let mut lines: Vec<OcrLine> = Vec::new();

    if ppocr::available() {
        // 1) 按元素框识别：跳过 det、小字放大到 48 高。这是准确率的主要来源。
        let t1 = Instant::now();
        let mut filled = 0usize;
        for e in &elems {
            // 图标和滚动条上没有文字，跑 rec 纯属浪费（一次约 20ms）。
            if e.kind == Kind::Icon || e.kind == Kind::Scroll {
                continue;
            }
            let Some(local) = e.rect.to_local(frame.x, frame.y, canvas.w, canvas.h) else {
                continue;
            };
            let Ok((text, score)) = ppocr::recognize_region(&canvas, local) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            filled += 1;
            lines.push(OcrLine { rect: e.rect, text, score });
        }
        println!("元素内识别 {filled} 条，{}ms", t1.elapsed().as_millis());

        // 2) det 补漏：只在元素覆盖的范围内做。既不会把其他窗口的文字捞进来，
        //    也顺带绕开整屏 960 的降采样——区域越小，det 越准。
        let t2 = Instant::now();
        if let Some((rx, ry, rw, rh)) = union_local(&elems, &frame, &canvas) {
            if let Some(sub) = output::crop_canvas(&canvas, (rx, ry, rw, rh)) {
                match ppocr::detect_lines(&sub) {
                    Ok(boxes) => {
                        let mut added = 0usize;
                        for (bx, by, bw, bh) in boxes {
                            let abs = Rect::new(
                                rx + bx + frame.x,
                                ry + by + frame.y,
                                bw,
                                bh,
                            );
                            let covered = elems.iter().any(|e| {
                                let x0 = abs.x.max(e.rect.x);
                                let y0 = abs.y.max(e.rect.y);
                                let x1 = abs.right().min(e.rect.right());
                                let y1 = abs.bottom().min(e.rect.bottom());
                                x1 > x0 && y1 > y0
                            });
                            if covered {
                                continue;
                            }
                            if let Ok((text, score)) =
                                ppocr::recognize_region(&canvas, (rx + bx, ry + by, bw, bh))
                            {
                                if !text.is_empty() {
                                    lines.push(OcrLine { rect: abs, text, score });
                                    added += 1;
                                }
                            }
                        }
                        println!("det 补漏 {added} 行，{}ms", t2.elapsed().as_millis());
                    }
                    Err(e) => eprintln!("det 失败：{e}"),
                }
            }
        }
    } else {
        eprintln!("PaddleOCR 模型未安装，跳过文字识别（元素框照常输出）");
    }

    let fused = uielem::fuse(elems, &lines);
    print_table(&fused);
    println!("总计 {}ms", t0.elapsed().as_millis());
}

/// 所有元素框在画布上的并集，供 det 限定范围。
fn union_local(
    elems: &[uielem::Element],
    frame: &screen::Frame,
    canvas: &zx_ui::Canvas,
) -> Option<(i32, i32, i32, i32)> {
    let mut acc: Option<(i32, i32, i32, i32)> = None;
    for e in elems {
        let Some((x, y, w, h)) = e.rect.to_local(frame.x, frame.y, canvas.w, canvas.h) else {
            continue;
        };
        acc = Some(match acc {
            None => (x, y, w, h),
            Some((ax, ay, aw, ah)) => {
                let x0 = ax.min(x);
                let y0 = ay.min(y);
                let x1 = (ax + aw).max(x + w);
                let y1 = (ay + ah).max(y + h);
                (x0, y0, x1 - x0, y1 - y0)
            }
        });
    }
    acc
}

fn print_table(items: &[uielem::Element]) {
    println!();
    println!("{:<8} {:<20} {:<6} 文本", "类型", "矩形", "置信");
    println!("{}", "-".repeat(72));
    for e in items {
        let rect = format!("{},{} {}x{}", e.rect.x, e.rect.y, e.rect.w, e.rect.h);
        let text = if e.name.is_empty() { "-" } else { e.name.as_str() };
        println!("{:<8} {:<20} {:<6.2} {}", e.kind.label(), rect, e.score, text);
    }
    println!("{}", "-".repeat(72));
    println!("共 {} 个元素", items.len());
}

fn parse_point(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}
