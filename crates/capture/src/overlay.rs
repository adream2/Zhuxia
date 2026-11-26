//! 全屏选区 / 编辑器窗口。
//!
//! ```text
//!           ┌──────── 单击（鼠标下窗口）────────┐
//! Choosing ──拖动──▶ Dragging ──松手──▶ Selected ──Enter/双击/✓──▶ Editing ──复制/保存/贴图──▶ 关
//!    ▲                                  │  ▲                        │
//!    └────────── Esc（先清选区再退出）────┘  └──── Esc（回到可调整）────┘
//! ```
//!
//! 两个阶段共用**同一个分层窗口**，因为它们共用同一套坐标：
//!
//! - **选区阶段**（`frame` 有值）：屏幕被冻结成一帧，单击截窗口、拖动框选、松手可调整，
//!   放大镜、窗口轮廓都在这一步。
//! - **编辑阶段**（`doc` 有值）：选区被裁成一张独立的图，之后所有操作（标注、美化、
//!   拼接、长截图、OCR）都只跟这张图有关。图比屏幕大时自动缩放到能放下（长截图必须）。
//!
//! 把"编辑源"从"屏幕上的某块区域"改成"一张图"，是后面四个功能能接上的前提：
//! 拼接要显示一张不存在于屏幕上的合成图，长截图要显示一张几千像素高的图。

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_core::RecordFormat;
use zx_ui::window::{create_window, register_class};
use zx_ui::Canvas;

use crate::annotate::{self, AHdl, Anno, Tool, PALETTE};
use crate::design::{self, Icon, Tokens};
use crate::longshot;
use crate::ocr;
use crate::output;
use crate::picker;
use crate::pin;
use crate::record;
use crate::screen::{self, Frame};
use crate::stitch;
use crate::style;
use crate::stylepanel;
use crate::target::{self, Target};
use crate::uielem::{self, Element};

const CLASS: &str = "ZhuxiaCaptureOverlay";
const TEXT_CLASS: &str = "ZhuxiaCaptureText";
const WM_TEXT_COMMIT: u32 = WM_APP + 81;
const WM_TEXT_CANCEL: u32 = WM_APP + 82;
/// UIA 元素快照到位（后台线程通知覆盖层重画一次）
const WM_ELEMS_READY: u32 = WM_APP + 84;
pub const WM_STYLE_CHANGED: u32 = WM_APP + 83;

const BG_DIM: u32 = 0x9900_0000;
/// 悬停预览时的压暗强度（比确定选区的 60% 轻，得让人看清下面的内容）
const HOVER_DIM: u32 = 0x5900_0000;
const ACCENT: u32 = 0xFF2E90FA;
const EDITOR_BG: u32 = 0xFF141414;
/// 工具条分组竖线占位：1px 线 + 左右各 4px。
const SEP_W: f32 = 9.0;

// ─── 浮动工具条尺寸（逻辑像素，绘制时统一乘 `s`）─────────
/// `h-10`
const BAR_H: f32 = 40.0;
/// `px-1.5`
const BAR_PAD: f32 = 6.0;
/// `h-7 w-7`（ContextBar 的 `size="sm"`）
const BTN: f32 = 28.0;
/// `rounded-[7px]`
const BTN_R: f32 = 7.0;
/// `gap-1`
const GAP: f32 = 4.0;
/// 主操作按钮 `h-8 rounded-[9px] px-3 text-[12.5px]`
const PRIMARY_H: f32 = 32.0;
const PRIMARY_R: f32 = 9.0;
const PRIMARY_PAD: f32 = 12.0;

/// 工具条上的一个格子：按钮、竖线，或那个带文字的「复制」主操作。
#[derive(Clone, Copy)]
enum Slot {
    Btn(Act, f32),
    Sep,
    Primary(Act, f32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 普通截图
    Select,
    /// 拼接：连续框选，Enter 出成品
    Append,
    /// 长截图：选区后交给 `longshot` 的控制条
    Long,
    /// 录屏：选区后交给 `record`，框完即开录（格式按设置）
    Record,
    /// 录 GIF：同上，但本次强制 GIF
    RecordGif,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage { Choosing, Dragging, Selected, Editing }

/// 色板上的一个格子：预设色，或"自定义…"。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    Preset(usize),
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle { N, S, E, W, NW, NE, SW, SE, Move }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Tool(Tool),
    Undo,
    Redo,
    /// 当前颜色：点开 8 色板
    ColorPick,
    Beautify,
    Stitch,
    Ocr,
    /// 长截图（滚动拼接）
    Long,
    /// 录屏：按设置里的格式（视频 / GIF）
    Record,
    /// 录 GIF：本次强制 GIF，不改配置
    RecordGif,
    Copy,
    Save,
    /// 上传到图床（宿主接线，没接线时不显示）
    Upload,
    Pin,
    Cancel,
}

/// 悬停气泡上的小按钮。和工具条的 `Act` 分开：它只活在"光标压着一个元素"的时候。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chip {
    /// 复制元素的文本（UIA 的 Name，已经洗干净）
    CopyText,
    /// 复制元素的类型/名称/矩形，一行制表符分隔，方便贴进 issue 或表格
    CopyInfo,
    /// 把元素框里的像素原样存成 PNG（图标提取最常用）
    SaveIcon,
}

struct Ov {
    hwnd: HWND,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
    dpi: u32,
    canvas: Canvas,
    /// 选区阶段的背景（冻结的整屏）
    frame: Option<Frame>,
    /// 编辑阶段的图（不含标注）
    doc: Option<Canvas>,
    /// 图在屏幕上的显示矩形（缩放后）
    doc_rect: RECT,
    doc_scale: f32,
    stage: Stage,
    mode: Mode,
    cursor: (i32, i32),
    anchor: (i32, i32),
    sel: RECT,
    chain: Vec<Target>,
    level: usize,
    /// 悬停链第 0 层如果是 UI 元素就放这里（窗口/显示器那一层没有）
    hover_elem: Option<Element>,
    /// 上一次记过日志的悬停结果（true=命中元素）。只在翻转时打一行，排除刷屏。
    hover_log: Option<bool>,
    last_detect: Instant,
    last_key: (i32, i32),
    drag_move: Option<(i32, i32)>,
    adjust: Option<(Handle, (i32, i32), RECT)>,
    annos: Vec<Anno>,
    hist: Vec<Vec<Anno>>,
    redo: Vec<Vec<Anno>>,
    tool: Tool,
    color: u32,
    width: f32,
    /// 滚轮改粗细的"手势"：同一个标注 800ms 内连续滚动只记一次撤销点
    width_edit: Option<(usize, Instant)>,
    /// 色板是否展开
    color_open: bool,
    /// 色板里每个格子的命中区（每帧重算）
    palette: Vec<(RECT, Pick)>,
    drawing: Option<Anno>,
    dragging_anno: Option<(usize, (i32, i32))>,
    /// 正在拖的标注控制点：(下标, 哪个点, 按下时的快照)
    anno_resize: Option<(usize, AHdl, Anno)>,
    selected: Option<usize>,
    text_win: HWND,
    text_edit: HWND,
    text_font: isize,
    text_pos: (i32, i32),
    toolbar: Vec<(RECT, Act)>,
    /// 悬停气泡上的小按钮命中区（每帧重算）
    chips: Vec<(RECT, Chip)>,
    /// 刚复制过的那个按钮，1.2 秒内显示"已复制"
    chip_flash: Option<(Chip, Instant)>,
    hidden_pins: Vec<HWND>,
    /// 这两项在截图期间不会变（改设置要重启会话），缓存下来避免每次鼠标移动
    /// 都从 settings.yaml 的内存副本里深度拷贝一遍 Settings。
    magnifier: bool,
    detect_window: bool,
    /// 元素层开关。PixPin 的 Tab：在"只认窗口"和"识别元素"之间切；这里同义。
    elements_on: bool,
    /// UIA 元素快照（截图开始前另一条线程采）。空数组 = 还没到或这个窗口没有 a11y。
    /// 悬停时它比"窗口/子窗口"更细一层，是选区阶段最常用的落点。
    elems: Arc<Mutex<Vec<Element>>>,
    /// 刚刚复制过的颜色值（放大镜信息条上闪一下，1 秒后自动消失）
    color_flash: Option<String>,
    /// 本次会话的设计 token（读注册表取浅色/深色，每帧查太贵，开会话时定一次）
    tk: Tokens,
}

thread_local! {
    static OV: Cell<*mut Ov> = const { Cell::new(std::ptr::null_mut()) };
}

static TEXT_OWNER: AtomicIsize = AtomicIsize::new(0);
static EDIT_OLD: AtomicIsize = AtomicIsize::new(0);

/// 长截图要在覆盖层关闭之后才能跑（否则滚轮事件发不到页面），
/// 这里把"关掉后去跑长截图"的意图暂存一下。
static PENDING_LONG: OnceLock<std::sync::Mutex<Option<RECT>>> = OnceLock::new();

fn pending_long() -> &'static std::sync::Mutex<Option<RECT>> {
    PENDING_LONG.get_or_init(|| std::sync::Mutex::new(None))
}

/// 录屏同理：覆盖层必须先从屏幕上消失（`CAPTUREBLT` 会把它拍进每一帧），
/// 关窗之后再把选区交给 `record::run`。
/// 第二个元素是“本次强制格式”：`None` = 按设置，`Some(Gif)` = 工具条的 GIF 按钮。
/// 待处理的录屏请求：选区 + 本次强制格式（`None` = 按设置）。
type PendingRecord = (RECT, Option<RecordFormat>);
static PENDING_RECORD: OnceLock<std::sync::Mutex<Option<PendingRecord>>> = OnceLock::new();

fn pending_record() -> &'static std::sync::Mutex<Option<PendingRecord>> {
    PENDING_RECORD.get_or_init(|| std::sync::Mutex::new(None))
}

fn with_ov<R>(f: impl FnOnce(&mut Ov) -> R) -> Option<R> {
    OV.with(|c| {
        let p = c.get();
        if p.is_null() { None } else { Some(unsafe { f(&mut *p) }) }
    })
}

pub fn is_active() -> bool {
    OV.with(|c| !c.get().is_null())
}

/// 当前覆盖层句柄（样式面板改完样式后通知它重绘）。
pub fn hwnd() -> Option<HWND> {
    OV.with(|c| {
        let p = c.get();
        if p.is_null() { None } else { Some(unsafe { (*p).hwnd }) }
    })
}

/// 开始一次截图。失败（锁屏、安全桌面、抓屏被拒）返回 false。
pub fn begin() -> bool {
    begin_with(Mode::Select)
}

/// 按指定意图开覆盖层：`Mode::Record` 时框完松手直接开录（Ctrl+R 一步到位）。
pub fn begin_with(mode: Mode) -> bool {
    if is_active() {
        return false;
    }
    // 录制期间不弹截图覆盖层：开了也会被自己的录制拍进去
    if record::is_active() {
        zx_core::log::warn("正在录屏，先结束录制再截图（Ctrl+Shift+R 停止）");
        return false;
    }
    let hidden = pin::hide_all();
    let Some(frame) = Frame::grab() else {
        pin::restore(hidden);
        zx_core::log::warn("截图失败：抓不到屏幕（锁屏/UAC 安全桌面/远程会话？）");
        return false;
    };
    let ok = spawn_overlay(Some(frame), None, hidden, true, Stage::Choosing);
    if ok && mode != Mode::Select {
        with_ov(|o| {
            o.mode = mode;
            o.paint();
        });
    }
    ok
}

/// 用一张现成的图开编辑器（长截图完成后走这里）。
pub fn open_doc(doc: Canvas) -> bool {
    if is_active() {
        return false;
    }
    let hidden = pin::hide_all();
    spawn_overlay(None, Some(doc), hidden, true, Stage::Editing)
}

fn spawn_overlay(frame: Option<Frame>, doc: Option<Canvas>, hidden: Vec<HWND>,
                 detect: bool, stage: Stage) -> bool {
    let (vx, vy, vw, vh) = match &frame {
        Some(f) => (f.x, f.y, f.w, f.h),
        None => unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        },
    };
    // UIA 元素快照得赶在覆盖层出现之前把目标窗口句柄定下来：覆盖层一旦显示，
    // 它就是前台窗口，再查就查到自己头上了。真正的遍历丢给后台线程，不拖慢选区。
    let pending = start_element_snapshot(stage == Stage::Choosing);
    let Some(hwnd) = create_window(CLASS, "驻匣 · 截图", WS_POPUP,
                                   WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, vw, vh) else {
        zx_core::log::warn("截图失败：选区窗口创建失败");
        pin::restore(hidden);
        return false;
    };
    spawn_element_worker(&pending, hwnd);
    let dpi = unsafe {
        let d = GetDpiForWindow(hwnd);
        if d == 0 { 96 } else { d }
    };
    let c = crate::cfg();
    let mut canvas = Canvas::new(vw, vh, dpi);
    if let Some(f) = &frame {
        f.blit_into(&mut canvas);
    }
    let mut ov = Box::new(Ov {
        hwnd,
        vx, vy, vw, vh, dpi,
        canvas,
        frame,
        doc,
        doc_rect: RECT { left: vx, top: vy, right: vx, bottom: vy },
        doc_scale: 1.0,
        stage,
        mode: Mode::Select,
        cursor: (vx + vw / 2, vy + vh / 2),
        anchor: (0, 0),
        sel: RECT { left: 0, top: 0, right: 0, bottom: 0 },
        chain: Vec::new(),
        level: 0,
        hover_elem: None,
        hover_log: None,
        last_detect: Instant::now() - std::time::Duration::from_secs(1),
        last_key: (0, 0),
        drag_move: None,
        adjust: None,
        annos: Vec::new(),
        hist: Vec::new(),
        redo: Vec::new(),
        tool: Tool::Rect,
        color: PALETTE[0],
        width: 3.0,
        width_edit: None,
        color_open: false,
        palette: Vec::new(),
        drawing: None,
        dragging_anno: None,
        anno_resize: None,
        selected: None,
        text_win: HWND::default(),
        text_edit: HWND::default(),
        text_font: 0,
        text_pos: (0, 0),
        toolbar: Vec::new(),
        chips: Vec::new(),
        chip_flash: None,
        hidden_pins: hidden,
        magnifier: c.magnifier,
        detect_window: c.detect_window,
        elements_on: c.detect_window,
        elems: pending.slot,
        color_flash: None,
        tk: design::tokens(),
    });
    let mut pt = POINT::default();
    unsafe { let _ = GetCursorPos(&mut pt); }
    if pt.x >= vx && pt.y >= vy && pt.x < vx + vw && pt.y < vy + vh {
        ov.cursor = (pt.x, pt.y);
    }
    if ov.stage == Stage::Editing {
        ov.layout_doc(true);
    }
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, vx, vy, vw, vh, SWP_SHOWWINDOW);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    force_foreground(hwnd);
    let ptr = Box::into_raw(ov);
    OV.with(|c| c.set(ptr));
    unsafe {
        let _ = SetCursor(LoadCursorW(None, IDC_CROSS).unwrap_or_default());
        let _ = SetFocus(hwnd);
    }
    with_ov(|o| {
        if detect {
            o.detect(true);
        }
        o.paint();
    });
    // 自检钩子：直接摆一个选区进编辑态并画几条标注，用来核对工具条/标注/美化入口。
    if std::env::var("ZHUXIA_CAPTURE_SELFTEST_EDIT").ok().as_deref() == Some("1") {
        with_ov(|o| {
            if o.stage != Stage::Choosing || o.frame.is_none() {
                return;
            }
            let w = (o.vw as f32 * 0.55) as i32;
            let h = (o.vh as f32 * 0.5) as i32;
            let rect = RECT {
                left: o.vx + (o.vw - w) / 2,
                top: o.vy + (o.vh - h) / 2,
                right: o.vx + (o.vw - w) / 2 + w,
                bottom: o.vy + (o.vh - h) / 2 + h,
            };
            o.enter_editing(rect);
            let mut r1 = Anno::new(Tool::Rect, (40, 40), PALETTE[0], 0);
            r1.set_end((320, 180));
            let mut a1 = Anno::new(Tool::Arrow, (60, 300), PALETTE[3], 0);
            a1.set_end((300, 220));
            let mut t1 = Anno::new(Tool::Text, (60, 360), PALETTE[6], 0);
            t1.text = "编辑态自检".into();
            o.annos = vec![r1, a1, t1];
            // 序号：红 / 蓝 / 白三个（白色要自动换深色数字）
            for (n, at, color) in [
                (1u32, (400i32, 80i32), PALETTE[0]),
                (2, (470, 190), PALETTE[4]),
                (3, (400, 250), PALETTE[7]),
            ] {
                let mut c = Anno::new(Tool::Counter, at, color, n);
                c.width = Tool::Counter.default_width();
                o.annos.push(c);
            }
            // 自检钩子：画完就选中第一条，核对「可编辑」描边 + 控制点
            if std::env::var("ZHUXIA_CAPTURE_SELFTEST_SELECT").is_ok() {
                o.selected = Some(0);
            }
            o.paint();

            if std::env::var("ZHUXIA_CAPTURE_SELFTEST_STYLEPANEL").ok().as_deref() == Some("1") {
                o.open_style();
            }
        });
    }
    if let Ok(v) = std::env::var("ZHUXIA_CAPTURE_SELFTEST_UI") {
        // "1" = 默认 1.2s；也可以直接给毫秒数（截图核对面板时要留更久）
        let ms = if v == "1" { 1200 } else { v.parse::<u32>().unwrap_or(1200) };
        unsafe { let _ = SetTimer(hwnd, 1, ms.max(300), None); }
    }
    // 自检：延迟注入"鼠标悬停 + 打开色板"。不能放在建窗时——窗口一显示就会收到
    // 一条 WM_MOUSEMOVE（指针正好在下面），会把注入的坐标冲掉。
    if std::env::var("ZHUXIA_CAPTURE_SELFTEST_HOVER").ok().as_deref() == Some("1") {
        unsafe { let _ = SetTimer(hwnd, 3, 400, None); }
    }
    zx_core::log::info(format!("截图开始：虚拟桌面 {vx},{vy} {vw}x{vh} dpi={dpi}"));
    true
}

/// 起一条后台线程采 UIA 元素，采到就写进共享数组，并让覆盖层重画一次。
///
/// 不同步采：一个浏览器窗口要走几百毫秒，Alt+A 会明显顿一下。早到早用，晚到就
/// 当作这次没有元素——降级路径本来就只有"窗口轮廓 + 子窗口"，体验不会坏。
struct PendingElems {
    slot: Arc<Mutex<Vec<Element>>>,
    /// 要采的窗口句柄（isize）。必须在覆盖层创建之前定下来——之后前台就是覆盖层自己了。
    ///
    /// 不止前台一个：还有光标底下那个和任务栏族。`Shell_TrayWnd` 永远不是前台窗口，
    /// 只采前台的话，识别任务栏在架构上就不可能成立。
    targets: Vec<isize>,
}

fn start_element_snapshot(active: bool) -> PendingElems {
    let slot: Arc<Mutex<Vec<Element>>> = Arc::new(Mutex::new(Vec::new()));
    let targets = if active && crate::cfg().detect_window {
        uielem::capture_targets()
    } else {
        Vec::new()
    };
    if !targets.is_empty() {
        zx_core::log::info(format!("UIA 目标窗口 {} 个", targets.len()));
    } else if active && crate::cfg().detect_window {
        zx_core::log::warn("UIA 目标窗口为空：capture_targets 没拿到任何句柄");
    }
    PendingElems { slot, targets }
}

fn spawn_element_worker(p: &PendingElems, hwnd: HWND) {
    if p.targets.is_empty() {
        return;
    }
    let targets = p.targets.clone();
    let out = Arc::clone(&p.slot);
    // HWND 不实现 Send，线程里只传 isize 再还原。
    let mine = hwnd.0 as isize;
    let _ = std::thread::Builder::new()
        .name("zx-uia-snap".into())
        .spawn(move || {
            let elems = uielem::snapshot_targets(&targets, std::time::Duration::from_millis(2500));
            if !elems.is_empty() {
                zx_core::log::info(format!("UI 元素快照 {} 个", elems.len()));
            }
            if let Ok(mut g) = out.lock() {
                *g = elems;
            }
            // 快照到位就重画一次，否则用户得先抖一下鼠标才看得到元素框。
            let me = HWND(mine as *mut std::ffi::c_void);
            unsafe { let _ = PostMessageW(me, WM_ELEMS_READY, WPARAM(0), LPARAM(0)); }
        });
}

/// `SetForegroundWindow` 在"用户刚在别的程序里"时会被系统拒绝。
/// 把自己挂到前台线程的输入队列上再抢前台，是这类工具的标准解法。
fn force_foreground(hwnd: HWND) {
    unsafe {
        let me = windows::Win32::System::Threading::GetCurrentThreadId();
        let fg = GetForegroundWindow();
        let fg_tid = if fg.is_invalid() { 0 } else { GetWindowThreadProcessId(fg, None) };
        if fg_tid != 0 && fg_tid != me {
            let _ = AttachThreadInput(me, fg_tid, true);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(me, fg_tid, false);
        } else {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// 取消/结束：销毁窗口、放回贴图、清掉线程状态。
///
/// 如果按下的是 Ctrl+L（长截图），这里在**窗口销毁之后**才去跑长截图：
/// 覆盖层必须先离开屏幕，页面才收得到滚轮。
fn close() {
    let ptr = OV.with(|c| c.replace(std::ptr::null_mut()));
    if ptr.is_null() {
        return;
    }
    let mut ov = unsafe { Box::from_raw(ptr) };
    unsafe {
        if !ov.text_win.is_invalid() {
            let _ = DestroyWindow(ov.text_win);
        }
        let _ = DestroyWindow(ov.hwnd);
    }
    pin::restore(std::mem::take(&mut ov.hidden_pins));
    if stylepanel::is_open() {
        stylepanel::close();
    }
    zx_core::log::info("截图结束");

    let pending = pending_long().lock().map(|mut g| g.take()).unwrap_or(None);
    if let Some(rect) = pending {
        if let Some(doc) = longshot::run(rect) {
            open_doc(doc);
        }
    }

    // 录屏：同样在窗口销毁之后才开始（否则第一帧里就有覆盖层）
    let rec = pending_record().lock().map(|mut g| g.take()).unwrap_or(None);
    if let Some((rect, forced)) = rec {
        record::run_with(record::source::Rect::from(rect), forced);
    }
}

/// 悬停气泡上的动作文案；刚复制过就显示"已复制"。
fn chip_label(c: Chip, done: bool) -> &'static str {
    match (c, done) {
        (Chip::SaveIcon, true) => "已保存",
        (_, true) => "已复制",
        (Chip::CopyText, _) => "复制文本",
        (Chip::CopyInfo, _) => "复制信息",
        (Chip::SaveIcon, _) => "存图标",
    }
}

impl Ov {
    fn scale(&self) -> f32 { (self.dpi as f32 / 96.0).max(1.0) }

    fn cursor_in(&self, l: LPARAM) -> (i32, i32) {
        let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
        let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
        (x + self.vx, y + self.vy)
    }

    fn sel_rect(&self) -> RECT {
        let (x0, x1) = (self.anchor.0.min(self.cursor.0), self.anchor.0.max(self.cursor.0));
        let (y0, y1) = (self.anchor.1.min(self.cursor.1), self.anchor.1.max(self.cursor.1));
        let r = RECT { left: x0, top: y0, right: x1, bottom: y1 };
        screen::clamp_rect(r, self.frame_bounds())
    }

    fn frame_bounds(&self) -> RECT {
        match &self.frame {
            Some(f) => f.bounds(),
            None => RECT { left: self.vx, top: self.vy, right: self.vx + self.vw, bottom: self.vy + self.vh },
        }
    }

    fn normalize_sel(&mut self) {
        let r = screen::clamp_rect(self.sel, self.frame_bounds());
        self.sel = RECT {
            left: r.left.min(r.right), top: r.top.min(r.bottom),
            right: r.right.max(r.left), bottom: r.bottom.max(r.top),
        };
    }

    fn sel_ok(&self) -> bool {
        self.sel.right - self.sel.left >= 2 && self.sel.bottom - self.sel.top >= 2
    }

    // ------------------------------------------------------------ 编辑区几何

    /// 编辑阶段：把图摆到屏幕上。`center` 为真时居中（拼接/长截图），否则贴在原选区位置。
    fn layout_doc(&mut self, center: bool) {
        let Some(doc) = &self.doc else { return };
        let max_w = (self.vw - 140).max(160) as f32;
        let max_h = (self.vh - 230).max(120) as f32;
        self.doc_scale = (max_w / doc.w as f32).min(max_h / doc.h as f32).min(1.0);
        let dw = (doc.w as f32 * self.doc_scale).round().max(1.0);
        let dh = (doc.h as f32 * self.doc_scale).round().max(1.0);
        let (ox, oy) = if center || self.doc_scale < 0.999 {
            (
                self.vx + ((self.vw as f32 - dw) / 2.0) as i32,
                self.vy + ((self.vh as f32 - dh) / 2.0) as i32,
            )
        } else {
            (self.sel.left, self.sel.top)
        };
        self.doc_rect = RECT {
            left: ox, top: oy, right: ox + dw as i32, bottom: oy + dh as i32,
        };
    }

    fn doc_rect(&self) -> RECT { self.doc_rect }

    /// 屏幕坐标 → 图坐标。
    fn to_doc(&self, x: i32, y: i32) -> (f32, f32) {
        (
            (x - self.doc_rect.left) as f32 / self.doc_scale.max(0.01),
            (y - self.doc_rect.top) as f32 / self.doc_scale.max(0.01),
        )
    }

    fn to_doc_clamped(&self, x: i32, y: i32) -> (f32, f32) {
        let (mut lx, mut ly) = self.to_doc(x, y);
        if let Some(doc) = &self.doc {
            lx = lx.clamp(0.0, doc.w as f32);
            ly = ly.clamp(0.0, doc.h as f32);
        }
        (lx, ly)
    }

    fn enter_editing(&mut self, rect: RECT) {
        let Some(frame) = self.frame.as_ref() else { return };
        let Some(doc) = output::crop(frame, rect) else { return };
        self.sel = rect;
        self.doc = Some(doc);
        self.stage = Stage::Editing;
        self.selected = None;
        self.drawing = None;
        self.layout_doc(false);
        self.trace_stage("编辑态就绪");
    }

    // ------------------------------------------------------------ 目标检测

    fn detect(&mut self, force: bool) {
        if !self.detect_window {
            self.chain.clear();
            self.hover_elem = None;
            return;
        }
        let moved = (self.cursor.0 - self.last_key.0).abs() + (self.cursor.1 - self.last_key.1).abs() > 1;
        if !force && !moved {
            return;
        }
        if !force && self.last_detect.elapsed().as_millis() < 24 {
            return;
        }
        self.last_detect = Instant::now();
        self.last_key = self.cursor;
        // 链的第一层是 UIA 元素（如果光标下正好有一个），后面才是子窗口 → 顶层
        // 窗口 → 显示器。滚轮切层级的老逻辑不用改，只是多了一档更细的。
        // 光标上的层级链：[命中的元素, 它所在的块]。再往上交给窗口链。
        let stack = if self.elements_on {
            match self.elems.try_lock() {
                Ok(g) => uielem::stack_at(&g, self.cursor.0, self.cursor.1),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let mut chain: Vec<Target> = Vec::new();
        for e in &stack {
            chain.push(Target { rect: e.to_rect(), label: e.label() });
        }
        chain.extend(target::chain_at(self.cursor.0, self.cursor.1, std::process::id()));
        self.hover_elem = stack.first().cloned();
        if chain.len() != self.chain.len() {
            // 默认停在最小元素上，往上滚才是"包含它的更大元素"——和 PixPin 一致：
            // 先精准，需要更大的范围再滚。
            self.level = 0;
        }
        self.chain = chain;
        if self.level >= self.chain.len() {
            self.level = self.chain.len().saturating_sub(1);
        }
        // 悬停结果只在翻转时记一行：截图一次最多两行，但足够区分"快照有元素却没命中"
        // 和"命中窗口"这两种完全不同的毛病。
        let hit = self.hover_elem.is_some();
        if self.hover_log != Some(hit) {
            self.hover_log = Some(hit);
            // 打的是"当前显示的这一层"（chain[level]），不是最小元素——
            // 排查时用户看到的是哪个框，日志就该是哪个。
            let layers: Vec<String> = self
                .chain
                .iter()
                .map(|t| {
                    format!(
                        "{}[{}x{}]",
                        t.label,
                        t.rect.right - t.rect.left,
                        t.rect.bottom - t.rect.top
                    )
                })
                .collect();
            zx_core::log::info(format!("层级链 {} 层：{}", layers.len(), layers.join(" > ")));
            match self.chain.get(self.level) {
                Some(t) => zx_core::log::info(format!(
                    "悬停→{} {}x{} @{},{}（光标 {},{}，第 {}/{} 层）",
                    t.label,
                    t.rect.right - t.rect.left,
                    t.rect.bottom - t.rect.top,
                    t.rect.left,
                    t.rect.top,
                    self.cursor.0,
                    self.cursor.1,
                    self.level + 1,
                    self.chain.len()
                )),
                None => {
                    let n = match self.elems.try_lock() {
                        Ok(g) => g.len().to_string(),
                        Err(_) => "取不到".into(),
                    };
                    zx_core::log::info(format!(
                        "悬停→窗口（快照 {n} 个，光标 {},{}）",
                        self.cursor.0, self.cursor.1
                    ));
                }
            }
        }
    }

    fn hover_target(&self) -> Option<&Target> {
        self.chain.get(self.level)
    }

    fn hover_rect(&self) -> Option<RECT> {
        self.hover_target().map(|t| screen::clamp_rect(t.rect, self.frame_bounds()))
    }

    // ------------------------------------------------------------ 绘制

    fn dim_outside(&mut self, r: RECT, color: u32) {
        let (l, t) = (r.left - self.vx, r.top - self.vy);
        let (rr, b) = (r.right - self.vx, r.bottom - self.vy);
        self.canvas.fill_rect(0.0, 0.0, self.vw as f32, t as f32, color);
        self.canvas.fill_rect(0.0, b as f32, self.vw as f32, (self.vh - b) as f32, color);
        self.canvas.fill_rect(0.0, t as f32, l as f32, (b - t) as f32, color);
        self.canvas.fill_rect(rr as f32, t as f32, (self.vw - rr) as f32, (b - t) as f32, color);
    }

    fn paint(&mut self) {
        let s = self.scale();
        if let Some(f) = &self.frame {
            f.blit_into(&mut self.canvas);
        } else {
            self.canvas.clear(EDITOR_BG);
        }
        self.toolbar.clear();
        self.chips.clear();
        self.palette.clear();

        match self.stage {
            Stage::Choosing | Stage::Dragging | Stage::Selected => self.paint_select(s),
            Stage::Editing => self.paint_edit(s),
        }

        if self.mode == Mode::Long {
            self.paint_long_badge(s);
        }

        self.canvas.present(self.hwnd, self.vx, self.vy);
    }

    fn paint_select(&mut self, s: f32) {
        let tk = self.tk;
        let selected = self.stage == Stage::Selected
            || (self.stage == Stage::Dragging && self.sel_ok());

        if selected {
            self.dim_outside(self.sel, BG_DIM);
        }

        // 放大镜先画：它跟着光标走，正好压在"元素气泡"的位置上（任务栏这种贴底的
        // 元素，气泡只能往上放）。气泡是用户这一刻最需要读的东西，不能被它盖住。
        if self.magnifier {
            self.paint_magnifier(s);
        }

        if self.stage == Stage::Choosing {
            if let Some(r) = self.hover_rect() {
                // 整屏压暗、只留这一块亮——这是截图工具里"选中"最直观的表达，
                // 光描边框在浅色背景上根本不够看（用户反馈"不够明显"）。
                // 悬停用 35%，确定选区才用 60%：悬停是"预览"，太黑会看不见自己要框的东西。
                self.dim_outside(r, HOVER_DIM);
                let (x, y) = ((r.left - self.vx) as f32, (r.top - self.vy) as f32);
                let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
                // 高亮：淡淡铺一层 + 半透明黑打底 + 加粗蓝线。
                // 单画 2px 蓝线在浅色/花哨背景上几乎看不见（用户反馈"不够明显"）。
                self.canvas
                    .fill_round(x, y, w, h, 4.0 * s, 0x1A2E90FA);
                self.canvas.stroke_round(
                    x - 1.5 * s,
                    y - 1.5 * s,
                    w + 3.0 * s,
                    h + 3.0 * s,
                    5.5 * s,
                    1.5 * s,
                    0x99000000,
                );
                self.canvas
                    .stroke_round(x, y, w, h, 4.0 * s, 3.0 * s, ACCENT);

                let elem = self.hover_elem.clone();
                if let Some(target) = self.hover_target() {
                    // 被配额截断时明说一句：否则用户看到的是"框不上去"，会以为程序坏了。
                    let cut = if uielem::last_truncated() { "   · 结构已截断" } else { "" };
                    let label = format!(
                        "{}   {} × {}{cut}",
                        target.label,
                        r.right - r.left,
                        r.bottom - r.top
                    );
                    // 光标压着元素时，气泡右侧再接两个动作。复制完的 1.2 秒里
                    // 那一格显示"已复制"，不用弹提示，视线本来就在这儿。
                    let flash = self
                        .chip_flash
                        .filter(|(_, t)| t.elapsed().as_millis() < 1200)
                        .map(|(c, _)| c);
                    let chips: Vec<(Chip, &'static str)> = if elem.is_some() {
                        [Chip::CopyText, Chip::CopyInfo, Chip::SaveIcon]
                            .into_iter()
                            .map(|c| (c, chip_label(c, flash == Some(c))))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let lw = self.canvas.measure(&label, 11.5 * s, false) + 20.0 * s;
                    let seg_w: Vec<f32> = chips
                        .iter()
                        .map(|(_, t)| self.canvas.measure(t, 11.5 * s, false) + 30.0 * s)
                        .collect();
                    let total = lw + seg_w.iter().sum::<f32>();
                    let lh = 24.0 * s;
                    let lx = (x + 6.0 * s).min((self.vw as f32 - total - 6.0 * s).max(6.0 * s));
                    let ly = if y - lh - 6.0 * s > 0.0 {
                        y - lh - 6.0 * s
                    } else {
                        y + 6.0 * s
                    };
                    design::bubble(&mut self.canvas, lx, ly, total, lh, &tk, s);
                    design::text(
                        &mut self.canvas,
                        &label,
                        lx + 8.0 * s,
                        ly,
                        lw - 16.0 * s,
                        lh,
                        11.5 * s,
                        tk.bubble_text,
                        zx_ui::TextAlign::Near,
                        zx_ui::VAlign::Center,
                        500,
                    );
                    let mut cx = lx + lw;
                    for ((chip, text), sw) in chips.iter().copied().zip(seg_w.iter().copied()) {
                        design::divider(
                            &mut self.canvas,
                            cx,
                            ly + 5.0 * s,
                            lh - 10.0 * s,
                            &tk,
                            s,
                        );
                        design::text(
                            &mut self.canvas,
                            text,
                            cx + 10.0 * s,
                            ly,
                            sw - 20.0 * s,
                            lh,
                            11.5 * s,
                            tk.bubble_text,
                            zx_ui::TextAlign::Near,
                            zx_ui::VAlign::Center,
                            500,
                        );
                        self.chips.push((
                            RECT {
                                left: (cx + self.vx as f32) as i32,
                                top: (ly + self.vy as f32) as i32,
                                right: (cx + sw + self.vx as f32) as i32,
                                bottom: (ly + lh + self.vy as f32) as i32,
                            },
                            chip,
                        ));
                        cx += sw;
                    }
                }
            }
        }

        if selected {
            let sel = self.sel;
            let (x, y) = ((sel.left - self.vx) as f32, (sel.top - self.vy) as f32);
            let (w, h) = ((sel.right - sel.left) as f32, (sel.bottom - sel.top) as f32);
            self.canvas.stroke_round(x, y, w, h, 0.0, 1.0 * s, 0xFFFFFFFF);

            let hs = 6.0 * s;
            for (hx, hy) in [
                (x, y),
                (x + w / 2.0, y),
                (x + w, y),
                (x, y + h / 2.0),
                (x + w, y + h / 2.0),
                (x, y + h),
                (x + w / 2.0, y + h),
                (x + w, y + h),
            ] {
                self.canvas.fill_rect(hx - hs / 2.0, hy - hs / 2.0, hs, hs, 0xFFFFFFFF);
                self.canvas.stroke_round(
                    hx - hs / 2.0,
                    hy - hs / 2.0,
                    hs,
                    hs,
                    1.0 * s,
                    (1.0 * s).max(1.0),
                    0x59000000,
                );
            }

            let size = format!("{} × {}", sel.right - sel.left, sel.bottom - sel.top);
            let tw = self.canvas.measure(&size, 11.5 * s, false) + 16.0 * s;
            let th = 24.0 * s;
            let tx = x.max(0.0).min(self.vw as f32 - tw - 4.0 * s);
            let ty = if y - th - 4.0 * s > 0.0 {
                y - th - 4.0 * s
            } else {
                y + 4.0 * s
            };
            design::bubble(&mut self.canvas, tx, ty, tw, th, &tk, s);
            design::text(
                &mut self.canvas,
                &size,
                tx + 8.0 * s,
                ty,
                tw - 16.0 * s,
                th,
                11.5 * s,
                tk.bubble_text,
                zx_ui::TextAlign::Near,
                zx_ui::VAlign::Center,
                500,
            );
        }

        if self.stage == Stage::Choosing {
            self.paint_hint(s);
        }
    }

    fn paint_edit(&mut self, s: f32) {
        let r = self.doc_rect();
        self.dim_outside(r, BG_DIM);

        if let Some(doc) = self.doc.take() {
            self.canvas.draw_canvas(
                &doc,
                (r.left - self.vx) as f32,
                (r.top - self.vy) as f32,
                (r.right - r.left) as f32,
                (r.bottom - r.top) as f32,
            );
            self.doc = Some(doc);
        }

        let (ox, oy) = ((r.left - self.vx) as f32, (r.top - self.vy) as f32);
        if let Some(a) = &self.drawing {
            annotate::draw_one(&mut self.canvas, a, ox, oy, self.doc_scale);
        }
        annotate::draw_all(
            &mut self.canvas,
            &self.annos,
            ox,
            oy,
            self.selected,
            self.doc_scale,
        );
        self.canvas.stroke_round(
            ox,
            oy,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            0.0,
            1.0 * s,
            0x40FFFFFF,
        );
        self.paint_toolbar(s);
    }

    fn paint_long_badge(&mut self, s: f32) {
        let tk = self.tk;
        let text = "长截图准备中…";
        let w = self.canvas.measure(text, 11.5 * s, false) + 20.0 * s;
        let h = 26.0 * s;
        let x = (self.vw as f32 - w) / 2.0;
        design::bubble(&mut self.canvas, x, 16.0 * s, w, h, &tk, s);
        design::text(
            &mut self.canvas,
            text,
            x,
            16.0 * s,
            w,
            h,
            11.5 * s,
            tk.bubble_text,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            500,
        );
    }

    fn paint_hint(&mut self, s: f32) {
        let tk = self.tk;
        // 放大镜开着时右键被征用成"取色"，提示里要说清楚，别让用户以为右键还能取消
        let pick = if self.magnifier { " · 右键取色" } else { "" };
        let text = match self.mode {
            Mode::Append => format!(
                "拼接（{}）：已 {} 张 · 继续框选 · 滚轮换方向 · Enter 完成 · Esc 取消",
                stitch::dir().name(),
                stitch::count()
            ),
            _ if self.stage == Stage::Selected => format!(
                "拖动调整 · 方向键微调 · Enter 标注 · Ctrl+L 长截图{pick} · Esc 返回"
            ),
            _ if self.chain.is_empty() => {
                format!("拖动框选区域 · 单击控件{pick} · Esc 退出")
            }
            _ => format!("拖动框选 · 单击控件 · 滚轮切层级 · Tab 只认窗口{pick} · Esc 退出"),
        };
        // 元素识别因为反复卡死被停用时必须说出来，否则用户只会觉得"悬停没反应"
        let text = match uielem::unavailable_note() {
            Some(n) if self.elements_on => format!("{text} · {n}"),
            _ => text,
        };
        let w = (self.canvas.measure(&text, 11.5 * s, false) + 24.0 * s)
            .min(self.vw as f32 - 12.0 * s);
        let h = 28.0 * s;
        let x = ((self.vw as f32 - w) / 2.0).max(6.0 * s);
        let y = if self.mode == Mode::Append { 52.0 * s } else { 16.0 * s };
        design::bubble(&mut self.canvas, x, y, w, h, &tk, s);
        design::text(
            &mut self.canvas,
            &text,
            x,
            y,
            w,
            h,
            11.5 * s,
            tk.bubble_text,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            500,
        );
    }

    fn paint_magnifier(&mut self, s: f32) {
        let Some(frame) = &self.frame else { return };
        let cell = 9.0 * s;
        let cells = 13i32;
        let side = cell * cells as f32;
        let cx = (self.cursor.0 - self.vx) as f32 + 22.0 * s;
        let cy = (self.cursor.1 - self.vy) as f32 + 22.0 * s;
        let bx = cx.min(self.vw as f32 - side - 6.0 * s).max(6.0 * s);
        let by = cy.min(self.vh as f32 - side - 34.0 * s).max(6.0 * s);
        let tk = self.tk;

        design::shadow(
            &mut self.canvas,
            design::Rect::new(bx - 2.0 * s, by - 2.0 * s, side + 4.0 * s, side + 4.0 * s, 5.0 * s),
            s,
        );
        self.canvas.fill_round(
            bx - 2.0 * s,
            by - 2.0 * s,
            side + 4.0 * s,
            side + 4.0 * s,
            5.0 * s,
            tk.raised,
        );

        let half = cells / 2;
        for gy in 0..cells {
            for gx in 0..cells {
                let p = frame.pixel(self.cursor.0 + gx - half, self.cursor.1 + gy - half);
                self.canvas.fill_rect(
                    bx + gx as f32 * cell,
                    by + gy as f32 * cell,
                    cell,
                    cell,
                    p | 0xFF00_0000,
                );
            }
        }
        self.canvas.stroke_round(
            bx + half as f32 * cell,
            by + half as f32 * cell,
            cell,
            cell,
            0.0,
            1.5 * s,
            tk.accent,
        );

        let px = frame.pixel(self.cursor.0, self.cursor.1);
        // 刚复制过就显示「#RRGGBB 已复制」，否则显示坐标 + 颜色值
        let info = match &self.color_flash {
            Some(hex) => format!("{hex} 已复制"),
            None => format!(
                "{},{}  #{:02X}{:02X}{:02X}",
                self.cursor.0,
                self.cursor.1,
                (px >> 16) & 0xFF,
                (px >> 8) & 0xFF,
                px & 0xFF
            ),
        };
        design::bubble(
            &mut self.canvas,
            bx - 2.0 * s,
            by + side + 4.0 * s,
            side + 4.0 * s,
            22.0 * s,
            &tk,
            s,
        );
        design::text(
            &mut self.canvas,
            &info,
            bx,
            by + side + 4.0 * s,
            side,
            22.0 * s,
            11.0 * s,
            tk.bubble_text,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            500,
        );
    }

    /// 计算工具条槽位；宽度不足时整体等比缩小。
    fn toolbar_layout(&self, s: f32, primary_w: f32) -> (f32, f32, Vec<Slot>, f32) {
        let bh = BTN * s;
        let mut slots = Vec::new();
        for tool in Tool::ALL {
            slots.push(Slot::Btn(Act::Tool(tool), bh));
        }
        slots.push(Slot::Sep);
        slots.push(Slot::Btn(Act::ColorPick, bh));
        slots.push(Slot::Sep);
        slots.push(Slot::Btn(Act::Undo, bh));
        slots.push(Slot::Btn(Act::Redo, bh));
        slots.push(Slot::Sep);

        let recording = crate::cfg().record.enabled;
        for act in [
            Act::Beautify,
            Act::Stitch,
            Act::Ocr,
            Act::Long,
            Act::RecordGif,
            Act::Record,
            Act::Pin,
        ] {
            if !recording && matches!(act, Act::Record | Act::RecordGif) {
                continue;
            }
            slots.push(Slot::Btn(act, bh));
        }
        slots.push(Slot::Sep);
        if crate::upload_available() {
            slots.push(Slot::Btn(Act::Upload, bh));
        }
        slots.push(Slot::Btn(Act::Save, bh));
        slots.push(Slot::Primary(Act::Copy, primary_w));
        slots.push(Slot::Sep);
        slots.push(Slot::Btn(Act::Cancel, bh));

        let raw = slots
            .iter()
            .map(|slot| match slot {
                Slot::Btn(_, w) | Slot::Primary(_, w) => *w,
                Slot::Sep => SEP_W * s,
            })
            .sum::<f32>()
            + GAP * (slots.len() as f32 - 1.0)
            + BAR_PAD * 2.0;
        let available = (self.vw as f32 - 16.0 * s).max(120.0);
        let fit = if raw > available { available / raw } else { 1.0 };
        if fit < 1.0 {
            for slot in &mut slots {
                match slot {
                    Slot::Btn(_, w) | Slot::Primary(_, w) => *w *= fit,
                    Slot::Sep => {}
                }
            }
        }
        (raw * fit, BAR_H * s * fit.min(1.0), slots, GAP * fit.min(1.0))
    }

    fn paint_toolbar(&mut self, s: f32) {
        let t = self.tk;
        let fs = 12.5 * s;
        let primary_w = PRIMARY_PAD * 2.0 * s
            + 15.0 * s
            + 6.0 * s
            + design::measure(&mut self.canvas, "复制", fs, 600);
        let (total_w, total_h, slots, gap) = self.toolbar_layout(s, primary_w);
        let ts = (total_h / (BAR_H * s)).clamp(0.0, 1.0);
        let s2 = s * ts;
        let pad = BAR_PAD * s * ts;
        let bh = BTN * s * ts;

        let r = self.doc_rect();
        let max_x = self.vw as f32 - total_w - 6.0 * s;
        let sx = (((r.left + r.right) as f32 / 2.0 - total_w / 2.0) - self.vx as f32)
            .clamp(6.0 * s, max_x.max(6.0 * s));
        let below = (r.bottom - self.vy) as f32 + 8.0 * s;
        let sy = if below + total_h + 26.0 * s < self.vh as f32 {
            below
        } else {
            let above = (r.top - self.vy) as f32 - total_h - 8.0 * s;
            if above > 26.0 * s {
                above
            } else {
                (self.vh as f32 - total_h - 8.0 * s).max(0.0)
            }
        };

        design::floating_panel(
            &mut self.canvas,
            design::Rect::new(sx, sy, total_w, total_h, 12.0 * s2),
            &t,
            s2,
        );

        let mut x = sx + pad;
        let mut hovered: Option<(RECT, Act)> = None;
        for slot in &slots {
            match slot {
                Slot::Sep => {
                    design::divider(
                        &mut self.canvas,
                        x + SEP_W * s2 / 2.0,
                        sy + (total_h - 20.0 * s2) / 2.0,
                        20.0 * s2,
                        &t,
                        s2,
                    );
                    x += SEP_W * s2 + gap;
                }
                Slot::Btn(act, w) => {
                    let rc = RECT {
                        left: (x + self.vx as f32) as i32,
                        top: (sy + (total_h - bh) / 2.0 + self.vy as f32) as i32,
                        right: (x + w + self.vx as f32) as i32,
                        bottom: (sy + (total_h + bh) / 2.0 + self.vy as f32) as i32,
                    };
                    let inside = target::hit(&rc, self.cursor.0, self.cursor.1);
                    if inside {
                        hovered = Some((rc, *act));
                    }
                    self.paint_slot(*act, x, sy + (total_h - bh) / 2.0, *w, bh, s2, inside);
                    self.toolbar.push((rc, *act));
                    x += w + gap;
                }
                Slot::Primary(act, w) => {
                    let ph = (PRIMARY_H * s2).min(total_h - 2.0 * pad);
                    let py = sy + (total_h - ph) / 2.0;
                    let rc = RECT {
                        left: (x + self.vx as f32) as i32,
                        top: (py + self.vy as f32) as i32,
                        right: (x + w + self.vx as f32) as i32,
                        bottom: (py + ph + self.vy as f32) as i32,
                    };
                    let inside = target::hit(&rc, self.cursor.0, self.cursor.1);
                    if inside {
                        hovered = Some((rc, *act));
                    }
                    self.paint_slot(*act, x, py, *w, ph, s2, inside);
                    self.toolbar.push((rc, *act));
                    x += w + gap;
                }
            }
        }

        if self.color_open {
            self.paint_palette(sx, sy, total_w, s);
        }
        if let Some((rc, act)) = hovered {
            self.paint_tip(rc, act, sy, sy + total_h, s);
        }
    }

    /// 一个工具条按钮。x/y/w/h 各自用到、且是同一块矩形，收结构体只会让内部更绕，
    /// 这里保留平铺参数。
    #[allow(clippy::too_many_arguments)]
    fn paint_slot(&mut self, act: Act, x: f32, y: f32, w: f32, h: f32, s: f32, hover: bool) {
        let t = self.tk;
        let active = match act {
            Act::Tool(tool) => tool == self.tool,
            Act::ColorPick => self.color_open,
            Act::Beautify => style::current().enabled,
            Act::Stitch => stitch::busy(),
            _ => false,
        };

        if matches!(act, Act::Copy) {
            let bg = if hover { t.primary_hover } else { t.primary };
            self.canvas.fill_round(x, y, w, h, PRIMARY_R * s, bg);
            let fs = 12.5 * s;
            let icon = 15.0 * s;
            let tw = design::measure(&mut self.canvas, "复制", fs, 600);
            let left = x + (w - (icon + 6.0 * s + tw)) / 2.0;
            design::draw_icon(
                &mut self.canvas,
                Icon::Copy,
                left + icon / 2.0,
                y + h / 2.0,
                icon,
                t.primary_text,
                s,
            );
            design::text(
                &mut self.canvas,
                "复制",
                left + icon + 6.0 * s,
                y,
                tw,
                h,
                fs,
                t.primary_text,
                zx_ui::TextAlign::Near,
                zx_ui::VAlign::Center,
                600,
            );
            return;
        }

        let bg = if active {
            t.active
        } else if hover {
            t.hover
        } else {
            0
        };
        if bg != 0 {
            self.canvas.fill_round(x, y, w, h, BTN_R * s, bg);
        }
        let color = if active || hover { t.text } else { t.text2 };
        design::draw_icon(
            &mut self.canvas,
            Self::icon_of(act),
            x + w / 2.0,
            y + h / 2.0,
            16.0 * s,
            color,
            s,
        );
    }

    fn icon_of(act: Act) -> Icon {
        match act {
            Act::Tool(Tool::Select) => Icon::Pointer,
            Act::Tool(Tool::Rect) => Icon::Square,
            Act::Tool(Tool::Ellipse) => Icon::Circle,
            Act::Tool(Tool::Arrow) => Icon::ArrowUpRight,
            Act::Tool(Tool::Line) => Icon::Slash,
            Act::Tool(Tool::Pen) => Icon::Pencil,
            Act::Tool(Tool::Highlighter) => Icon::Highlighter,
            Act::Tool(Tool::Mosaic) => Icon::Pixelate,
            Act::Tool(Tool::Text) => Icon::Type,
            Act::Tool(Tool::Counter) => Icon::Counter,
            Act::ColorPick => Icon::Droplet,
            Act::Undo => Icon::Undo,
            Act::Redo => Icon::Redo,
            Act::Beautify => Icon::Sparkles,
            Act::Stitch => Icon::ImagePlus,
            Act::Ocr => Icon::ScanText,
            Act::Long => Icon::UnfoldVertical,
            Act::Record => Icon::Record,
            Act::RecordGif => Icon::Gif,
            Act::Copy => Icon::Copy,
            Act::Save => Icon::Download,
            Act::Upload => Icon::Upload,
            Act::Pin => Icon::Pin,
            Act::Cancel => Icon::Close,
        }
    }

    fn paint_palette(&mut self, bar_x: f32, bar_y: f32, bar_w: f32, s: f32) {
        let t = self.tk;
        let cell = 26.0 * s;
        let gap = 6.0 * s;
        let pad = 10.0 * s;
        let slots = PALETTE.len() + 1;
        let w = slots as f32 * cell + (slots as f32 - 1.0) * gap + pad * 2.0;
        let h = cell + pad * 2.0 + 17.0 * s;
        let x = (bar_x + bar_w - w)
            .min((self.vw as f32 - w - 6.0 * s).max(6.0 * s))
            .max(6.0 * s);
        let y = (bar_y - h - 8.0 * s).max(6.0 * s);
        design::floating_panel(&mut self.canvas, design::Rect::new(x, y, w, h, 12.0 * s), &t, s);

        let mut cx = x + pad;
        let cy = y + pad;
        let r = cell / 2.0 - 2.0 * s;
        for (i, color) in PALETTE.iter().enumerate() {
            let rc = RECT {
                left: (cx + self.vx as f32) as i32,
                top: (cy + self.vy as f32) as i32,
                right: (cx + cell + self.vx as f32) as i32,
                bottom: (cy + cell + self.vy as f32) as i32,
            };
            let inside = target::hit(&rc, self.cursor.0, self.cursor.1);
            let (mx, my) = (cx + cell / 2.0, cy + cell / 2.0);
            if inside {
                self.canvas.fill_ellipse(
                    mx - r - 3.0 * s,
                    my - r - 3.0 * s,
                    (r + 3.0 * s) * 2.0,
                    (r + 3.0 * s) * 2.0,
                    t.hover,
                );
            }
            self.canvas.fill_ellipse(mx - r, my - r, r * 2.0, r * 2.0, *color);
            let ring = if zx_ui::luma(*color) > 0.62 { 0x33000000 } else { 0x40FFFFFF };
            self.canvas.stroke_ellipse(
                mx - r,
                my - r,
                r * 2.0,
                r * 2.0,
                (1.0 * s).max(1.0),
                ring,
            );
            if *color == self.color {
                self.canvas.stroke_ellipse(
                    mx - r - 3.0 * s,
                    my - r - 3.0 * s,
                    (r + 3.0 * s) * 2.0,
                    (r + 3.0 * s) * 2.0,
                    1.5 * s,
                    t.text,
                );
            }
            self.palette.push((rc, Pick::Preset(i)));
            cx += cell + gap;
        }

        let rc = RECT {
            left: (cx + self.vx as f32) as i32,
            top: (cy + self.vy as f32) as i32,
            right: (cx + cell + self.vx as f32) as i32,
            bottom: (cy + cell + self.vy as f32) as i32,
        };
        let inside = target::hit(&rc, self.cursor.0, self.cursor.1);
        let (mx, my) = (cx + cell / 2.0, cy + cell / 2.0);
        if inside {
            self.canvas.fill_ellipse(
                mx - r - 3.0 * s,
                my - r - 3.0 * s,
                (r + 3.0 * s) * 2.0,
                (r + 3.0 * s) * 2.0,
                t.hover,
            );
        }

        let (dx, dy, dw) = (mx - r, my - r, r * 2.0);
        self.canvas.clip_round(dx, dy, dw, dw, r);
        let stops = [0xFFFF4D4F, 0xFFFFD600, 0xFF22C55E, 0xFF2E90FA, 0xFFA855F7];
        let seg = dw / 4.0;
        for i in 0..4 {
            self.canvas.gradient_rect(
                dx + seg * i as f32,
                dy,
                seg + 1.0,
                dw,
                stops[i],
                stops[i + 1],
                0.0,
            );
        }
        self.canvas.reset_clip();
        self.canvas.stroke_ellipse(dx, dy, dw, dw, (1.0 * s).max(1.0), 0x40FFFFFF);
        self.canvas.line(mx - 5.0 * s, my, mx + 5.0 * s, my, 1.8 * s, 0xFFFFFFFF);
        self.canvas.line(mx, my - 5.0 * s, mx, my + 5.0 * s, 1.8 * s, 0xFFFFFFFF);
        self.palette.push((rc, Pick::Custom));

        let hex = picker::hex(self.color);
        design::text(
            &mut self.canvas,
            &hex,
            x + pad,
            y + pad + cell,
            w - pad * 2.0,
            17.0 * s,
            10.5 * s,
            t.text3,
            zx_ui::TextAlign::Center,
            zx_ui::VAlign::Center,
            500,
        );
    }

    fn paint_tip(&mut self, btn: RECT, act: Act, bar_top: f32, bar_bottom: f32, s: f32) {
        let t = self.tk;
        let (name, key) = tip_of(act);
        let fs = 11.5 * s;
        let name_w = design::measure(&mut self.canvas, name, fs, 500);
        let key_w = if key.is_empty() {
            0.0
        } else {
            design::measure(&mut self.canvas, key, 10.5 * s, 600)
        };
        let chip_w = if key.is_empty() { 0.0 } else { key_w + 8.0 * s };
        let gap = if key.is_empty() { 0.0 } else { 6.0 * s };
        let tw = name_w + gap + chip_w + 16.0 * s;
        let th = 24.0 * s;
        let cx = ((btn.left + btn.right) as f32 / 2.0 - self.vx as f32 - tw / 2.0)
            .clamp(6.0 * s, (self.vw as f32 - tw - 6.0 * s).max(6.0 * s));
        let above = bar_top - th - 8.0 * s;
        let y = if above >= 4.0 * s { above } else { bar_bottom + 8.0 * s };

        design::bubble(&mut self.canvas, cx, y, tw, th, &t, s);
        design::text(
            &mut self.canvas,
            name,
            cx + 8.0 * s,
            y,
            name_w,
            th,
            fs,
            t.bubble_text,
            zx_ui::TextAlign::Near,
            zx_ui::VAlign::Center,
            500,
        );
        if !key.is_empty() {
            let kx = cx + 8.0 * s + name_w + gap;
            let kh = 16.0 * s;
            self.canvas.fill_round(
                kx,
                y + (th - kh) / 2.0,
                chip_w,
                kh,
                4.0 * s,
                0x24FFFFFF,
            );
            design::text(
                &mut self.canvas,
                key,
                kx,
                y + (th - kh) / 2.0,
                chip_w,
                kh,
                10.5 * s,
                0xB3FFFFFF,
                zx_ui::TextAlign::Center,
                zx_ui::VAlign::Center,
                600,
            );
        }
    }

    // ------------------------------------------------------------ 标注

    fn push_hist(&mut self) {
        self.hist.push(self.annos.clone());
        if self.hist.len() > 100 {
            self.hist.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.hist.pop() {
            self.redo.push(std::mem::replace(&mut self.annos, prev));
            self.selected = None;
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.hist.push(std::mem::replace(&mut self.annos, next));
            self.selected = None;
        }
    }

    // ------------------------------------------------------------ 文本输入

    fn open_text(&mut self, local: (f32, f32)) {
        self.close_text(false);
        let s = self.scale();
        let px = (self.width * 4.0 * self.doc_scale).max(14.0);
        let width = (260.0 * s) as i32;
        let height = (px * 1.7) as i32;
        // 输入框自己带一圈描边（见 `paint_text_frame`）：压在白色/浅色截图上，
        // 光秃秃的 EDIT 连边界都看不出来。外壳把描边画在 EDIT 外面，
        // 同时让 EDIT 的左上角仍然落在点击点上，提交后文字不会跳位置。
        let inset = text_frame_inset(s);
        let (mut wx, mut wy) = (
            self.doc_rect.left + (local.0 * self.doc_scale) as i32 - inset,
            self.doc_rect.top + (local.1 * self.doc_scale) as i32 - inset,
        );
        let (ww, wh) = (width + inset * 2, height + inset * 2);
        // 夹进虚拟屏幕：多了一圈外框之后更容易被顶出屏幕外
        wx = wx.clamp(self.vx + 2, (self.vx + self.vw - ww - 2).max(self.vx + 2));
        wy = wy.clamp(self.vy + 2, (self.vy + self.vh - wh - 2).max(self.vy + 2));
        let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        let Ok(win) = (unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                PCWSTR(win_class().as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                wx, wy, ww, wh,
                self.hwnd, HMENU::default(), HINSTANCE(hinst.0), None,
            )
        }) else { return };
        zx_ui::painter::set_round_region(win, ww, wh, (6.0 * s) as i32);
        let edit = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("EDIT"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP
                    | WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WINDOW_STYLE(ES_LEFT as u32),
                inset, inset, width, height,
                win, HMENU::default(), HINSTANCE(hinst.0), None,
            )
        };
        let Ok(edit) = edit else {
            unsafe { let _ = DestroyWindow(win); }
            return;
        };
        let font = unsafe {
            let face = zx_core::win::wide("Microsoft YaHei UI");
            CreateFontW(-(px as i32), 0, 0, 0, 500, 0, 0, 0,
                        DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
                        CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32,
                        DEFAULT_PITCH.0 as u32, PCWSTR(face.as_ptr()))
        };
        unsafe {
            SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            // 字形和描边之间留 2px，不然第一个字贴着框
            SendMessageW(edit, 0x00D3, WPARAM(0x0001 | 0x0002),
                         LPARAM(2 | (2 << 16)));
            let _ = SetWindowPos(win, HWND_TOPMOST, wx, wy, ww, wh, SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(win);
            let _ = SetFocus(edit);
        }
        TEXT_OWNER.store(self.hwnd.0 as isize, Ordering::SeqCst);
        self.text_win = win;
        self.text_edit = edit;
        self.text_font = font.0 as isize;
        self.text_pos = (local.0 as i32, local.1 as i32);
        subclass_text_edit(edit);
    }

    fn close_text(&mut self, commit: bool) {
        if self.text_win.is_invalid() {
            return;
        }
        if commit && !self.text_edit.is_invalid() {
            let mut buf = [0u16; 1024];
            let n = unsafe { GetWindowTextW(self.text_edit, &mut buf) };
            let text = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() };
            if !text.trim().is_empty() {
                self.push_hist();
                let mut a = Anno::new(Tool::Text, self.text_pos, self.color, 0);
                a.width = self.width;
                a.text = text;
                self.annos.push(a);
            }
        }
        unsafe {
            let _ = DestroyWindow(self.text_win);
            if self.text_font != 0 {
                let _ = DeleteObject(HGDIOBJ(self.text_font as *mut _));
            }
        }
        self.text_win = HWND::default();
        self.text_edit = HWND::default();
        self.text_font = 0;
        unsafe {
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(self.hwnd);
        }
    }

    // ------------------------------------------------------------ 输出

    /// 原图 + 标注（不套美化）。
    fn flat(&mut self) -> Option<Canvas> {
        let doc = self.doc.take()?;
        let out = output::compose_doc(&doc, &self.annos);
        self.doc = Some(doc);
        Some(out)
    }

    /// 最终成品：套用当前美化样式。
    fn final_image(&mut self) -> Option<Canvas> {
        let doc = self.doc.take()?;
        let st = style::current();
        let unit = self.dpi as f32 / 96.0;
        let out = if st.enabled {
            style::render(&doc, &self.annos, &st, unit)
        } else {
            output::compose_doc(&doc, &self.annos)
        };
        self.doc = Some(doc);
        Some(out)
    }

    fn do_copy(&mut self) {
        let Some(c) = self.final_image() else { return };
        let ok = output::copy_to_clipboard(&c);
        let saved = output::auto_save(&c);
        if ok || saved.is_some() {
            zx_core::log::info(format!("截图已复制（{ok}），自动存盘={saved:?}"));
            self.close_soon();
        } else {
            // 剪贴板被占用且没开自动存盘：留在编辑器里，用户还能重试或改存盘
            zx_core::log::warn("截图复制失败（剪贴板被占用？），保持在编辑器");
        }
    }

    fn do_save(&mut self) {
        if let Some(c) = self.final_image() {
            let dir = crate::cfg().save_dir;
            match output::save_png(&c, &dir) {
                Some(p) => {
                    zx_core::log::info(format!("截图已保存 {}", p.display()));
                    self.close_soon();
                }
                None => zx_core::log::warn("截图保存失败"),
            }
        }
    }

    /// 上传：把成品 PNG 交给宿主接的钩子，立刻关窗。
    ///
    /// 钩子必须是"丢队列就返回"的实现（imgbed 就是这么做的）：上传要走网络，
    /// 在这里同步等就把覆盖层的消息循环卡死了。
    fn do_upload(&mut self) {
        let Some(c) = self.final_image() else { return };
        let Some(png) = c.png_bytes() else {
            zx_core::log::warn("图床上传：PNG 编码失败");
            return;
        };
        zx_core::log::info(format!("截图上传：{} KB", png.len() / 1024));
        if crate::send_upload(png) {
            self.close_soon();
        } else {
            zx_core::log::warn("图床上传：宿主没有接线（imgbed 未启用？）");
        }
    }

    fn do_pin(&mut self) {
        if let Some(mut c) = self.final_image() {
            output::auto_save(&c);
            let mut pt = POINT::default();
            unsafe { let _ = GetCursorPos(&mut pt); }
            pin::create(&mut c, pt);
            self.close_soon();
        }
    }

    /// Esc / 右键：从编辑态退一层。
    ///
    /// 有冻结帧就退回"可调整选区"（图能从帧里重新裁）；
    /// 长截图/拼接的成品没有对应的屏幕区域，退无可退——直接当作取消，
    /// 否则会落到一个没有图像的选区态（一片深灰 + 八个手柄，看起来像卡死）。
    fn back_out(&mut self) {
        self.close_text(true);
        if self.frame.is_some() {
            self.stage = Stage::Selected;
            self.selected = None;
            self.trace_stage("退出编辑回到选区");
            self.paint();
        } else {
            self.trace_stage("退出编辑关闭");
            self.close_soon();
        }
    }

    /// 滚轮改某条标注的粗细。连续滚动合并成一次撤销（不然滚 10 格就是 10 步）。
    fn bump_width(&mut self, i: usize, up: bool) {
        let Some(a) = self.annos.get(i) else { return };
        // 选择框和马赛克没有"粗细"可调
        if matches!(a.tool, Tool::Select | Tool::Mosaic) {
            return;
        }
        let same = self
            .width_edit
            .map(|(j, t)| j == i && t.elapsed().as_millis() < 800)
            .unwrap_or(false);
        if !same {
            self.push_hist();
        }
        self.width_edit = Some((i, Instant::now()));
        if let Some(a) = self.annos.get_mut(i) {
            a.width = clamp_width(a.width, up);
        }
        self.selected = Some(i);
    }

    /// 自检用：把"鼠标"停在左侧「矩形」按钮上并展开色板，核对提示条与色板排版。
    fn selftest_hover(&mut self) {
        let rc = self
            .toolbar
            .iter()
            .find(|(_, a)| matches!(a, Act::Tool(Tool::Rect)))
            .map(|(r, _)| *r);
        if let Some(rc) = rc {
            self.cursor = ((rc.left + rc.right) / 2, (rc.top + rc.bottom) / 2);
        }
        self.color_open = true;
        self.paint();
    }

    fn do_ocr(&mut self) {
        let Some(doc) = self.doc.take() else { return };
        unsafe { let _ = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap_or_default()); }
        let result = ocr::recognize(&doc);
        // 结果窗口要留一份原图像素：表格识别得重新跑 det，只有文本是不够的。
        let px = Some((doc.w, doc.h, doc.pixels().to_vec()));
        self.doc = Some(doc);
        unsafe { let _ = SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default()); }
        let mut pt = POINT::default();
        unsafe { let _ = GetCursorPos(&mut pt); }
        match result {
            Ok(text) => {
                zx_core::log::info(format!("OCR 完成，{} 字", text.chars().count()));
                ocr::show(text, px, pt);
            }
            Err(e) => {
                zx_core::log::warn(format!("OCR 失败：{e}"));
                ocr::show(e, px, pt);
            }
        }
    }

    fn open_style(&mut self) {
        // 预览给"原图 + 标注"，否则面板里的缩略图和成品长得不一样。
        // 标注交给面板在缩略尺寸上画，避免为了一张预览复制整张大图。
        let Some(doc) = self.doc.take() else { return };
        let mut pt = POINT::default();
        unsafe { let _ = GetCursorPos(&mut pt); }
        stylepanel::open(&doc, &self.annos, self.doc_rect(), pt);
        self.doc = Some(doc);
    }

    fn start_stitch(&mut self) {
        if self.mode == Mode::Append {
            return;
        }
        // 先把已有标注烘进图里，再作为第一块（否则标注会丢）
        let Some(flat) = self.flat() else { return };
        let n = stitch::begin(flat, stitch::Dir::Vertical);
        zx_core::log::info(format!("开始拼接，第 {n} 块"));
        self.annos.clear();
        self.hist.clear();
        self.redo.clear();
        self.selected = None;
        self.doc = None;
        self.mode = Mode::Append;
        self.stage = Stage::Choosing;
        self.sel = RECT::default();
        self.refreeze();
        self.paint();
    }

    fn refreeze(&mut self) {
        if let Some(f) = Frame::grab() {
            f.blit_into(&mut self.canvas);
            self.frame = Some(f);
            let mut pt = POINT::default();
            unsafe { let _ = GetCursorPos(&mut pt); }
            self.cursor = (pt.x, pt.y);
            self.detect(true);
        }
    }

    fn push_piece(&mut self) {
        let rect = self.sel;
        let mut added = false;
        if let Some(frame) = self.frame.as_ref() {
            if let Some(piece) = output::crop(frame, rect) {
                added = stitch::push(piece) > 0;
            }
        }
        if !added {
            // 没会话 / 超上限：保持当前选区，别把用户框好的内容吞掉
            zx_core::log::warn("拼接：这一块没有加入（会话已结束或超上限）");
            self.paint();
            return;
        }
        self.refreeze();
        self.sel = RECT::default();
        self.stage = Stage::Choosing;
        self.paint();
    }

    fn finish_stitch(&mut self) {
        let Some(doc) = stitch::render() else {
            // 尺寸超限等失败：留在收集态，用户可以继续加/删（Esc 取消）
            zx_core::log::warn("拼接失败：没有成品（尺寸超限？），仍在收集态");
            return;
        };
        stitch::cancel();
        self.mode = Mode::Select;
        self.doc = Some(doc);
        self.stage = Stage::Editing;
        self.selected = None;
        self.layout_doc(true);
        self.paint();
    }

    fn cancel_stitch(&mut self) {
        stitch::cancel();
        self.mode = Mode::Select;
        self.close_soon();
    }

    fn start_long(&mut self) {
        if !self.sel_ok() {
            return;
        }
        *pending_long().lock().unwrap_or_else(|p| p.into_inner()) = Some(self.sel);
        self.mode = Mode::Long;
        self.close_soon();
    }

    /// 选区 → 记录意图 → 关窗。真正的录制在 `close()` 里、窗口销毁之后才开始。
    ///
    /// `forced` 只影响这一次：GIF 按钮传 `Some(Gif)`，普通录屏传 `None`（按设置）。
    fn start_record(&mut self, forced: Option<RecordFormat>) {
        if !self.sel_ok() {
            return;
        }
        *pending_record().lock().unwrap_or_else(|p| p.into_inner()) = Some((self.sel, forced));
        self.mode = if forced == Some(RecordFormat::Gif) { Mode::RecordGif } else { Mode::Record };
        self.close_soon();
    }

    /// 悬停气泡上的动作：复制元素文本 / 元素信息。
    ///
    /// 只读 UIA 的 Name——它就是这个控件的标题文本，比 OCR 可靠得多（见 uielem 的
    /// `pick_text`）。名字是空的（纯图标那种）就没什么可复制的，什么都不做。
    fn run_chip(&mut self, chip: Chip) {
        let Some(el) = self.hover_elem.clone() else {
            return;
        };
        let payload = match chip {
            Chip::CopyText => Some(el.name.clone()),
            Chip::CopyInfo => Some(el.info_line()),
            Chip::SaveIcon => None,
        };
        let ok = match payload {
            Some(text) => {
                if text.trim().is_empty() {
                    zx_core::log::info("这个元素没有名称，没有可复制的文本");
                    return;
                }
                crate::copy_text(&text)
            }
            None => self.save_icon(&el),
        };
        if ok {
            self.chip_flash = Some((chip, Instant::now()));
            zx_core::log::info(format!("{}：{}", chip_label(chip, false), el.label()));
        }
    }

    /// 把元素框里的像素原样存成 PNG。
    ///
    /// 从**冻结帧**裁，不从编辑后的图裁：元素框就是屏幕上的物理像素，1:1 才叫"存图标"。
    /// 文件名 `图标_<名字>_<时间戳>.png`，名字里 Windows 不认的字符由 `output::safe_stem` 洗掉。
    fn save_icon(&self, el: &Element) -> bool {
        let Some(frame) = self.frame.as_ref() else {
            return false;
        };
        let Some(canvas) = output::crop(frame, el.to_rect()) else {
            zx_core::log::warn("存图标失败：元素框裁不出来（可能已滚出屏幕）");
            return false;
        };
        let c = crate::cfg();
        let name = output::safe_stem(&el.name, 24);
        let prefix = if name.is_empty() {
            "图标".to_string()
        } else {
            format!("图标_{name}")
        };
        match output::save_png_as(&canvas, &c.save_dir, &prefix) {
            Some(p) => {
                zx_core::log::info(format!("已存图标 {}×{}：{}", el.rect.w, el.rect.h, p.display()));
                true
            }
            None => false,
        }
    }

    fn run(&mut self, act: Act) {
        match act {
            Act::Tool(t) => {
                self.tool = t;
                self.width = t.default_width();
                self.selected = None;
                self.width_edit = None;
            }
            // 展开状态在点击处已经算好（见 WM_LBUTTONDOWN），这里不用再翻一次
            Act::ColorPick => {}
            Act::Undo => self.undo(),
            Act::Redo => self.redo(),
            Act::Beautify => self.open_style(),
            Act::Stitch => self.start_stitch(),
            Act::Ocr => self.do_ocr(),
            Act::Long => {
                // 标注会丢：长截图是从屏幕上重新抓帧，画上去的标注没法跟着滚动
                if !self.annos.is_empty() {
                    zx_core::log::warn("进长截图：已有标注会被丢弃");
                }
                self.start_long();
            }
            Act::Record | Act::RecordGif => {
                // 录制是从屏幕重新抓帧，画上去的标注不会跟着走
                if !self.annos.is_empty() {
                    zx_core::log::warn("进录屏：已有标注不会被录进去");
                }
                let forced = matches!(act, Act::RecordGif).then_some(RecordFormat::Gif);
                self.start_record(forced);
            }
            Act::Copy => self.do_copy(),
            Act::Save => self.do_save(),
            Act::Upload => self.do_upload(),
            Act::Pin => self.do_pin(),
            Act::Cancel => self.close_soon(),
        }
    }

    /// 关闭不要就地 `close()`：那样会在 WndProc 里把自己 free 掉。
    /// 投一条 WM_CLOSE 排到消息队列尾部，等这一轮消息处理完再真正销毁。
    fn close_soon(&mut self) {
        unsafe {
            let _ = PostMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

// ---------------------------------------------------------------- 窗口过程

fn win_class() -> Vec<u16> { zx_core::win::wide(TEXT_CLASS) }

pub fn register() {
    crate::register_class_dbl(CLASS, wndproc);
    register_class(TEXT_CLASS, text_wndproc, None);
}

/// 输入框外框的宽度（像素）：2 道 1px 线 + 主题色环 + 内衬留白。
fn text_frame_inset(s: f32) -> i32 {
    let hair = (1.0 * s).round().max(1.0) as i32;
    let ring = (2.0 * s).round().max(2.0) as i32;
    hair * 2 + ring + (3.0 * s).round().max(2.0) as i32
}

/// 画输入框外框：由外向内 1px 深色 / 1px 白 / 2px 主题蓝，最后是白色底。
///
/// 为什么要三层：截图的背景可能是纯白、纯黑或者花花绿绿的窗口。
/// 只压一条系统灰边（`WS_BORDER`）在白底上等于没有；只压蓝边在深色照片上又偏闷。
/// 深色线负责和白底分开，白线负责和深底分开，蓝环负责表达「这里是输入框」。
/// EDIT 本身不透明白底，所以输入的文字永远是白底黑字，跟背后的图无关。
fn paint_text_frame(hwnd: HWND, hdc: HDC, s: f32) {
    let hair = (1.0 * s).round().max(1.0) as i32;
    let ring = (2.0 * s).round().max(2.0) as i32;
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    // 逐层向内覆盖：每一层都把内部整块刷成自己的颜色，最后剩下的是白底
    for (inset, color) in [
        (0, 0xFF10131Au32),
        (hair, 0xFFFFFFFF),
        (hair * 2, 0xFF2E90FA),
        (hair * 2 + ring, 0xFFFFFFFF),
    ] {
        let band = RECT {
            left: r.left + inset,
            top: r.top + inset,
            right: r.right - inset,
            bottom: r.bottom - inset,
        };
        if band.right <= band.left || band.bottom <= band.top {
            break;
        }
        unsafe {
            let brush = CreateSolidBrush(rgb_ref(color));
            FillRect(hdc, &band, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }
}

fn rgb_ref(argb: u32) -> COLORREF {
    let (r, g, b) = ((argb >> 16) & 0xFF, (argb >> 8) & 0xFF, argb & 0xFF);
    COLORREF(b << 16 | g << 8 | r)
}

unsafe extern "system" fn text_wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { text_wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn text_wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    match msg {
        WM_ACTIVATE => {
            if (w.0 & 0xFFFF) == 0 {
                if let Ok(parent) = GetParent(hwnd) {
                    let _ = PostMessageW(parent, WM_TEXT_COMMIT, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
            paint_text_frame(hwnd, hdc, s);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// EDIT 的子类化：回车提交、Esc 取消。EDIT 本身不发这两个通知，只能挂 WndProc。
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { edit_proc_inner(hwnd, msg, w, l) })
}

unsafe fn edit_proc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    if msg == WM_KEYDOWN {
        let owner = TEXT_OWNER.load(Ordering::SeqCst);
        let target = if owner != 0 { HWND(owner as *mut _) } else { GetParent(hwnd).unwrap_or_default() };
        if w.0 as u16 == VK_RETURN.0 {
            let _ = PostMessageW(target, WM_TEXT_COMMIT, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        if w.0 as u16 == VK_ESCAPE.0 {
            let _ = PostMessageW(target, WM_TEXT_CANCEL, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
    }
    let old = EDIT_OLD.load(Ordering::SeqCst);
    if old == 0 {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    type Proc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    let old: Proc = std::mem::transmute(old);
    CallWindowProcW(Some(old), hwnd, msg, w, l)
}

fn subclass_text_edit(edit: HWND) {
    unsafe {
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as usize as isize);
        EDIT_OLD.store(old, Ordering::SeqCst);
    }
}

// ------------------------------------------------------------ 消息 → 状态迁移
//
// 每次阶段迁移都是一个具名方法：看一个方法就知道这一阶段在做什么，
// wndproc 只剩分流与 LRESULT。原来的写法把迁移埋在六层缩进加闭包 return 里，
// 想确认「松手后到底会不会重绘」必须顺着 `return` 一路往回找。

impl Ov {
    /// 阶段迁移追踪：只有设了 `ZHUXIA_CAPTURE_TRACE=1` 才写日志。
    /// 覆盖层的行为全在状态迁移里，没有这条就没法确认「按下-拖动-松手」走对了分支。
    fn trace_stage(&self, what: &str) {
        if std::env::var_os("ZHUXIA_CAPTURE_TRACE").is_none() {
            return;
        }
        zx_core::log::info(format!(
            "覆盖层 {what}: stage={:?} sel=({},{},{},{}) 标注={} 选中={:?} 工具={:?}",
            self.stage,
            self.sel.left,
            self.sel.top,
            self.sel.right,
            self.sel.bottom,
            self.annos.len(),
            self.selected,
            self.tool
        ));
    }

    fn on_move(&mut self, l: LPARAM) {
        self.cursor = self.cursor_in(l);
        match self.stage {
            Stage::Choosing => self.detect(false),
            Stage::Dragging => self.sel = self.sel_rect(),
            Stage::Selected => self.slide_selection(),
            Stage::Editing => self.slide_annotation(),
        }
        self.paint();
    }

    /// 已框选：有控制点就改尺寸，没有就整块平移。两者都是相对「上一次位置」的增量。
    fn slide_selection(&mut self) {
        if let Some((h, start, origin)) = self.adjust {
            self.sel = resize(origin, h, start, self.cursor, self.frame_bounds());
        } else if let Some(start) = self.drag_move {
            let (dx, dy) = (self.cursor.0 - start.0, self.cursor.1 - start.1);
            self.sel = shift(self.sel, dx, dy, self.frame_bounds());
            self.drag_move = Some(self.cursor);
        }
    }

    /// 编辑态：正在画 > 拖控制点 > 平移整条标注，优先级不能换。
    fn slide_annotation(&mut self) {
        if self.drawing.is_some() {
            let (lx, ly) = self.to_doc_clamped(self.cursor.0, self.cursor.1);
            if let Some(a) = &mut self.drawing {
                a.set_end((lx as i32, ly as i32));
            }
            return;
        }
        if let Some((i, h, orig)) = self.anno_resize.clone() {
            let (lx, ly) = self.to_doc(self.cursor.0, self.cursor.1);
            if let Some(a) = self.annos.get_mut(i) {
                annotate::resize(a, h, lx.max(0.0), ly.max(0.0), &orig);
            }
            return;
        }
        if let Some((i, last)) = self.dragging_anno {
            // 标注坐标在图空间，鼠标位移要按显示缩放折回去。
            let k = self.doc_scale.max(0.01);
            let (dx, dy) = (self.cursor.0 - last.0, self.cursor.1 - last.1);
            if let Some(a) = self.annos.get_mut(i) {
                a.translate((dx as f32 / k).round() as i32, (dy as f32 / k).round() as i32);
            }
            self.dragging_anno = Some((i, self.cursor));
        }
    }

    /// 左键按下。返回「需要弹取色器」的请求：取色器内部跑局部消息循环，
    /// 持有 `&mut Ov` 时调用会让回调再次 `with_ov`，形成别名可变借用。
    fn on_press(&mut self, hwnd: HWND, l: LPARAM) -> Option<(u32, POINT)> {
        self.cursor = self.cursor_in(l);
        self.width_edit = None;
        self.close_text(true);
        // 悬停气泡上的小按钮优先于"开始框选"：点在"复制文本"上不能顺手拉出一个新框。
        let hit = self
            .chips
            .iter()
            .find(|(rc, _)| target::hit(rc, self.cursor.0, self.cursor.1))
            .map(|(_, c)| *c);
        if let Some(chip) = hit {
            self.run_chip(chip);
            self.paint();
            self.refresh_cursor();
            return None;
        }
        let req = match self.stage {
            Stage::Choosing | Stage::Dragging | Stage::Selected => {
                self.begin_selection(hwnd);
                None
            }
            Stage::Editing => self.press_editing(hwnd),
        };
        self.paint();
        self.refresh_cursor();
        req
    }

    fn begin_selection(&mut self, hwnd: HWND) {
        // 已框选时先判手柄：点中控制点不能重新拉一个新框。
        if self.stage == Stage::Selected {
            if let Some(h) = handle_at(self.sel, self.cursor, (8.0 * self.scale()) as i32) {
                if h == Handle::Move {
                    self.drag_move = Some(self.cursor);
                } else {
                    self.adjust = Some((h, self.cursor, self.sel));
                }
                return;
            }
        }
        self.anchor = self.cursor;
        self.sel = RECT {
            left: self.cursor.0,
            top: self.cursor.1,
            right: self.cursor.0,
            bottom: self.cursor.1,
        };
        self.stage = Stage::Dragging;
        unsafe {
            SetCapture(hwnd);
        }
        self.trace_stage("开始框选");
    }

    fn press_editing(&mut self, hwnd: HWND) -> Option<(u32, POINT)> {
        let palette_open = self.color_open;
        if palette_open {
            if let Some(pick) = self
                .palette
                .iter()
                .find(|(rc, _)| target::hit(rc, self.cursor.0, self.cursor.1))
                .map(|(_, p)| *p)
            {
                self.color_open = false;
                return match pick {
                    Pick::Preset(i) => {
                        if let Some(c) = PALETTE.get(i) {
                            self.color = *c;
                        }
                        None
                    }
                    Pick::Custom => {
                        Some((self.color, POINT { x: self.cursor.0, y: self.cursor.1 }))
                    }
                };
            }
        }
        if let Some(act) = self
            .toolbar
            .iter()
            .find(|(r, _)| target::hit(r, self.cursor.0, self.cursor.1))
            .map(|(_, a)| *a)
        {
            // 点工具条任何按钮都收起色板；点「颜色」本身是开关。
            self.color_open = matches!(act, Act::ColorPick) && !palette_open;
            self.run(act);
            return None;
        }
        self.color_open = false;
        self.press_canvas(hwnd);
        None
    }

    fn press_canvas(&mut self, hwnd: HWND) {
        let r = self.doc_rect();
        if !target::hit(&r, self.cursor.0, self.cursor.1) {
            // 图外的压暗区不是画布：点它不该在边界上留一坨标注。
            self.trace_stage("点在图外");
            return;
        }
        let (lx, ly) = self.to_doc(self.cursor.0, self.cursor.1);

        // 选中框上的控制点优先，且不看当前工具：刚画完的图形工具还停在
        // 「矩形」上，这时拖控制点应该改尺寸，而不是又画一个新矩形。
        if self.grab_anno_handle(lx, ly, hwnd) {
            return;
        }

        match self.tool {
            Tool::Select => {
                self.selected = annotate::hit(&self.annos, lx, ly);
                if let Some(i) = self.selected {
                    self.push_hist();
                    self.dragging_anno = Some((i, self.cursor));
                }
                self.trace_stage("画布选择");
            }
            Tool::Text => {
                let (clx, cly) = self.to_doc_clamped(self.cursor.0, self.cursor.1);
                self.open_text((clx, cly));
            }
            _ => {
                let (clx, cly) = self.to_doc_clamped(self.cursor.0, self.cursor.1);
                let n = if self.tool == Tool::Counter {
                    annotate::next_counter(&self.annos)
                } else {
                    0
                };
                let mut a = Anno::new(self.tool, (clx as i32, cly as i32), self.color, n);
                // 滚轮调过的线宽要生效（`Anno::new` 只给工具默认值）。
                a.width = self.width;
                self.drawing = Some(a);
                unsafe {
                    SetCapture(hwnd);
                }
                self.trace_stage("开始画标注");
            }
        }
    }

    /// 点中当前选中标注的控制点：先存撤销点，再把快照交给 `anno_resize`。
    fn grab_anno_handle(&mut self, lx: f32, ly: f32, hwnd: HWND) -> bool {
        let Some(i) = self.selected else { return false };
        let Some(a) = self.annos.get(i).cloned() else { return false };
        let tol = (9.0 / self.doc_scale.max(0.01)).max(4.0);
        let Some(h) = annotate::handle_at(&a, lx, ly, tol) else { return false };
        self.push_hist();
        self.anno_resize = Some((i, h, a));
        unsafe {
            SetCapture(hwnd);
        }
        true
    }

    fn on_release(&mut self, l: LPARAM) {
        self.cursor = self.cursor_in(l);
        match self.stage {
            Stage::Dragging => {
                // 拼接/录制会把窗口交出去，这两条路径不能再重绘。
                if !self.settle_drag() {
                    return;
                }
            }
            Stage::Selected => {
                self.adjust = None;
                self.drag_move = None;
                self.normalize_sel();
            }
            Stage::Editing => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                if let Some(a) = self.drawing.take() {
                    if a.is_meaningful() {
                        self.push_hist();
                        self.annos.push(a);
                        // 画完就选中：立刻给出可编辑的框和控制点，
                        // 不用再切回选择工具点一下才看得到。
                        self.selected = Some(self.annos.len() - 1);
                        self.trace_stage("标注落笔");
                    }
                }
                self.dragging_anno = None;
                self.anno_resize = None;
            }
            Stage::Choosing => {}
        }
        self.paint();
        self.refresh_cursor();
    }

    /// 松手定框。返回 false 表示已经转交给拼接或录制，不要继续按选区处理。
    fn settle_drag(&mut self) -> bool {
        unsafe {
            let _ = ReleaseCapture();
        }
        self.sel = self.sel_rect();
        let tiny = (self.sel.right - self.sel.left) < 4 && (self.sel.bottom - self.sel.top) < 4;
        if tiny {
            // 没拖动就是单击：鼠标下的窗口整个框进来。
            match self.hover_rect() {
                Some(r) => self.sel = r,
                None => {
                    self.stage = Stage::Choosing;
                    self.sel = RECT::default();
                    self.trace_stage("单击空白，回到选择");
                    return true;
                }
            }
        }
        if self.mode == Mode::Append {
            self.trace_stage("追加拼接块");
            self.push_piece();
            return false;
        }
        if matches!(self.mode, Mode::Record | Mode::RecordGif) {
            let forced = (self.mode == Mode::RecordGif).then_some(RecordFormat::Gif);
            self.trace_stage("松手转录制");
            self.start_record(forced);
            return false;
        }
        self.trace_stage("松手进入编辑");
        self.enter_editing(self.sel);
        true
    }

    fn on_dblclick(&mut self) {
        match self.stage {
            Stage::Selected => self.enter_editing(self.sel),
            Stage::Editing => self.do_copy(),
            _ => {}
        }
        self.paint();
    }

    fn on_right_up(&mut self) {
        if self.stage == Stage::Editing {
            self.back_out();
        } else if self.mode == Mode::Append {
            self.cancel_stitch();
        } else if self.magnifier && self.frame.is_some()
            && matches!(self.stage, Stage::Choosing | Stage::Selected | Stage::Dragging)
        {
            // 放大镜开着的时候右键 = 取色：把光标下的颜色写进剪贴板。
            // 取消仍然有 Esc（`paint_hint` 里也这么写），不抢 Esc 的活。
            self.copy_color();
        } else {
            self.close_soon();
        }
    }

    /// 放大镜取色：`#RRGGBB` 写进剪贴板，信息条上闪一下确认。
    fn copy_color(&mut self) {
        let Some(frame) = &self.frame else { return };
        let px = frame.pixel(self.cursor.0, self.cursor.1);
        let hex = format!("#{:02X}{:02X}{:02X}", (px >> 16) & 0xFF, (px >> 8) & 0xFF, px & 0xFF);
        if crate::copy_text(&hex) {
            self.color_flash = Some(hex);
            unsafe {
                // 1.2 秒后把「已复制」擦掉（WM_TIMER 4）
                let _ = KillTimer(self.hwnd, 4);
                SetTimer(self.hwnd, 4, 1200, None);
            }
            self.paint();
        }
    }

    fn on_wheel(&mut self, delta: i32) {
        // 滚轮消息的坐标是屏幕坐标，先同步一次光标，避免用到上一个位置。
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        if pt.x >= self.vx && pt.y >= self.vy && pt.x < self.vx + self.vw && pt.y < self.vy + self.vh {
            self.cursor = (pt.x, pt.y);
        }
        match self.stage {
            Stage::Choosing => {
                if self.mode == Mode::Append {
                    stitch::set_dir(stitch::dir().next());
                } else if !self.chain.is_empty() {
                    // 多层叠加时滚轮在上层窗口之间切换。
                    let n = self.chain.len();
                    self.level = if delta > 0 { (self.level + 1) % n } else { (self.level + n - 1) % n };
                }
                self.detect(true);
            }
            Stage::Editing => {
                // 悬停在某条标注上就改它；空白处改「下一条」的默认线宽。
                let (lx, ly) = self.to_doc(self.cursor.0, self.cursor.1);
                match annotate::hit(&self.annos, lx, ly) {
                    Some(i) => self.bump_width(i, delta > 0),
                    None => self.width = clamp_width(self.width, delta > 0),
                }
            }
            _ => {}
        }
        self.paint();
    }

    fn on_key(&mut self, key: u16) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000 != 0 };
        let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 };
        let redraw = if self.stage == Stage::Editing {
            self.key_editing(key, ctrl, shift_down)
        } else {
            self.key_frame(key, ctrl, shift_down)
        };
        if redraw {
            self.paint();
            self.refresh_cursor();
        }
    }

    /// 编辑态快捷键。返回 false 表示动作已经把窗口收掉了，不要再重绘。
    fn key_editing(&mut self, key: u16, ctrl: bool, shift_down: bool) -> bool {
        if ctrl {
            match key {
                k if k == b'C' as u16 => {
                    self.do_copy();
                    return false;
                }
                k if k == b'S' as u16 => {
                    self.do_save();
                    return false;
                }
                k if k == b'U' as u16 => {
                    if crate::upload_available() {
                        self.do_upload();
                    }
                    return false;
                }
                k if k == b'Z' as u16 => {
                    if shift_down {
                        self.redo();
                    } else {
                        self.undo();
                    }
                    return true;
                }
                k if k == b'O' as u16 => {
                    self.do_ocr();
                    return false;
                }
                _ => {}
            }
        }
        match key {
            k if k == VK_ESCAPE.0 => {
                self.back_out();
                false
            }
            k if k == VK_RETURN.0 => {
                self.do_copy();
                false
            }
            k if k == VK_DELETE.0 => {
                if let Some(i) = self.selected.take() {
                    self.push_hist();
                    if i < self.annos.len() {
                        self.annos.remove(i);
                    }
                }
                true
            }
            k => {
                if let Some(t) = Tool::from_key(k) {
                    self.tool = t;
                    self.width = t.default_width();
                    self.selected = None;
                    self.width_edit = None;
                }
                true
            }
        }
    }

    /// 选区阶段快捷键：拼接/长截图/录制入口与方向键微调。
    fn key_frame(&mut self, key: u16, ctrl: bool, shift_down: bool) -> bool {
        if self.mode == Mode::Append {
            if key == VK_RETURN.0 {
                self.finish_stitch();
                return false;
            }
            if key == VK_ESCAPE.0 {
                self.cancel_stitch();
                return false;
            }
            return true;
        }
        if key == VK_ESCAPE.0 {
            if self.stage == Stage::Selected {
                // 先退回重新框选，再按一次才退出。
                self.stage = Stage::Choosing;
                self.sel = RECT::default();
                return true;
            }
            self.close_soon();
            return false;
        }
        // Tab：在"识别元素"和"只认窗口"之间切（PixPin 文档里的同款操作）。
        if key == VK_TAB.0 && self.stage == Stage::Choosing {
            self.elements_on = !self.elements_on;
            self.hover_log = None;
            self.detect(true);
            zx_core::log::info(format!(
                "元素检测：{}",
                if self.elements_on { "开" } else { "只认窗口" }
            ));
            return true;
        }
        if key == VK_RETURN.0 {
            if self.stage == Stage::Selected && self.sel_ok() {
                self.enter_editing(self.sel);
                return true;
            }
            // 没拖动也能进：光标下那个元素直接当选区，和单击同一个语义。
            if self.stage == Stage::Choosing {
                if let Some(r) = self.hover_rect() {
                    self.sel = r;
                    self.trace_stage("Enter 取元素");
                    self.enter_editing(r);
                    return false;
                }
            }
            return true;
        }
        if ctrl {
            match key {
                k if k == b'L' as u16 => {
                    if self.stage == Stage::Selected && self.sel_ok() {
                        self.start_long();
                        return false;
                    }
                }
                k if k == b'R' as u16 => {
                    if self.stage == Stage::Selected && self.sel_ok() {
                        self.start_record(None);
                        return false;
                    }
                }
                k if k == b'G' as u16
                    && self.stage == Stage::Selected && self.sel_ok() => {
                        self.start_record(Some(RecordFormat::Gif));
                        return false;
                    }
                _ => {}
            }
        }
        if self.stage == Stage::Selected {
            let step = if shift_down { 10 } else { 1 };
            let (dx, dy) = match key {
                k if k == VK_LEFT.0 => (-step, 0),
                k if k == VK_RIGHT.0 => (step, 0),
                k if k == VK_UP.0 => (0, -step),
                k if k == VK_DOWN.0 => (0, step),
                _ => (0, 0),
            };
            if dx != 0 || dy != 0 {
                self.sel = shift(self.sel, dx, dy, self.frame_bounds());
            }
        }
        true
    }

    /// 手柄决定光标形状：只有已框选阶段才可能是缩放/移动。
    fn cursor_kind(&self) -> PCWSTR {
        // 工具条、色板上的东西都是可点的：手型，别拿十字去指按钮。
        if self.over_ui() {
            return IDC_HAND;
        }
        match self.stage {
            Stage::Choosing | Stage::Dragging => IDC_CROSS,
            Stage::Selected => {
                let (x, y) = self.cursor;
                match handle_at(self.sel, (x, y), (8.0 * self.scale()) as i32) {
                    Some(Handle::N) | Some(Handle::S) => IDC_SIZENS,
                    Some(Handle::E) | Some(Handle::W) => IDC_SIZEWE,
                    Some(Handle::NW) | Some(Handle::SE) => IDC_SIZENWSE,
                    Some(Handle::NE) | Some(Handle::SW) => IDC_SIZENESW,
                    Some(Handle::Move) => IDC_SIZEALL,
                    None => IDC_CROSS,
                }
            }
            Stage::Editing => self.cursor_editing(),
        }
    }

    /// 指针是否压在工具条按钮或色板上（每帧重算的命中区已经存好了）。
    fn over_ui(&self) -> bool {
        let (x, y) = self.cursor;
        self.toolbar.iter().any(|(r, _)| target::hit(r, x, y))
            || self.palette.iter().any(|(r, _)| target::hit(r, x, y))
    }

    /// 编辑态光标：画图工具十字、文字工字、选择工具箭头，
    /// 压在选中图形（或框线）上则是缩放/移动——这才是"点下去会发生什么"的预告。
    fn cursor_editing(&self) -> PCWSTR {
        if self.drawing.is_some() {
            return IDC_CROSS;
        }
        if let Some((_, h, _)) = self.anno_resize {
            return anno_cursor(h);
        }
        let (lx, ly) = self.to_doc(self.cursor.0, self.cursor.1);
        if let Some(a) = self.selected.and_then(|i| self.annos.get(i)) {
            let tol = (9.0 / self.doc_scale.max(0.01)).max(4.0);
            if let Some(h) = annotate::handle_at(a, lx, ly, tol) {
                return anno_cursor(h);
            }
        }
        match self.tool {
            Tool::Select => {
                if annotate::hit(&self.annos, lx, ly).is_some() {
                    IDC_SIZEALL
                } else {
                    IDC_ARROW
                }
            }
            Tool::Text => IDC_IBEAM,
            _ => IDC_CROSS,
        }
    }

    /// 改了工具/拖完手柄之后立刻换光标，不用等鼠标再动一下。
    fn refresh_cursor(&self) {
        let cur = self.cursor_kind();
        unsafe {
            let _ = SetCursor(LoadCursorW(None, cur).unwrap_or_default());
        }
    }
}

/// 标注控制点 → 系统光标。
fn anno_cursor(h: AHdl) -> PCWSTR {
    match h {
        AHdl::N | AHdl::S => IDC_SIZENS,
        AHdl::E | AHdl::W => IDC_SIZEWE,
        AHdl::NW | AHdl::SE => IDC_SIZENWSE,
        AHdl::NE | AHdl::SW => IDC_SIZENESW,
        AHdl::P0 | AHdl::P1 => IDC_CROSS,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, w, l), { wndproc_inner(hwnd, msg, w, l) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {

    match msg {
        WM_ELEMS_READY => {
            // 元素快照晚到了：重算一次悬停链并重画，别让用户先抖鼠标才看见框。
            with_ov(|o| {
                let n = match o.elems.try_lock() {
                    Ok(g) => g.len().to_string(),
                    Err(_) => "锁被占".into(),
                };
                zx_core::log::info(format!("元素到位：覆盖层看到 {n} 个，重新命中"));
                o.hover_log = None;
                o.detect(true);
                o.paint();
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with_ov(|o| o.on_move(l));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let req = with_ov(|o| o.on_press(hwnd, l)).flatten();
            if let Some((cur, near)) = req {
                let picked = picker::pick(cur, near);
                with_ov(|o| {
                    if let Some(c) = picked {
                        o.color = c;
                    }
                    o.paint();
                });
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_ov(|o| o.on_release(l));
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            with_ov(|o| o.on_dblclick());
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            with_ov(|o| o.on_right_up());
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((w.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            with_ov(|o| o.on_wheel(delta));
            LRESULT(0)
        }
        WM_KEYDOWN => {
            with_ov(|o| o.on_key(w.0 as u16));
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let cur = with_ov(|o| o.cursor_kind()).unwrap_or(IDC_ARROW);
            unsafe {
                let _ = SetCursor(LoadCursorW(None, cur).unwrap_or_default());
            }
            LRESULT(1)
        }
        WM_TEXT_COMMIT => {
            with_ov(|o| {
                o.close_text(true);
                o.paint();
            });
            LRESULT(0)
        }
        WM_TEXT_CANCEL => {
            with_ov(|o| {
                o.close_text(false);
                o.paint();
            });
            LRESULT(0)
        }
        WM_STYLE_CHANGED => {
            with_ov(|o| o.paint());
            LRESULT(0)
        }
        WM_TIMER => {
            if w.0 == 3 {
                unsafe {
                    let _ = KillTimer(hwnd, 3);
                }
                with_ov(|o| o.selftest_hover());
                return LRESULT(0);
            }
            // 4 = 取色成功提示的自动消失
            if w.0 == 4 {
                unsafe {
                    let _ = KillTimer(hwnd, 4);
                }
                with_ov(|o| {
                    o.color_flash = None;
                    o.paint();
                });
                return LRESULT(0);
            }
            unsafe {
                let _ = KillTimer(hwnd, 1);
            }
            with_ov(|o| o.close_soon());
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

// ---------------------------------------------------------------- 几何

/// 平移选区，保持尺寸不变，并夹在可用屏幕范围内。
fn shift(r: RECT, dx: i32, dy: i32, bounds: RECT) -> RECT {
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    let left = (r.left + dx).clamp(bounds.left, (bounds.right - w).max(bounds.left));
    let top = (r.top + dy).clamp(bounds.top, (bounds.bottom - h).max(bounds.top));
    RECT { left, top, right: left + w, bottom: top + h }
}

/// 拖控制点缩放；越过对边时自动翻转，结果始终 `left<=right`、`top<=bottom`。
fn resize(origin: RECT, handle: Handle, start: (i32, i32), cur: (i32, i32), bounds: RECT) -> RECT {
    let dx = cur.0 - start.0;
    let dy = cur.1 - start.1;
    let (mut left, mut top, mut right, mut bottom) =
        (origin.left, origin.top, origin.right, origin.bottom);

    if matches!(handle, Handle::W | Handle::NW | Handle::SW) {
        left = origin.left + dx;
    }
    if matches!(handle, Handle::E | Handle::NE | Handle::SE) {
        right = origin.right + dx;
    }
    if matches!(handle, Handle::N | Handle::NW | Handle::NE) {
        top = origin.top + dy;
    }
    if matches!(handle, Handle::S | Handle::SW | Handle::SE) {
        bottom = origin.bottom + dy;
    }

    let (left, right) = (left.min(right), left.max(right));
    let (top, bottom) = (top.min(bottom), top.max(bottom));
    let left = left.clamp(bounds.left, bounds.right);
    let right = right.clamp(bounds.left, bounds.right);
    let top = top.clamp(bounds.top, bounds.bottom);
    let bottom = bottom.clamp(bounds.top, bounds.bottom);
    RECT { left, top, right, bottom }
}

/// 鼠标落在哪个控制点；四角优先于四边，内部返回 Move。
fn handle_at(r: RECT, p: (i32, i32), tol: i32) -> Option<Handle> {
    let (x, y) = p;
    if x < r.left - tol || x > r.right + tol || y < r.top - tol || y > r.bottom + tol {
        return None;
    }

    let (left, top) = (r.left.min(r.right), r.top.min(r.bottom));
    let (right, bottom) = (r.left.max(r.right), r.top.max(r.bottom));
    let vertical = if (y - top).abs() <= tol {
        Some(Handle::N)
    } else if (y - bottom).abs() <= tol {
        Some(Handle::S)
    } else {
        None
    };
    let horizontal = if (x - left).abs() <= tol {
        Some(Handle::W)
    } else if (x - right).abs() <= tol {
        Some(Handle::E)
    } else {
        None
    };

    match (vertical, horizontal) {
        (Some(v), Some(Handle::W)) => Some(match v {
            Handle::N => Handle::NW,
            _ => Handle::SW,
        }),
        (Some(v), Some(_)) => Some(match v {
            Handle::N => Handle::NE,
            _ => Handle::SE,
        }),
        (Some(v), None) => Some(v),
        (None, Some(h)) => Some(h),
        _ => {
            if x > left && x < right && y > top && y < bottom {
                Some(Handle::Move)
            } else {
                None
            }
        }
    }
}

/// 滚轮调线宽：0.5 一档，范围 1–32。
fn clamp_width(w: f32, up: bool) -> f32 {
    (if up { w + 0.5 } else { w - 0.5 }).clamp(1.0, 32.0)
}

fn tip_of(act: Act) -> (&'static str, &'static str) {
    match act {
        Act::Tool(tool) => match tool {
            Tool::Select => ("选择 / 移动", "V"),
            Tool::Rect => ("矩形", "R"),
            Tool::Ellipse => ("椭圆", "O"),
            Tool::Arrow => ("箭头", "A"),
            Tool::Line => ("直线", "L"),
            Tool::Pen => ("画笔", "P"),
            Tool::Highlighter => ("荧光笔", "H"),
            Tool::Mosaic => ("马赛克", "M"),
            Tool::Text => ("文字", "T"),
            Tool::Counter => ("序号工具：在图上点一下放一个号", "N"),
        },
        Act::ColorPick => ("颜色", ""),
        Act::Undo => ("撤销", "Ctrl+Z"),
        Act::Redo => ("重做", "Ctrl+Shift+Z"),
        Act::Beautify => ("美化：背景 / 边距 / 圆角 / 水印", ""),
        Act::Stitch => ("拼接：连续框选，Enter 出成品", ""),
        Act::Ocr => ("识别文字", "Ctrl+O"),
        Act::Long => ("长截图：选区内滚动页面，自动拼接", "Ctrl+L"),
        Act::Record => ("录屏：按设置里的格式（视频或 GIF），可叠加摄像头画面", "Ctrl+R"),
        Act::RecordGif => ("录 GIF：本次强制录成 GIF（无音轨，长边默认 960）", "Ctrl+G"),
        Act::Cancel => ("取消", "Esc"),
        Act::Copy => ("复制并退出", "Ctrl+C / Enter"),
        Act::Save => ("保存 PNG 并退出", "Ctrl+S"),
        Act::Upload => ("上传到图床，链接进剪贴板", "Ctrl+U"),
        Act::Pin => ("贴图到屏幕", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> RECT { RECT { left: 0, top: 0, right: 1000, bottom: 800 } }

    #[test]
    fn resize_from_west_flips_past_east_edge() {
        let origin = RECT { left: 100, top: 100, right: 300, bottom: 200 };
        let r = resize(origin, Handle::W, (100, 150), (400, 150), bounds());
        assert_eq!((r.left, r.right), (300, 400));
        assert!(r.left <= r.right);
    }

    #[test]
    fn resize_is_clamped_to_screen() {
        let origin = RECT { left: 100, top: 100, right: 300, bottom: 200 };
        let r = resize(origin, Handle::SE, (300, 200), (5000, 5000), bounds());
        assert_eq!((r.right, r.bottom), (1000, 800));
    }

    #[test]
    fn shift_keeps_size_and_stays_on_screen() {
        let r = RECT { left: 900, top: 700, right: 1000, bottom: 800 };
        let moved = shift(r, 500, 500, bounds());
        assert_eq!(moved.right - moved.left, 100);
        assert_eq!(moved.bottom - moved.top, 100);
        assert_eq!((moved.left, moved.top), (900, 700));
    }

    #[test]
    fn width_steps_are_clamped() {
        assert_eq!(clamp_width(3.0, true), 3.5);
        assert_eq!(clamp_width(3.0, false), 2.5);
        assert_eq!(clamp_width(31.9, true), 32.0);
        assert_eq!(clamp_width(1.1, false), 1.0);
    }

    #[test]
    fn handle_hit_returns_corners_and_move() {
        let r = RECT { left: 100, top: 100, right: 300, bottom: 200 };
        assert_eq!(handle_at(r, (100, 100), 8), Some(Handle::NW));
        assert_eq!(handle_at(r, (300, 200), 8), Some(Handle::SE));
        assert_eq!(handle_at(r, (200, 200), 8), Some(Handle::S));
        assert_eq!(handle_at(r, (200, 150), 8), Some(Handle::Move));
        assert_eq!(handle_at(r, (600, 600), 8), None);
    }
}
