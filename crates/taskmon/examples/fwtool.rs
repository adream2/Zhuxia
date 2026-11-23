//! 防火墙规则诊断工具：验证 `netstat::set_blocked` 真的能加上/删掉出站规则。
//!
//! 需要管理员权限运行（`netsh advfirewall` 本身就要）。
//!
//! 用法：
//!   fwtool block "C:\path\to\app.exe"
//!   fwtool unblock "C:\path\to\app.exe"

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        println!("用法: fwtool <block|unblock> <exe 路径>");
        return;
    }
    let cmd = args[1].as_str();
    let path = &args[2];
    // rule-* 走"只下规则"的路径（设置面板用的那个），不会动 netstat.json
    let (block, rule_only) = match cmd {
        "block" => (true, false),
        "unblock" => (false, false),
        "rule-block" => (true, true),
        "rule-unblock" => (false, true),
        _ => { println!("未知命令 {cmd}"); return; }
    };
    println!("管理员 = {}", taskmon::win_admin());
    let ok = if rule_only {
        taskmon::netstat::set_blocked_rule(path, block)
    } else {
        taskmon::netstat::set_blocked(path, block)
    };
    println!("set_blocked({path}, {block}) -> {ok}");
    println!("is_blocked -> {}", taskmon::netstat::is_blocked(path));
}
