//! 截图模块的热键线程。
//!
//! 这个线程同时是**选区和贴图的 UI 线程**：`RegisterHotKey` 绑定线程，`WM_HOTKEY`
//! 就在这里被派发；选区窗口、文本输入窗、贴图窗也都建在这个线程上，消息循环天然共用。
//! 于是整个模块只有一条消息循环、一个窗口过程族，不需要像剪贴板那样再做一层
//! "dispatcher 窗口 + PostMessage 转发"。
//!
//! 热键解析在 `zx_core::hotkey`（与剪贴板共用同一份语法）。

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, SetTimer, TranslateMessage, MSG, WM_HOTKEY,
    WM_TIMER,
};

use crate::overlay;

const ID_CAPTURE: i32 = 1;
/// 录屏入口。固定 `Ctrl+Shift+R`，刻意**不用** `Ctrl+R`——那个键在覆盖层里表示
/// "把已框好的选区拿去录"，全局也占着会让同一次按键触发两遍。
const ID_RECORD: i32 = 2;
/// 配置的热键被占用时依次尝试（微信默认把 Alt+A 拿去做截屏，冲突很常见）。
const FALLBACKS: [&str; 3] = ["Ctrl+Alt+A", "Ctrl+Shift+A", "Alt+Shift+A"];
const WM_RELOAD_HOTKEY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 61;
const WM_TRIGGER: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 62;
const WM_TRIGGER_RECORD: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 63;

/// 线程定时器：定期检查 OCR 引擎是不是闲置了（`msg.hwnd` 为空的那种）。
///
/// 必须在**这个线程**上查：WinML 会话不是 `Send`，引擎缓存在本线程的
/// `thread_local` 里，别的线程释放不掉它。
const TIMER_OCR_IDLE: usize = 0x20;
const OCR_IDLE_TICK_MS: u32 = 15_000;

static TID: AtomicU32 = AtomicU32::new(0);

pub fn start() {
    // 线程 id 必须**在 start() 返回前**就绪：`--capture` 是紧跟着 `start()` 调
    // `trigger()` 的，晚一步就会因为 TID==0 而静默丢掉这次触发（之前就是这样）。
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    std::thread::Builder::new()
        .name("zx-capture-ui".into())
        .spawn(move || unsafe {
            let tid = windows::Win32::System::Threading::GetCurrentThreadId();
            TID.store(tid, Ordering::SeqCst);
            register_all();
            // 线程定时器（hwnd 传空）：只用来周期性放掉闲置的 OCR 引擎。
            let _ = SetTimer(HWND(std::ptr::null_mut()), TIMER_OCR_IDLE, OCR_IDLE_TICK_MS, None);
            let _ = tx.send(tid);
            let mut msg = MSG::default();
            loop {
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == -1 || r.0 == 0 {
                    break;
                }
                match msg.message {
                    WM_HOTKEY => match msg.wParam.0 as i32 {
                        ID_CAPTURE => {
                            overlay::begin();
                        }
                        ID_RECORD => {
                            // 录制中再按 = 停止（toggle），免得又弹一层覆盖层
                            if crate::record::is_active() {
                                crate::record::stop();
                            } else {
                                overlay::begin_with(overlay::Mode::Record);
                            }
                        }
                        _ => {}
                    },
                    WM_RELOAD_HOTKEY => register_all(),
                    // 线程定时器（没有窗口）：窗口定时器走 `DispatchMessageW`，这里只管自己的。
                    WM_TIMER if msg.hwnd.0.is_null() => {
                        crate::ppocr::unload_if_idle();
                    }
                    WM_TRIGGER_RECORD => {
                        overlay::begin_with(overlay::Mode::Record);
                    }
                    WM_TRIGGER => {
                        overlay::begin();
                    }
                    _ => {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        })
        .ok();
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(_) => {}
        Err(_) => zx_core::log::warn("截图 UI 线程 3 秒内没起来"),
    }
}

/// 从任意线程触发一次截图（`--capture` 命令行 / 托盘入口）。
pub fn trigger() {
    let tid = TID.load(Ordering::SeqCst);
    if tid == 0 {
        zx_core::log::warn("截图热键线程还没起来");
        return;
    }
    unsafe {
        let _ = PostThreadMessageW(tid, WM_TRIGGER, WPARAM(0), LPARAM(0));
    }
}

/// 从任意线程触发"框选即录"（`--record` / 托盘菜单）。
///
/// **必须投递到热键线程**：`overlay` 的状态是 thread_local，录制结束时 `record::run`
/// 会在调用线程上阻塞跑控制条消息循环。若从托盘/主线程直接调 `begin_with()`，
/// 阻塞的就是任务栏那条 UI 线程——录制期间整个任务栏会卡死。
pub fn trigger_record() {
    let tid = TID.load(Ordering::SeqCst);
    if tid == 0 {
        zx_core::log::warn("录屏热键线程还没起来");
        return;
    }
    unsafe {
        let _ = PostThreadMessageW(tid, WM_TRIGGER_RECORD, WPARAM(0), LPARAM(0));
    }
}

/// 配置里的热键变了：让热键线程整体重注册。
pub fn reload() {
    let tid = TID.load(Ordering::SeqCst);
    if tid == 0 {
        return;
    }
    unsafe {
        let _ = PostThreadMessageW(tid, WM_RELOAD_HOTKEY, WPARAM(0), LPARAM(0));
    }
}

/// 把"当前实际生效的热键 / 期望的热键 / 失败原因"写到 exe 目录，
/// 设置页（独立进程）读它来显示状态——进程间的内存不共享，这是最省事的通道。
fn publish(active: &str, wanted: &str, error: &str) {
    let path = zx_core::cfg::run_dir().join("capture-hotkey.txt");
    let text = format!("active={active}\nwanted={wanted}\nerror={error}\n");
    let _ = std::fs::write(path, text);
}

fn register_all() {
    unsafe {
        let _ = UnregisterHotKey(None, ID_CAPTURE);
        let _ = UnregisterHotKey(None, ID_RECORD);
        // 录屏入口：Ctrl+Shift+R。模块禁用时不要再注册——否则用户按下去会看到
        // 选区、框完却什么都没录（设置页写的是"关闭后热键不工作"）。
        if crate::cfg().record.enabled {
            if RegisterHotKey(None, ID_RECORD, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, b'R' as u32).is_ok() {
                zx_core::log::info("录屏热键已注册：Ctrl+Shift+R");
            } else {
                zx_core::log::warn("录屏热键 Ctrl+Shift+R 注册失败（被别的程序占用？）");
            }
        } else {
            zx_core::log::info("录屏已禁用：Ctrl+Shift+R 不注册");
        }
        let cfg = crate::cfg();
        let wanted = cfg.hotkey.trim().to_string();
        let mut tried: Vec<String> = Vec::new();
        let mut last_err = String::new();

        for cand in std::iter::once(wanted.clone())
            .chain(FALLBACKS.iter().map(|s| s.to_string()))
        {
            let cand = cand.trim().to_string();
            if cand.is_empty() || tried.contains(&cand) {
                continue;
            }
            tried.push(cand.clone());
            let Some((mods, vk)) = zx_core::hotkey::parse(&cand) else {
                last_err = format!("{cand} 格式不合法");
                continue;
            };
            match RegisterHotKey(None, ID_CAPTURE, mods | MOD_NOREPEAT, vk as u32) {
                Ok(()) => {
                    if cand == wanted {
                        zx_core::log::info(format!("截图热键已注册：{cand}"));
                        publish(&cand, &wanted, "");
                    } else {
                        // 不能静默换键：用户按原热键没反应会以为功能坏了
                        let why = format!("{wanted} 已被别的程序占用");
                        zx_core::log::warn(format!("{why}，已临时改用 {cand}（可在设置里改）"));
                        publish(&cand, &wanted, &why);
                    }
                    return;
                }
                Err(e) => {
                    last_err = format!("{cand} 注册失败（{e}）");
                }
            }
        }
        zx_core::log::warn(format!(
            "截图热键全部注册失败（最后一个：{last_err}），请在设置里换一个组合键"
        ));
        publish("", &wanted, &last_err);
    }
}
