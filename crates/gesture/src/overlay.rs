//! 手势轨迹覆盖层：一整块虚拟桌面的透明分层窗口，画轨迹线与动作提示。
//!
//! 复用 `zx-ui` 的 Canvas（DIB + GDI+ + `UpdateLayeredWindow`），和任务栏覆盖层、
//! 贴图是同一套上屏路径，常驻时只有一块 DIB，没有额外的渲染进程。
//!
//! 跨线程的做法与 taskmon 一致：**谁都不直接画**。解析线程把"要画什么"写进
//! `TRAIL` / `HINT` 两个快照，然后 `PostMessage` 一个自定义消息；UI 线程收到后重绘一次。
//! 这样 GDI+ 的 `Graphics` 永远只被创建它的线程碰，不需要为画布加锁。
//!
//! 三个非显然的点：
//!
//! - `WS_EX_TRANSPARENT` 让鼠标事件穿透，覆盖层不抢焦点也不挡操作；
//! - 窗口必须每次手势开始时重新顶到 topmost 的最上面：晚于我们启动的置顶窗口会压住它；
//! - 分层窗口的命中是按 alpha 判定的，但这里全程 `WS_EX_TRANSPARENT`，不存在"只有笔画能点中"的问题。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::window::create_window;
use zx_ui::{Canvas, TextAlign, VAlign};

const CLASS: &str = "ZhuxiaGestureTrail";
const WM_TRAIL: u32 = WM_APP + 51;
const WM_HINT: u32 = WM_APP + 52;
const WM_CLEAR: u32 = WM_APP + 53;
/// 闲置后释放轨迹画布（只在覆盖层线程 drop GDI+ 对象）。
const WM_RELEASE: u32 = WM_APP + 54;
const TIMER_HINT: usize = 1;

static HWND_ATOM: AtomicIsize = AtomicIsize::new(0);
/// 最后一次上屏的时刻（`GetTickCount64`，毫秒）。闲置回收用。
static LAST_VISIBLE_MS: AtomicU64 = AtomicU64::new(0);
/// 待处理的重绘：一次 WM_TRAIL 处理期间再来的更新会被合并，队列里最多只有一条。
/// 没有这个闸门时，125Hz 的鼠标移动会灌满窗口消息队列（上限 1 万条），
/// 后期 `PostMessage` 静默失败，表现就是"轨迹画到一半不动了"。
static REDRAW_PENDING: AtomicBool = AtomicBool::new(false);

type TrailState = (Vec<(i32, i32)>, bool);

fn trail_cell() -> &'static Mutex<TrailState> {
    static CELL: OnceLock<Mutex<TrailState>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new((Vec::new(), false)))
}

fn hint_cell() -> &'static Mutex<Option<String>> {
    static CELL: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// 轨迹外观：(已匹配色, 未匹配色, 线宽)。由 `apply_config` 推进来，
/// 避免每次鼠标移动重绘都去克隆一份 `Settings`。
fn style() -> &'static Mutex<(u32, u32, i32)> {
    static CELL: OnceLock<Mutex<(u32, u32, i32)>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new((0xFF3B82F6, 0xFFBDBDBD, 3)))
}

pub fn set_style(active: u32, inactive: u32, thickness: i32) {
    *style().lock().unwrap_or_else(|p| p.into_inner()) = (active, inactive, thickness.max(1));
}

fn overlay() -> &'static Mutex<Option<Overlay>> {
    static CELL: OnceLock<Mutex<Option<Overlay>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

// 覆盖层只在主线程创建与绘制；放进 `static Mutex` 需要一个 `Send` 标记。
// 真正的跨线程只发生在一句 `PostMessage` 上，窗口句柄本身是可安全传递的。
unsafe impl Send for Overlay {}

struct Overlay {
    hwnd: HWND,
    /// 画布按需分配：没划过手势的用户不付这块虚拟桌面大小 DIB 的内存。
    canvas: Option<Canvas>,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
    dpi: u32,
    visible: bool,
}

/// 注册窗口类（宿主启动时调用一次，必须在主线程）。
pub fn register() {
    zx_ui::window::register_class(CLASS, wndproc, None);
}

/// 建覆盖层窗口（隐藏状态）。失败只记日志，不影响手势识别。
pub fn create() -> bool {
    if HWND_ATOM.load(Ordering::SeqCst) != 0 {
        return true;
    }
    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1);
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1);

    let Some(hwnd) = create_window(
        CLASS,
        "驻匣 · 手势",
        WS_POPUP,
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        vw,
        vh,
    ) else {
        zx_core::log::warn("手势覆盖层创建失败");
        return false;
    };

    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            vx,
            vy,
            vw,
            vh,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_HIDE);
    }

    let dpi = zx_ui::window::dpi_of(hwnd);
    *overlay().lock().unwrap_or_else(|p| p.into_inner()) = Some(Overlay {
        hwnd,
        canvas: None,
        vx,
        vy,
        vw,
        vh,
        dpi,
        visible: false,
    });
    HWND_ATOM.store(hwnd.0 as isize, Ordering::SeqCst);
    zx_core::log::info(format!(
        "手势覆盖层已创建：虚拟桌面 {vw}x{vh} @ ({vx},{vy})，DPI {dpi}（画布首次手势时分配）"
    ));
    true
}

pub fn hwnd() -> Option<HWND> {
    let h = HWND_ATOM.load(Ordering::SeqCst);
    if h == 0 { None } else { Some(HWND(h as *mut core::ffi::c_void)) }
}

/// 请求一次重绘：已有待处理的就跳过，等 UI 线程消化完再说。
fn request_redraw() {
    if REDRAW_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    if !post(WM_TRAIL) {
        // 投递失败（窗口还没建好/已被销毁）必须把闸门退回去：
        // 否则 REDRAW_PENDING 永远为 true，之后所有重绘都被跳过，轨迹再也不更新。
        REDRAW_PENDING.store(false, Ordering::SeqCst);
    }
}

fn post(msg: u32) -> bool {
    match hwnd() {
        Some(hwnd) => unsafe { PostMessageW(hwnd, msg, WPARAM(0), LPARAM(0)).is_ok() },
        None => false,
    }
}

/// 更新轨迹（每次鼠标移动都会来一次）。
pub fn show_trail(points: &[(i32, i32)], matched: bool) {
    {
        let mut cell = trail_cell().lock().unwrap_or_else(|p| p.into_inner());
        cell.0.clear();
        cell.0.extend_from_slice(points);
        cell.1 = matched;
    }
    request_redraw();
}

/// 显示动作名（会盖掉上一条）。
pub fn show_hint(name: &str) {
    let mut cell = hint_cell().lock().unwrap_or_else(|p| p.into_inner());
    if cell.as_deref() == Some(name) {
        return;
    }
    *cell = Some(name.to_string());
    drop(cell);
    request_redraw();
}

/// 收掉提示但保留轨迹：画到一半从"命中"变成"没命中"时用。
pub fn clear_hint() {
    let mut cell = hint_cell().lock().unwrap_or_else(|p| p.into_inner());
    if cell.is_none() {
        return;
    }
    *cell = None;
    drop(cell);
    request_redraw();
}

/// 手势结束：保留轨迹与提示，`duration_ms` 后由定时器清掉。
///
/// 定时器**必须无条件设**：命中规则但用户关了提示、或者压根没命中规则时，
/// 只有轨迹没有 hint，早先"仅在有 hint 时设定时器"会让轨迹永久留在屏幕上。
pub fn settle(duration_ms: u32) {
    let Some(hwnd) = hwnd() else { return };
    if is_empty() {
        // 不要在这里直接 ShowWindow：调用方是解析线程，跨线程 ShowWindow 会同步等 UI 线程，
        // 而 UI 线程可能正在 redraw 里等画布锁 —— 两边互等就是死锁，表现是"手势卡住"。
        // 交给 UI 线程：空状态的 redraw 自己会隐藏窗口。
        request_redraw();
        return;
    }
    unsafe {
        SetTimer(hwnd, TIMER_HINT, duration_ms.max(100), None);
    }
}

pub fn clear() {
    trail_cell().lock().unwrap_or_else(|p| p.into_inner()).0.clear();
    *hint_cell().lock().unwrap_or_else(|p| p.into_inner()) = None;
    if let Some(hwnd) = hwnd() {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_HINT);
        }
    }
    REDRAW_PENDING.store(false, Ordering::SeqCst);
    post(WM_CLEAR);
}

/// 收掉轨迹层并等它**真的**从屏幕上消失，给截图前的抓屏用。
///
/// 轨迹画在一个盖住整个虚拟桌面的 `WS_EX_LAYERED` 窗口上，`BitBlt` 抓的是合成后的
/// 屏幕，不收掉就会把手势路径一起截进图里（用户实测就是这么踩到的）。
/// `clear()` 只投消息，真正的 `SW_HIDE` 发生在监听线程，所以这里轮询 `IsWindowVisible`
/// ——纯查询、不跨线程同步调用，不会撞上 redraw 里那把画布锁。
///
/// 返回 `false` 表示超时（此时宁可带着轨迹也继续截，不能把截图卡住）。
pub fn hide_blocking(timeout_ms: u32) -> bool {
    clear();
    let Some(hwnd) = hwnd() else { return true };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    while std::time::Instant::now() < deadline {
        if unsafe { !windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool() } {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    zx_core::log::warn(format!("手势轨迹层 {timeout_ms}ms 内没收起来，这次截图可能带上轨迹"));
    false
}

/// 闲置回收：轨迹层不可见且超过 `idle_ms` 没上过屏，就让覆盖层线程把 DIB 放掉。
///
/// 只投消息，不在调用线程 drop（GDI+ 对象必须留在创建它的线程）。
/// 下次手势进入 `redraw()` 时 `canvas.is_none()` 会重新分配，路径本来就有。
pub fn release_canvas_if_idle(idle_ms: u64) {
    let Some(hwnd) = hwnd() else { return };
    let tick = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let last = LAST_VISIBLE_MS.load(Ordering::Relaxed);
    if last == 0 || tick.saturating_sub(last) < idle_ms {
        return;
    }
    let should = {
        let guard = overlay().lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().map(|o| !o.visible && o.canvas.is_some()).unwrap_or(false)
    };
    if should {
        unsafe {
            let _ = PostMessageW(hwnd, WM_RELEASE, WPARAM(0), LPARAM(0));
        }
    }
}

fn has_hint() -> bool {
    hint_cell().lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

fn is_empty() -> bool {
    let trail_empty = trail_cell()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .0
        .is_empty();
    trail_empty && !has_hint()
}


unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // panic 屏障：回调里 panic 会穿过 extern "system" 边界直接终止进程
    zx_ui::guard_proc!(DefWindowProcW(hwnd, msg, wparam, lparam), { wndproc_inner(hwnd, msg, wparam, lparam) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {

    match msg {
        WM_TRAIL | WM_HINT => {
            REDRAW_PENDING.store(false, Ordering::SeqCst);
            if crate::hook::trace_enabled() {
                static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n.is_multiple_of(20) {
                    crate::hook::trace_log(format!("收到重绘消息 #{n}"));
                }
            }
            redraw();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HINT => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_HINT);
            }
            *hint_cell().lock().unwrap_or_else(|p| p.into_inner()) = None;
            trail_cell().lock().unwrap_or_else(|p| p.into_inner()).0.clear();
            redraw();
            LRESULT(0)
        }
        WM_CLEAR => {
            redraw();
            LRESULT(0)
        }
        WM_RELEASE => {
            // 必须在本线程释放：Canvas 里是 GDI+ 对象，跨线程 drop 不安全。
            let mut guard = overlay().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(o) = guard.as_mut() {
                if !o.visible && o.canvas.take().is_some() {
                    zx_core::log::info("手势轨迹画布已闲置释放（下次划手势按需重建）");
                }
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // 分辨率/显示器拓扑变了：重建画布（分层窗口不能原地改尺寸）。
            resize_to_virtual_screen();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn resize_to_virtual_screen() {
    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1);
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1);
    let mut guard = overlay().lock().unwrap_or_else(|p| p.into_inner());
    let Some(o) = guard.as_mut() else { return };
    if (o.vx, o.vy, o.vw, o.vh) == (vx, vy, vw, vh) {
        return;
    }
    o.vx = vx;
    o.vy = vy;
    o.vw = vw;
    o.vh = vh;
    if let Some(canvas) = o.canvas.as_mut() {
        canvas.resize(vw, vh);
    }
    unsafe {
        let _ = SetWindowPos(
            o.hwnd,
            HWND_TOPMOST,
            vx,
            vy,
            vw,
            vh,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
    zx_core::log::info(format!("手势覆盖层跟随桌面变化：{vw}x{vh} @ ({vx},{vy})"));
}

fn redraw() {
    let started = std::time::Instant::now();
    let (points, matched) = {
        let cell = trail_cell().lock().unwrap_or_else(|p| p.into_inner());
        (cell.0.clone(), cell.1)
    };
    let hint = hint_cell().lock().unwrap_or_else(|p| p.into_inner()).clone();

    let mut guard = overlay().lock().unwrap_or_else(|p| p.into_inner());
    let Some(o) = guard.as_mut() else { return };

    if points.len() < 2 && hint.is_none() {
        if o.visible {
            unsafe {
                let _ = ShowWindow(o.hwnd, SW_HIDE);
            }
            o.visible = false;
        }
        return;
    }

    if !o.visible {
        if crate::hook::trace_enabled() {
            crate::hook::trace_log(format!(
                "覆盖层首次上屏：{} 点，hwnd={:?}，画布={:?}，尺寸 {}x{}",
                points.len(),
                o.hwnd,
                o.canvas.as_ref().map(|c| (c.w, c.h)),
                o.vw,
                o.vh
            ));
        }
        unsafe {
            // 重新插到 topmost 的最上层：晚启动的置顶窗口会长期压住覆盖层。
            let _ = SetWindowPos(
                o.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            let _ = ShowWindow(o.hwnd, SW_SHOWNOACTIVATE);
        }
        o.visible = true;
        LAST_VISIBLE_MS.store(
            unsafe { windows::Win32::System::SystemInformation::GetTickCount64() },
            Ordering::Relaxed,
        );
    }

    let (hwnd, vx, vy, vw, vh, dpi) = (o.hwnd, o.vx, o.vy, o.vw, o.vh, o.dpi);
    if o.canvas.is_none() {
        o.canvas = Some(Canvas::new(vw, vh, dpi));
    }
    let Some(canvas) = o.canvas.as_mut() else { return };
    if crate::hook::trace_enabled() && canvas.w <= 1 {
        crate::hook::trace_log(format!("画布退化成 {}x{}，画不出轨迹", canvas.w, canvas.h));
    }

    let (active_color, inactive_color, thickness) =
        *style().lock().unwrap_or_else(|p| p.into_inner());
    let scale = dpi as f32 / 96.0;
    canvas.clear(0);

    if points.len() >= 2 {
        let local: Vec<(f32, f32)> = points
            .iter()
            .map(|(x, y)| ((*x - vx) as f32, (*y - vy) as f32))
            .collect();
        let color = if matched { active_color } else { inactive_color };
        let width = (thickness.max(1) as f32) * scale;
        // 先描一圈半透明深色再画本色：浅灰线在白色窗口上几乎看不见（默认就是这样），
        // 加个描边后在任何背景上都读得出来，也不必把默认色改成刺眼的亮色。
        canvas.polyline(&local, width + 2.0 * scale, 0x6600_0000);
        canvas.polyline(&local, width, color);
    }

    if let Some(name) = hint.as_deref() {
        // 锚点是轨迹末端（= 手指画到的地方），不是 GetCursorPos：
        // 提权进程、远程桌面、指针被驱动接管时 GetCursorPos 可能不跟手，提示就会飘到别处。
        let anchor = points.last().copied().map(|(x, y)| POINT { x, y }).unwrap_or_else(|| {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            p
        });
        draw_hint(canvas, vx, vy, vw, vh, name, scale, anchor);
    }

    canvas.present(hwnd, vx, vy);

    // 每帧一次全桌面 UpdateLayeredWindow，慢的时候必须看得见：超过 25ms 记一条（1 秒最多一条）。
    let elapsed = started.elapsed();
    if crate::hook::trace_enabled() && elapsed.as_millis() >= 25 {
        static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tick = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
        let prev = LAST.swap(tick, Ordering::Relaxed);
        if tick.saturating_sub(prev) > 1000 {
            crate::hook::trace_log(format!(
                "重绘耗时 {}ms（{} 点）",
                elapsed.as_millis(),
                points.len()
            ));
        }
    }
}

/// 画布里有几个非透明像素（自检用：证明 GDI+ 真的画进去了，而不是只提交了一张空图）。
fn painted_pixels(o: &Overlay) -> usize {
    o.canvas
        .as_ref()
        .map(|c| c.pixels().chunks(4).filter(|p| p[3] != 0).count())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn draw_hint(
    canvas: &mut Canvas,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
    name: &str,
    scale: f32,
    anchor: POINT,
) {
    // 提示挂在锚点所在显示器上（多屏时不能固定用主屏）。
    let mon = unsafe { MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let monitor = unsafe {
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            info.rcMonitor
        } else {
            RECT { left: 0, top: 0, right: vw, bottom: vh }
        }
    };

    let px = 14.0 * scale;
    let text_w = canvas.measure(name, px, false);
    let pad_x = 18.0 * scale;
    let h = 34.0 * scale;
    let w = text_w + pad_x * 2.0;
    let gap = 24.0 * scale;

    // 贴着光标放（默认右下）：画手势时视线本来就在指针上，固定钉在屏幕底部等于看不见。
    // 超出显示器就翻到另一侧，保证既不挡指针也不跑出屏幕。
    let mut x = anchor.x as f32 + gap;
    let mut y = anchor.y as f32 + gap * 0.75;
    if x + w > monitor.right as f32 {
        x = anchor.x as f32 - gap - w;
    }
    if y + h > monitor.bottom as f32 {
        y = anchor.y as f32 - gap * 0.75 - h;
    }
    x = x.max(monitor.left as f32 + 6.0 * scale);
    y = y.max(monitor.top as f32 + 6.0 * scale);

    let lx = x - vx as f32;
    let ly = y - vy as f32;

    canvas.fill_round(lx, ly, w, h, h / 2.0, 0xE6202024);
    canvas.text(
        name,
        lx,
        ly,
        w,
        h,
        px,
        0xFFF2_F2F2,
        TextAlign::Center,
        VAlign::Center,
    );
}

/// 无鼠标自检：画一条 →↓ 的折线并显示提示，1.2 秒后清掉。
/// 供 `zhuxia.exe --gesture-selftest` 验证分层窗口链路（不装钩子、不改配置）。
///
/// 返回值要求"画布上真的有非透明像素"：只看 `UpdateLayeredWindow` 的返回值不够，
/// 空画布同样能提交成功（截图的透明窗口踩过这个坑）。
pub fn selftest() -> bool {
    if !create() {
        return false;
    }
    let (vx, vy, vw, vh) = {
        let guard = overlay().lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(o) => (o.vx, o.vy, o.vw, o.vh),
            None => return false,
        }
    };

    let mid_y = vy + vh * 55 / 100;
    let start_x = vx + vw * 30 / 100;
    let mut points = Vec::new();
    for i in 0..=24 {
        points.push((start_x + i * vw / 120, mid_y));
    }
    for i in 1..=18 {
        points.push((start_x + vw / 5, mid_y + i * vh / 90));
    }

    show_trail(&points, true);
    show_hint("手势自检：→ ↓");
    settle(1200);

    // 同步画一遍并读回像素：消息循环还没跑，不能指望 wndproc。
    let painted = {
        let mut guard = overlay().lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_mut() {
            Some(o) => {
                let (active_color, _inactive, thickness) =
                    *style().lock().unwrap_or_else(|p| p.into_inner());
                let (hwnd, ox, oy, ow, oh, dpi) = (o.hwnd, o.vx, o.vy, o.vw, o.vh, o.dpi);
                let scale = dpi as f32 / 96.0;
                if o.canvas.is_none() {
                    o.canvas = Some(Canvas::new(ow, oh, dpi));
                }
                let Some(canvas) = o.canvas.as_mut() else { return false };
                let local: Vec<(f32, f32)> = points
                    .iter()
                    .map(|(x, y)| ((*x - ox) as f32, (*y - oy) as f32))
                    .collect();
                canvas.clear(0);
                canvas.polyline(&local, thickness as f32 * scale, active_color);
                let anchor = points.last().copied().map(|(x, y)| POINT { x, y }).unwrap_or_default();
                draw_hint(canvas, ox, oy, ow, oh, "手势自检：→ ↓", scale, anchor);
                canvas.present(hwnd, ox, oy);
                painted_pixels(o)
            }
            None => 0,
        }
    };

    zx_core::log::info(format!("手势自检：轨迹 + 提示已绘制，非透明像素 {painted} 个"));
    painted > 1000
}

/// 覆盖层是否已经建好（设置页/自检读）。
pub fn ready() -> bool {
    HWND_ATOM.load(Ordering::SeqCst) != 0
}
