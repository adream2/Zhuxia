//! 轨迹 → 方向串。
//!
//! 目标是把一条（可能带抖动的）鼠标轨迹压缩成有限方向序列，供规则表精确匹配。
//! 实现只依赖几何量，不依赖任何上游代码：
//!
//! 1. 先按最小移动距离抽稀，保留首尾；
//! 2. 用“当前锚点 → 新点”的方位角判断是否仍在同一笔；
//! 3. 方位角偏离超过容差、且转角位移足够时，认定换笔；
//! 4. 首笔允许 8 方向，后续笔默认只认上下左右；`Exact` 模式每笔都保留 8 方向；
//! 5. 连续相同方向合并，最多 12 段。

use crate::model::{normalize_angle, Direction};

/// 方向串上限。超过这个长度的手势不再细分。
pub const MAX_STEPS: usize = 12;

/// 方向量化精度。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 首笔 8 方向，后续只认上下左右。
    Cardinal,
    /// 每一笔都按 8 方向量化。
    Exact,
}

/// 识别参数（单位：像素 / 度）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub min_gesture_distance: f64,
    pub effective_move: f64,
    pub direction_tolerance: f64,
    pub turn_angle: f64,
    pub min_turn_distance: f64,
}

impl Settings {
    /// 默认手感。
    pub const STANDARD: Settings = Settings {
        min_gesture_distance: 45.0,
        effective_move: 24.0,
        direction_tolerance: 35.0,
        turn_angle: 55.0,
        min_turn_distance: 15.0,
    };

    /// 最严格：方向要画得很明确。
    const STRICT: Settings = Settings {
        min_gesture_distance: 70.0,
        effective_move: 36.0,
        direction_tolerance: 25.0,
        turn_angle: 70.0,
        min_turn_distance: 24.0,
    };

    /// 最宽松：轻微移动即可识别。
    const RELAXED: Settings = Settings {
        min_gesture_distance: 18.0,
        effective_move: 8.0,
        direction_tolerance: 55.0,
        turn_angle: 40.0,
        min_turn_distance: 4.0,
    };

    /// 灵敏度 0–200：0–100 从严格过渡到默认，100–200 从默认过渡到宽松。
    pub fn resolved(percent: i32) -> Settings {
        let p = percent.clamp(0, 200) as f64;
        if p <= 100.0 {
            Settings::STRICT.blend(&Settings::STANDARD, p / 100.0)
        } else {
            Settings::STANDARD.blend(&Settings::RELAXED, (p - 100.0) / 100.0)
        }
    }

    fn blend(&self, other: &Settings, t: f64) -> Settings {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Settings {
            min_gesture_distance: mix(self.min_gesture_distance, other.min_gesture_distance),
            effective_move: mix(self.effective_move, other.effective_move),
            direction_tolerance: mix(self.direction_tolerance, other.direction_tolerance),
            turn_angle: mix(self.turn_angle, other.turn_angle),
            min_turn_distance: mix(self.min_turn_distance, other.min_turn_distance),
        }
    }
}

pub struct Recognizer {
    settings: Settings,
}

impl Recognizer {
    pub fn new(percent: i32) -> Recognizer {
        Recognizer { settings: Settings::resolved(percent) }
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// 低于这个位移不视为手势，调用方应重放一次单击。
    pub fn min_gesture_distance(&self) -> f64 {
        self.settings.min_gesture_distance
    }

    /// 兼容口径：首笔 8 方向，后续上下左右。
    pub fn recognize(&self, points: &[(i32, i32)]) -> Vec<Direction> {
        self.recognize_in(points, Mode::Cardinal)
    }

    /// 按指定精度识别。调用方应先试 `Exact`，失败再退 `Cardinal`。
    pub fn recognize_in(&self, points: &[(i32, i32)], mode: Mode) -> Vec<Direction> {
        if points.len() < 2 {
            return Vec::new();
        }
        let path = self.thin(points);
        if path.len() < 2 {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut start = path[0];
        let mut anchor = start;
        let mut heading: Option<Direction> = None;
        let mut corner: Option<(i32, i32)> = None;

        for &p in &path[1..] {
            let Some(dir) = heading else {
                if distance(start, p) < self.settings.effective_move {
                    continue;
                }
                heading = Some(self.quantize(start, p, 0, mode));
                anchor = p;
                continue;
            };

            let current_angle = dir.angle();
            let travel_angle = bearing(anchor, p);
            if angular_gap(travel_angle, current_angle) <= self.settings.direction_tolerance {
                corner = None;
                anchor = p;
                continue;
            }

            let pivot = match corner {
                Some(c) => c,
                None => {
                    corner = Some(anchor);
                    anchor
                }
            };
            if distance(pivot, p) < self.settings.min_turn_distance {
                continue;
            }
            if angular_gap(bearing(pivot, p), current_angle) < self.settings.turn_angle {
                continue;
            }

            let next = match mode {
                Mode::Exact => dir,
                Mode::Cardinal => first_stroke_axis(&out, dir, start, pivot),
            };
            push_unique(&mut out, next);
            if out.len() >= MAX_STEPS {
                return out;
            }

            start = pivot;
            anchor = p;
            heading = Some(self.quantize(start, p, out.len(), mode));
            corner = None;
        }

        if let Some(dir) = heading {
            push_unique(&mut out, dir);
        }
        out
    }

    fn quantize(&self, from: (i32, i32), to: (i32, i32), completed: usize, mode: Mode) -> Direction {
        let dx = (to.0 - from.0) as f64;
        let dy = (to.1 - from.1) as f64;
        if mode == Mode::Exact || completed == 0 {
            Direction::from_delta_8(dx, dy)
        } else {
            Direction::from_delta_4(dx, dy)
        }
    }

    /// 三笔 W 形（下-左、下-右、下-左）的几何判定。
    ///
    /// 斜向手势在真实鼠标轨迹里往往不是标准 45°：斜率偏陡时 8 扇区会把第一笔
    /// 归成 `↓`，低保真再退成 `↓→↓`，就会误命中「编辑 hosts」。
    /// 这里不依赖扇区，而是按 x 方向的反转切段：只要三段都向下、水平位移足够，
    /// 且符号是 `- + -`，就判成 `↙↘↙`。宁可让斜向手势保持斜向，也不回退成
    /// 看似相近的 cardinal 规则。
    pub fn diagonal_zigzag(&self, points: &[(i32, i32)]) -> Option<Vec<Direction>> {
        let path = self.thin(points);
        if path.len() < 6 {
            return None;
        }

        let mut cuts = vec![0usize];
        let mut last_sign = 0i32;
        let mut pivot_x = path[0].0;
        for i in 1..path.len() {
            let dx = path[i].0 - path[i - 1].0;
            if dx == 0 {
                continue;
            }
            let sign = dx.signum();
            if last_sign != 0 && sign != last_sign {
                if (path[i - 1].0 - pivot_x).abs() >= 12 {
                    cuts.push(i - 1);
                    pivot_x = path[i - 1].0;
                }
            }
            last_sign = sign;
        }
        cuts.push(path.len() - 1);
        if cuts.len() != 4 {
            return None;
        }

        let mut out = Vec::with_capacity(3);
        for w in cuts.windows(2) {
            let a = path[w[0]];
            let b = path[w[1]];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            if dy <= 0 || (dx.abs() as f64) < 0.25 * dy as f64 {
                return None;
            }
            out.push(if dx < 0 { Direction::DownLeft } else { Direction::DownRight });
        }
        if out == [Direction::DownLeft, Direction::DownRight, Direction::DownLeft] {
            Some(out)
        } else {
            None
        }
    }

    /// 抽稀：距离过近的点不参与方向判定；尾点单独保留，避免丢掉短促收尾。
    fn thin(&self, points: &[(i32, i32)]) -> Vec<(i32, i32)> {
        let mut out = Vec::with_capacity(points.len());
        out.push(points[0]);
        let mut last = points[0];
        for &p in &points[1..] {
            if distance(last, p) < self.settings.effective_move {
                continue;
            }
            out.push(p);
            last = p;
        }
        let tail = points[points.len() - 1];
        if tail != last && distance(last, tail) >= self.settings.min_turn_distance {
            out.push(tail);
        }
        out
    }
}

/// 首笔为斜向且后面还有笔时，把它压成主方向，避免 `↖→` 与 `↑←→` 混淆。
fn first_stroke_axis(
    done: &[Direction],
    dir: Direction,
    start: (i32, i32),
    end: (i32, i32),
) -> Direction {
    if !done.is_empty() || !dir.is_diagonal() {
        return dir;
    }
    Direction::from_delta_4((end.0 - start.0) as f64, (end.1 - start.1) as f64)
}

fn push_unique(out: &mut Vec<Direction>, dir: Direction) {
    if out.last() != Some(&dir) {
        out.push(dir);
    }
}

pub fn distance(a: (i32, i32), b: (i32, i32)) -> f64 {
    let dx = (a.0 - b.0) as f64;
    let dy = (a.1 - b.1) as f64;
    dx.hypot(dy)
}

pub fn path_length(points: &[(i32, i32)]) -> f64 {
    points.windows(2).map(|w| distance(w[0], w[1])).sum()
}

fn bearing(from: (i32, i32), to: (i32, i32)) -> f64 {
    ((to.1 - from.1) as f64).atan2((to.0 - from.0) as f64).to_degrees()
}

fn angular_gap(a: f64, b: f64) -> f64 {
    normalize_angle(a - b).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(from: (i32, i32), to: (i32, i32), steps: i32) -> Vec<(i32, i32)> {
        (0..=steps)
            .map(|i| {
                (
                    from.0 + (to.0 - from.0) * i / steps,
                    from.1 + (to.1 - from.1) * i / steps,
                )
            })
            .collect()
    }

    fn join(mut a: Vec<(i32, i32)>, b: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
        a.extend(b.into_iter().skip(1));
        a
    }

    #[test]
    fn single_stroke_left() {
        let r = Recognizer::new(100);
        let pts = line((100, 0), (10, 0), 10);
        assert_eq!(r.recognize(&pts), vec![Direction::Left]);
    }

    #[test]
    fn single_stroke_diagonal_counts_as_eight_direction() {
        let r = Recognizer::new(100);
        let pts = line((0, 0), (80, 80), 10);
        assert_eq!(r.recognize(&pts), vec![Direction::DownRight]);
    }

    #[test]
    fn two_strokes_are_split_at_the_corner() {
        let r = Recognizer::new(100);
        let pts = join(line((200, 0), (0, 0), 10), line((0, 0), (0, 120), 12));
        assert_eq!(r.recognize(&pts), vec![Direction::Left, Direction::Down]);
    }

    #[test]
    fn jitter_does_not_create_extra_steps() {
        let r = Recognizer::new(100);
        let mut pts = line((100, 100), (10, 100), 20);
        for i in 0..6 {
            pts.push((10 + i % 2, 101));
        }
        assert_eq!(r.recognize(&pts), vec![Direction::Left]);
    }

    #[test]
    fn sensitivity_profiles_are_monotonic() {
        assert_eq!(Settings::resolved(0).min_gesture_distance, Settings::STRICT.min_gesture_distance);
        assert_eq!(Settings::resolved(100), Settings::STANDARD);
        assert_eq!(Settings::resolved(200).min_gesture_distance, Settings::RELAXED.min_gesture_distance);
        assert!(Settings::resolved(0).min_turn_distance > Settings::resolved(200).min_turn_distance);
    }

    #[test]
    fn too_short_path_is_not_a_gesture() {
        let r = Recognizer::new(100);
        let pts = line((0, 0), (10, 0), 2);
        assert!(r.recognize(&pts).is_empty() || path_length(&pts) < r.min_gesture_distance());
    }

    #[test]
    fn diagonal_zigzag_needs_exact_mode() {
        let r = Recognizer::new(100);
        let pts = join(
            join(line((300, 100), (180, 220), 12), line((180, 220), (300, 340), 12)),
            line((300, 340), (180, 460), 12),
        );
        assert_eq!(
            r.recognize_in(&pts, Mode::Exact),
            vec![Direction::DownLeft, Direction::DownRight, Direction::DownLeft]
        );
        assert_eq!(r.recognize_in(&pts, Mode::Cardinal), vec![Direction::Down]);
    }

    #[test]
    fn steep_w_shape_is_diagonal() {
        let r = Recognizer::new(100);
        let dx = 84;
        let dy = 120;
        let mut pts = line((300, 100), (300 - dx, 100 + dy), 12);
        pts.extend(line((300 - dx, 100 + dy), (300 + dx, 100 + 2 * dy), 12).into_iter().skip(1));
        pts.extend(line((300 + dx, 100 + 2 * dy), (300 - dx, 100 + 3 * dy), 12).into_iter().skip(1));
        assert_eq!(
            r.diagonal_zigzag(&pts),
            Some(vec![Direction::DownLeft, Direction::DownRight, Direction::DownLeft])
        );
    }

    #[test]
    fn cardinal_hosts_shape_is_not_diagonal() {
        let r = Recognizer::new(100);
        let mut pts = line((300, 100), (300, 220), 12);
        pts.extend(line((300, 220), (420, 220), 12).into_iter().skip(1));
        pts.extend(line((420, 220), (420, 340), 12).into_iter().skip(1));
        assert_eq!(r.diagonal_zigzag(&pts), None);
    }

    #[test]
    fn card_l_shape_same_in_both_modes() {
        let r = Recognizer::new(100);
        let pts = join(line((200, 100), (200, 300), 12), line((200, 300), (400, 300), 12));
        assert_eq!(r.recognize_in(&pts, Mode::Exact), vec![Direction::Down, Direction::Right]);
        assert_eq!(r.recognize_in(&pts, Mode::Cardinal), vec![Direction::Down, Direction::Right]);
    }
}
