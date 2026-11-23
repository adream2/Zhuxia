//! 显示器 / 任务栏拓扑。
//!
//! 一个显示器最多对应一个任务栏：主屏是 `Shell_TrayWnd`，其余每个开着任务栏的副屏各有一个
//! `Shell_SecondaryTrayWnd`（类名相同，所以只能枚举窗口再按显示器归位）。
//!
//! 显示器标识用 **设备接口名**（`\\?\DISPLAY#GSM5B7D#5&…&0&UID4352#{e6f07b5f-…}`），
//! 它跨重启稳定，适合写进配置；`\\.\DISPLAY1` 这种序号会随插拔漂移，只用于本次查询。

use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, DISPLAY_DEVICEW,
    HDC, HMONITOR, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, EDD_GET_DEVICE_INTERFACE_NAME,
    MONITORINFOF_PRIMARY,
};

#[derive(Clone, Debug, Default)]
pub struct DisplayInfo {
    /// 设备接口名，跨重启稳定 —— 写进 settings.yaml 的就是它。
    pub id: String,
    /// 友好名（EDID 里的 13 字节型号名，没有就退回系统名字）。
    pub name: String,
    /// `\\.\DISPLAY1`，仅本次会话内有效。
    pub device_name: String,
    pub primary: bool,
    /// HMONITOR
    pub monitor: isize,
    /// 该显示器上的任务栏窗口；副屏没开任务栏时是 0。
    pub taskbar: isize,
    pub bounds: (i32, i32, i32, i32),
}

impl DisplayInfo {
    pub fn has_taskbar(&self) -> bool { self.taskbar != 0 }
}

/// 枚举全部显示器（含各自的任务栏），主屏排第一，其余按 x 坐标排序。
pub fn displays() -> Vec<DisplayInfo> {
    let mut acc = Acc2 { taskbars: find_taskbars(), out: Vec::new() };
    unsafe {
        let p = &mut acc as *mut Acc2 as isize;
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(enum_monitor), LPARAM(p));
    }
    let mut out = acc.out;
    out.sort_by_key(|d| (!d.primary, d.bounds.0, d.bounds.1));
    disambiguate(&mut out);
    out
}

unsafe extern "system" fn enum_monitor(h: HMONITOR, _hdc: HDC, _rc: *mut RECT, data: LPARAM) -> BOOL {
    let acc = &mut *(data.0 as *mut Acc2);
    let (taskbars, out) = (&acc.taskbars, &mut acc.out);

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(h, &mut info as *mut _ as *mut _).as_bool() { return BOOL(1); }

    let device_name = utf16z(&info.szDevice);
    let (id, driver) = display_device(&device_name);
    let rc = info.monitorInfo.rcMonitor;
    out.push(DisplayInfo {
        name: edid_name(&id).or_else(|| non_blank(&driver)).unwrap_or_else(|| fallback_name(&device_name)),
        id: if id.is_empty() { device_name.clone() } else { id },
        device_name,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        monitor: h.0 as isize,
        taskbar: taskbars.get(&(h.0 as isize)).copied().unwrap_or(0),
        bounds: (rc.left, rc.top, rc.right, rc.bottom),
    });
    BOOL(1)
}

struct Acc2 { taskbars: HashMap<isize, isize>, out: Vec<DisplayInfo> }

/// 显示器 → 任务栏窗口。
///
/// 主任务栏按类名直接找；副任务栏只能枚举（`FindWindowW` 只能拿到第一个），
/// 然后靠 `MonitorFromWindow` 归到所属显示器。同一显示器只记第一次出现的那个。
fn find_taskbars() -> HashMap<isize, isize> {
    let mut map: HashMap<isize, isize> = HashMap::new();
    unsafe {
        let primary = FindWindowW(windows::core::w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default();
        add_taskbar(&mut map, primary.0 as isize);

        let mut found: Vec<isize> = Vec::new();
        let list_ptr = &mut found as *mut Vec<isize> as isize;
        let _ = EnumWindows(Some(enum_secondary), LPARAM(list_ptr));
        for h in found { add_taskbar(&mut map, h); }
    }
    map
}

fn add_taskbar(map: &mut HashMap<isize, isize>, hwnd: isize) {
    if hwnd == 0 { return; }
    unsafe {
        let m = MonitorFromWindow(windows::Win32::Foundation::HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST);
        let key = m.0 as isize;
        if key != 0 { map.entry(key).or_insert(hwnd); }
    }
}

unsafe extern "system" fn enum_secondary(hwnd: windows::Win32::Foundation::HWND, data: LPARAM) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<isize>);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    if n > 0 && String::from_utf16_lossy(&buf[..n as usize]).eq_ignore_ascii_case("Shell_SecondaryTrayWnd") {
        list.push(hwnd.0 as isize);
    }
    BOOL(1)
}

/// `\\.\DISPLAY1` → (设备接口名, 适配器描述)。
fn display_device(device_name: &str) -> (String, String) {
    let mut dev = DISPLAY_DEVICEW {
        cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let wide = crate::win::wide(device_name);
    unsafe {
        if !EnumDisplayDevicesW(PCWSTR(wide.as_ptr()), 0, &mut dev, EDD_GET_DEVICE_INTERFACE_NAME).as_bool() {
            return (String::new(), String::new());
        }
    }
    (utf16z(&dev.DeviceID), utf16z(&dev.DeviceString))
}

/// 从 EDID 里取显示器型号名。
///
/// EDID 基础块的 54..126 是四个 18 字节描述块，tag `0xFC` 的那块装着 13 字节型号名。
/// 好处是"GSM5B7D"这种型号码能变成"LG ULTRAGEAR"——和系统设置里显示的一致。
fn edid_name(device_id: &str) -> Option<String> {
    if device_id.is_empty() { return None; }
    // 型号名不会变：缓存一份，避免托盘每 3 秒自检时反复读注册表
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Option<String>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(v) = map.get(device_id) { return v.clone(); }
    }
    let val = edid_name_uncached(device_id);
    if let Ok(mut map) = cache.lock() { map.insert(device_id.to_string(), val.clone()); }
    val
}

fn edid_name_uncached(device_id: &str) -> Option<String> {
    let parts: Vec<&str> = device_id.split('#').collect();
    if parts.len() < 3 { return None; }
    if !parts[0].eq_ignore_ascii_case(r"\\?\DISPLAY") { return None; }
    let path = format!(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{}\{}\Device Parameters", parts[1], parts[2]);
    let edid = crate::win::reg_bytes(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, &path, "EDID")?;
    let mut off = 54usize;
    while off + 17 < edid.len() && off < 126 {
        if edid[off] == 0 && edid[off + 1] == 0 && edid[off + 2] == 0 && edid[off + 3] == 0xFC {
            let raw: Vec<u8> = edid[off + 5..off + 18].iter().copied()
                .take_while(|b| *b != 0 && *b != b'\r' && *b != b'\n').collect();
            return non_blank(&String::from_utf8_lossy(&raw));
        }
        off += 18;
    }
    None
}

fn disambiguate(v: &mut [DisplayInfo]) {
    for i in 0..v.len() {
        let name = v[i].name.clone();
        let dup = v.iter().filter(|d| d.name == name).count() > 1;
        if dup {
            let dev = v[i].device_name.rsplit('Y').next().unwrap_or("").to_string();
            v[i].name = format!("{name}（显示器 {dev}）");
        }
    }
}

fn fallback_name(device_name: &str) -> String {
    match device_name.rsplit_once("DISPLAY") {
        Some((_, n)) if !n.is_empty() => format!("显示器 {n}"),
        _ => "未知显示器".to_string(),
    }
}

fn non_blank(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

fn utf16z(buf: &[u16]) -> String {
    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}
