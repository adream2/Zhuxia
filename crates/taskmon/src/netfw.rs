//! 防火墙规则的读取与增删。
//!
//! 添加/删除走 `netsh advfirewall`（一条命令搞定，规则按**程序**匹配，不需要端口）；
//! 读取走 COM 的 `INetFwPolicy2`——`netsh show rule` 每次都起进程、几百毫秒，
//! 没法在界面上反复调。
//!
//! 规则名统一是 `Zhuxia Block <文件名>`，这样用户在 `wf.msc` 里也能一眼认出是哪条。

use windows::core::BSTR;
use windows::Win32::NetworkManagement::WindowsFirewall::{INetFwPolicy2, INetFwRules, NetFwPolicy2};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

pub const RULE_PREFIX: &str = "Zhuxia Block ";

/// 规则名：`Zhuxia Block <文件名>`。
pub fn rule_name(path: &str) -> String {
    let base = std::path::Path::new(path)
        .file_name().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "程序".to_string());
    format!("{RULE_PREFIX}{base}")
}

fn rules() -> Option<INetFwRules> {
    unsafe {
        // 面板进程可能刚起来，套间要先有；重复调用无害
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let policy: INetFwPolicy2 =
            CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).ok()?;
        policy.Rules().ok()
    }
}

/// 从候选路径里筛出"系统里确实有阻止规则"的那些（小写返回）。
///
/// 不遍历全部规则：`INetFwRules` 的枚举要经过 `IEnumVARIANT` + `IDispatch` 两层，
/// 而我们的候选集本来就只有几十条（当前有流量的程序 + 上次记下的封禁），
/// 逐条按名字查既简单又快（每次 <50µs）。
pub fn filter_blocked(candidates: &[String]) -> Vec<String> {
    let Some(rules) = rules() else { return Vec::new() };
    let mut out = Vec::new();
    for p in candidates {
        if p.is_empty() { continue; }
        let name = rule_name(p);
        if unsafe { rules.Item(&BSTR::from(name)) }.is_ok() {
            out.push(p.to_lowercase());
        }
    }
    out
}

// ---------------- 增删 ----------------

fn run_netsh(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new("netsh");
    cmd.args(["advfirewall", "firewall"]).args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    matches!(cmd.status(), Ok(s) if s.success())
}

/// 加/删这条程序的出站阻止规则。返回是否成功。
pub fn apply(path: &str, block: bool) -> bool {
    if path.is_empty() { return false; }
    let rule = rule_name(path);
    if block {
        run_netsh(&["add", "rule", &format!("name={rule}"), "dir=out",
                    &format!("program={path}"), "action=block", "enable=yes", "profile=any"])
    } else {
        run_netsh(&["delete", "rule", &format!("name={rule}")])
    }
}
