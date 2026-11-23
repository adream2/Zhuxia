//! 开机自启：计划任务（`schtasks /XML`），任务本身就是状态——不额外存配置。

use std::process::Command;

const TASK: &str = "Zhuxia";

/// 计划任务是否已安装。查询要走一次 schtasks（约 1–5 秒），所以只在启动/切换时刷新，
/// 绘制路径绝不能直接调用。
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn refresh() {
    let on = query();
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_enabled() -> bool { ENABLED.load(std::sync::atomic::Ordering::Relaxed) }

fn query() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn enable() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let xml = format!(r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{}</Command><WorkingDirectory>{}</WorkingDirectory></Exec>
  </Actions>
</Task>"#,
        exe.display(),
        exe.parent().map(|p| p.display().to_string()).unwrap_or_default());

    let path = std::env::temp_dir().join("zhuxia-task.xml");
    // schtasks 读 XML 要 UTF-16LE + BOM
    let mut bytes = vec![0xFFu8, 0xFE];
    for u in xml.encode_utf16() { bytes.extend_from_slice(&u.to_le_bytes()); }
    if std::fs::write(&path, bytes).is_err() { return false; }
    let ok = Command::new("schtasks")
        .args(["/Create", "/TN", TASK, "/XML", &path.to_string_lossy(), "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&path);
    ENABLED.store(ok, std::sync::atomic::Ordering::Relaxed);
    ok
}

pub fn disable() -> bool {
    let ok = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK, "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
    ok
}
