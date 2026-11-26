//! PaddleOCR（PP-OCRv4）本地推理：模型可选下载，推理走**系统自带的 Windows ML**。
//!
//! 为什么可行：Win11 / Win10 1809+ 自带 Windows ML（内含 ONNX Runtime），
//! `LearningModel::LoadFromFilePath` 能直接吃 RapidOCR 发布的 `.onnx`。实测本机
//! det 960×960 一次 89ms、rec 320×48 一次 9ms（8 核 CPU），没有 GPU 也够用。
//! 于是"高质量中文 OCR"不需要打包 12MB 的 onnxruntime，只需要按需下载 ~15MB 模型。
//!
//! 流程与 PaddleOCR 一致，但**后处理做了适配截图的简化**：
//!
//! ```text
//! 原图 ──等比缩到 max 960 / 32 倍数──▶ det.onnx ──sigmoid──▶ 概率图
//!   概率图 ──0.3 阈值 + 连通域──▶ 文本框（轴对齐 bbox，不做 minAreaRect/多边形 unclip）
//!   每个框 ──裁切、高 48、宽按比例──▶ rec.onnx ──CTC──▶ 文本
//! ```
//!
//! 简化点：Paddle 原本用 `minAreaRect` + 多边形 unclip 处理倾斜文本；截图里的文字
//! 基本是水平的，用"连通域外接矩形 + 按行高外扩"更简单也更稳（斜置文本会略差）。

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use windows::AI::MachineLearning::*;
use windows::core::{Interface, HSTRING};
use zx_ui::Canvas;

use crate::ocr_model;

const DET_LIMIT: i32 = 960;
const DET_THRESH: f32 = 0.30;
const BOX_THRESH: f32 = 0.50;
const MIN_AREA: i32 = 6;
const REC_H: i32 = 48;
const REC_MAX_W: i32 = 640;
/// 长行分段的源图重叠像素（按比例换算，避免切在字中间）。
const REC_OVERLAP: i32 = 16;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

struct Engine {
    det_model: LearningModel,
    det_session: LearningModelSession,
    rec_model: LearningModel,
    rec_session: LearningModelSession,
    dict: Vec<String>,
    /// rec 输出的类别数（含 blank），从模型输出描述里读。
    rec_classes: usize,
    signature: Vec<(u64, u64)>,
}

thread_local! {
    /// WinML 对象不是 `Send`，而识别就在热键/UI 线程上同步跑，直接放线程局部。
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

fn signature() -> Vec<(u64, u64)> {
    let dir = ocr_model::dir();
    ["det.onnx", "rec.onnx", "keys.txt"]
        .iter()
        .map(|n| {
            let p = dir.join(n);
            let m = std::fs::metadata(&p).ok();
            let len = m.as_ref().map(|m| m.len()).unwrap_or(0);
            let mt = m
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (len, mt)
        })
        .collect()
}

pub fn available() -> bool {
    ocr_model::status().installed
}

/// 模型被删掉后把会话放掉，别让 15MB 模型 + ONNX 工作区一直挂在进程里。
pub fn unload() {
    ENGINE.with(|slot| {
        if slot.borrow().is_some() {
            *slot.borrow_mut() = None;
            zx_core::log::info("PaddleOCR 引擎已释放");
        }
    });
}

/// 引擎空闲多久后释放。
///
/// OCR 是**按需**功能，但 WinML 会话一建就一直挂着：实测 det 会话 183MB、rec 36MB，
/// 识别一次就把常驻进程从 ~40MB 顶到 ~250MB，且永不归还。闲置就放掉，下次再懒加载
/// （实测重新加载 276ms，不值得为它常驻 220MB）。
/// `ZHUXIA_OCR_IDLE_SECS=0` 关闭自动释放。
const IDLE_UNLOAD_SECS: u64 = 120;

static CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// `u64::MAX` = 还没用过。别用 0 当哨兵：第一次 `touch()` 时 CLOCK 刚初始化，
/// `elapsed()` 就是 0 毫秒，会跟"没记录"撞在一起，导致永远不释放。
const NEVER_USED: u64 = u64::MAX;
static LAST_USE_MS: AtomicU64 = AtomicU64::new(NEVER_USED);

fn clock() -> &'static Instant {
    CLOCK.get_or_init(Instant::now)
}

fn idle_limit() -> Duration {
    let secs = std::env::var("ZHUXIA_OCR_IDLE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(IDLE_UNLOAD_SECS);
    Duration::from_secs(secs)
}

/// 记一次使用时间（每次识别时调用）。
fn touch() {
    LAST_USE_MS.store(clock().elapsed().as_millis() as u64, Ordering::Relaxed);
}

/// 引擎已加载且空闲超过阈值就释放。
///
/// **必须在加载它的那个线程上调用**：WinML 会话不是 `Send`，引擎放在 `thread_local`
/// 里，别的线程调 `unload()` 摸到的是空槽（等于啥也没干）。所以调用点放在截图 UI
/// 线程（`hotkeys.rs`）的线程定时器上。
pub fn unload_if_idle() -> bool {
    if ENGINE.with(|slot| slot.borrow().is_none()) {
        return false;
    }
    let limit = idle_limit();
    if limit.is_zero() {
        return false;
    }
    let last = LAST_USE_MS.load(Ordering::Relaxed);
    if last == NEVER_USED {
        return false;
    }
    let idle_ms = clock().elapsed().as_millis() as u64;
    if idle_ms < last.saturating_add(limit.as_millis() as u64) {
        return false;
    }
    unload();
    zx_core::log::info(format!(
        "OCR 引擎空闲 {} 秒，已释放（内存还给系统）",
        idle_ms.saturating_sub(last) / 1000
    ));
    true
}

fn with_engine<R>(f: impl FnOnce(&mut Engine) -> Result<R, String>) -> Result<R, String> {
    ENGINE.with(|slot| {
        let mut g = slot.borrow_mut();
        let sig = signature();
        let stale = g.as_ref().map(|e| e.signature != sig).unwrap_or(true);
        if stale {
            *g = None;
            let e = load_engine(sig)?;
            *g = Some(e);
        }
        f(g.as_mut().unwrap())
    })
}

fn load_engine(sig: Vec<(u64, u64)>) -> Result<Engine, String> {
    let dir = ocr_model::dir();
    let load = |name: &str| -> Result<(LearningModel, LearningModelSession), String> {
        let path = dir.join(name);
        let model = LearningModel::LoadFromFilePath(&HSTRING::from(path.to_string_lossy().as_ref()))
            .map_err(|e| format!("加载 {} 失败：{e}", path.display()))?;
        let session = LearningModelSession::CreateFromModel(&model)
            .map_err(|e| format!("创建会话失败（{name}）：{e}"))?;
        Ok((model, session))
    };
    let started = std::time::Instant::now();
    let (det_model, det_session) = load("det.onnx")?;
    let (rec_model, rec_session) = load("rec.onnx")?;
    let raw = std::fs::read_to_string(dir.join("keys.txt"))
        .map_err(|e| format!("读字典失败：{e}"))?;
    let mut dict: Vec<String> = raw
        .lines()
        .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // PaddleOCR 的 use_space_char=True：字典末尾追加一个空格
    dict.push(" ".to_string());
    let rec_classes = output_classes(&rec_model)
        .filter(|c| *c > 1)
        .unwrap_or(dict.len() + 1);
    if rec_classes < dict.len() + 1 {
        zx_core::log::warn(format!(
            "字典 {} 字，但模型输出 {rec_classes} 类，可能不匹配", dict.len()));
    }
    zx_core::log::info(format!(
        "PaddleOCR 引擎就绪：字典 {} 字 / 输出 {} 类，加载 {}ms",
        dict.len(), rec_classes, started.elapsed().as_millis()
    ));
    Ok(Engine { det_model, det_session, rec_model, rec_session, dict, rec_classes, signature: sig })
}

/// 识别一张图。返回按行拼接的文本。
pub fn recognize(canvas: &Canvas) -> Result<String, String> {
    if !available() {
        return Err("PaddleOCR 模型未安装".into());
    }
    // 从这次识别开始重新计时（空闲自动释放看的是这个时间戳）。
    touch();
    with_engine(|e| {
        let boxes = detect(e, canvas)?;
        if boxes.is_empty() {
            return Ok(String::new());
        }
        let debug = std::env::var("ZHUXIA_CAPTURE_PPOCR_DEBUG").ok().as_deref() == Some("1");
        let mut items: Vec<(i32, i32, i32, i32, String)> = Vec::new();
        for (bi, b) in boxes.iter().enumerate() {
            let rect = clamp_box(*b, canvas.w, canvas.h);
            if rect.2 < 2 || rect.3 < 2 {
                continue;
            }
            if debug {
                if let Some(c) = crate::output::crop_canvas(canvas, rect) {
                    if let Some(png) = c.png_bytes() {
                        let p = zx_core::cfg::run_dir().join(format!("ppocr-box-{bi}.png"));
                        let _ = std::fs::write(&p, png);
                        zx_core::log::info(format!("box {bi}: {:?} → {}", rect, p.display()));
                    }
                }
            }
            match recognize_line(e, canvas, rect) {
                Ok(t) => {
                    let t = t.trim().to_string();
                    if !t.is_empty() {
                        items.push((rect.1, rect.3, rect.0, rect.2, t));
                    }
                }
                Err(err) => zx_core::log::warn(format!("单行识别失败：{err}")),
            }
        }
        Ok(join_lines(items))
    })
}

/// 识别一个**已知矩形**里的文字（UI 元素框 / 用户框选），跳过 det。
///
/// 这条路径是"OCR 准"的关键：整屏识别时 [`DET_LIMIT`] 会把 4K 屏压到 1/4，
/// 12px 的界面文字在 det 阶段就已经糊掉、框都出不来。既然元素框已经有了，
/// 就直接裁出来送 rec——小字被放大到 48 高，准确率是另一个量级。
///
/// 返回 `(文本, 置信度)`。置信度在长行切段时取各段最小值（一段错，整块就算错）。
pub fn recognize_region(
    canvas: &Canvas,
    rect: (i32, i32, i32, i32),
) -> Result<(String, f32), String> {
    if !available() {
        return Err("PaddleOCR 模型未安装".into());
    }
    let (x, y, w, h) = clamp_box(rect, canvas.w, canvas.h);

    // 多行元素（段落、表格单元格）：跳过 det 反而更差，裁出来走完整流程。
    // 关键是**先裁再识别**，这样不再受整屏 960 上限的降采样影响。
    if h as f32 > REC_H as f32 * 2.2 {
        let Some(sub) = crate::output::crop_canvas(canvas, (x, y, w, h)) else {
            return Err("裁切失败".into());
        };
        let text = recognize(&sub)?;
        return Ok((text, 0.80));
    }

    touch();
    with_engine(|e| {
        // 上下留白很多的元素（按钮里的文字）不要按整个框高放大，否则字被压扁。
        let ratio = (REC_H as f32 / h.max(1) as f32).min(6.0);
        let full_w = (w as f32 * ratio).round().max(8.0) as i32;
        if full_w <= REC_MAX_W {
            return rec_segment_scored(e, canvas, (x, y, w, h), full_w.clamp(16, REC_MAX_W))
                .map(|(t, s)| (t.trim().to_string(), s));
        }
        // 单行但很长（日志行、地址栏）：按 640 宽切段，段间重叠。
        let mut text = String::new();
        let mut worst = 1f32;
        let mut x_src = 0i32;
        while x_src < w {
            let seg_src = (((REC_MAX_W as f32) / ratio).round() as i32).min(w - x_src);
            if seg_src <= 0 {
                break;
            }
            let dst_w = ((seg_src as f32 * ratio).round() as i32).clamp(16, REC_MAX_W);
            if let Ok((t, s)) = rec_segment_scored(e, canvas, (x + x_src, y, seg_src, h), dst_w) {
                text.push_str(&t);
                worst = worst.min(s);
            }
            if x_src + seg_src >= w {
                break;
            }
            let step = seg_src - REC_OVERLAP.max(2);
            if step <= 0 {
                break;
            }
            x_src += step;
        }
        Ok((text.trim().to_string(), worst))
    })
}

/// 只做 det：返回整图的文本框（原图坐标）。
///
/// 给元素融合层用——UIA 框不到的文字（自绘 UI、图片里的字、canvas 画出来的标签）
/// 靠这批框补上，这就是"识别范围"的兜底。
pub fn detect_lines(canvas: &Canvas) -> Result<Vec<(i32, i32, i32, i32)>, String> {
    if !available() {
        return Err("PaddleOCR 模型未安装".into());
    }
    touch();
    with_engine(|e| detect(e, canvas))
}

/// 按行聚合：纵向中心接近的框拼成一行。
/// 一行文字：竖直位置 + 若干横向片段（x 升序）。
///
/// 表格识别和普通拼接都要"先分行"，所以这里抽出来共用。
pub struct TextRow {
    pub top: i32,
    pub bottom: i32,
    /// (x, w, 文本)
    pub parts: Vec<(i32, i32, String)>,
}

impl TextRow {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, (_, _, t)) in self.parts.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(t);
        }
        out
    }
}

/// 把识别碎片聚成行：(top, h, x, w, text)。
///
/// 判定"同一行"用**垂直重叠**，不是中心点距离。之前拿固定 20px 当行高、比中心点差
/// 10px 以内就算同一行，字号一大就散、行距一紧就并。按重叠比例判：两个框竖直方向
/// 重叠超过较矮那个的一半，就算同一行。
pub fn rows_of(mut items: Vec<(i32, i32, i32, i32, String)>) -> Vec<TextRow> {
    items.sort_by_key(|(top, _, x, _, _)| (*top, *x));
    let mut rows: Vec<TextRow> = Vec::new();
    for (top, h, x, w, text) in items {
        let bottom = top + h.max(1);
        let mut hit: Option<usize> = None;
        for (i, row) in rows.iter_mut().enumerate() {
            let ov = bottom.min(row.bottom) - top.max(row.top);
            let shorter = (bottom - top).min(row.bottom - row.top).max(1);
            if ov * 2 > shorter {
                row.top = row.top.min(top);
                row.bottom = row.bottom.max(bottom);
                hit = Some(i);
                break;
            }
        }
        match hit {
            Some(i) => rows[i].parts.push((x, w, text)),
            None => rows.push(TextRow { top, bottom, parts: vec![(x, w, text)] }),
        }
    }
    for r in rows.iter_mut() {
        r.parts.sort_by_key(|(x, _, _)| *x);
    }
    rows.sort_by_key(|r| r.top);
    rows
}

fn join_lines(items: Vec<(i32, i32, i32, i32, String)>) -> String {
    let mut out = String::new();
    for (i, r) in rows_of(items).iter().enumerate() {
        if i > 0 {
            out.push(char::from(10));
        }
        out.push_str(&r.text());
    }
    out
}

/// 把一块区域按**行列结构**识别成 Markdown 表格。
///
/// 做法：det 出行框 → 逐框识别 → 按垂直重叠聚成行 → 按水平区间聚成列 → 按格填字。
/// 这不是真正的表格线条检测，靠的是"表格里的文字天然按列对齐"，对截图里的数据表
/// （设置页、报表、Excel 截图）够用；画了合并单元格的复杂表会退化成空格子。
/// 表格识别的软截止（毫秒）。
///
/// **为什么不丢到工作线程**：WinML 的 `LearningModelSession` 不是 `Send`，引擎放在
/// `thread_local` 里（见 `with_engine`），换线程就得重新加载 det+rec 两个 ONNX，
/// 那点开销比这一秒大得多。而 `Canvas` 也不是 `Send`（握着 HDC）。
/// 所以留在 UI 线程，只做两件事：**限制总时长**（Windows 5 秒才判"未响应"，
/// 2.5 秒的安全垫足够）+ 超时后返回已识别到的部分，而不是让用户盯着黑屏等。
const TABLE_DEADLINE_MS: u128 = 2500;

pub fn table_markdown(canvas: &Canvas) -> Result<String, String> {
    if !available() {
        return Err("PaddleOCR 模型未安装".into());
    }
    touch();
    with_engine(|e| {
        let boxes = detect(e, canvas)?;
        let mut items: Vec<(i32, i32, i32, i32, String)> = Vec::new();
        let start = std::time::Instant::now();
        let mut cut = false;
        for b in boxes.iter() {
            if start.elapsed().as_millis() > TABLE_DEADLINE_MS {
                cut = true;
                break;
            }
            let rect = clamp_box(*b, canvas.w, canvas.h);
            if rect.2 < 2 || rect.3 < 2 {
                continue;
            }
            if let Ok(t) = recognize_line(e, canvas, rect) {
                let t = t.trim().to_string();
                if !t.is_empty() {
                    items.push((rect.1, rect.3, rect.0, rect.2, t));
                }
            }
        }
        if cut {
            zx_core::log::warn(format!(
                "表格识别超出 {}ms，只处理了 {}/{} 行（图片过大）",
                TABLE_DEADLINE_MS,
                items.len(),
                boxes.len()
            ));
        }
        if items.is_empty() {
            return Err("没识别到文字（表格至少要两行两列）".into());
        }
        Ok(to_markdown(rows_of(items)))
    })
}

/// 行 → 列 → Markdown。列按水平区间聚类，中心点相近的算同一列。
fn to_markdown(rows: Vec<TextRow>) -> String {
    if rows.len() < 2 {
        return String::new();
    }
    // 列：所有片段的水平中心，间隔超过中位宽度的一半就断开
    let mut centers: Vec<i32> = rows
        .iter()
        .flat_map(|r| r.parts.iter().map(|(x, w, _)| x + w / 2))
        .collect();
    if centers.is_empty() {
        return String::new();
    }
    centers.sort_unstable();
    let widths: Vec<i32> = rows
        .iter()
        .flat_map(|r| r.parts.iter().map(|(_, w, _)| *w))
        .collect();
    let mut sorted_w = widths.clone();
    sorted_w.sort_unstable();
    let med_w = sorted_w.get(sorted_w.len() / 2).copied().unwrap_or(40).max(8);
    let mut cols: Vec<i32> = Vec::new();
    for c in centers {
        match cols.last() {
            Some(prev) if (c - prev).abs() <= med_w / 2 => {}
            _ => cols.push(c),
        }
    }
    if cols.len() < 2 {
        return String::new();
    }

    let mut grid: Vec<Vec<String>> = vec![vec![String::new(); cols.len()]; rows.len()];
    for (ri, row) in rows.iter().enumerate() {
        for (x, w, text) in &row.parts {
            let c = x + w / 2;
            let ci = cols
                .iter()
                .enumerate()
                .min_by_key(|(_, cc)| (*cc - c).abs())
                .map(|(i, _)| i)
                .unwrap_or(0);
            if grid[ri][ci].is_empty() {
                grid[ri][ci] = text.clone();
            } else {
                grid[ri][ci].push(' ');
                grid[ri][ci].push_str(text);
            }
        }
    }

    let mut out = String::new();
    let row_line = |cells: &[String]| {
        let mut l = String::from("|");
        for c in cells {
            l.push(' ');
            l.push_str(&c.replace('|', "/"));
            l.push_str(" |");
        }
        l
    };
    out.push_str(&row_line(&grid[0]));
    out.push(char::from(10));
    out.push('|');
    for _ in 0..cols.len() {
        out.push_str(" --- |");
    }
    for row in grid.iter().skip(1) {
        out.push(char::from(10));
        out.push_str(&row_line(row));
    }
    out
}


// ---------------------------------------------------------------- det
// ---------------------------------------------------------------- det

/// 文本框：(x, y, w, h)，原图坐标。
type BBox = (i32, i32, i32, i32);

fn detect(e: &mut Engine, canvas: &Canvas) -> Result<Vec<BBox>, String> {
    let (dw, dh) = det_size(canvas.w, canvas.h);
    let input = to_tensor(canvas, (0, 0, canvas.w, canvas.h), dw, dh, &MEAN, &STD);
    let tensor = TensorFloat::CreateFromShapeArrayAndDataArray(&[1, 3, dh as i64, dw as i64], &input)
        .map_err(|e| format!("det 输入张量失败：{e}"))?;
    let binding = LearningModelBinding::CreateFromSession(&e.det_session)
        .map_err(|e| format!("det 绑定失败：{e}"))?;
    let name = input_name(&e.det_model).unwrap_or_else(|| "x".into());
    binding
        .Bind(&HSTRING::from(name.as_str()), &tensor)
        .map_err(|e| format!("det 绑输入失败：{e}"))?;
    let out = e.det_session.Evaluate(&binding, &HSTRING::new())
        .map_err(|e| format!("det 推理失败：{e}"))?;
    let prob = tensor_data(&out).ok_or_else(|| "det 没有输出".to_string())?;
    let expect = (dw * dh) as usize;
    if prob.len() < expect {
        return Err(format!("det 输出长度 {} < {}", prob.len(), expect));
    }
    let sx = canvas.w as f32 / dw as f32;
    let sy = canvas.h as f32 / dh as f32;
    let boxes = components(&prob[..expect], dw as usize, dh as usize);
    let mut out: Vec<BBox> = boxes
        .into_iter()
        .map(|(x, y, w, h)| {
            let x0 = ((x as f32) * sx).floor() as i32;
            let y0 = ((y as f32) * sy).floor() as i32;
            let x1 = (((x + w) as f32) * sx).ceil() as i32;
            let y1 = (((y + h) as f32) * sy).ceil() as i32;
            (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
        })
        // 只丢太小和几乎铺满整幅的（后者基本都是背景/边框的误检）。
        // 之前用"高度 < 画布一半"，把「只截了一行字」的图整块过滤掉了。
        .filter(|(_, _, w, h)| *w >= 6 && *h >= 6 && *h <= canvas.h.max(8))
        .collect();
    out.sort_by_key(|(x, y, _, _)| (*y, *x));
    Ok(out)
}

fn det_size(w: i32, h: i32) -> (i32, i32) {
    let ratio = (DET_LIMIT as f32 / w.max(h) as f32).min(1.0);
    let nw = ((w as f32 * ratio) as i32).max(32);
    let nh = ((h as f32 * ratio) as i32).max(32);
    // 32 的倍数（DBNet 有 5 次下采样），向上取整
    (((nw + 31) / 32) * 32, ((nh + 31) / 32) * 32)
}

/// 连通域 → 外接矩形（det 坐标）。截图里的文字是水平的，bbox 足够。
fn components(prob: &[f32], w: usize, h: usize) -> Vec<BBox> {
    let mut seen = vec![false; w * h];
    let mut out: Vec<BBox> = Vec::new();
    let mut stack: Vec<usize> = Vec::with_capacity(4096);
    for start in 0..w * h {
        if seen[start] || prob[start] < DET_THRESH {
            continue;
        }
        seen[start] = true;
        stack.clear();
        stack.push(start);
        let (mut minx, mut maxx, mut miny, mut maxy) = (w, 0usize, h, 0usize);
        let (mut count, mut sum) = (0i32, 0f32);
        while let Some(idx) = stack.pop() {
            let (x, y) = (idx % w, idx / w);
            minx = minx.min(x);
            maxx = maxx.max(x);
            miny = miny.min(y);
            maxy = maxy.max(y);
            count += 1;
            sum += prob[idx];
            if x > 0 && !seen[idx - 1] && prob[idx - 1] >= DET_THRESH {
                seen[idx - 1] = true;
                stack.push(idx - 1);
            }
            if x + 1 < w && !seen[idx + 1] && prob[idx + 1] >= DET_THRESH {
                seen[idx + 1] = true;
                stack.push(idx + 1);
            }
            if y > 0 && !seen[idx - w] && prob[idx - w] >= DET_THRESH {
                seen[idx - w] = true;
                stack.push(idx - w);
            }
            if y + 1 < h && !seen[idx + w] && prob[idx + w] >= DET_THRESH {
                seen[idx + w] = true;
                stack.push(idx + w);
            }
        }
        if count < MIN_AREA || (sum / count as f32) < BOX_THRESH {
            continue;
        }
        let bw = (maxx - minx + 1) as i32;
        let bh = (maxy - miny + 1) as i32;
        // 按行高外扩：DBNet 的概率图是**收缩后**的文字区域（训练时 shrink 0.4），
        // 必须再放回去，否则上下会被切掉一截——实测 30% 不够，字高会少 1/3。
        let pad_y = ((bh as f32 * 0.75) as i32).clamp(2, 32);
        let pad_x = ((bh as f32 * 0.30) as i32).clamp(2, 20);
        out.push((
            (minx as i32 - pad_x).max(0),
            (miny as i32 - pad_y).max(0),
            bw + pad_x * 2,
            bh + pad_y * 2,
        ));
    }
    out
}

fn clamp_box(b: BBox, cw: i32, ch: i32) -> BBox {
    let x0 = b.0.clamp(0, cw.saturating_sub(1));
    let y0 = b.1.clamp(0, ch.saturating_sub(1));
    let x1 = (b.0 + b.2).clamp(x0 + 1, cw);
    let y1 = (b.1 + b.3).clamp(y0 + 1, ch);
    (x0, y0, x1 - x0, y1 - y0)
}

// ---------------------------------------------------------------- rec

fn recognize_line(e: &mut Engine, canvas: &Canvas, b: BBox) -> Result<String, String> {
    let (bx, by, bw, bh) = b;
    let ratio = REC_H as f32 / bh as f32;
    let full_w = ((bw as f32) * ratio).round() as i32;
    if full_w <= REC_MAX_W {
        return rec_segment(e, canvas, b, full_w.clamp(16, REC_MAX_W));
    }
    // 太长：按 640 宽切段，段间重叠 REC_OVERLAP 个源图像素
    let mut text = String::new();
    let mut x_src = 0i32;
    while x_src < bw {
        let seg_src = (((REC_MAX_W as f32) / ratio).round() as i32).min(bw - x_src);
        if seg_src <= 0 {
            break;
        }
        let dst_w = ((seg_src as f32 * ratio).round() as i32).clamp(16, REC_MAX_W);
        let rect = (bx + x_src, by, seg_src, bh);
        if let Ok(t) = rec_segment(e, canvas, rect, dst_w) {
            text.push_str(&t);
        }
        if x_src + seg_src >= bw {
            break;
        }
        let step = seg_src - REC_OVERLAP.max(2);
        if step <= 0 {
            break;
        }
        x_src += step;
    }
    Ok(text)
}

fn rec_segment(e: &mut Engine, canvas: &Canvas, rect: BBox, dst_w: i32) -> Result<String, String> {
    rec_segment_scored(e, canvas, rect, dst_w).map(|(t, _)| t)
}

fn rec_segment_scored(
    e: &mut Engine,
    canvas: &Canvas,
    rect: BBox,
    dst_w: i32,
) -> Result<(String, f32), String> {
    let (x, y, w, h) = rect;
    let input = to_tensor(canvas, (x, y, w, h), dst_w, REC_H, &[0.5, 0.5, 0.5], &[0.5, 0.5, 0.5]);
    let tensor = TensorFloat::CreateFromShapeArrayAndDataArray(
        &[1, 3, REC_H as i64, dst_w as i64], &input)
        .map_err(|e| format!("rec 输入张量失败：{e}"))?;
    let binding = LearningModelBinding::CreateFromSession(&e.rec_session)
        .map_err(|e| format!("rec 绑定失败：{e}"))?;
    let name = input_name(&e.rec_model).unwrap_or_else(|| "x".into());
    binding
        .Bind(&HSTRING::from(name.as_str()), &tensor)
        .map_err(|e| format!("rec 绑输入失败：{e}"))?;
    let out = e.rec_session.Evaluate(&binding, &HSTRING::new())
        .map_err(|e| format!("rec 推理失败：{e}"))?;
    let px = tensor_data(&out).ok_or_else(|| "rec 没有输出".to_string())?;
    let classes = e.rec_classes.max(1);
    if px.len() < classes {
        return Ok((String::new(), 0.0));
    }
    let steps = px.len() / classes;
    let mut text = String::new();
    let mut last = 0usize;
    // 置信度：只累加"真的出了一个字"的那些步，blank 步会把平均值拉虚高。
    let mut conf = 0f32;
    let mut conf_n = 0usize;
    for t in 0..steps {
        let base = t * classes;
        let row = &px[base..base + classes];
        let (mut best, mut best_i) = (f32::MIN, 0usize);
        for (i, v) in row.iter().enumerate() {
            if *v > best {
                best = *v;
                best_i = i;
            }
        }
        // CTC：0 = blank，连续重复只取一次
        if best_i != 0 && best_i != last {
            if let Some(ch) = e.dict.get(best_i - 1) {
                text.push_str(ch);
                conf += best;
                conf_n += 1;
            }
        }
        last = best_i;
    }
    let score = if conf_n == 0 { 0.0 } else { conf / conf_n as f32 };
    Ok((text, score))
}

// ---------------------------------------------------------------- WinML 小工具

fn input_name(model: &LearningModel) -> Option<String> {
    let features = model.InputFeatures().ok()?;
    let d = features.GetAt(0).ok()?;
    d.Name().ok().map(|s| s.to_string())
}

/// rec 模型的输出类别数（`[1, T, C]` 的 C）。
fn output_classes(model: &LearningModel) -> Option<usize> {
    let features = model.OutputFeatures().ok()?;
    let d = features.GetAt(0).ok()?;
    let shape = d.cast::<TensorFeatureDescriptor>().ok()?.Shape().ok()?;
    let n = shape.Size().ok()?;
    let mut dims = Vec::new();
    for i in 0..n {
        dims.push(shape.GetAt(i).ok()?);
    }
    dims.last().map(|v| (*v).max(0) as usize)
}

/// 取第一个输出张量的数据（一维展开）。
fn tensor_data(result: &LearningModelEvaluationResult) -> Option<Vec<f32>> {
    let outputs = result.Outputs().ok()?;
    let it = outputs.First().ok()?;
    if !matches!(it.HasCurrent(), Ok(true)) {
        return None;
    }
    let kv = it.Current().ok()?;
    let obj = kv.Value().ok()?;
    let t = obj.cast::<TensorFloat>().ok()?;
    let view = t.GetAsVectorView().ok()?;
    let len = view.Size().ok()? as usize;
    let mut data = Vec::with_capacity(len);
    for i in 0..len {
        if let Ok(v) = view.GetAt(i as u32) {
            data.push(v);
        }
    }
    Some(data)
}

// ---------------------------------------------------------------- 张量预处理

/// 双线性缩放 + 归一化，输出 CHW 的 f32。`rect` 是源图矩形（原图像素）。
///
/// det 用 ImageNet 的 mean/std，rec 用 0.5/0.5（与 PaddleOCR 默认一致）。
fn to_tensor(canvas: &Canvas, rect: (i32, i32, i32, i32), dw: i32, dh: i32,
             mean: &[f32; 3], std: &[f32; 3]) -> Vec<f32> {
    let (rx, ry, rw, rh) = rect;
    let src = canvas.pixels();
    let stride = canvas.stride();
    let plane = (dw * dh) as usize;
    let mut out = vec![0f32; plane * 3];
    let scale_x = rw as f32 / dw as f32;
    let scale_y = rh as f32 / dh as f32;
    for dy in 0..dh {
        for dx in 0..dw {
            let fx = rx as f32 + (dx as f32 + 0.5) * scale_x - 0.5;
            let fy = ry as f32 + (dy as f32 + 0.5) * scale_y - 0.5;
            let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let o = (dy * dw + dx) as usize;
            for c in 0..3 {
                let p00 = sample(src, stride, canvas.w, canvas.h, x0, y0, c);
                let p10 = sample(src, stride, canvas.w, canvas.h, x0 + 1, y0, c);
                let p01 = sample(src, stride, canvas.w, canvas.h, x0, y0 + 1, c);
                let p11 = sample(src, stride, canvas.w, canvas.h, x0 + 1, y0 + 1, c);
                let top = p00 + (p10 - p00) * tx;
                let bottom = p01 + (p11 - p01) * tx;
                let v = (top + (bottom - top) * ty) / 255.0;
                out[c * plane + o] = (v - mean[c]) / std[c];
            }
        }
    }
    out
}

#[inline]
fn sample(px: &[u8], stride: usize, w: i32, h: i32, x: i32, y: i32, c: usize) -> f32 {
    let x = x.clamp(0, w - 1) as usize;
    let y = y.clamp(0, h - 1) as usize;
    // 内存是 BGRA；c=0/1/2 依次取 B/G/R。
    // PaddleOCR 的预处理按 BGR 喂网络（cv2 解码的顺序），所以这里也按 BGR 取。
    let o = y * stride + x * 4;
    let v = match c {
        0 => px[o],
        1 => px[o + 1],
        _ => px[o + 2],
    };
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn det_size_is_multiple_of_32() {
        for (w, h) in [(1920, 1080), (300, 90), (4000, 20000), (100, 100)] {
            let (dw, dh) = det_size(w, h);
            assert_eq!(dw % 32, 0);
            assert_eq!(dh % 32, 0);
            assert!(dw <= DET_LIMIT + 31 && dh <= DET_LIMIT + 31);
        }
    }

    #[test]
    fn join_lines_groups_nearby_rows() {
        // (top, h, x, text)：同一行的两个片段竖直重叠，第二行差得远
        let items = vec![
            (10, 18, 0, 50, "hello".to_string()),
            (11, 18, 100, 50, "world".to_string()),
            (60, 18, 0, 60, "second".to_string()),
        ];
        assert_eq!(join_lines(items), "hello world\nsecond");
    }

    #[test]
    fn join_lines_keeps_tight_leading_separate() {
        // 行距很紧（12px 字高、14px 行距）时不能并成一行——UI 里这种密度很常见
        let items = vec![
            (0, 12, 0, 40, "第一行".to_string()),
            (14, 12, 0, 40, "第二行".to_string()),
            (28, 12, 0, 40, "第三行".to_string()),
        ];
        assert_eq!(join_lines(items).lines().count(), 3);
    }

    #[test]
    fn to_markdown_builds_a_table() {
        // 两行三列，列心对齐
        let items = vec![
            (0, 20, 10, 60, "姓名".to_string()),
            (0, 20, 200, 60, "年龄".to_string()),
            (0, 20, 400, 60, "城市".to_string()),
            (30, 20, 10, 60, "张三".to_string()),
            (30, 20, 200, 40, "28".to_string()),
            (30, 20, 400, 90, "北京".to_string()),
        ];
        let md = to_markdown(rows_of(items));
        let lines: Vec<&str> = md.lines().collect();
        assert_eq!(lines.len(), 3, "表头 + 分隔 + 一行数据");
        assert_eq!(lines[0], "| 姓名 | 年龄 | 城市 |");
        assert!(lines[1].starts_with("| --- |"));
        assert_eq!(lines[2], "| 张三 | 28 | 北京 |");
    }

    #[test]
    fn to_markdown_refuses_single_column() {
        // 只有一列的"表格"没有意义，返回空串让调用方走普通拼接
        let items = vec![
            (0, 20, 10, 60, "第一行".to_string()),
            (30, 20, 10, 60, "第二行".to_string()),
        ];
        assert!(to_markdown(rows_of(items)).is_empty());
    }

    #[test]
    fn components_finds_two_lines() {
        // 40x20 的概率图，两行各 10x4 的实心块
        let (w, h) = (40usize, 20usize);
        let mut prob = vec![0f32; w * h];
        for y in 4..8 {
            for x in 2..12 {
                prob[y * w + x] = 0.9;
            }
        }
        for y in 13..17 {
            for x in 2..12 {
                prob[y * w + x] = 0.9;
            }
        }
        let boxes = components(&prob, w, h);
        assert_eq!(boxes.len(), 2);
        // 上下两个框不应重叠
        assert!(boxes[0].1 < boxes[1].1);
    }
}
