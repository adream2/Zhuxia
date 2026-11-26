//! 运行时胶水：窗口注册表（采样线程用它通知 UI）、单实例、开关。

use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

use zx_ui::window::WM_SNAPSHOT;

static WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

pub fn register_snapshot_window(hwnd: HWND) {
    let v = hwnd.0 as isize;
    if let Ok(mut g) = WINDOWS.lock() {
        if !g.contains(&v) { g.push(v); }
    }
}

pub fn unregister_snapshot_window(hwnd: HWND) {
    let v = hwnd.0 as isize;
    if let Ok(mut g) = WINDOWS.lock() { g.retain(|x| *x != v); }
}

/// 采样线程：一 tick 一次，给每个窗口投一条 WM_SNAPSHOT（不阻塞、不等待）。
pub fn notify_snapshot() {
    let list = match WINDOWS.lock() { Ok(g) => g.clone(), Err(_) => return };
    for h in list {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(HWND(h as *mut _), WM_SNAPSHOT, WPARAM(0), LPARAM(0));
        }
    }
}

/// 把「设备清单 + 能力标志」写进 `runtime.json`（设置面板在另一个进程里读它）。
pub fn write_cache(snap: &crate::snapshot::Snapshot) {
    let cfg = zx_core::cfg::cfg();
    let disks: Vec<_> = snap.disks.iter().map(|d| serde_json::json!({
        "index": d.index, "name": d.name, "letters": d.letters,
        "kind": zx_core::fmt::disk_kind(d.kind),
    })).collect();
    let gpus: Vec<_> = snap.gpus.iter().map(|g| serde_json::json!({
        "index": g.index, "name": g.name,
    })).collect();
    let adapters: Vec<_> = crate::sampling::net::enumerate().into_iter().map(|a| serde_json::json!({
        "id": a.id, "name": a.name,
    })).collect();
    // 显示器拓扑：副屏任务的覆盖层要按这个列表挂，设置页也要拿它做多选
    let displays: Vec<_> = zx_core::monitor::displays().iter().map(|d| serde_json::json!({
        "id": d.id, "name": d.name, "device": d.device_name,
        "primary": d.primary, "taskbar": d.has_taskbar(),
    })).collect();
    let th = crate::sampling::thermal::latest();
    // 流量统计：设置面板的"流量"页直接读这里（面板是独立进程，自己没有采样）
    let flows: Vec<_> = crate::netstat::top_with(60).iter().map(|f| serde_json::json!({
        "name": f.name,
        "path": f.path,
        "up": f.up,
        "down": f.down,
        "blocked": f.blocked,
    })).collect();
    let (net_up, net_down) = crate::netstat::totals();
    let v = serde_json::json!({
        "disks": disks, "gpus": gpus, "adapters": adapters, "displays": displays,
        "cpuTemp": { "valid": th.valid, "source": th.source },
        "net": {
            "flows": flows,
            "up": net_up,
            "down": net_down,
            "count": crate::netstat::flow_count(),
            "blocked": crate::netstat::blocked_count(),
        },
        "admin": crate::win_admin(),
        "etw": crate::etw_ok(),
        "srum": crate::srum_ok(),
        "floating": cfg.floating(),
        "interval": cfg.sample_interval_ms(),
    });
    let path = zx_core::cfg::run_dir().join("runtime.json");
    let _ = std::fs::write(path, serde_json::to_string(&v).unwrap_or_default());
}

/// 通知所有窗口重新读取配置（设置面板进程改完 settings.yaml 后调用）。
pub fn broadcast_reload() {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            windows::Win32::Foundation::HWND(0xffff as *mut _),
            zx_ui::window::WM_APP_RELOAD,
            WPARAM(0), LPARAM(0));
    }
}

/// 让常驻进程退出（设置面板的“退出驻匣”按钮）。
pub fn broadcast_quit() {
    request_quit();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            windows::Win32::Foundation::HWND(0xffff as *mut _),
            zx_ui::window::WM_APP_QUIT, WPARAM(0), LPARAM(0));
    }
}

// ---- 单实例 ----
static MUT_EX_HANDLE: Mutex<Option<isize>> = Mutex::new(None);
// 互斥体的名字就是「单实例」的身份：换名字等于换锁，旧实例不会被识别。
const MUTEX_NAME: &str = "Global\\Zhuxia.Module.Sysmon.7f3a";

/// 常驻进程是否还活着（只查询、不持有）。
///
/// 独立进程的设置面板用它做"主人走了我就走"的判断——面板是另一个进程，
/// 常驻被托盘菜单退出时不会通知它。
pub fn instance_alive() -> bool {
    // SYNCHRONIZE 在 windows-rs 里是 SYNCHRONIZATION_ACCESS_RIGHTS 的常量（值 0x100000）
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_ACCESS_RIGHTS};
    unsafe {
        let name = zx_core::win::wide(MUTEX_NAME);
        match OpenMutexW(SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000), false,
                         windows::core::PCWSTR(name.as_ptr())) {
            Ok(h) => {
                let _ = windows::Win32::Foundation::CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// 返回 true = 本进程是唯一实例；false = 已有实例在跑。
/// 拿单实例互斥。
///
/// **提权重启后必须等**：父进程刚 `ShellExecuteExW` 就退出，但它持有的互斥句柄要等
/// 进程真正结束才释放。子进程（提权后的自己）如果恰好在那个窗口期醒来，
/// `CreateMutexW` 返回 `ERROR_ALREADY_EXISTS`，然后弹一句"已在运行"就退——
/// 用户看到的是"点了没反应、什么都没起来"。
///
/// 但**普通重复启动不该等**：用户双击第二次，应该立刻看到"已在运行"。
/// 所以只在 `after_elevate`（命令行带 `--after-elevate`，由
/// [`zx_core::win::relaunch_elevated`] 自动加上）时拉长重试窗口。
pub fn acquire_single_instance(after_elevate: bool) -> bool {
    // 提权：8 × 200ms = 1.6s，够父进程退干净；普通启动：3 × 120ms，肉眼几乎无感
    let (tries, gap) = if after_elevate { (8usize, 200u64) } else { (3, 120) };
    for i in 0..tries {
        if try_acquire_single_instance() {
            return true;
        }
        if i + 1 < tries {
            std::thread::sleep(std::time::Duration::from_millis(gap));
        }
    }
    false
}

fn try_acquire_single_instance() -> bool {
    unsafe {
        let name = zx_core::win::wide(MUTEX_NAME);
        match windows::Win32::System::Threading::CreateMutexW(None, false, windows::core::PCWSTR(name.as_ptr())) {
            Ok(h) => {
                let already = windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
                if already {
                    // 不是自己的，句柄留着没用（也别覆盖已存的那个）
                    let _ = windows::Win32::Foundation::CloseHandle(h);
                    return false;
                }
                *MUT_EX_HANDLE.lock().unwrap_or_else(|p| p.into_inner()) = Some(h.0 as isize);
                true
            }
            Err(_) => true,
        }
    }
}

/// 哨兵文件：跨权限级别可靠（广播消息会被 UIPI 拦，文件 I/O 不会）。
pub fn request_open_settings() {
    let p = zx_core::cfg::run_dir().join("open-panel.flag");
    let _ = std::fs::write(p, b"1");
}

/// 请求常驻进程退出。
///
/// 广播是快路径，但跨权限级别（设置面板未提权、常驻进程提权）会被 UIPI 拦掉，
/// 所以同时写一个哨兵文件，常驻侧的 1 秒轮询兜底——两条路都通才叫"能退出"。
pub fn request_quit() {
    let p = zx_core::cfg::run_dir().join("quit.flag");
    let _ = std::fs::write(p, b"1");
}

/// 启动时清掉上一次留下的退出哨兵。
///
/// `--quit` 在没有实例运行时也会写文件；不清的话，下一次启动 1 秒后就被自己误杀。
pub fn clear_quit_flag() {
    let p = zx_core::cfg::run_dir().join("quit.flag");
    if p.exists() { let _ = std::fs::remove_file(&p); }
}

pub fn take_quit_request() -> bool {
    let p = zx_core::cfg::run_dir().join("quit.flag");
    if p.exists() { let _ = std::fs::remove_file(&p); return true; }
    false
}

/// 从后台线程请求 UI 线程退出。
///
/// 不能用 `PostQuitMessage`——它只作用于调用线程自己的消息队列；也不能依赖
/// `overlay::MAP`（thread_local，后台线程看不到）。所以按类名找窗，逐个投递。
pub fn post_quit_to_ui() {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let cls = zx_core::win::wide(crate::overlay::CLASS);
        if let Ok(h) = FindWindowW(windows::core::PCWSTR(cls.as_ptr()), windows::core::PCWSTR::null()) {
            let _ = PostMessageW(h, zx_ui::window::WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// 常驻侧轮询：有人要求打开面板就消费掉这个哨兵。
pub fn take_open_settings_request() -> bool {
    let p = zx_core::cfg::run_dir().join("open-panel.flag");
    if p.exists() { let _ = std::fs::remove_file(&p); return true; }
    false
}

/// 通知正在运行的实例打开设置面板（同权限级别下的快路径）。
pub fn signal_open_settings() {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            windows::Win32::Foundation::HWND(0xffff as *mut _),
            zx_ui::window::WM_APP_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
    }
}

/// 通知正在运行的实例：把它的窗口叫到前面来。
pub fn signal_running_instance() {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let cls = zx_core::win::wide("ZhuxiaTaskbarOverlay");
        if let Ok(h) = FindWindowW(windows::core::PCWSTR(cls.as_ptr()), windows::core::PCWSTR::null()) {
            let _ = PostMessageW(h, WM_SNAPSHOT + 100, WPARAM(0), LPARAM(0));
        }
    }
}
