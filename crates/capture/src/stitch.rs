//! 拼接：把多张截图合成一张。
//!
//! 拼接在收集阶段完成：进入拼接后当前图是第一块，之后每框选一次追加一块，
//! 回车出成品。这样标注坐标始终相对最终成品。

use std::sync::{Mutex, OnceLock};

use zx_ui::Canvas;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Vertical,
    Horizontal,
    Grid,
}

impl Dir {
    pub fn name(&self) -> &'static str {
        match self {
            Dir::Vertical => "纵向",
            Dir::Horizontal => "横向",
            Dir::Grid => "网格",
        }
    }

    pub fn next(&self) -> Dir {
        match self {
            Dir::Vertical => Dir::Horizontal,
            Dir::Horizontal => Dir::Grid,
            Dir::Grid => Dir::Vertical,
        }
    }
}

pub struct Session {
    pub dir: Dir,
    pub gap: i32,
    pub pieces: Vec<Canvas>,
}

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

fn lock() -> std::sync::MutexGuard<'static, Option<Session>> {
    slot().lock().unwrap_or_else(|p| p.into_inner())
}

pub fn busy() -> bool {
    lock().is_some()
}

pub fn begin(first: Canvas, dir: Dir) -> usize {
    let mut g = lock();
    *g = Some(Session { dir, gap: 16, pieces: vec![first] });
    g.as_ref().map(|s| s.pieces.len()).unwrap_or(0)
}

const MAX_PIECES: usize = 12;
const MAX_PIXELS: i64 = 40_000_000;

pub fn push(piece: Canvas) -> usize {
    let mut g = lock();
    let Some(s) = g.as_mut() else { return 0 };

    let used: i64 = s.pieces.iter().map(|p| p.w as i64 * p.h as i64).sum();
    let add = piece.w as i64 * piece.h as i64;
    if s.pieces.len() >= MAX_PIECES || used + add > MAX_PIXELS {
        zx_core::log::warn(format!(
            "拼接：已达上限（{} 块 / {} 万像素），忽略这一块",
            s.pieces.len(),
            (used + add) / 10_000
        ));
        return 0;
    }
    s.pieces.push(piece);
    s.pieces.len()
}

pub fn count() -> usize {
    lock().as_ref().map(|s| s.pieces.len()).unwrap_or(0)
}

pub fn dir() -> Dir {
    lock().as_ref().map(|s| s.dir).unwrap_or(Dir::Vertical)
}

pub fn set_dir(d: Dir) {
    if let Some(s) = lock().as_mut() {
        s.dir = d;
    }
}

pub fn cancel() {
    *lock() = None;
}

fn columns(n: usize) -> i32 {
    (n as f32).sqrt().ceil() as i32
}

pub fn render() -> Option<Canvas> {
    let g = lock();
    let s = g.as_ref()?;
    if s.pieces.is_empty() {
        return None;
    }
    if s.pieces.len() == 1 {
        let p = &s.pieces[0];
        let mut out = Canvas::new(p.w, p.h, 96);
        out.draw_canvas(p, 0.0, 0.0, p.w as f32, p.h as f32);
        return Some(out);
    }

    let gap = s.gap.max(0);
    let count = s.pieces.len() as i32;
    let max_w = s.pieces.iter().map(|p| p.w).max().unwrap_or(1);
    let max_h = s.pieces.iter().map(|p| p.h).max().unwrap_or(1);

    let (w, h) = match s.dir {
        Dir::Vertical => (
            max_w,
            s.pieces.iter().map(|p| p.h).sum::<i32>() + gap * (count - 1),
        ),
        Dir::Horizontal => (
            s.pieces.iter().map(|p| p.w).sum::<i32>() + gap * (count - 1),
            max_h,
        ),
        Dir::Grid => {
            let cols = columns(s.pieces.len());
            let rows = (count + cols - 1) / cols.max(1);
            (cols * max_w + gap * (cols - 1), rows * max_h + gap * (rows - 1))
        }
    };

    if w <= 0 || h <= 0 {
        return None;
    }
    if w > 12_000 || h > 20_000 || (w as i64 * h as i64) > MAX_PIXELS {
        zx_core::log::warn(format!("拼接成品 {w}x{h} 超过上限，先减少块数"));
        return None;
    }

    let mut out = Canvas::new(w, h, 96);
    match s.dir {
        Dir::Vertical => {
            let mut y = 0;
            for p in &s.pieces {
                let x = (w - p.w) / 2;
                out.draw_canvas(p, x as f32, y as f32, p.w as f32, p.h as f32);
                y += p.h + gap;
            }
        }
        Dir::Horizontal => {
            let mut x = 0;
            for p in &s.pieces {
                let y = (h - p.h) / 2;
                out.draw_canvas(p, x as f32, y as f32, p.w as f32, p.h as f32);
                x += p.w + gap;
            }
        }
        Dir::Grid => {
            let cols = columns(s.pieces.len());
            for (i, p) in s.pieces.iter().enumerate() {
                let c = i as i32 % cols;
                let r = i as i32 / cols;
                let x = c * (max_w + gap) + (max_w - p.w) / 2;
                let y = r * (max_h + gap) + (max_h - p.h) / 2;
                out.draw_canvas(p, x as f32, y as f32, p.w as f32, p.h as f32);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(w: i32, h: i32) -> Canvas {
        Canvas::new(w, h, 96)
    }

    static LOCK: Mutex<()> = Mutex::new(());
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn vertical_stacks_heights_with_gap() {
        let _g = guard();
        cancel();
        begin(c(100, 40), Dir::Vertical);
        push(c(80, 30));
        let out = render().unwrap();
        assert_eq!(out.w, 100);
        assert_eq!(out.h, 40 + 16 + 30);
        cancel();
    }

    #[test]
    fn horizontal_sums_widths() {
        let _g = guard();
        cancel();
        begin(c(50, 20), Dir::Horizontal);
        push(c(70, 40));
        let out = render().unwrap();
        assert_eq!(out.w, 50 + 16 + 70);
        assert_eq!(out.h, 40);
        cancel();
    }

    #[test]
    fn push_stops_at_piece_limit() {
        let _g = guard();
        cancel();
        begin(c(4, 4), Dir::Vertical);
        for _ in 1..MAX_PIECES {
            assert!(push(c(4, 4)) > 0);
        }
        assert_eq!(push(c(4, 4)), 0);
        assert_eq!(count(), MAX_PIECES);
        cancel();
    }

    #[test]
    fn grid_uses_ceil_sqrt_columns() {
        let _g = guard();
        cancel();
        begin(c(40, 30), Dir::Grid);
        push(c(40, 30));
        push(c(40, 30));
        push(c(40, 30));
        let out = render().unwrap();
        assert_eq!(out.w, 40 * 2 + 16);
        assert_eq!(out.h, 30 * 2 + 16);
        cancel();
    }
}
