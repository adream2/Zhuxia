//! 手势模型：方向、鼠标键、作用域、动作、规则与默认表。
//!
//! 这一层只做数据与解析，不碰钩子、不画界面。配置里的规则先经过这里归一化，
//! 再交给匹配器与引擎。

use zx_core::{GestureAppCfg, GestureRuleCfg, GestureSettings};

/// 八个方向，按屏幕坐标顺时针排列。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Up,
    UpRight,
    Right,
    DownRight,
    Down,
    DownLeft,
    Left,
    UpLeft,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::UpRight => "up-right",
            Direction::Right => "right",
            Direction::DownRight => "down-right",
            Direction::Down => "down",
            Direction::DownLeft => "down-left",
            Direction::Left => "left",
            Direction::UpLeft => "up-left",
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Up => "↑",
            Direction::UpRight => "↗",
            Direction::Right => "→",
            Direction::DownRight => "↘",
            Direction::Down => "↓",
            Direction::DownLeft => "↙",
            Direction::Left => "←",
            Direction::UpLeft => "↖",
        }
    }

    /// 该方向的标准角度（屏幕坐标，y 向下为正）。
    pub fn angle(self) -> f64 {
        match self {
            Direction::Right => 0.0,
            Direction::DownRight => 45.0,
            Direction::Down => 90.0,
            Direction::DownLeft => 135.0,
            Direction::Left => 180.0,
            Direction::UpLeft => -135.0,
            Direction::Up => -90.0,
            Direction::UpRight => -45.0,
        }
    }

    pub fn is_diagonal(self) -> bool {
        matches!(
            self,
            Direction::UpRight | Direction::DownRight | Direction::DownLeft | Direction::UpLeft
        )
    }

    /// 把位移量化到 8 方向；45° 扇区，边界居中。
    pub fn from_delta_8(dx: f64, dy: f64) -> Direction {
        const RING: [Direction; 8] = [
            Direction::Right,
            Direction::DownRight,
            Direction::Down,
            Direction::DownLeft,
            Direction::Left,
            Direction::UpLeft,
            Direction::Up,
            Direction::UpRight,
        ];
        let angle = dy.atan2(dx).to_degrees();
        let shifted = wrap_360(angle + 22.5);
        let sector = (shifted / 45.0).floor() as i32;
        RING[sector.rem_euclid(8) as usize]
    }

    /// 把位移量化到 4 方向：取主轴。
    pub fn from_delta_4(dx: f64, dy: f64) -> Direction {
        if dx.abs() > dy.abs() {
            if dx >= 0.0 { Direction::Right } else { Direction::Left }
        } else if dy >= 0.0 {
            Direction::Down
        } else {
            Direction::Up
        }
    }

    pub fn parse(text: &str) -> Option<Direction> {
        let t = text.trim().to_ascii_lowercase();
        Some(match t.as_str() {
            "up" | "u" | "↑" => Direction::Up,
            "up-right" | "upright" | "ur" | "↗" => Direction::UpRight,
            "right" | "r" | "→" => Direction::Right,
            "down-right" | "downright" | "dr" | "↘" => Direction::DownRight,
            "down" | "d" | "↓" => Direction::Down,
            "down-left" | "downleft" | "dl" | "↙" => Direction::DownLeft,
            "left" | "l" | "←" => Direction::Left,
            "up-left" | "upleft" | "ul" | "↖" => Direction::UpLeft,
            _ => return None,
        })
    }
}

/// 归一到 (-180, 180]。
pub fn normalize_angle(angle: f64) -> f64 {
    let mut a = angle % 360.0;
    if a > 180.0 {
        a -= 360.0;
    } else if a < -180.0 {
        a += 360.0;
    }
    a
}

fn wrap_360(angle: f64) -> f64 {
    let mut a = angle % 360.0;
    if a < 0.0 {
        a += 360.0;
    }
    a
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Right,
    Middle,
}

impl MouseButton {
    pub fn parse(text: &str) -> Option<MouseButton> {
        match text.trim().to_ascii_lowercase().as_str() {
            "right" | "r" => Some(MouseButton::Right),
            "middle" | "m" => Some(MouseButton::Middle),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WheelDir {
    Up,
    Down,
}

impl WheelDir {
    pub fn parse(text: &str) -> Option<WheelDir> {
        match text.trim().to_ascii_lowercase().as_str() {
            "up" | "↑" => Some(WheelDir::Up),
            "down" | "↓" => Some(WheelDir::Down),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WheelDir::Up => "up",
            WheelDir::Down => "down",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowOp {
    ToggleTopmost,
    ToggleMaximize,
    Minimize,
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VolumeOp {
    Increase,
    Decrease,
    Mute,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// 虚拟键码序列：正序按下、逆序抬起。
    Hotkey(Vec<u16>),
    Window(WindowOp),
    Program { path: String, args: Vec<String> },
    Volume(VolumeOp),
    /// 宿主内置动作，具体执行由宿主注入。
    Internal(String),
}

impl Action {
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Hotkey(_) => "hotkey",
            Action::Window(_) => "window",
            Action::Program { .. } => "program",
            Action::Volume(_) => "volume",
            Action::Internal(_) => "internal",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Scope {
    Global,
    App(String),
    Category(String),
}

impl Scope {
    pub fn parse(text: &str) -> Scope {
        let t = text.trim().to_ascii_lowercase();
        if t.is_empty() || t == "global" {
            return Scope::Global;
        }
        if let Some(rest) = t.strip_prefix("app:") {
            return Scope::App(clean_app(rest));
        }
        if let Some(rest) = t.strip_prefix("category:") {
            return Scope::Category(rest.trim().to_string());
        }
        Scope::App(clean_app(&t))
    }
}

fn clean_app(name: &str) -> String {
    let t = name.trim().to_ascii_lowercase();
    t.trim_end_matches(".exe").trim().to_string()
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    pub pattern: Vec<Direction>,
    pub scope: Scope,
    pub button: MouseButton,
    pub wheel: Option<WheelDir>,
    pub on_wheel: bool,
    pub name: String,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    /// 归一化进程名；`*` 结尾表示前缀匹配。
    pub apps: Vec<String>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeContext {
    pub app: Option<String>,
    pub categories: Vec<String>,
}

impl ScopeContext {
    pub fn for_app(app: Option<String>, apps: &[GestureAppCfg]) -> ScopeContext {
        let Some(name) = app else {
            return ScopeContext::default();
        };
        let mut categories = Vec::new();
        for entry in apps {
            let key = entry.name.as_deref().unwrap_or("").trim().to_ascii_lowercase();
            if key.is_empty() || !name.contains(key.trim_end_matches(".exe")) {
                continue;
            }
            for c in entry.categories.clone().unwrap_or_default() {
                let c = c.trim().to_string();
                if !c.is_empty() && !categories.contains(&c) {
                    categories.push(c);
                }
            }
        }
        ScopeContext { app: Some(name), categories }
    }
}

/// `rules` 缺省装默认表；显式空表则一条都不装。
pub fn parse_rules(settings: &GestureSettings) -> Vec<Rule> {
    match &settings.rules {
        None => defaults(),
        Some(list) => list.iter().filter_map(rule_from_cfg).collect(),
    }
}

/// 进程名归一化：小写、去 `.exe`、保留末尾 `*`。
pub fn app_key(name: &str) -> String {
    let t = name.trim().to_ascii_lowercase();
    let t = t.trim_end_matches(".exe").trim().to_string();
    if let Some(prefix) = t.strip_suffix('*') {
        format!("{}*", prefix.trim())
    } else {
        t
    }
}

/// `groups` 缺省装内置浏览器组；显式空表则一个组都不装。
pub fn parse_groups(settings: &GestureSettings) -> Vec<Group> {
    let Some(list) = &settings.groups else {
        return default_groups();
    };
    list.iter()
        .map(|g| {
            let apps: Vec<String> = g
                .apps
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|a| app_key(a))
                .filter(|a| !a.is_empty() && a != "*")
                .collect();
            let rules: Vec<Rule> = g
                .rules
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter_map(|c| {
                    rule_from_cfg(c).map(|mut r| {
                        r.scope = Scope::Global;
                        r
                    })
                })
                .collect();
            Group {
                name: g
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "未命名分组".into()),
                apps,
                rules,
            }
        })
        .collect()
}

fn rule_from_cfg(cfg: &GestureRuleCfg) -> Option<Rule> {
    let pattern = parse_pattern(cfg.pattern.as_deref().unwrap_or(&[]));
    if pattern.is_empty() {
        return None;
    }
    let action = parse_action(cfg)?;
    Some(Rule {
        pattern,
        scope: Scope::parse(cfg.scope.as_deref().unwrap_or("global")),
        button: cfg
            .button
            .as_deref()
            .and_then(MouseButton::parse)
            .unwrap_or(MouseButton::Right),
        wheel: cfg.wheel.as_deref().and_then(WheelDir::parse),
        on_wheel: cfg.execute_on_wheel == Some(true),
        name: cfg
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| action_label(&action)),
        action,
    })
}

fn parse_pattern(items: &[String]) -> Vec<Direction> {
    let mut out = Vec::new();
    for item in items {
        let mut word = String::new();
        for ch in item.chars() {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                word.push(ch);
                continue;
            }
            push_word(&mut word, &mut out);
            if let Some(d) = Direction::parse(&ch.to_string()) {
                push_once(&mut out, d);
            }
        }
        push_word(&mut word, &mut out);
    }
    out
}

fn push_word(word: &mut String, out: &mut Vec<Direction>) {
    if word.is_empty() {
        return;
    }
    if let Some(d) = Direction::parse(word) {
        push_once(out, d);
    } else if word.chars().count() > 1
        && word.chars().all(|c| matches!(c.to_ascii_lowercase(), 'u' | 'r' | 'd' | 'l'))
    {
        for c in word.chars() {
            if let Some(d) = Direction::parse(&c.to_string()) {
                push_once(out, d);
            }
        }
    }
    word.clear();
}

fn push_once(out: &mut Vec<Direction>, d: Direction) {
    if out.last() != Some(&d) {
        out.push(d);
    }
}

fn parse_action(cfg: &GestureRuleCfg) -> Option<Action> {
    let a = cfg.action.clone().unwrap_or_default();
    let kind = a.r#type.as_deref().unwrap_or("hotkey").trim().to_ascii_lowercase();
    match kind.as_str() {
        "hotkey" => {
            let keys: Vec<u16> = a
                .keys
                .clone()
                .unwrap_or_default()
                .iter()
                .filter_map(|k| vk_of(k))
                .collect();
            if keys.is_empty() { None } else { Some(Action::Hotkey(keys)) }
        }
        "window" => {
            let op = match a.operation.as_deref().unwrap_or("").trim().to_ascii_lowercase().as_str() {
                "toggle-topmost" | "topmost" => WindowOp::ToggleTopmost,
                "toggle-maximize" | "maximize" => WindowOp::ToggleMaximize,
                "minimize" => WindowOp::Minimize,
                "close" => WindowOp::Close,
                _ => return None,
            };
            Some(Action::Window(op))
        }
        "program" => {
            let path = a.path.clone().unwrap_or_default();
            if path.trim().is_empty() {
                return None;
            }
            Some(Action::Program { path, args: a.args.clone().unwrap_or_default() })
        }
        "volume" => {
            let op = match a.operation.as_deref().unwrap_or("").trim().to_ascii_lowercase().as_str() {
                "increase" | "up" => VolumeOp::Increase,
                "decrease" | "down" => VolumeOp::Decrease,
                "mute" | "toggle" => VolumeOp::Mute,
                _ => return None,
            };
            Some(Action::Volume(op))
        }
        "internal" | "builtin" => {
            let op = a.operation.as_deref().unwrap_or("").trim().to_ascii_lowercase();
            if !matches!(
                op.as_str(),
                "capture"
                    | "clipboard"
                    | "imgbed"
                    | "devtools"
                    | "settings"
                    | "toggle-exclude"
                    | "close-similar"
                    | "edit-hosts"
            ) {
                return None;
            }
            Some(Action::Internal(op))
        }
        _ => None,
    }
}

/// 键名 → 虚拟键码；修饰键单独处理。
pub fn vk_of(name: &str) -> Option<u16> {
    let t = name.trim().to_ascii_lowercase();
    Some(match t.as_str() {
        "ctrl" | "control" => 0x11,
        "alt" | "menu" => 0x12,
        "shift" => 0x10,
        "win" | "super" | "meta" => 0x5B,
        other => zx_core::hotkey::vk_of(other)?,
    })
}

fn action_label(action: &Action) -> String {
    match action {
        Action::Hotkey(keys) => format!("快捷键（{} 个键）", keys.len()),
        Action::Window(op) => match op {
            WindowOp::ToggleTopmost => "切换置顶",
            WindowOp::ToggleMaximize => "最大化/还原",
            WindowOp::Minimize => "最小化",
            WindowOp::Close => "关闭窗口",
        }
        .into(),
        Action::Program { path, .. } => format!("运行 {}", path),
        Action::Volume(op) => match op {
            VolumeOp::Increase => "音量 +",
            VolumeOp::Decrease => "音量 −",
            VolumeOp::Mute => "静音",
        }
        .into(),
        Action::Internal(op) => match op.as_str() {
            "capture" => "截图",
            "clipboard" => "剪贴板",
            "imgbed" => "图床",
            "devtools" => "开发工具",
            "settings" => "打开设置",
            "toggle-exclude" => "排除本程序",
            "close-similar" => "关闭相似窗口",
            "edit-hosts" => "编辑 hosts",
            _ => "内置动作",
        }
        .into(),
    }
}

/// 内置通用规则。
pub fn defaults() -> Vec<Rule> {
    fn rule(pattern: &[Direction], name: &str, action: Action) -> Rule {
        Rule {
            pattern: pattern.to_vec(),
            scope: Scope::Global,
            button: MouseButton::Right,
            wheel: None,
            on_wheel: false,
            name: name.into(),
            action,
        }
    }
    vec![
        rule(&[Direction::Right], "前进", Action::Hotkey(vec![0x12, 0x27])),
        rule(&[Direction::Left], "后退", Action::Hotkey(vec![0x12, 0x25])),
        rule(&[Direction::Down, Direction::Right], "关闭窗口", Action::Window(WindowOp::Close)),
        rule(
            &[Direction::Down, Direction::Left],
            "关闭相似窗口",
            Action::Internal("close-similar".into()),
        ),
        rule(&[Direction::UpRight], "最大化/还原", Action::Window(WindowOp::ToggleMaximize)),
        rule(&[Direction::DownLeft], "最小化", Action::Window(WindowOp::Minimize)),
        rule(
            &[Direction::Right, Direction::DownLeft, Direction::Right],
            "排除当前程序",
            Action::Internal("toggle-exclude".into()),
        ),
        rule(
            &[Direction::DownLeft, Direction::DownRight, Direction::DownLeft],
            "截图",
            Action::Internal("capture".into()),
        ),
        rule(
            &[Direction::Down, Direction::Right, Direction::Down],
            "编辑 hosts",
            Action::Internal("edit-hosts".into()),
        ),
        rule(
            &[Direction::Right, Direction::Left, Direction::Down],
            "打开剪贴板",
            Action::Internal("clipboard".into()),
        ),
        rule(
            &[Direction::Down, Direction::Right, Direction::Up],
            "打开图床",
            Action::Internal("imgbed".into()),
        ),
        rule(
            &[Direction::Left, Direction::Down, Direction::Right],
            "打开开发工具",
            Action::Internal("devtools".into()),
        ),
    ]
}

/// 内置浏览器分组。
pub fn default_groups() -> Vec<Group> {
    fn hotkey(pattern: &[Direction], name: &str, keys: &[u16]) -> Rule {
        Rule {
            pattern: pattern.to_vec(),
            scope: Scope::Global,
            button: MouseButton::Right,
            wheel: None,
            on_wheel: false,
            name: name.into(),
            action: Action::Hotkey(keys.to_vec()),
        }
    }
    let apps = [
        "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "arc", "chromium",
        "360se", "360chrome", "qqbrowser", "sogouexplorer", "maxthon",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();

    vec![Group {
        name: "浏览器".into(),
        apps,
        rules: vec![
            hotkey(&[Direction::Up], "页面顶部", &[0x11, 0x24]),
            hotkey(&[Direction::Down], "页面底部", &[0x11, 0x23]),
            hotkey(&[Direction::Down, Direction::Up], "刷新", &[0x74]),
            hotkey(&[Direction::Up, Direction::Down], "强制刷新", &[0x11, 0x74]),
            hotkey(&[Direction::Up, Direction::Left], "切换到左侧标签页", &[0x11, 0x10, 0x09]),
            hotkey(&[Direction::Up, Direction::Right], "切换到右侧标签页", &[0x11, 0x09]),
            hotkey(&[Direction::Down, Direction::Right], "关闭当前标签页", &[0x11, 0x57]),
            hotkey(&[Direction::Down, Direction::Left], "关闭所有标签页", &[0x11, 0x10, 0x57]),
            hotkey(
                &[Direction::Down, Direction::Up, Direction::Right, Direction::Down],
                "打开主页",
                &[0x12, 0x24],
            ),
        ],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_patterns_in_every_notation() {
        assert_eq!(parse_pattern(&["Left".into()]), vec![Direction::Left]);
        assert_eq!(parse_pattern(&["left".into()]), vec![Direction::Left]);
        assert_eq!(parse_pattern(&["l".into()]), vec![Direction::Left]);
        assert_eq!(parse_pattern(&["←".into()]), vec![Direction::Left]);
        assert_eq!(parse_pattern(&["←↓".into()]), vec![Direction::Left, Direction::Down]);
        assert_eq!(parse_pattern(&["left,down".into()]), vec![Direction::Left, Direction::Down]);
        assert_eq!(parse_pattern(&["ul".into()]), vec![Direction::UpLeft]);
        assert_eq!(parse_pattern(&["ld".into()]), vec![Direction::Left, Direction::Down]);
        assert_eq!(
            parse_pattern(&["Down".into(), "Right".into()]),
            vec![Direction::Down, Direction::Right]
        );
    }

    #[test]
    fn eight_direction_quantization() {
        assert_eq!(Direction::from_delta_8(10.0, 0.0), Direction::Right);
        assert_eq!(Direction::from_delta_8(10.0, 10.0), Direction::DownRight);
        assert_eq!(Direction::from_delta_8(0.0, 10.0), Direction::Down);
        assert_eq!(Direction::from_delta_8(-10.0, 10.0), Direction::DownLeft);
        assert_eq!(Direction::from_delta_8(-10.0, 0.0), Direction::Left);
        assert_eq!(Direction::from_delta_8(-10.0, -10.0), Direction::UpLeft);
        assert_eq!(Direction::from_delta_8(0.0, -10.0), Direction::Up);
        assert_eq!(Direction::from_delta_8(10.0, -10.0), Direction::UpRight);
    }

    #[test]
    fn scope_parsing_and_app_normalization() {
        assert_eq!(Scope::parse("global"), Scope::Global);
        assert_eq!(Scope::parse(""), Scope::Global);
        assert_eq!(Scope::parse("app:Chrome.exe"), Scope::App("chrome".into()));
        assert_eq!(Scope::parse("category:浏览器"), Scope::Category("浏览器".into()));
    }

    #[test]
    fn default_rules_are_the_expected_set() {
        let rules = defaults();
        let find = |p: Vec<Direction>| {
            rules
                .iter()
                .find(|r| r.pattern == p)
                .unwrap_or_else(|| panic!("缺少方向串 {:?}", p.iter().map(|d| d.arrow()).collect::<String>()))
        };

        assert_eq!(find(vec![Direction::Right]).action, Action::Hotkey(vec![0x12, 0x27]));
        assert_eq!(find(vec![Direction::Left]).action, Action::Hotkey(vec![0x12, 0x25]));
        assert_eq!(
            find(vec![Direction::Down, Direction::Right]).action,
            Action::Window(WindowOp::Close)
        );
        assert_eq!(
            find(vec![Direction::Down, Direction::Left]).action,
            Action::Internal("close-similar".into())
        );
        assert_eq!(
            find(vec![Direction::UpRight]).action,
            Action::Window(WindowOp::ToggleMaximize)
        );
        assert_eq!(
            find(vec![Direction::DownLeft]).action,
            Action::Window(WindowOp::Minimize)
        );
        assert_eq!(
            find(vec![Direction::Right, Direction::DownLeft, Direction::Right]).action,
            Action::Internal("toggle-exclude".into())
        );
        assert_eq!(
            find(vec![Direction::DownLeft, Direction::DownRight, Direction::DownLeft]).action,
            Action::Internal("capture".into())
        );
        assert_eq!(
            find(vec![Direction::Down, Direction::Right, Direction::Down]).action,
            Action::Internal("edit-hosts".into())
        );
        assert_eq!(
            find(vec![Direction::Right, Direction::Left, Direction::Down]).action,
            Action::Internal("clipboard".into())
        );
        assert_eq!(
            find(vec![Direction::Down, Direction::Right, Direction::Up]).action,
            Action::Internal("imgbed".into())
        );
        assert_eq!(
            find(vec![Direction::Left, Direction::Down, Direction::Right]).action,
            Action::Internal("devtools".into())
        );
        assert_eq!(rules.len(), 12);
    }

    #[test]
    fn groups_parse_and_keep_drafts() {
        let mut settings = zx_core::Settings::default().gesture();
        assert_eq!(parse_groups(&settings).len(), 1);
        settings.groups = Some(vec![]);
        assert!(parse_groups(&settings).is_empty());

        settings.groups = Some(vec![
            zx_core::GestureGroupCfg {
                name: Some("浏览器".into()),
                apps: Some(vec!["Chrome.exe".into(), "360*".into()]),
                rules: Some(vec![GestureRuleCfg {
                    pattern: Some(vec!["↓".into()]),
                    action: Some(zx_core::GestureActionCfg {
                        r#type: Some("hotkey".into()),
                        keys: Some(vec!["Ctrl".into(), "W".into()]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            },
            zx_core::GestureGroupCfg { name: Some("草稿".into()), ..Default::default() },
        ]);
        let groups = parse_groups(&settings);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].apps, vec!["chrome".to_string(), "360*".to_string()]);
        assert_eq!(groups[0].rules.len(), 1);
        assert_eq!(groups[0].rules[0].action, Action::Hotkey(vec![0x11, b'W' as u16]));
        assert!(groups[1].apps.is_empty() && groups[1].rules.is_empty());
    }

    #[test]
    fn default_browser_group_is_usable() {
        let groups = default_groups();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].apps.iter().any(|a| a == "chrome"));
        let find = |p: Vec<Direction>| {
            groups[0]
                .rules
                .iter()
                .find(|r| r.pattern == p)
                .unwrap_or_else(|| panic!("浏览器组缺少方向串"))
                .action
                .clone()
        };
        assert_eq!(find(vec![Direction::Up]), Action::Hotkey(vec![0x11, 0x24]));
        assert_eq!(find(vec![Direction::Down]), Action::Hotkey(vec![0x11, 0x23]));
        assert_eq!(
            find(vec![Direction::Down, Direction::Up]),
            Action::Hotkey(vec![0x74])
        );
        assert_eq!(
            find(vec![Direction::Up, Direction::Down]),
            Action::Hotkey(vec![0x11, 0x74])
        );
        assert_eq!(
            find(vec![Direction::Up, Direction::Left]),
            Action::Hotkey(vec![0x11, 0x10, 0x09])
        );
        assert_eq!(
            find(vec![Direction::Up, Direction::Right]),
            Action::Hotkey(vec![0x11, 0x09])
        );
        assert_eq!(
            find(vec![
                Direction::Down,
                Direction::Up,
                Direction::Right,
                Direction::Down
            ]),
            Action::Hotkey(vec![0x12, 0x24])
        );
        assert_eq!(
            find(vec![Direction::Down, Direction::Right]),
            Action::Hotkey(vec![0x11, 0x57])
        );
        assert_eq!(
            find(vec![Direction::Down, Direction::Left]),
            Action::Hotkey(vec![0x11, 0x10, 0x57])
        );
        assert_eq!(groups[0].rules.len(), 9);
    }

    #[test]
    fn internal_action_parses_and_rejects_unknown_operation() {
        let mut cfg = GestureRuleCfg {
            pattern: Some(vec!["↑".into()]),
            action: Some(zx_core::GestureActionCfg {
                r#type: Some("internal".into()),
                operation: Some("capture".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut settings = zx_core::Settings::default().gesture();
        settings.rules = Some(vec![cfg.clone()]);
        let rules = crate::model::parse_rules(&settings);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, Action::Internal("capture".into()));

        cfg.action.as_mut().unwrap().operation = Some("nope".into());
        settings.rules = Some(vec![cfg]);
        assert!(crate::model::parse_rules(&settings).is_empty());
    }
}
