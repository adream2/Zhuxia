//! UI 元素识别：截图场景下的元素定位、分类与文本校正。
//!
//! 设计取舍（详见 `docs/UIELEM.md`）：
//!
//! 1. **UIA 为主，视觉为辅**。标准 Windows 控件（Win32 / WinForms / WPF / UWP /
//!    Electron / 浏览器 / Office）都能从 `IUIAutomation` 拿到 ControlType、Name 和
//!    物理像素矩形，这是唯一可靠的"语义"来源，且零模型、零推理延迟。
//!    纯视觉检测只在自绘 UI（微信/QQ/canvas/游戏）上才有存在意义，因此作为第二期。
//! 2. **必须在覆盖层显示前取快照**。覆盖层是 `WS_EX_LAYERED | WS_EX_TOPMOST` 的
//!    全屏窗口，一旦显示，`ElementFromPoint` 命中的就是它自己。所以元素采集发生在
//!    创建覆盖层之前，覆盖层内只对快照做命中测试，不再跨进程调 COM。
//! 3. **UIA 调用一律在专用线程上带超时**。跨进程 COM 在目标程序无响应时会卡 3-5 秒，
//!    放在 UI 线程上就是整个覆盖层冻结。这里的 worker 线程 + deadline 是硬要求，
//!    真卡住了就丢弃该线程（宁可泄漏一个线程，也不能冻住界面）。
//! 4. **UIA 的 Name 用来校正 OCR**，而不是反过来。像素读数会被小字号、深色主题、
//!    抗锯齿弄错；但 UIA 的 Name 可能被截断、可能带 `(&S)` 加速符。融合规则见
//!    [`pick_text`]。
//!
//! 坐标约定：全程是**虚拟桌面物理像素**，与 [`crate::screen::Frame`] 一致。
//! 本模块不处理 DPI 换算——渲染用进程是 per-monitor v2，UIA 返回的就是物理像素。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement,
    UIA_BoundingRectanglePropertyId, UIA_ButtonControlTypeId,
    UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId, UIA_CustomControlTypeId,
    UIA_DataItemControlTypeId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    UIA_DataGridControlTypeId, UIA_GroupControlTypeId, UIA_HeaderItemControlTypeId,
    UIA_HyperlinkControlTypeId, UIA_ListControlTypeId, UIA_TableControlTypeId,
    UIA_ToolBarControlTypeId, UIA_TreeControlTypeId,
    UIA_ImageControlTypeId, UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId,
    UIA_PaneControlTypeId, UIA_RadioButtonControlTypeId, UIA_ScrollBarControlTypeId,
    UIA_SliderControlTypeId, UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId,
    UIA_ControlTypePropertyId, UIA_IsOffscreenPropertyId, UIA_NamePropertyId,
    UIA_ProcessIdPropertyId, UIA_TextControlTypeId, UIA_TreeItemControlTypeId,
    UIA_WindowControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow, GetSystemMetrics,
    IsChild, IsWindowVisible, WindowFromPoint, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

/// 一次遍历的上限。浏览器一屏动辄 2000+ 节点，没有上限就会拖死 deadline。
// RawView 的树很深：Chromium 的页面内容在 Document 下面还有 8-12 层，
// 深度给少了整页内容都进不来（实测 14 层时只采到容器，页面正文一个没有）。
const MAX_NODES: usize = 4000;
const MAX_DEPTH: u32 = 32;
const MAX_SIBLINGS: usize = 200;
/// 单点探测向上爬的层数上限（Text → 匿名 Pane → Button 这种嵌套很常见）。
const MAX_ASCEND: usize = 8;
/// 小于这个尺寸的框没有识别价值（分隔线、滚动条端点、装饰像素）。
const MIN_SIDE: i32 = 8;

/// 落不进任何控件的 OCR 行，置信度低于这个就不要了。
///
/// 实测依据见 `examples/ocr_invert_probe.rs`：图标被当字读出来的置信度集中在
/// 0.2–0.65，而真实文本行在 0.87 以上。0.5 这条线两边都不挨着。
const ORPHAN_SCORE: f32 = 0.5;

/// 大于这个长度的 Name 截断。浏览器里一个 a11y "列表项" 经常挂着整篇文章的正文，
/// 原样塞进元素列表会把内存和界面一起搞崩。
const MAX_NAME_CHARS: usize = 120;

// ---------------------------------------------------------------- 数据模型

/// 屏幕矩形，虚拟桌面物理像素。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn area(&self) -> i64 {
        (self.w.max(0) as i64) * (self.h.max(0) as i64)
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// 带容差的命中。光标压在元素边线上（差 1px）时也算选中——用户觉得自己明明
    /// 在上面，框不出来就成了"这功能是坏的"。
    pub fn contains_tol(&self, x: i32, y: i32) -> bool {
        const T: i32 = 2;
        x >= self.x - T
            && x < self.right() + T
            && y >= self.y - T
            && y < self.bottom() + T
    }

    pub fn to_rect(&self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }

    /// 转成截图画布内的相对坐标；完全落在画布外时返回 None。
    pub fn to_local(&self, ox: i32, oy: i32, cw: i32, ch: i32) -> Option<(i32, i32, i32, i32)> {
        let x0 = (self.x - ox).clamp(0, cw.saturating_sub(1));
        let y0 = (self.y - oy).clamp(0, ch.saturating_sub(1));
        let x1 = (self.right() - ox).clamp(x0 + 1, cw);
        let y1 = (self.bottom() - oy).clamp(y0 + 1, ch);
        if x1 - x0 < 2 || y1 - y0 < 2 {
            return None;
        }
        Some((x0, y0, x1 - x0, y1 - y0))
    }

    fn from_rect(r: RECT) -> Option<Rect> {
        let w = r.right - r.left;
        let h = r.bottom - r.top;
        if w <= 0 || h <= 0 {
            return None;
        }
        Some(Rect::new(r.left, r.top, w, h))
    }
}

/// 元素类型。够用就好——分太细反而每类都判不准。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Button,
    Input,
    Text,
    Icon,
    Link,
    Check,
    Tab,
    Menu,
    Cell,
    Scroll,
    Other,
    /// 几何聚合出来的"块"（一行卡片、一列菜单）：UIA 里没有对应元素，是我们造的
    Group,
}

impl Kind {
    /// 可交互元素：单点探测时优先停在这一类上。
    pub fn interactive(self) -> bool {
        matches!(
            self,
            Kind::Button | Kind::Input | Kind::Link | Kind::Check | Kind::Tab | Kind::Menu | Kind::Cell
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Button => "按钮",
            Kind::Input => "输入框",
            Kind::Text => "文本",
            Kind::Icon => "图标",
            Kind::Link => "链接",
            Kind::Check => "勾选",
            Kind::Tab => "标签页",
            Kind::Menu => "菜单项",
            Kind::Cell => "列表项",
            Kind::Scroll => "滚动条",
            Kind::Other => "其他",
            Kind::Group => "块",
        }
    }

    fn from_control_type(ct: i32) -> Kind {
        if ct == UIA_ButtonControlTypeId.0 || ct == UIA_SplitButtonControlTypeId.0 {
            Kind::Button
        } else if ct == UIA_EditControlTypeId.0 || ct == UIA_ComboBoxControlTypeId.0 {
            Kind::Input
        } else if ct == UIA_TextControlTypeId.0 || ct == UIA_DocumentControlTypeId.0 {
            Kind::Text
        } else if ct == UIA_ImageControlTypeId.0 {
            Kind::Icon
        } else if ct == UIA_HyperlinkControlTypeId.0 {
            Kind::Link
        } else if ct == UIA_CheckBoxControlTypeId.0 || ct == UIA_RadioButtonControlTypeId.0 {
            Kind::Check
        } else if ct == UIA_TabItemControlTypeId.0 {
            Kind::Tab
        } else if ct == UIA_MenuItemControlTypeId.0 || ct == UIA_HeaderItemControlTypeId.0 {
            Kind::Menu
        } else if ct == UIA_ListItemControlTypeId.0
            || ct == UIA_DataItemControlTypeId.0
            || ct == UIA_TreeItemControlTypeId.0
        {
            Kind::Cell
        } else if ct == UIA_ScrollBarControlTypeId.0 || ct == UIA_SliderControlTypeId.0 {
            Kind::Scroll
        } else {
            Kind::Other
        }
    }

    /// 容器类：本身不是元素，只是用来往上爬的中间层。
    fn container(ct: i32) -> bool {
        ct == UIA_PaneControlTypeId.0
            || ct == UIA_GroupControlTypeId.0
            || ct == UIA_CustomControlTypeId.0
            || ct == UIA_WindowControlTypeId.0
            // 下面这几种是最容易被漏掉的"更大的元素"：网页里一行卡片通常就是一个
            // List（父容器），侧边栏是 Tree，表格是 Table。之前只认 Pane/Group，
            // 结果这些容器被当匿名容器丢掉——"向上滚轮选更大元素"就无从谈起。
            || ct == UIA_ListControlTypeId.0
            || ct == UIA_TreeControlTypeId.0
            || ct == UIA_TableControlTypeId.0
            || ct == UIA_DataGridControlTypeId.0
            || ct == UIA_ToolBarControlTypeId.0
    }
}

/// 元素来源。融合时靠它决定信任顺序。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Uia,
    Ocr,
}

/// 一个识别结果。
#[derive(Clone, Debug)]
pub struct Element {
    pub rect: Rect,
    pub kind: Kind,
    pub name: String,
    pub source: Source,
    /// 置信度：UIA 记 1.0；OCR 用 CTC 平均概率；视觉检测用模型分数。
    pub score: f32,
}

impl Element {
    fn uia(rect: Rect, kind: Kind, name: String) -> Element {
        Element { rect, kind, name, source: Source::Uia, score: 1.0 }
    }

    /// 悬停气泡上显示的文本："按钮 · 保存"。
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.kind.label().to_string()
        } else {
            format!("{} · {}", self.kind.label(), self.name)
        }
    }

    pub fn to_rect(&self) -> RECT {
        self.rect.to_rect()
    }

    /// 一行的元素摘要：`类型 · 名称<TAB>x,y 宽×高`。
    /// 制表符分隔是故意的——贴进 issue 好读，粘进表格自动分列。
    pub fn info_line(&self) -> String {
        format!(
            "{} · {}	{},{} {}×{}",
            self.kind.label(),
            self.name,
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h
        )
    }
}

/// 光标位置上的**层级链**，由小到大。
///
/// 这就是 PixPin 文档里那句「向上滚动滚轮，选中包含它的更大元素，向下滚动回到更小的
/// 元素」——链上的每一格都是一个**真实存在**的元素（列表项 → 它所在的 List → 面板 →
/// 文档 → 窗口），不是我们编出来的框。
///
/// 用几何包含推父级，而不是 `GetParentElement`：每取一次父级都是一趟跨进程调用，
/// 一条链要爬五六层，代价比一次批量采集还高。
pub fn stack_at(elems: &[Element], x: i32, y: i32) -> Vec<Element> {
    const MAX_LEVELS: usize = 6;
    let Some(hit) = pick_at(elems, x, y) else {
        return Vec::new();
    };
    let mut chain = vec![hit.clone()];
    let mut cur = hit.rect;
    while chain.len() < MAX_LEVELS {
        let next = elems
            .iter()
            // 面积只大一点点的跳过：RawView 里同一块区域常有几层矩形几乎相同的 Group
            // （差一两像素），全收进来滚轮要滚三次才动一格，看起来像卡住了。
            .filter(|e| e.rect.area() * 20 >= cur.area() * 21)
            // 容差 2px：父容器的边框有时比子元素只大一两像素
            .filter(|e| {
                let (p, c) = (e.rect, cur);
                p.x <= c.x + 2
                    && p.y <= c.y + 2
                    && p.right() >= c.right() - 2
                    && p.bottom() >= c.bottom() - 2
            })
            .min_by_key(|e| e.rect.area());
        let Some(e) = next else { break };
        if e.rect.area() > cur.area() {
            chain.push(e.clone());
            cur = e.rect;
        } else {
            break;
        }
    }
    chain
}

/// 光标落在哪个元素上。/// 光标落在哪个元素上。**优先可交互、其次面积最小**——点按钮里的图标时，
/// 用户要的是按钮，不是那个 Image。
pub fn pick_at(elems: &[Element], x: i32, y: i32) -> Option<&Element> {
    let mut best: Option<&Element> = None;
    for e in elems {
        if !e.rect.contains_tol(x, y) {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => match (e.kind.interactive(), b.kind.interactive()) {
                (true, false) => true,
                (false, true) => false,
                _ => e.rect.area() < b.rect.area(),
            },
        };
        if better {
            best = Some(e);
        }
    }
    best
}

/// OCR 给回来的一行。
#[derive(Clone, Debug)]
pub struct OcrLine {
    pub rect: Rect,
    pub text: String,
    pub score: f32,
}

// ---------------------------------------------------------------- 几何

/// 交并比。融合去重用它。
pub fn iou(a: Rect, b: Rect) -> f32 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let inter = ((x1 - x0) as f32) * ((y1 - y0) as f32);
    let union = a.area() as f32 + b.area() as f32 - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 小框有多少比例压在大框里。OCR 行比按钮小得多，用 IoU 会失效，得看这个。
fn coverage(small: Rect, big: Rect) -> f32 {
    let x0 = small.x.max(big.x);
    let y0 = small.y.max(big.y);
    let x1 = small.right().min(big.right());
    let y1 = small.bottom().min(big.bottom());
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let inter = ((x1 - x0) as f32) * ((y1 - y0) as f32);
    let a = small.area() as f32;
    if a <= 0.0 {
        0.0
    } else {
        inter / a
    }
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// 虚拟桌面矩形（物理像素），用于剔掉滚动到屏幕外的元素。
fn screen_bounds() -> Rect {
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

// ---------------------------------------------------------------- UIA 通道

enum Req {
    Probe { pt: POINT, deadline: Instant, reply: Sender<Option<Element>> },
    /// HWND 内部是裸指针，不实现 Send；跨线程一律传 `isize` 再还原。
    /// 一次采多个窗口：前台 + 光标下的 + 任务栏族（见 [`capture_targets`]）。
    Snapshot { handles: Vec<isize>, budget: Duration, reply: Sender<Vec<Element>> },
    Path { pt: POINT, reply: Sender<Vec<Element>> },
}

/// worker 句柄。`STUCK` 为真说明上一次请求超时了，下一次调用要换一个新线程。
static TX: OnceLock<Mutex<Option<Sender<Req>>>> = OnceLock::new();
/// 上一次采集中途被截断（配额/兄弟数/截止时间）。`IsOffscreen` 那种"正常的丢"
/// 不算——只有"树没走完"才算。
///
/// 为什么要在界面上露出来：截断之后滚轮上不去了，用户看到的是"这个页面框不出来"，
/// 而日志里那行 `节点配额用尽` 他看不到。悬停气泡上标一句，性质就从"程序坏了"
/// 变成"这个页面太大，被截断了"。
static TRUNCATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 记账用的原因字符串。**只在这里定义**：记的地方和判的地方各写一遍字面量，
/// 改一处就会静默失配（界面上永远不显示"已截断"）。
const WHY_QUOTA: &str = "节点配额用尽";
const WHY_SIBLINGS: &str = "兄弟过多";
const WHY_DEADLINE: &str = "到达截止时间";

/// 这个原因算不算"树没走完"。屏幕外的子树被剪掉是正常优化，不算。
fn is_cut(reason: &str) -> bool {
    matches!(reason, WHY_QUOTA | WHY_SIBLINGS | WHY_DEADLINE)
}

/// 本次快照是否被截断（供界面显示）。
pub fn last_truncated() -> bool {
    TRUNCATED.load(std::sync::atomic::Ordering::Relaxed)
}

static STUCK: AtomicBool = AtomicBool::new(false);
/// UIA 彻底不可用（COM 创建失败 / 泄漏封顶）时置位，避免每次调用都白起一个线程。
static DISABLED: AtomicBool = AtomicBool::new(false);

/// 已经放弃（泄漏）的 worker 线程数。
///
/// 超时的处理是"放弃等待、让那条线程自生自灭"——因为 `FindAll` / 跨进程 COM 调用
/// **不可打断**。问题是每次 Alt+A 都会新起一条，而卡住的那条永远不会回来：对着一个
/// UIA 无响应的程序连按 100 次，就是 100 条僵线程 + 100 套 MTA 公寓常驻。
/// 所以必须封顶：到 [`MAX_LEAKS`] 之后直接关掉元素识别，并在界面上说清楚。
static LEAKED: AtomicUsize = AtomicUsize::new(0);
const MAX_LEAKS: usize = 3;

/// 被封掉的窗口句柄（isize）。同一个卡死的程序不该反复吃掉预算。
static BLACKLIST: Mutex<Vec<isize>> = Mutex::new(Vec::new());
/// worker 正在遍历的窗口。超时后靠它定位"是哪个窗口把线程卡住了"。
static CURRENT: AtomicIsize = AtomicIsize::new(0);
/// 停用时刻。到 [`DISABLED_TTL`] 自动恢复。
///
/// 停用必须是**可恢复**的：用户碰到一个 UIA 无响应的程序（Electron 卡死、远程桌面）
/// 连按三次 Alt+A 就会触发停用，而那个程序通常一会儿就关了/重启了。
/// 如果停用是单向的，用户只能退出托盘程序再启动——对一个常驻一整天的工具，
/// 这就是"越用越坏"。
static DISABLED_AT: Mutex<Option<Instant>> = Mutex::new(None);
const DISABLED_TTL: Duration = Duration::from_secs(600);

/// 去掉黑名单里的窗口（拉黑 = 之前把采集线程卡死过）。
fn keep_live(handles: &[isize], black: &[isize]) -> Vec<isize> {
    handles.iter().copied().filter(|h| !black.contains(h)).collect()
}

fn disable(reason: &str) {
    DISABLED.store(true, Ordering::SeqCst);
    if let Ok(mut g) = DISABLED_AT.lock() {
        *g = Some(Instant::now());
    }
    zx_core::log::warn(format!(
        "{reason}（{} 分钟后自动重试，也可以在设置里手动恢复）",
        DISABLED_TTL.as_secs() / 60
    ));
}

/// 现在是否处于停用状态。到点的自动恢复就发生在这里。
fn disabled_now() -> bool {
    if !DISABLED.load(Ordering::Relaxed) {
        return false;
    }
    let expired = DISABLED_AT
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|t| t.elapsed() >= DISABLED_TTL)
        .unwrap_or(true);
    if expired {
        reset();
        return false;
    }
    true
}

/// 重新启用元素识别：清计数、清黑名单、清停用标记。
///
/// 三条路径共用：TTL 到期、设置页手动点「恢复」、以及以后的任何诊断入口。
pub fn reset() {
    DISABLED.store(false, Ordering::SeqCst);
    LEAKED.store(0, Ordering::SeqCst);
    STUCK.store(false, Ordering::SeqCst);
    if let Ok(mut g) = DISABLED_AT.lock() {
        *g = None;
    }
    if let Ok(mut g) = BLACKLIST.lock() {
        g.clear();
    }
    zx_core::log::info("UI 元素识别已重新启用");
}

/// 供设置页显示：元素识别当前是否被停用（不触发自动恢复的副作用）。
pub fn is_disabled() -> bool {
    DISABLED.load(Ordering::Relaxed)
}

/// 元素识别是否因为反复卡死而被停用（供界面显示一次提示）。
pub fn unavailable_note() -> Option<&'static str> {
    if disabled_now() && LEAKED.load(Ordering::Relaxed) >= MAX_LEAKS {
        Some("元素识别已停用（有程序不响应 UI 自动化）")
    } else {
        None
    }
}

fn ensure_tx() -> Option<Sender<Req>> {
    if disabled_now() {
        return None;
    }
    let slot = TX.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().ok()?;
    let stuck = STUCK.swap(false, Ordering::SeqCst);
    if stuck {
        // 上一条 worker 卡死了：记账，到顶就彻底关掉，别再起新的
        let n = LEAKED.fetch_add(1, Ordering::SeqCst) + 1;
        let who = CURRENT.swap(0, Ordering::SeqCst);
        if who != 0 {
            if let Ok(mut g) = BLACKLIST.lock() {
                if !g.contains(&who) && g.len() < 32 {
                    g.push(who);
                }
            }
        }
        if n >= MAX_LEAKS {
            disable(&format!("UI 元素识别已停用：累计 {n} 条采集线程卡死（有程序不响应 UI 自动化）"));
            return None;
        }
        zx_core::log::warn(format!("UIA 采集线程卡死 {n}/{MAX_LEAKS} 次，重启 worker"));
    }
    let recycle = guard.is_none() || stuck;
    if recycle {
        let (tx, rx) = std::sync::mpsc::channel::<Req>();
        // 线程名便于在 Process Explorer 里认出来。
        std::thread::Builder::new()
            .name(format!("zx-uia-{}", LEAKED.load(Ordering::Relaxed)))
            .spawn(move || worker(rx))
            .ok()?;
        *guard = Some(tx);
    }
    guard.clone()
}

fn worker(rx: Receiver<Req>) {
    // UIA 是跨进程 COM，MTA 比 STA 少一层封送，而且不需要消息泵。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let created: windows::core::Result<IUIAutomation> =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) };
    let uia = match created {
        Ok(u) => u,
        Err(e) => {
            disable(&format!("UI 元素识别不可用：UIAutomation 创建失败 {e}"));
            unsafe { CoUninitialize() };
            return;
        }
    };
    zx_core::log::info("UI 元素识别就绪：UIAutomation（MTA）");

    for req in rx {
        match req {
            Req::Probe { pt, deadline, reply } => {
                let got = unsafe { probe(&uia, pt, deadline) };
                let _ = reply.send(got);
            }
            Req::Path { pt, reply } => {
                let got = unsafe { path_uncached(&uia, pt) };
                let _ = reply.send(got);
            }
            Req::Snapshot { handles, budget, reply } => {
                TRUNCATED.store(false, std::sync::atomic::Ordering::Relaxed);
                let start = Instant::now();
                let mut out: Vec<Element> = Vec::new();
                // 每个窗口单独记一行账：采不到元素时，得知道是"没去采"还是"采了但是空的"。
                let mut detail: Vec<String> = Vec::new();
                let deadline = start + budget;
                for h in &handles {
                    // 总体预算：一个卡死的程序不能把后面几个窗口的机会一起吃掉。
                    if start.elapsed() >= budget {
                        detail.push("预算用尽".into());
                        break;
                    }
                    let hwnd = HWND(*h as *mut std::ffi::c_void);
                    // 记下"正在遍历谁"：这条线程要是卡死，超时那一侧靠它把窗口拉黑
                    CURRENT.store(*h, Ordering::SeqCst);
                    let got = unsafe { walk(&uia, hwnd, deadline) };
                    CURRENT.store(0, Ordering::SeqCst);
                    detail.push(format!("{}→{}", unsafe { win_label(hwnd) }, got.len()));
                    out.extend(got);
                }
                zx_core::log::info(format!("UIA 各窗口：{}", detail.join(" · ")));
                // 跨窗口再合一次：重叠的窗口（输入法候选框、置顶工具条）会给出同一块矩形
                dedupe_nested(&mut out);
                out.sort_by_key(|e| (e.rect.y, e.rect.x));
                // `ZHUXIA_UIA_DUMP=1` 时把整份清单落盘：排查"采到了但没命中"时，
                // 光看数量没用，得看矩形到底在哪儿。
                if std::env::var_os("ZHUXIA_UIA_DUMP").is_some() {
                    let body: String = out
                        .iter()
                        .map(|e| {
                            format!(
                                "{}	{}	{},{} {}x{}
",
                                e.kind.label(),
                                e.name,
                                e.rect.x,
                                e.rect.y,
                                e.rect.w,
                                e.rect.h
                            )
                        })
                        .collect();
                    let path = zx_core::cfg::run_dir().join("uia-dump.txt");
                    let _ = std::fs::write(&path, body);
                    zx_core::log::info(format!("UIA 清单已落盘：{}", path.display()));
                }
                let _ = reply.send(out);
            }
        }
    }
    unsafe { CoUninitialize() };
}

/// 单点探测：鼠标下面那个元素。覆盖层显示**之前**用。
///
/// `budget` 到点就返回 None，不会为了一个查不动的程序把调用方拖住。
pub fn probe_at(x: i32, y: i32, budget: Duration) -> Option<Element> {
    let tx = ensure_tx()?;
    let (reply, done) = std::sync::mpsc::channel();
    let req = Req::Probe {
        pt: POINT { x, y },
        deadline: Instant::now() + budget,
        reply,
    };
    if tx.send(req).is_err() {
        STUCK.store(true, Ordering::SeqCst);
        return None;
    }
    match done.recv_timeout(budget + Duration::from_millis(60)) {
        Ok(v) => v,
        Err(_) => {
            // worker 卡在某个不响应的程序里了，标记回收，下次换线程。
            STUCK.store(true, Ordering::SeqCst);
            zx_core::log::warn("UIA 单点探测超时，已丢弃该 worker 线程");
            None
        }
    }
}

/// 截图要采的那几个窗口，按 isize 返回（HWND 不是 Send）。
///
/// **必须只调一次、且在覆盖层创建之前**：覆盖层一显示就是前台窗口，再查就查到自己头上了。
///
/// 采哪些，为什么：
///
/// * **前台窗口** —— 用户按热键时大概就在看它。
/// * **光标下的窗口** —— 前台不一定就是它。悬停到任务栏、托盘、或者一个没激活的窗口时，
///   光标底下那个才是目标。
/// * **任务栏族** —— `Shell_TrayWnd` **永远不是前台窗口**（它不激活），所以"只采前台"
///   的实现在架构上就不可能识别任务栏。副屏是 `Shell_SecondaryTrayWnd`，托盘溢出是
///   `NotifyIconOverflowWindow`（只在弹出时可见）。
pub fn capture_targets() -> Vec<isize> {
    let mut out: Vec<isize> = Vec::new();
    unsafe {
        let fg = GetForegroundWindow();
        if !fg.0.is_null() {
            out.push(fg.0 as isize);
        }
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            if let Some(h) = window_from_point(pt) {
                let raw = h.0 as isize;
                if !out.contains(&raw) {
                    out.push(raw);
                }
            }
        }
        // 父子窗口只留父：Chrome_RenderWidgetHostHWND 是 Chrome_WidgetWin_1 的子窗口，
        // 采父窗口时内容已经全在树里了，再采一遍纯属双倍开销（实测 820+750 个元素、
        // 直接顶穿预算，覆盖层最后拿到 0 个）。
        let mut kept: Vec<isize> = Vec::with_capacity(out.len());
        for (i, h) in out.iter().enumerate() {
            let me = HWND(*h as *mut std::ffi::c_void);
            let covered = out.iter().enumerate().any(|(j, other)| {
                i != j && IsChild(HWND(*other as *mut std::ffi::c_void), me).as_bool()
            });
            if !covered {
                kept.push(*h);
            }
        }
        out = kept;

        let mut trays: Vec<isize> = Vec::new();
        let lparam = LPARAM(&mut trays as *mut Vec<isize> as isize);
        let _ = EnumWindows(Some(enum_taskbar), lparam);
        for t in trays {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// 光标底下那个窗口，**跳过我们自己的**。
///
/// `WindowFromPoint` 会把我们自己的置顶窗口（手势层、任务栏小组件）也算进来；采自己
/// 的窗口没有意义（元素的 pid 过滤会全丢掉），而且会把真正的目标挡在后面。
/// 命中自己时沿 Z 序往下走，直到碰到别人的窗口。
unsafe fn window_from_point(pt: POINT) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindow, GetWindowThreadProcessId, GW_HWNDNEXT};
    let own = std::process::id();
    let mut h = WindowFromPoint(pt);
    for _ in 0..8 {
        if h.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != own {
            return Some(h);
        }
        h = GetWindow(h, GW_HWNDNEXT).unwrap_or_default();
    }
    None
}

/// 只收任务栏族的窗口类名。`IsWindowVisible` 顺手把"托盘溢出没弹出来"挡掉。
unsafe extern "system" fn enum_taskbar(hwnd: HWND, l: LPARAM) -> BOOL {
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    let cls = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    if matches!(
        cls.as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow"
    ) {
        let out = &mut *(l.0 as *mut Vec<isize>);
        out.push(hwnd.0 as isize);
    }
    TRUE
}

/// 截图场景的元素快照：一次采完 [`capture_targets`] 里的所有窗口并合并。
///
/// 覆盖层创建**之前**调，返回的坐标可直接和截图画布对齐。
pub fn snapshot_capture(budget: Duration) -> Vec<Element> {
    snapshot_targets(&capture_targets(), budget)
}

/// 采一组窗口。`budget` 是**总**预算，不是每个窗口的。
pub fn snapshot_targets(handles: &[isize], budget: Duration) -> Vec<Element> {
    if handles.is_empty() {
        return Vec::new();
    }
    // 卡死过的窗口直接跳过：同一个不响应 UIA 的程序不该每次都吃掉整份预算
    let handles = keep_live(handles, &BLACKLIST.lock().map(|g| g.clone()).unwrap_or_default());
    if handles.is_empty() {
        zx_core::log::info("UIA：目标窗口都在黑名单里（之前卡死过），本次跳过");
        return Vec::new();
    }
    let Some(tx) = ensure_tx() else { return Vec::new() };
    let (reply, done) = std::sync::mpsc::channel();
    let req = Req::Snapshot { handles: handles.to_vec(), budget, reply };
    if tx.send(req).is_err() {
        STUCK.store(true, Ordering::SeqCst);
        return Vec::new();
    }
    match done.recv_timeout(budget + Duration::from_millis(120)) {
        Ok(v) => v,
        Err(_) => {
            let who = CURRENT.load(Ordering::SeqCst);
            STUCK.store(true, Ordering::SeqCst);
            zx_core::log::warn(format!(
                "UIA 快照超时，丢弃该 worker 线程{}",
                if who != 0 { format!("（卡在 hwnd {who:#x}）") } else { String::new() }
            ));
            Vec::new()
        }
    }
}

/// 窗口的短标签（类名 + pid），只用在日志里。
unsafe fn win_label(h: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut buf = [0u16; 64];
    let n = GetClassNameW(h, &mut buf).max(0) as usize;
    let cls = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(h, Some(&mut pid));
    format!("{cls}({pid})")
}

/// 读一个 UIA 元素。返回 None 表示"不是一个值得展示的元素"。
/// 一次元素读取的原值，活引用和缓存两条路都归到这里，后面的判定只写一遍。
struct Raw {
    pid: i32,
    offscreen: bool,
    control_type: i32,
    name: String,
    rect: Option<Rect>,
}

unsafe fn read(el: &IUIAutomationElement, own_pid: i32, bounds: Rect) -> Result<Element, &'static str> {
    let raw = Raw {
        pid: el.CurrentProcessId().unwrap_or(-1),
        offscreen: el.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or(true),
        control_type: el
            .CurrentControlType()
            .map_err(|_| "活引用读不到类型")?
            .0,
        name: el.CurrentName().map(|b| b.to_string()).unwrap_or_default(),
        rect: el.CurrentBoundingRectangle().ok().and_then(Rect::from_rect),
    };
    judge(raw, own_pid, bounds)
}

/// 从缓存读。`FindAllBuildCache` 拿回来的数组必须走这条，`Current*` 会再跑一趟跨进程。
unsafe fn read_cached(
    el: &IUIAutomationElement,
    own_pid: i32,
    bounds: Rect,
) -> Result<Element, &'static str> {
    // 每个字段单独标出来：`ok()?` 会把"哪个属性没缓存到"吞掉，排查时完全看不见。
    let raw = Raw {
        pid: el.CachedProcessId().unwrap_or(-1),
        offscreen: el.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(true),
        control_type: el
            .CachedControlType()
            .map_err(|_| "缓存无类型")?
            .0,
        name: el.CachedName().map(|b| b.to_string()).unwrap_or_default(),
        rect: el.CachedBoundingRectangle().ok().and_then(Rect::from_rect),
    };
    judge(raw, own_pid, bounds)
}

fn judge(raw: Raw, own_pid: i32, bounds: Rect) -> Result<Element, &'static str> {
    if raw.pid < 0 {
        return Err("pid读不到");
    }
    if raw.pid == own_pid {
        return Err("自己的窗口");
    }
    // **不要信 `IsOffscreen`。** Chromium 在窗口失去焦点时会把整个页面标成 offscreen
    // （实测一页 1800+ 个元素全被这么判掉，只剩几个容器）。矩形才是事实：下面用
    // `intersects(rect, bounds)` 判，屏幕上就是屏幕上。
    let _ = raw.offscreen;
    let Some(rect) = raw.rect else {
        return Err("无矩形");
    };
    if rect.w < MIN_SIDE || rect.h < MIN_SIDE {
        return Err("太小");
    }
    // 滚动容器里已滚出屏幕的元素矩形会是负几万，`IsOffscreen` 并不总能拦住。
    if !intersects(rect, bounds) {
        return Err("在屏幕外");
    }
    let kind = Kind::from_control_type(raw.control_type);
    let name = clean_name(&raw.name);
    // 匿名容器也留着。用户要的"块"（一行卡片、侧边栏、内容面板）往往就是无名容器：
    // 之前把它们全丢掉，结果是悬停在面板空白处只能框到整个窗口，看起来像"只认得窗口"。
    // 噪声不靠这里挡，靠 `pick_stack` 的排序：可交互 > 有名字 > 面积小的。
    let useful = kind != Kind::Other || Kind::container(raw.control_type);
    if !useful {
        return Err("匿名容器");
    }
    Ok(Element::uia(rect, kind, name))
}

/// 洗 UIA 的 Name：去空白、去 `(&S)` 加速键、去掉尾部冒号。
fn clean_name(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    // `保存(&S)`、`编辑(E)` 这类加速键标记整组去掉；只去 `&` 会留下一个 `(S)`。
    // 按字节判断，但截断点必须落回字符边界（中文名 + 括号很常见）。
    let mut s = s;
    let b = s.as_bytes();
    let tail_len = if b.len() >= 4
        && b[b.len() - 1] == b')'
        && b[b.len() - 4] == b'('
        && b[b.len() - 3] == b'&'
        && b[b.len() - 2].is_ascii_alphanumeric()
        && s.is_char_boundary(b.len() - 4)
    {
        4
    } else if b.len() >= 3
        && b[b.len() - 1] == b')'
        && b[b.len() - 3] == b'('
        && b[b.len() - 2].is_ascii_uppercase()
        && s.is_char_boundary(b.len() - 3)
    {
        3
    } else {
        0
    };
    if tail_len > 0 {
        s = s[..b.len() - tail_len].trim_end();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // `&&` 是字面量 &，单个 & 是加速符，从显示文本里都去掉。
            if chars.peek() == Some(&'&') {
                chars.next();
                out.push('&');
            }
            continue;
        }
        out.push(c);
    }
    let out = out.trim();
    let out = out.strip_suffix('：').or_else(|| out.strip_suffix(':')).unwrap_or(out);
    let out = out.trim();
    if out.chars().count() > MAX_NAME_CHARS {
        let mut t: String = out.chars().take(MAX_NAME_CHARS).collect();
        t.push('…');
        t
    } else {
        out.to_string()
    }
}

unsafe fn probe(uia: &IUIAutomation, pt: POINT, deadline: Instant) -> Option<Element> {
    let mut cur = uia.ElementFromPoint(pt).ok()?;
    let walker = uia.ControlViewWalker().ok()?;
    let own_pid = std::process::id() as i32;
    let bounds = screen_bounds();

    let mut target: Option<Element> = None;
    let mut fallback: Option<Element> = None;
    for _ in 0..MAX_ASCEND {
        if Instant::now() >= deadline {
            break;
        }
        if let Ok(e) = read(&cur, own_pid, bounds) {
            // 命中可交互元素就停：这是"最内层可交互"，也正是用户心里的那个目标。
            // 点按钮里的图标时，图标只是 Image，真正的目标是外侧 Button。
            if e.kind.interactive() {
                target = Some(e);
                break;
            }
            if fallback.is_none() {
                fallback = Some(e);
            }
        }
        let ct = cur.CurrentControlType().map(|c| c.0).unwrap_or(0);
        if ct == UIA_WindowControlTypeId.0 {
            break;
        }
        match walker.GetParentElement(Some(&cur)) {
            Ok(p) => cur = p,
            Err(_) => break,
        }
    }
    target.or(fallback)
}

/// 光标处的**完整父链**（RawView，由小到大）。用于排查/接线验证。
///
/// 这条路不依赖预采集，是拿"当前这一刻"的真实层级——代价是每次要用
/// `ElementFromPoint` + 逐层 `GetParentElement`。
pub fn path_at(x: i32, y: i32) -> Vec<Element> {
    let Some(tx) = ensure_tx() else { return Vec::new() };
    let (reply, done) = std::sync::mpsc::channel();
    let pt = POINT { x, y };
    if tx.send(Req::Path { pt, reply }).is_err() {
        return Vec::new();
    }
    done.recv_timeout(Duration::from_millis(1200)).unwrap_or_default()
}

/// `ElementFromPoint` 取点上的元素，再用 RawViewWalker 一路往上，得到完整层级。
unsafe fn path_uncached(uia: &IUIAutomation, pt: POINT) -> Vec<Element> {
    let own_pid = std::process::id() as i32;
    let bounds = screen_bounds();
    let Ok(el) = uia.ElementFromPoint(pt) else {
        return Vec::new();
    };
    // 命中自己（覆盖层）就说明这条路被挡住了，直接报出来
    if el.CurrentProcessId().unwrap_or(-1) == own_pid {
        zx_core::log::warn("ElementFromPoint 命中了覆盖层自己");
        return Vec::new();
    }
    let Ok(walker) = uia.RawViewWalker() else {
        return Vec::new();
    };
    let mut out: Vec<Element> = Vec::new();
    let mut cur = Some(el);
    let mut depth = 0;
    while let Some(e) = cur {
        match read(&e, own_pid, bounds) {
            Ok(x) => out.push(x),
            Err(r) => {
                // 无名容器也要：它们正是"更大的元素"
                if let (Ok(ct), Ok(r2)) = (e.CurrentControlType(), e.CurrentBoundingRectangle()) {
                    if let Some(rect) = Rect::from_rect(r2) {
                        if r != "自己的窗口" {
                            out.push(Element {
                                rect,
                                kind: Kind::from_control_type(ct.0),
                                name: String::new(),
                                source: Source::Uia,
                                score: 1.0,
                            });
                        }
                    }
                }
            }
        }
        depth += 1;
        if depth > 12 {
            break;
        }
        cur = walker.GetParentElement(&e).ok();
    }
    out
}

/// 采一棵窗口子树。
///
/// **用 RawView + walker，而不是 FindAll + 条件**。这是这轮实测换来的结论：
///
/// * `FindAllBuildCache(Subtree, RawViewCondition)` 拿不到布局层级——条件只是交给
///   provider 过滤，Chromium 只提供控制视图那一棵（实测 357 个 ≈ ControlView 的 380 个）；
/// * 但 `RawViewWalker` 能走出完整层级：一个文本往上能爬到"段落 → 消息块 → 列表 →
///   滚动容器"十几层，每层都有矩形。PixPin 那种"一行卡片"的框就在这些层里。
///
/// 性能靠 `GetFirstChildElementBuildCache`：每个节点只一次跨进程调用，属性全在缓存里
/// 本地读。整棵树封顶 `MAX_NODES`，够覆盖网页的可见结构。
unsafe fn walk(uia: &IUIAutomation, hwnd: HWND, deadline: Instant) -> Vec<Element> {
    let Ok(walker) = uia.RawViewWalker() else {
        return Vec::new();
    };
    let own_pid = std::process::id() as i32;
    let bounds = screen_bounds();

    let Ok(req) = uia.CreateCacheRequest() else {
        return Vec::new();
    };
    for id in [
        UIA_ControlTypePropertyId,
        UIA_NamePropertyId,
        UIA_BoundingRectanglePropertyId,
        UIA_IsOffscreenPropertyId,
        UIA_ProcessIdPropertyId,
    ] {
        let _ = req.AddProperty(id);
    }
    // 注意：这里**不能**设 `AutomationElementMode_None`。那个模式只给缓存值、不给活引用，
    // 而 walker 遍历必须要活引用——实测设了之后 `GetFirstChildElementBuildCache` 直接
    // 返回空，整棵树只剩根节点一个。
    let root = match uia.ElementFromHandle(hwnd) {
        Ok(r) => r,
        Err(e) => {
            zx_core::log::warn(format!("UIA 取窗口元素失败：{e}"));
            return Vec::new();
        }
    };
    let root_area = root
        .CurrentBoundingRectangle()
        .ok()
        .and_then(Rect::from_rect)
        .map(|r| r.area())
        .unwrap_or(0);

    let mut out: Vec<Element> = Vec::new();
    let mut why: std::collections::BTreeMap<&'static str, usize> = std::collections::BTreeMap::new();
    let mut queue: std::collections::VecDeque<(IUIAutomationElement, u32)> =
        std::collections::VecDeque::new();
    queue.push_back((root, 0));
    let mut visited = 0usize;

    // 广度优先：先铺满上层，块状容器先到手；深度优先会被一条长列表吃光配额。
    while let Some((el, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_NODES {
            *why.entry(WHY_QUOTA).or_insert(0) += 1;
            break;
        }
        // 内部截止时间：调用方超时后会换一个新 worker，老的却还在爬——真遇到超大的
        // 树就会堆线程。这里自己收手，最多晚几十毫秒。
        if visited.is_multiple_of(64) && Instant::now() > deadline {
            *why.entry(WHY_DEADLINE).or_insert(0) += 1;
            break;
        }
        match read_cached(&el, own_pid, bounds) {
            Ok(e) => {
                if root_area == 0 || e.rect.area() * 10 < root_area * 7 {
                    out.push(e);
                } else {
                    *why.entry("铺满窗口").or_insert(0) += 1;
                }
            }
            Err(r) => *why.entry(r).or_insert(0) += 1,
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        // 父矩形完全在屏幕外 → 整棵子树都不用看。虚拟列表动辄几万像素高，
        // 不剪枝的话配额全耗在看不见的内容上（实测 1600ms 还超时）。
        let parent_rect = el.CachedBoundingRectangle().ok().and_then(Rect::from_rect);
        if let Some(r) = parent_rect {
            if !intersects(r, bounds) {
                *why.entry("子树在屏外").or_insert(0) += 1;
                continue;
            }
        }
        let mut child = walker.GetFirstChildElementBuildCache(Some(&el), Some(&req)).ok();
        let mut n = 0usize;
        while let Some(c) = child {
            n += 1;
            if n > MAX_SIBLINGS {
                *why.entry(WHY_SIBLINGS).or_insert(0) += 1;
                break;
            }
            queue.push_back((c.clone(), depth + 1));
            child = walker.GetNextSiblingElementBuildCache(Some(&c), Some(&req)).ok();
        }
    }

    if !why.is_empty() {
        zx_core::log::info(format!("UIA RawView 采到 {} 个 {why:?}", out.len()));
    }
    // 只有"树没走完"才算截断；`子树在屏外` / `铺满窗口` 是正常剪枝。
    if why.keys().any(|k| is_cut(k)) {
        TRUNCATED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // RawView 里同一块区域常常有 3-5 层矩形完全相同的 Group，必须合并，
    // 否则滚轮要滚十几次才动一格。
    dedupe_nested(&mut out);
    out.sort_by_key(|e| (e.rect.y, e.rect.x));
    out
}

/// 父子同框（容器和它唯一的子元素矩形完全一致）只留一个。
///
/// ControlView 里这种情况极多：一个 Pane 包一个 Button，两者 bbox 一模一样。
/// 规则：无名字的那个让位给有名字的；都没名字就留小的（更可能是真正控件）。
/// 把"同一块区域被报了多次"的元素合并掉。
///
/// 分两道，因为两种情况的数量级完全不同：
///
/// 1. **矩形完全相同的**（同一个元素被两个窗口各报了一次）走哈希，O(n)。
///    这是最常见的情况，以前也要走 O(n²) 的全量比对。
/// 2. **只差几像素的**（dom 盒模型里嵌套 Group）按面积排序后只在窗口内比 `iou`。
///    关键不等式：若 A ≤ B 且 `iou(A,B) ≥ 0.92`，则 `B/A ≤ 1/0.92 ≈ 1.087`。
///    所以面积差超过 9% 的候选可以直接跳过——这一条把窗口压得很短。
///
/// 以前是纯 O(n²) 全量 `iou`：4000 个元素最坏 1600 万次（两边各扫一遍），
/// 实测吃掉采集预算的一大截。现在常见的整页（450 元素）基本退化成线性。
fn dedupe_nested(v: &mut Vec<Element>) {
    if v.len() < 2 {
        return;
    }
    let mut drop = vec![false; v.len()];

    // ---- 第一道：矩形完全相同 ----
    let mut keep: std::collections::HashMap<(i32, i32, i32, i32), usize> =
        std::collections::HashMap::with_capacity(v.len());
    for i in 0..v.len() {
        let key = (v[i].rect.x, v[i].rect.y, v[i].rect.w, v[i].rect.h);
        match keep.get(&key).copied() {
            // 同名状态下保留先来的；有名的那份赢（名字是之后要显示的）
            Some(j) if v[j].name.is_empty() && !v[i].name.is_empty() => {
                drop[j] = true;
                keep.insert(key, i);
            }
            Some(_) => drop[i] = true,
            None => {
                keep.insert(key, i);
            }
        }
    }

    // ---- 第二道：面积接近、矩形只差几像素 ----
    let mut idx: Vec<usize> = (0..v.len()).filter(|&i| !drop[i]).collect();
    // 稳定排序：面积相同的保持原顺序，后面的"平手取靠前"规则才成立
    idx.sort_by_key(|&i| v[i].rect.area());
    for a in 0..idx.len() {
        let i = idx[a];
        if drop[i] {
            continue;
        }
        let area_i = v[i].rect.area();
        for &j in &idx[a + 1..] {
            if drop[j] {
                continue;
            }
            // 面积差超 9% 时 iou 不可能到 0.92，而且后面只会更大 —— 直接收工
            if v[j].rect.area() as f64 > area_i as f64 * 1.09 {
                break;
            }
            let (ri, rj) = (v[i].rect, v[j].rect);
            // 四个整数比较就能挡掉绝大多数：不相交的一对不可能 iou ≥ 0.92
            if ri.right() <= rj.x
                || rj.right() <= ri.x
                || ri.y + ri.h <= rj.y
                || rj.y + rj.h <= ri.y
            {
                continue;
            }
            if iou(ri, rj) < 0.92 {
                continue;
            }
            let (mi, mj) = (v[i].name.is_empty(), v[j].name.is_empty());
            if mi && !mj {
                // i 无名、j 有名：i 淘汰，这一轮不用再比了
                drop[i] = true;
                break;
            }
            // 其余情况保留 i（面积更小或相同且靠前）
            let _ = mj;
            drop[j] = true;
        }
    }

    retain_keep(v, &drop);
}

// ---------------------------------------------------------------- 融合

/// 把 UIA 元素和 OCR 行合成一份结果。
///
/// * OCR 行落在某个元素里（压住 ≥55% 面积）→ 补进该元素的文本；
/// * 落不进任何元素 → 自己成为 `Kind::Text`；
/// * UIA 有名字、OCR 也有读数的，交给 [`pick_text`] 裁。
pub fn fuse(mut ui: Vec<Element>, ocr: &[OcrLine]) -> Vec<Element> {
    let mut orphans: Vec<Element> = Vec::new();
    for line in ocr {
        if line.text.trim().is_empty() {
            continue;
        }
        let mut best: Option<(usize, f32)> = None;
        for (i, e) in ui.iter().enumerate() {
            // 元素一般比文字行大：先看行被元素盖住多少，再看元素多大（越小越精确）。
            let cover = coverage(line.rect, e.rect);
            if cover < 0.55 {
                continue;
            }
            let score = cover - (e.rect.area() as f32 / 1.0e7);
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((i, score));
            }
        }
        match best {
            Some((i, _)) => {
                let e = &mut ui[i];
                e.name = pick_text(e.kind, &e.name, &line.text, line.score);
                if e.score < line.score {
                    e.score = line.score;
                }
            }
            // 落不进任何控件、置信度又低的行，基本是图标被当成了字（实测按钮图标
            // 会被读出 0.2–0.65 的"Q""T""×"）。这种宁可不要，否则选区里多出一堆
            // 假文本区域，比漏掉更烦人。
            None if line.score >= ORPHAN_SCORE => orphans.push(Element {
                rect: line.rect,
                kind: Kind::Text,
                name: line.text.trim().to_string(),
                source: Source::Ocr,
                score: line.score,
            }),
            None => {}
        }
    }
    ui.extend(orphans);
    // 菜单项/按钮内部还挂着一堆 Text 子节点（"编辑" 的 "编辑("、"w"、"H"、"A"），
    // 它们和父控件的标签重复，留着只是噪声；同时保持独立文本区域不被误删。
    let mut drop = vec![false; ui.len()];
    for i in 0..ui.len() {
        if ui[i].kind != Kind::Text {
            continue;
        }
        for j in 0..ui.len() {
            if i == j || ui[j].kind == Kind::Text || ui[j].name.is_empty() {
                continue;
            }
            if ui[j].kind.interactive() && coverage(ui[i].rect, ui[j].rect) >= 0.9 {
                drop[i] = true;
                break;
            }
        }
    }
    retain_keep(&mut ui, &drop);
    dedupe_nested(&mut ui);
    ui.sort_by_key(|e| (e.rect.y, e.rect.x));
    ui
}

fn retain_keep(v: &mut Vec<Element>, drop: &[bool]) {
    let mut k = 0usize;
    v.retain(|_| {
        let keep = !drop.get(k).copied().unwrap_or(false);
        k += 1;
        keep
    });
}

/// UIA 名字和 OCR 读数冲突时怎么裁——这是准确率的关键一环，规则按类型分开。
///
/// 实机踩过的坑：按钮"最大化"的图形被 OCR 读成"口"、图标读成"0"，置信度还高达
/// 0.95 以上。**对可命名控件（按钮/菜单/链接/标签页/勾选）UIA 的 Name 就是它的
/// 标题文本，本身就是真值**，让 OCR 去"校正"它只会引入噪声。所以：
///
/// * 输入框：OCR 读的是框里的实际内容，比控件标签有用 → 信 OCR；
/// * 文本区：UIA 的 Name 经常被截断 → OCR 更长且置信度高时用 OCR；
/// * 其余控件：以 UIA 名为准，只有它明显被截断（以 `…`/`...` 结尾）才用 OCR；
/// * 图标：形状不是文字，OCR 结果一律丢掉；
/// * UIA 名为空：OCR 是唯一来源，照收（但空文本不要）。
pub fn pick_text(kind: Kind, uia_name: &str, ocr_text: &str, ocr_score: f32) -> String {
    let uia = uia_name.trim();
    let ocr = ocr_text.trim();
    if ocr.is_empty() {
        return uia.to_string();
    }
    if uia.is_empty() {
        return if kind == Kind::Icon { String::new() } else { ocr.to_string() };
    }
    match kind {
        Kind::Icon => uia.to_string(),
        Kind::Input => ocr.to_string(),
        Kind::Text => {
            let longer = ocr.chars().count() > uia.chars().count();
            if ocr_score >= 0.85 && longer {
                ocr.to_string()
            } else {
                uia.to_string()
            }
        }
        _ => {
            let truncated = uia.ends_with('…') || uia.ends_with("...");
            if truncated {
                ocr.to_string()
            } else {
                uia.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_of_identical_boxes_is_one() {
        let a = Rect::new(10, 10, 100, 40);
        assert!((iou(a, a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_of_disjoint_boxes_is_zero() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(100, 100, 10, 10);
        assert_eq!(iou(a, b), 0.0);
    }

    #[test]
    fn inside_ratio_handles_small_in_big() {
        let big = Rect::new(0, 0, 200, 60);
        let small = Rect::new(20, 20, 80, 20);
        assert!((coverage(small, big) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clean_name_strips_accelerator() {
        assert_eq!(clean_name("保存(&S)"), "保存");
        // Win32 约定：单个 & 是加速符，字面量 & 要写成 &&。
        assert_eq!(clean_name("R&&D"), "R&D");
        assert_eq!(clean_name("R&D"), "RD");
        assert_eq!(clean_name("编辑(E)"), "编辑");
        assert_eq!(clean_name("名称："), "名称");
        // 括号里的普通数字不能被当成加速键吃掉。
        assert_eq!(clean_name("回收站(2)"), "回收站(2)");
    }

    #[test]
    fn pick_text_respects_control_semantics() {
        // 按钮标题以 UIA 为准：OCR 把"最大化"的图形读成"口"也不能覆盖它。
        assert_eq!(pick_text(Kind::Button, "最大化", "口", 0.97), "最大化");
        // 图标是形状不是文字，OCR 结果一律丢。
        assert_eq!(pick_text(Kind::Icon, "", "0", 0.95), "");
        // 输入框要的是框里的实际内容，不是控件标签。
        assert_eq!(pick_text(Kind::Input, "搜索", "关键词", 0.90), "关键词");
        // 文本区被截断时，用更长的 OCR。
        assert_eq!(
            pick_text(Kind::Text, "文件名很长…", "文件名很长很长的完整标题", 0.95),
            "文件名很长很长的完整标题"
        );
        // 文本区 OCR 反而更短（漏字），以 UIA 为准。
        assert_eq!(pick_text(Kind::Text, "完整标题", "完整", 0.90), "完整标题");
        // UIA 空白时 OCR 是唯一来源。
        assert_eq!(pick_text(Kind::Text, "", "自绘文本", 0.60), "自绘文本");
    }

    #[test]
    fn fuse_attaches_ocr_text_to_enclosing_element() {
        let ui = vec![Element::uia(Rect::new(100, 100, 120, 36), Kind::Button, String::new())];
        let lines = vec![OcrLine {
            rect: Rect::new(130, 110, 60, 18),
            text: "确定".into(),
            score: 0.93,
        }];
        let fused = fuse(ui, &lines);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].name, "确定");
        assert_eq!(fused[0].kind, Kind::Button);
    }

    #[test]
    fn fuse_drops_low_confidence_orphan() {
        // 图标被当字读出来的那种：落不进任何控件，置信度很低 → 丢掉
        let fused = fuse(
            Vec::new(),
            &[OcrLine { rect: Rect::new(10, 10, 18, 18), text: "Q".into(), score: 0.40 }],
        );
        assert!(fused.is_empty());
        // 同样的位置、置信度够高 → 当成真实的文本区域留下
        let fused = fuse(
            Vec::new(),
            &[OcrLine { rect: Rect::new(10, 10, 18, 18), text: "Q".into(), score: 0.92 }],
        );
        assert_eq!(fused.len(), 1);
    }

    #[test]
    fn fuse_keeps_orphan_ocr_line_as_text() {
        let fused = fuse(Vec::new(), &[OcrLine {
            rect: Rect::new(10, 10, 80, 16),
            text: "自绘文本".into(),
            score: 0.9,
        }]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].kind, Kind::Text);
        assert_eq!(fused[0].source, Source::Ocr);
    }

    #[test]
    fn stack_at_skips_near_identical_levels() {
        // 同一块区域 561x1032 / 561x1033 这种差一像素的层不该算两层，否则滚轮
        // 滚三次画面才动一下。
        let outer = Element::uia(Rect::new(0, 0, 561, 1032), Kind::Other, "外".into());
        let mid = Element::uia(Rect::new(0, 0, 561, 1033), Kind::Other, "中".into());
        let big = Element::uia(Rect::new(0, 0, 800, 1200), Kind::Other, "大".into());
        let card = Element::uia(Rect::new(10, 10, 100, 40), Kind::Link, "卡片".into());
        let elems = vec![card, outer, mid, big];
        let chain = stack_at(&elems, 20, 20);
        // 差 1 像素的两个框谁被选中都行，但只能占一层，最后一定是"大"
        assert_eq!(chain.len(), 3, "差 1 像素的层应被合并掉");
        assert_eq!(chain[0].name, "卡片");
        assert!(chain[1].name == "外" || chain[1].name == "中");
        assert_eq!(chain[2].name, "大");
    }

    #[test]
    fn stack_at_walks_up_by_containment() {
        // 卡片（链接）在一行（List 容器）里，行又在页面里。
        // 层级链应该是：链接 → 行 → 页面，由小到大。
        let page = Element::uia(Rect::new(0, 0, 1200, 900), Kind::Other, "页面".into());
        let row = Element::uia(Rect::new(480, 480, 400, 60), Kind::Other, "卡片行".into());
        let card = Element::uia(Rect::new(500, 490, 180, 40), Kind::Link, "卡片".into());
        let elems = vec![page, row, card];
        let chain = stack_at(&elems, 510, 500);
        assert_eq!(chain.len(), 3, "应给出 元素→行→页面 三层");
        assert_eq!(chain[0].kind, Kind::Link);
        assert_eq!(chain[1].name, "卡片行");
        assert_eq!(chain[2].name, "页面");
    }

    /// 旧实现原样搬进来当参照：新的分桶版必须和它在随机用例上给出同样的存活集合。
    fn dedupe_reference(v: &mut Vec<Element>) {
        if v.len() < 2 {
            return;
        }
        let mut drop = vec![false; v.len()];
        for i in 0..v.len() {
            for j in 0..v.len() {
                if i == j || drop[i] || drop[j] {
                    continue;
                }
                if iou(v[i].rect, v[j].rect) < 0.92 {
                    continue;
                }
                let (a, b) = (&v[i], &v[j]);
                let lose = match (a.name.is_empty(), b.name.is_empty()) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => match a.rect.area().cmp(&b.rect.area()) {
                        std::cmp::Ordering::Greater => true,
                        std::cmp::Ordering::Less => false,
                        std::cmp::Ordering::Equal => i > j,
                    },
                };
                if lose {
                    drop[i] = true;
                    break;
                }
            }
        }
        retain_keep(v, &drop);
    }

    // 「这次采集是不是被截断了」直接决定界面上要不要标一句，判错的后果是用户
    // 一直看到"结构已截断"（或者一直看不到），所以把判定本身钉住。
    #[test]
    fn only_unfinished_trees_count_as_truncated() {
        assert!(is_cut(WHY_QUOTA));
        assert!(is_cut(WHY_SIBLINGS));
        assert!(is_cut(WHY_DEADLINE));
        // 屏幕外剪枝和"铺满窗口"是正常优化，不能提示成截断
        assert!(!is_cut("子树在屏外"));
        assert!(!is_cut("铺满窗口"));
        assert!(!is_cut(""));
    }

    #[test]
    fn dedupe_matches_reference_on_random_input() {
        // 线性同余，够用且不引依赖
        let mut seed: u64 = 0x2026_1126_abcd;
        let mut rnd = move |n: u32| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % n as u64) as i32
        };
        for round in 0..200 {
            let n = 3 + (rnd(18) as usize);
            let mut base = Vec::new();
            for _ in 0..n {
                let x = rnd(40);
                let y = rnd(30);
                let w = 10 + rnd(30);
                let h = 8 + rnd(20);
                let named = rnd(2) == 0;
                let mut e = Element::uia(
                    Rect::new(x, y, w, h),
                    Kind::Other,
                    if named { format!("n{}", rnd(100)) } else { String::new() },
                );
                // 一半的元素做成"只差一两像素"的孪生，专门喂给近似分支
                if rnd(2) == 0 {
                    e.rect = Rect::new(x + rnd(3), y + rnd(3), w + rnd(3), h + rnd(3));
                }
                base.push(e);
            }
            let mut a = base.clone();
            let mut b = base;
            dedupe_reference(&mut a);
            dedupe_nested(&mut b);
            let key = |v: &[Element]| {
                let mut k: Vec<(i32, i32, i32, i32, String)> =
                    v.iter().map(|e| (e.rect.x, e.rect.y, e.rect.w, e.rect.h, e.name.clone())).collect();
                k.sort();
                k
            };
            assert_eq!(key(&a), key(&b), "第 {round} 组随机用例结果不一致");
        }
    }

    /// 量一下分桶前后的差距。`cargo test -p capture bench_dedupe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_dedupe_scale() {
        let mut seed: u64 = 7;
        let mut rnd = move |n: u32| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) % n as u64) as i32
        };
        for count in [450usize, 4000] {
            let mut v = Vec::new();
            for _ in 0..count {
                v.push(Element::uia(
                    Rect::new(rnd(2400), rnd(1300), 20 + rnd(300), 12 + rnd(120)),
                    Kind::Other,
                    String::new(),
                ));
            }
            let mut a = v.clone();
            let mut b = v;
            let t0 = std::time::Instant::now();
            dedupe_reference(&mut a);
            let old = t0.elapsed();
            let t1 = std::time::Instant::now();
            dedupe_nested(&mut b);
            let new = t1.elapsed();
            println!("元素 {count}: 旧 {:?} → 新 {:?}（存活 {} / {}）", old, new, a.len(), b.len());
        }
    }

    #[test]
    fn dedupe_drops_exact_twins() {
        // 同一个元素被两个窗口各报一次（矩形和名字完全一样）时必须只留一个：
        // 面积相等时如果两边都"不比自己小"，谁都删不掉，元素会成对出现。
        let mk = || Element::uia(Rect::new(100, 100, 40, 20), Kind::Button, "保存".into());
        let mut v = vec![mk(), mk(), mk()];
        dedupe_nested(&mut v);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn info_line_is_one_tab_separated_line() {
        let el = Element::uia(Rect::new(1680, 188, 46, 30), Kind::Button, "最小化".into());
        assert_eq!(el.info_line(), "按钮 · 最小化	1680,188 46×30");
    }

    #[test]
    fn local_rect_clips_to_canvas() {
        let r = Rect::new(50, 50, 100, 40);
        assert_eq!(r.to_local(0, 0, 200, 100), Some((50, 50, 100, 40)));
        // 超出右边界的部分被裁掉。
        assert_eq!(r.to_local(0, 0, 120, 100), Some((50, 50, 70, 40)));
        // 完全在画布外。
        assert_eq!(r.to_local(500, 500, 200, 100), None);
    }

    #[test]
    fn blacklisted_windows_are_skipped() {
        // 卡死过的那个窗口跳过，其余照旧——不能因为它一个把整次采集变成空
        assert_eq!(super::keep_live(&[10, 20, 30], &[20]), vec![10, 30]);
        assert_eq!(super::keep_live(&[10], &[]), vec![10]);
        assert!(super::keep_live(&[10, 20], &[10, 20]).is_empty());
    }
}
