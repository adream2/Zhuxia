//! 手势会话状态机：按下 → 移动 →（滚轮）→ 抬起。
//!
//! 事件来自 `hook` 的队列，全部在本线程串行处理；UI 只通过 `overlay` 的
//! “写入快照 + PostMessage” 路径更新，避免跨线程画 GDI+。
//!
//! 三个必须保持的行为：
//!
//! - 匹配顺序：先 8 方向精确匹配，再退回首笔 8 方向/后续 4 方向；
//! - 滚轮一旦命中就冻结方向串，后续滚动沿用同一身份；`on_wheel` 立即执行，否则等抬手；
//! - 位移不足时把被吞掉的右键/中键单击重放回去。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::model::{Direction, MouseButton, Rule, ScopeContext, WheelDir};
use crate::recognizer::{distance, path_length, Mode, Recognizer};
use crate::{action, hook, matcher, overlay, target};

/// 采样点之间的最小距离。
const POINT_EPS: f64 = 3.0;
/// 单次手势的轨迹点上限。
const POINT_CAP: usize = 4000;
/// 单次滚轮事件最多展开的格数。
const NOTCH_CAP: i32 = 5;
/// 标准滚轮一格。
const NOTCH: i32 = 120;
/// 抬手点与最后一个移动点的最大可信距离。
const END_JUMP_MAX: f64 = 150.0;

static SENSITIVITY: AtomicI32 = AtomicI32::new(100);
static RUNNING: AtomicBool = AtomicBool::new(false);
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);

fn tick() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// 解析线程仍在工作。钩子用它决定是否继续吞手势键。
pub(crate) fn alive() -> bool {
    RUNNING.load(Ordering::Relaxed)
        && tick().saturating_sub(HEARTBEAT.load(Ordering::Relaxed)) < 3_000
}

pub fn set_sensitivity(percent: i32) {
    SENSITIVITY.store(percent, Ordering::Relaxed);
}

fn recognizer() -> Recognizer {
    Recognizer::new(SENSITIVITY.load(Ordering::Relaxed))
}

/// 先精确、后兼容；返回命中的规则和用于锁定的方向串。
fn lookup(
    points: &[(i32, i32)],
    ctx: &ScopeContext,
    button: MouseButton,
) -> Option<(Rule, Vec<Direction>)> {
    let rec = recognizer();

    // 三笔 W 形优先：斜向手势不能被低保真压成 ↓→↓（编辑 hosts）。
    if let Some(zigzag) = rec.diagonal_zigzag(points) {
        if let Some(rule) = matcher::match_rule(&zigzag, ctx, button) {
            return Some((rule, zigzag));
        }
        // 已经是明确的斜向手势，但没有对应规则：宁可不执行，也不退回 cardinal。
        return None;
    }

    let exact = rec.recognize_in(points, Mode::Exact);
    if !exact.is_empty() {
        if let Some(rule) = matcher::match_rule(&exact, ctx, button) {
            return Some((rule, exact));
        }
    }
    let fallback = rec.recognize_in(points, Mode::Cardinal);
    if fallback != exact {
        if let Some(rule) = matcher::match_rule(&fallback, ctx, button) {
            return Some((rule, fallback));
        }
    }
    None
}

fn lookup_wheel(
    dir: WheelDir,
    points: &[(i32, i32)],
    ctx: &ScopeContext,
    button: MouseButton,
) -> Option<(Rule, Vec<Direction>)> {
    let rec = recognizer();
    let exact = rec.recognize_in(points, Mode::Exact);
    if let Some(rule) = matcher::match_wheel(dir, &exact, ctx, button) {
        return Some((rule, exact));
    }
    let fallback = rec.recognize_in(points, Mode::Cardinal);
    if fallback != exact {
        if let Some(rule) = matcher::match_wheel(dir, &fallback, ctx, button) {
            return Some((rule, fallback));
        }
    }
    None
}

struct Run {
    seq: u64,
    button: MouseButton,
    points: Vec<(i32, i32)>,
    target: isize,
    scope: ScopeContext,
    activate: bool,
    hint: bool,
    hint_ms: u32,
    wheel_active: bool,
    frozen: Option<Vec<Direction>>,
    pending: Option<Rule>,
    shown: Option<String>,
}

/// 手势松开后轨迹继续显示多久。提示（动作名）另有 `hint_duration_ms` 控制。
///
/// 220ms 是"看得清自己画了什么"和"别挡着下一步"之间的取舍：再长就像没结束，
/// 再短会闪一下反而看不清。
const TRAIL_FADE_MS: u32 = 220;

#[derive(Default)]
struct Machine {
    run: Option<Run>,
    seq: u64,
    wheel_dir: Option<WheelDir>,
    wheel_remainder: i32,
}

pub fn start(rx: Receiver<hook::Event>) {
    RUNNING.store(true, Ordering::SeqCst);
    HEARTBEAT.store(tick(), Ordering::Relaxed);

    let spawned = std::thread::Builder::new()
        .name("zx-gesture-core".into())
        .spawn(move || {
            let mut machine = Machine::default();
            loop {
                match rx.recv_timeout(Duration::from_millis(400)) {
                    Ok(ev) => {
                        HEARTBEAT.store(tick(), Ordering::Relaxed);
                        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            machine.handle(ev);
                        }));
                        if ok.is_err() {
                            zx_core::log::warn("手势事件异常：重置会话后继续");
                            machine = Machine::default();
                            overlay::clear();
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        // 空转也刷新，避免静置后钩子误判线程已死。
                        HEARTBEAT.store(tick(), Ordering::Relaxed);
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            RUNNING.store(false, Ordering::SeqCst);
            zx_core::log::info("手势事件队列关闭，解析线程退出");
        });

    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
        zx_core::log::warn("手势解析线程启动失败：手势不会响应（右键不受影响）");
    }
}

fn add_end_point(points: &mut Vec<(i32, i32)>, x: i32, y: i32) {
    let Some(&last) = points.last() else { return };
    let d = distance(last, (x, y));
    if (POINT_EPS..=END_JUMP_MAX).contains(&d) {
        points.push((x, y));
    }
}

impl Machine {
    fn handle(&mut self, ev: hook::Event) {
        match ev {
            hook::Event::Start { button, x, y, .. } => self.begin(button, x, y),
            hook::Event::Move { x, y, .. } => self.moved(x, y),
            hook::Event::Wheel { delta } => self.wheel(delta),
            hook::Event::End { button, x, y, .. } => self.end(button, x, y),
        }
    }

    fn begin(&mut self, button: MouseButton, x: i32, y: i32) {
        self.seq += 1;
        self.wheel_dir = None;
        self.wheel_remainder = 0;

        let cfg = zx_core::cfg::cfg().gesture();
        let by_start = cfg.target_window != "current-window";
        let hwnd = if by_start { target::window_at(x, y) } else { target::foreground() };
        let app = target::process_name(hwnd);
        let scope = ScopeContext::for_app(app, &cfg.apps);

        hook::trace_log(format!(
            "会话 #{} 起点窗口 {hwnd:?} 进程 {:?} 目标模式={}",
            self.seq, scope.app, cfg.target_window
        ));
        overlay::clear();
        self.run = Some(Run {
            seq: self.seq,
            button,
            points: vec![(x, y)],
            target: hwnd.0 as isize,
            scope,
            activate: by_start,
            hint: cfg.hint,
            hint_ms: cfg.hint_duration_ms,
            wheel_active: false,
            frozen: None,
            pending: None,
            shown: None,
        });
    }

    fn moved(&mut self, x: i32, y: i32) {
        let Some(run) = self.run.as_mut() else { return };
        if run.wheel_active && run.frozen.is_some() {
            return;
        }
        let Some(&last) = run.points.last() else { return };
        if distance(last, (x, y)) < POINT_EPS || run.points.len() >= POINT_CAP {
            return;
        }
        run.points.push((x, y));

        let hit = lookup(&run.points, &run.scope, run.button);
        let matched = hit.is_some();

        if run.hint {
            let name = hit.as_ref().map(|(rule, _)| rule.name.clone());
            if name != run.shown {
                match name.as_deref() {
                    Some(n) => hook::trace_log(format!("实时提示：{n}")),
                    None => hook::trace_log("实时提示：无匹配"),
                }
                run.shown = name.clone();
                match name {
                    Some(n) => overlay::show_hint(&n),
                    None => overlay::clear_hint(),
                }
            }
        }
        if run.points.len().is_multiple_of(20) {
            hook::trace_log(format!(
                "轨迹 {} 点，当前匹配={:?}，末点=({x},{y})",
                run.points.len(),
                hit.as_ref().map(|(rule, _)| rule.name.as_str())
            ));
        }
        overlay::show_trail(&run.points, matched);
    }

    fn wheel(&mut self, delta: i32) {
        if self.run.is_none() || delta == 0 {
            return;
        }
        let dir = if delta > 0 { WheelDir::Up } else { WheelDir::Down };
        if self.wheel_dir != Some(dir) {
            self.wheel_dir = Some(dir);
            self.wheel_remainder = 0;
        }
        self.wheel_remainder += delta.abs();
        let mut notches = self.wheel_remainder / NOTCH;
        if notches <= 0 {
            return;
        }
        self.wheel_remainder -= notches * NOTCH;
        notches = notches.min(NOTCH_CAP);

        for _ in 0..notches {
            if self.run.is_none() {
                break;
            }
            self.notch(dir);
        }
    }

    fn notch(&mut self, dir: WheelDir) {
        let Some(run) = self.run.as_mut() else { return };
        run.wheel_active = true;

        let hit = match run.frozen.clone() {
            Some(pattern) => matcher::match_wheel(dir, &pattern, &run.scope, run.button)
                .map(|rule| (rule, pattern)),
            None => lookup_wheel(dir, &run.points, &run.scope, run.button),
        };

        let Some((rule, pattern)) = hit else {
            run.pending = None;
            return;
        };
        if run.frozen.is_none() {
            run.frozen = Some(pattern);
        }
        if run.shown.as_deref() != Some(rule.name.as_str()) {
            run.shown = Some(rule.name.clone());
            if run.hint {
                overlay::show_hint(&rule.name);
            }
        }

        if rule.on_wheel {
            run.pending = None;
            action::dispatch(rule, run.target, run.activate);
        } else {
            run.pending = Some(rule);
        }
    }

    fn end(&mut self, button: MouseButton, x: i32, y: i32) {
        let Some(mut run) = self.run.take() else { return };
        if run.button != button {
            self.run = Some(run);
            return;
        }

        if run.wheel_active {
            add_end_point(&mut run.points, x, y);
            overlay::show_trail(&run.points, true);
            overlay::fade_trail(TRAIL_FADE_MS);
            if let Some(rule) = run.pending.take() {
                if run.hint {
                    overlay::show_hint(&rule.name);
                }
                action::dispatch(rule, run.target, run.activate);
            }
            overlay::settle(run.hint_ms);
            hook::suppress_wheel_for_a_moment();
            return;
        }

        add_end_point(&mut run.points, x, y);

        if run.points.len() < 2 || path_length(&run.points) < recognizer().min_gesture_distance() {
            overlay::clear();
            action::replay_click(run.button == MouseButton::Middle);
            return;
        }

        let hit = lookup(&run.points, &run.scope, run.button);
        if hook::trace_enabled() {
            let rec = recognizer();
            let exact: String =
                rec.recognize_in(&run.points, Mode::Exact).iter().map(|d| d.arrow()).collect();
            let cardinal: String =
                rec.recognize_in(&run.points, Mode::Cardinal).iter().map(|d| d.arrow()).collect();
            hook::trace_log(format!(
                "会话结束：{} 点，exact={exact} cardinal={cardinal}，命中={:?}",
                run.points.len(),
                hit.as_ref().map(|(r, _)| r.name.clone())
            ));
        }

        let Some((rule, _)) = hit else {
            overlay::show_trail(&run.points, false);
            overlay::fade_trail(TRAIL_FADE_MS);
            overlay::settle(run.hint_ms);
            return;
        };

        overlay::show_trail(&run.points, true);
        overlay::fade_trail(TRAIL_FADE_MS);
        if run.hint {
            overlay::show_hint(&rule.name);
        }
        overlay::settle(run.hint_ms);
        action::dispatch(rule, run.target, run.activate);
        let _ = run.seq;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_run() -> Run {
        Run {
            seq: 1,
            button: MouseButton::Right,
            points: vec![(0, 0), (100, 0)],
            target: 0,
            scope: ScopeContext::default(),
            activate: false,
            hint: true,
            hint_ms: 700,
            wheel_active: false,
            frozen: None,
            pending: None,
            shown: None,
        }
    }

    #[test]
    fn wheel_remainder_accumulates_to_one_notch() {
        let mut m = Machine { run: Some(blank_run()), wheel_remainder: 60, ..Default::default() };
        assert_eq!(m.wheel_remainder / NOTCH, 0);
        m.wheel_remainder += 60;
        assert_eq!(m.wheel_remainder / NOTCH, 1);
    }

    #[test]
    fn teleport_end_point_is_ignored() {
        let mut pts = vec![(0, 0)];
        add_end_point(&mut pts, 10, 0);
        assert_eq!(pts.len(), 2);
        add_end_point(&mut pts, 400, 0);
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn tiny_end_point_is_ignored() {
        let mut pts = vec![(0, 0)];
        add_end_point(&mut pts, 1, 0);
        assert_eq!(pts.len(), 1);
    }
}
