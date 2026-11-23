//! 规则匹配：**先分组、后通用**。
//!
//! 两级结构（MouseInc 的做法，也是"同一个手势在不同软件里干不同事"最省心的模型）：
//!
//! ```text
//! 当前窗口进程 ──▶ 按顺序找第一个包含它的分组 ──▶ 组内规则精确匹配
//!                        │ 没命中
//!                        ▼
//!                   通用规则（gesture.rules，仍支持 app: / category: 窄化）
//! ```
//!
//! 分组只解决"这组程序共用一套规则"；组内规则的作用域就是组本身，所以组内 `scope` 字段被忽略。
//! 通用规则保留上游的三级作用域优先级（`app:` > `category:` > `global`），给需要更细控制的用户用。
//!
//! 滚轮规则同样分两级：组内的滚轮规则优先于通用滚轮规则。

use std::sync::{OnceLock, RwLock};

use crate::model::{app_key, Direction, Group, MouseButton, Rule, Scope, ScopeContext, WheelDir};

#[derive(Default)]
pub struct Config {
    pub global: Vec<Rule>,
    pub groups: Vec<Group>,
}

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();

fn config() -> &'static RwLock<Config> {
    CONFIG.get_or_init(|| RwLock::new(Config::default()))
}

pub fn set_config(global: Vec<Rule>, groups: Vec<Group>) {
    *config().write().unwrap_or_else(|p| p.into_inner()) = Config { global, groups };
}

pub fn rule_count() -> usize {
    let cfg = config().read().unwrap_or_else(|p| p.into_inner());
    cfg.global.len() + cfg.groups.iter().map(|g| g.rules.len()).sum::<usize>()
}

pub fn group_count() -> usize {
    config().read().unwrap_or_else(|p| p.into_inner()).groups.len()
}

/// 当前规则表里有没有"按住某键再滚轮"的规则（两级都算）。
///
/// 钩子用它决定要不要吞滚轮：**没有滚轮规则时必须放行**，否则用户会莫名其妙滚不动页面。
pub fn has_wheel_rules(button: MouseButton) -> bool {
    let cfg = config().read().unwrap_or_else(|p| p.into_inner());
    cfg.global.iter().any(|r| r.button == button && r.wheel.is_some())
        || cfg
            .groups
            .iter()
            .flat_map(|g| g.rules.iter())
            .any(|r| r.button == button && r.wheel.is_some())
}

pub fn match_rule(pattern: &[Direction], ctx: &ScopeContext, button: MouseButton) -> Option<Rule> {
    let cfg = config().read().unwrap_or_else(|p| p.into_inner());
    match_in(&cfg, pattern, ctx, button)
}

pub fn match_wheel(
    dir: WheelDir,
    pattern: &[Direction],
    ctx: &ScopeContext,
    button: MouseButton,
) -> Option<Rule> {
    let cfg = config().read().unwrap_or_else(|p| p.into_inner());
    match_wheel_in(&cfg, dir, pattern, ctx, button)
}

fn match_in(cfg: &Config, pattern: &[Direction], ctx: &ScopeContext, button: MouseButton) -> Option<Rule> {
    if let Some(group) = group_for(cfg, ctx.app.as_deref()) {
        if let Some(rule) = pick(&group.rules, pattern, ctx, button) {
            return Some(rule);
        }
    }
    pick(&cfg.global, pattern, ctx, button)
}

fn match_wheel_in(
    cfg: &Config,
    dir: WheelDir,
    pattern: &[Direction],
    ctx: &ScopeContext,
    button: MouseButton,
) -> Option<Rule> {
    if let Some(group) = group_for(cfg, ctx.app.as_deref()) {
        if let Some(rule) = pick_wheel(&group.rules, dir, pattern, ctx, button) {
            return Some(rule);
        }
    }
    pick_wheel(&cfg.global, dir, pattern, ctx, button)
}

/// 按配置顺序找第一个"包含当前进程"的组。
fn group_for<'a>(cfg: &'a Config, app: Option<&str>) -> Option<&'a Group> {
    let app = app?;
    cfg.groups.iter().find(|g| g.apps.iter().any(|entry| app_matches(entry, app)))
}

/// `entry` 已归一化：小写、无 `.exe`；以 `*` 结尾表示前缀匹配（`360*`）。
fn app_matches(entry: &str, app: &str) -> bool {
    if let Some(prefix) = entry.strip_suffix('*') {
        !prefix.is_empty() && app.starts_with(prefix)
    } else {
        entry.eq_ignore_ascii_case(app)
    }
}

fn pick(rules: &[Rule], pattern: &[Direction], ctx: &ScopeContext, button: MouseButton) -> Option<Rule> {
    let mut best: Option<(&Rule, i32)> = None;
    for rule in rules {
        if rule.button != button || rule.wheel.is_some() || rule.pattern != pattern {
            continue;
        }
        let Some(priority) = scope_priority(&rule.scope, ctx) else { continue };
        if best.is_none_or(|(_, p)| priority > p) {
            best = Some((rule, priority));
        }
    }
    best.map(|(r, _)| r.clone())
}

fn pick_wheel(
    rules: &[Rule],
    dir: WheelDir,
    pattern: &[Direction],
    ctx: &ScopeContext,
    button: MouseButton,
) -> Option<Rule> {
    let mut best: Option<(&Rule, i32)> = None;
    for rule in rules {
        if rule.button != button || rule.wheel != Some(dir) || rule.pattern != pattern {
            continue;
        }
        let Some(priority) = scope_priority(&rule.scope, ctx) else { continue };
        if best.is_none_or(|(_, p)| priority > p) {
            best = Some((rule, priority));
        }
    }
    best.map(|(r, _)| r.clone())
}

fn scope_priority(scope: &Scope, ctx: &ScopeContext) -> Option<i32> {
    match scope {
        Scope::Global => Some(0),
        Scope::App(name) => {
            let current = app_key(ctx.app.as_deref()?);
            if current.eq_ignore_ascii_case(name) { Some(i32::MAX) } else { None }
        }
        Scope::Category(name) => {
            let idx = ctx
                .categories
                .iter()
                .position(|c| c.eq_ignore_ascii_case(name))?;
            Some(ctx.categories.len() as i32 - idx as i32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, VolumeOp, WindowOp};

    fn rule(pattern: &[Direction], scope: Scope, button: MouseButton, action: Action) -> Rule {
        Rule {
            pattern: pattern.to_vec(),
            scope,
            button,
            wheel: None,
            on_wheel: false,
            name: action.kind().into(),
            action,
        }
    }

    fn cfg(global: Vec<Rule>, groups: Vec<Group>) -> Config {
        Config { global, groups }
    }

    fn group(name: &str, apps: &[&str], rules: Vec<Rule>) -> Group {
        Group {
            name: name.into(),
            apps: apps.iter().map(|a| app_key(a)).collect(),
            rules,
        }
    }

    #[test]
    fn app_rule_beats_category_and_global() {
        let rules = vec![
            rule(&[Direction::Left], Scope::Global, MouseButton::Right, Action::Hotkey(vec![0x25])),
            rule(
                &[Direction::Left],
                Scope::Category("浏览器".into()),
                MouseButton::Right,
                Action::Window(WindowOp::Minimize),
            ),
            rule(
                &[Direction::Left],
                Scope::App("chrome".into()),
                MouseButton::Right,
                Action::Window(WindowOp::Close),
            ),
        ];
        let c = cfg(rules, vec![]);
        let ctx = ScopeContext { app: Some("chrome".into()), categories: vec!["浏览器".into()] };
        assert_eq!(
            match_in(&c, &[Direction::Left], &ctx, MouseButton::Right).unwrap().action,
            Action::Window(WindowOp::Close)
        );

        let ctx = ScopeContext { app: Some("notepad".into()), categories: vec!["浏览器".into()] };
        assert_eq!(
            match_in(&c, &[Direction::Left], &ctx, MouseButton::Right).unwrap().action,
            Action::Window(WindowOp::Minimize)
        );

        let ctx = ScopeContext { app: Some("notepad".into()), categories: vec![] };
        assert_eq!(
            match_in(&c, &[Direction::Left], &ctx, MouseButton::Right).unwrap().action,
            Action::Hotkey(vec![0x25])
        );
    }

    #[test]
    fn group_wins_over_global_and_falls_back() {
        let c = cfg(
            vec![
                rule(&[Direction::Down], Scope::Global, MouseButton::Right, Action::Window(WindowOp::Minimize)),
                rule(&[Direction::Up], Scope::Global, MouseButton::Right, Action::Window(WindowOp::ToggleMaximize)),
            ],
            vec![group(
                "浏览器",
                &["chrome.exe", "msedge"],
                vec![rule(&[Direction::Down], Scope::Global, MouseButton::Right, Action::Hotkey(vec![0x11, 0x57]))],
            )],
        );

        // 浏览器里 ↓ 用组内规则（关标签），↑ 组里没有 → 落回通用
        let chrome = ScopeContext { app: Some("chrome".into()), categories: vec![] };
        assert_eq!(
            match_in(&c, &[Direction::Down], &chrome, MouseButton::Right).unwrap().action,
            Action::Hotkey(vec![0x11, 0x57])
        );
        assert_eq!(
            match_in(&c, &[Direction::Up], &chrome, MouseButton::Right).unwrap().action,
            Action::Window(WindowOp::ToggleMaximize)
        );

        // 非组内程序：↓ 走通用
        let notepad = ScopeContext { app: Some("notepad".into()), categories: vec![] };
        assert_eq!(
            match_in(&c, &[Direction::Down], &notepad, MouseButton::Right).unwrap().action,
            Action::Window(WindowOp::Minimize)
        );
    }

    #[test]
    fn group_prefix_matching() {
        let c = cfg(
            vec![rule(&[Direction::Down], Scope::Global, MouseButton::Right, Action::Window(WindowOp::Close))],
            vec![group(
                "国产浏览器",
                &["360*"],
                vec![rule(&[Direction::Down], Scope::Global, MouseButton::Right, Action::Hotkey(vec![0x11, 0x57]))],
            )],
        );
        let ctx = ScopeContext { app: Some("360chrome".into()), categories: vec![] };
        assert_eq!(
            match_in(&c, &[Direction::Down], &ctx, MouseButton::Right).unwrap().action,
            Action::Hotkey(vec![0x11, 0x57])
        );
        let ctx = ScopeContext { app: Some("chrome".into()), categories: vec![] };
        assert_eq!(
            match_in(&c, &[Direction::Down], &ctx, MouseButton::Right).unwrap().action,
            Action::Window(WindowOp::Close)
        );
    }

    #[test]
    fn earlier_category_wins() {
        let rules = vec![
            rule(&[Direction::Down], Scope::Category("A".into()), MouseButton::Right, Action::Window(WindowOp::Minimize)),
            rule(&[Direction::Down], Scope::Category("B".into()), MouseButton::Right, Action::Window(WindowOp::Close)),
        ];
        let c = cfg(rules, vec![]);
        let ctx = ScopeContext { app: Some("x".into()), categories: vec!["B".into(), "A".into()] };
        assert_eq!(
            match_in(&c, &[Direction::Down], &ctx, MouseButton::Right).unwrap().action,
            Action::Window(WindowOp::Close)
        );
    }

    #[test]
    fn wheel_rules_are_separate_identity() {
        let mut wheel = rule(&[Direction::Right], Scope::Global, MouseButton::Right, Action::Volume(VolumeOp::Increase));
        wheel.wheel = Some(WheelDir::Up);
        let plain = rule(&[Direction::Right], Scope::Global, MouseButton::Right, Action::Volume(VolumeOp::Decrease));
        let c = cfg(vec![wheel, plain], vec![]);
        let ctx = ScopeContext::default();
        assert_eq!(
            match_in(&c, &[Direction::Right], &ctx, MouseButton::Right).unwrap().action,
            Action::Volume(VolumeOp::Decrease)
        );
        assert_eq!(
            match_wheel_in(&c, WheelDir::Up, &[Direction::Right], &ctx, MouseButton::Right).unwrap().action,
            Action::Volume(VolumeOp::Increase)
        );
        assert!(
            match_wheel_in(&c, WheelDir::Down, &[Direction::Right], &ctx, MouseButton::Right).is_none()
        );
    }

    #[test]
    fn mouse_button_isolated() {
        let c = cfg(
            vec![rule(&[Direction::Up], Scope::Global, MouseButton::Middle, Action::Window(WindowOp::Minimize))],
            vec![],
        );
        let ctx = ScopeContext::default();
        assert!(match_in(&c, &[Direction::Up], &ctx, MouseButton::Right).is_none());
        assert!(match_in(&c, &[Direction::Up], &ctx, MouseButton::Middle).is_some());
    }

    /// 端到端（识别 → 匹配）：`↙↘↙` 折线必须命中内置的「截图」规则。
    #[test]
    fn zigzag_selects_builtin_screenshot_rule() {
        let c = cfg(crate::model::defaults(), crate::model::default_groups());
        let mut points = Vec::new();
        for (from, to) in [
            ((300, 100), (180, 220)),
            ((180, 220), (300, 340)),
            ((300, 340), (180, 460)),
        ] {
            for i in 0..=12 {
                points.push((
                    from.0 + (to.0 - from.0) * i / 12,
                    from.1 + (to.1 - from.1) * i / 12,
                ));
            }
        }

        let rec = crate::recognizer::Recognizer::new(100);
        let exact = rec.recognize_in(&points, crate::recognizer::Mode::Exact);
        let hit = match_in(&c, &exact, &ScopeContext::default(), MouseButton::Right)
            .expect("↙↘↙ 应当命中默认规则");
        assert_eq!(hit.action, Action::Internal("capture".into()));
        assert_eq!(hit.name, "截图");

        // 低保真会压成单笔 ↓，不该命中截图（这正是引擎要"先高保真、再低保真"的原因）
        let cardinal = rec.recognize_in(&points, crate::recognizer::Mode::Cardinal);
        assert!(match_in(&c, &cardinal, &ScopeContext::default(), MouseButton::Right).is_none());
    }

    /// 排除手势 `→↙→` 在通用规则里；浏览器里也不能被组规则吃掉。
    #[test]
    fn exclude_gesture_matches_in_browser_too() {
        let c = cfg(crate::model::defaults(), crate::model::default_groups());
        let ctx = ScopeContext { app: Some("chrome".into()), categories: vec![] };
        let hit = match_in(
            &c,
            &[Direction::Right, Direction::DownLeft, Direction::Right],
            &ctx,
            MouseButton::Right,
        )
        .expect("→↙→ 应当命中排除本程序");
        assert_eq!(hit.action, Action::Internal("toggle-exclude".into()));
    }
}
