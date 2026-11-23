//! 屏幕边缘滚轮：鼠标停在屏幕某条边上滚轮 → 音量 / 亮度 / 切标签 / 切程序。
//!
//! 为什么放在手势模块里：它和手势共用同一个低级鼠标钩子。钩子看到滚轮事件时先问一句
//! "现在是不是贴在边上"，是就把事件**吞掉**（否则页面会跟着滚），再丢给本模块的
//! 工作线程去执行。钩子里只做一次坐标比较（纯原子读），不查窗口、不做动作。
//!
//! 四条边各自独立开关。**默认只开左、右**（音量 / 亮度）：这两条边是"顺手调一下"
//! 的高频动作；上、下会抢走页面的滚轮，不主动开就不该影响正常滚动。

use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

use zx_core::cfg::{EdgeAction, EdgeSettings};

/// 哪条边。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
            Side::Top => 2,
            Side::Bottom => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Side::Left => "左",
            Side::Right => "右",
            Side::Top => "上",
            Side::Bottom => "下",
        }
    }
}

/// 0 = 不接管，1 = 音量，2 = 亮度，3 = 标签，4 = 程序。
static CODES: [AtomicU8; 4] = [
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
];
static MARGIN: AtomicI32 = AtomicI32::new(2);
static REPEAT_MS: AtomicI32 = AtomicI32::new(45);
static BOUNDS: OnceLock<(i32, i32, i32, i32)> = OnceLock::new();
static TX: OnceLock<Sender<(Side, i32)>> = OnceLock::new();

fn code_of(a: EdgeAction) -> u8 {
    match a {
        EdgeAction::Volume => 1,
        EdgeAction::Brightness => 2,
        EdgeAction::Tab => 3,
        EdgeAction::App => 4,
    }
}

fn action_of(code: u8) -> Option<EdgeAction> {
    match code {
        1 => Some(EdgeAction::Volume),
        2 => Some(EdgeAction::Brightness),
        3 => Some(EdgeAction::Tab),
        4 => Some(EdgeAction::App),
        _ => None,
    }
}

/// 虚拟桌面范围（多屏时是外接矩形）。
pub fn bounds() -> (i32, i32, i32, i32) {
    *BOUNDS.get_or_init(|| unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    })
}

pub fn start() {
    let (tx, rx) = channel::<(Side, i32)>();
    if TX.set(tx).is_err() {
        return;
    }
    apply(&crate::cfg().edges);
    std::thread::Builder::new()
        .name("zx-gesture-edge".into())
        .spawn(move || {
            let mut last = [Instant::now() - Duration::from_secs(1); 4];
            while let Ok((side, delta)) = rx.recv() {
                let gap = REPEAT_MS.load(Ordering::Relaxed) as u64;
                let now = Instant::now();
                if now.duration_since(last[side.index()]) < Duration::from_millis(gap) {
                    continue;
                }
                last[side.index()] = now;
                let code = CODES[side.index()].load(Ordering::Relaxed);
                let Some(action) = action_of(code) else { continue };
                if let Err(err) = run_action(action, delta) {
                    zx_core::log::warn(format!("边缘滚动（{}·{}）失败：{err}", side.name(), action.name()));
                }
            }
        })
        .ok();
}

/// 配置热更新：只改原子变量，钩子下一次滚轮就按新设置走。
pub fn apply(s: &EdgeSettings) {
    let set = |slot: &AtomicU8, v: Option<EdgeAction>| {
        slot.store(v.map(code_of).unwrap_or(0), Ordering::Relaxed);
    };
    set(&CODES[0], s.left);
    set(&CODES[1], s.right);
    set(&CODES[2], s.top);
    set(&CODES[3], s.bottom);
    MARGIN.store(s.margin, Ordering::Relaxed);
    REPEAT_MS.store(s.repeat_ms as i32, Ordering::Relaxed);
}

/// 这个坐标算不算贴在边上（钩子调用，必须极快）。
///
/// 角落同时满足两条边时按"左右优先"处理：竖边贴边更常见（窗口最大化时鼠标
/// 冲出去通常先撞到左右边），而且总得有个确定答案。
pub fn hit(x: i32, y: i32) -> Option<Side> {
    let (vx, vy, vw, vh) = bounds();
    let m = MARGIN.load(Ordering::Relaxed);
    if CODES[0].load(Ordering::Relaxed) != 0 && x <= vx + m {
        return Some(Side::Left);
    }
    if CODES[1].load(Ordering::Relaxed) != 0 && x >= vx + vw - 1 - m {
        return Some(Side::Right);
    }
    if CODES[2].load(Ordering::Relaxed) != 0 && y <= vy + m {
        return Some(Side::Top);
    }
    if CODES[3].load(Ordering::Relaxed) != 0 && y >= vy + vh - 1 - m {
        return Some(Side::Bottom);
    }
    None
}

/// 钩子把滚轮交给这里（已经吞掉了，页面不会再滚）。
pub fn on_wheel(side: Side, delta: i32) {
    let code = CODES[side.index()].load(Ordering::Relaxed);
    crate::hook::trace_log(format!(
        "边缘滚动：{}边 delta={delta} 动作={}",
        side.name(),
        action_of(code).map(|a| a.name()).unwrap_or("无")
    ));
    if let Some(tx) = TX.get() {
        let _ = tx.send((side, delta));
    }
}

/// 执行一次边缘动作。`delta` 只用来定方向（>0 = 加）。
pub fn run_action(action: EdgeAction, delta: i32) -> Result<(), String> {
    let up = delta > 0;
    match action {
        EdgeAction::Volume => crate::action::send_volume(if up {
            crate::model::VolumeOp::Increase
        } else {
            crate::model::VolumeOp::Decrease
        }),
        EdgeAction::Brightness => brightness::step(up),
        EdgeAction::Tab => {
            let keys: &[u16] = if up { &[0x11, 0x09] } else { &[0x11, 0x10, 0x09] }; // Ctrl(+Shift)+Tab
            crate::action::send_hotkey(keys)
        }
        EdgeAction::App => {
            let keys: &[u16] = if up { &[0x12, 0x09] } else { &[0x12, 0x10, 0x09] }; // Alt(+Shift)+Tab
            crate::action::send_hotkey(keys)
        }
    }
}

/// 显示器亮度：DDC/CI（`dxva2.dll`，Twinkle Tray 对外接屏走的就是这条路）。
///
/// 每格 ±5%。只调**光标所在**那台屏；不支持 DDC/CI 的屏（多数笔记本内屏、
/// 部分 HDMI 转接屏）会返回错误，日志里能看到原因——内屏走 WMI，那是另一套栈，
/// 暂时没接。
mod brightness {
    use windows::Win32::Devices::Display::{
        DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
        GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
    };
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    const STEP: f32 = 0.05;

    pub fn step(up: bool) -> Result<(), String> {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let mut count = 0u32;
        let ok = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut count) }.is_ok();
        if !ok || count == 0 {
            return Err("这台显示器没有 DDC/CI 接口".into());
        }
        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        unsafe {
            GetPhysicalMonitorsFromHMONITOR(hmon, &mut monitors).map_err(|e| e.to_string())?;
        }
        let mut result = Err("显示器不支持 DDC/CI 调亮度".to_string());
        for pm in &monitors {
            let (mut min, mut cur, mut max) = (0u32, 0u32, 100u32);
            if unsafe { GetMonitorBrightness(pm.hPhysicalMonitor, &mut min, &mut cur, &mut max) } == 0
                || max <= min
            {
                continue;
            }
            let span = (max - min) as f32;
            let pct = cur.saturating_sub(min) as f32 / span;
            let next_pct = if up { pct + STEP } else { pct - STEP }.clamp(0.0, 1.0);
            let next = min + (span * next_pct).round() as u32;
            if unsafe { SetMonitorBrightness(pm.hPhysicalMonitor, next) } != 0 {
                result = Ok(());
            }
            break;
        }
        unsafe {
            let _ = DestroyPhysicalMonitors(&monitors);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_hit_follows_toggles() {
        let (vx, vy, vw, vh) = bounds();
        apply(&EdgeSettings {
            left: Some(EdgeAction::Volume),
            right: Some(EdgeAction::Brightness),
            top: Some(EdgeAction::Tab),
            bottom: Some(EdgeAction::App),
            margin: 2,
            repeat_ms: 45,
        });
        assert_eq!(hit(vx + 1, vy + vh / 2), Some(Side::Left));
        assert_eq!(hit(vx + vw - 1, vy + vh / 2), Some(Side::Right));
        assert_eq!(hit(vx + vw / 2, vy + 1), Some(Side::Top));
        assert_eq!(hit(vx + vw / 2, vy + vh - 1), Some(Side::Bottom));
        assert_eq!(hit(vx + vw / 2, vy + vh / 2), None);
        // 明确全关时任何位置都不接管（滚轮必须照常滚页面）。
        // 注意不能用 EdgeSettings::default()：默认是"左右开"，那是有意的默认手感。
        apply(&EdgeSettings { left: None, right: None, top: None, bottom: None, margin: 2, repeat_ms: 45 });
        assert_eq!(hit(vx + 1, vy + vh / 2), None);
        assert_eq!(hit(vx + vw - 1, vy + vh / 2), None);
    }

    #[test]
    fn action_codes_round_trip() {
        for a in [EdgeAction::Volume, EdgeAction::Brightness, EdgeAction::Tab, EdgeAction::App] {
            assert_eq!(action_of(code_of(a)), Some(a));
        }
        assert_eq!(action_of(0), None);
    }
}
