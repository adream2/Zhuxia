//! 面板命令的登记一致性检查。
//!
//! `#[tauri::command]` 只是声明；必须再进 `run()` 里的 `generate_handler!` 才能被前端调用。
//! 漏登记在编译期与运行期都不报错——前端调用直接 reject，用户看到的是"点了没反应"。
//! 拆分 `panel/` 时最容易漏，所以这里做一次静态对账。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn panel_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/panel")
}

/// 从 `#[tauri::command]` 属性后面取出紧跟的函数名。
fn declared_commands(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(pos) = rest.find("#[tauri::command") {
        let after = &rest[pos..];
        // 跳过属性本身（可能带参数，如 #[tauri::command(rename_all = ...)]）
        let Some(close) = after.find(']') else { break };
        let tail = &after[close + 1..];
        // 属性与 fn 之间可能隔着 pub(crate) / async / 空行
        if let Some(fn_pos) = tail.find("fn ") {
            let name: String = tail[fn_pos + 3..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                out.push(name);
            }
        }
        rest = &after[close + 1..];
    }
    out
}

fn registered_commands(src: &str) -> BTreeSet<String> {
    let start = src.find("generate_handler![").expect("run() 里必须有 generate_handler!");
    let body = &src[start..];
    let end = body.find(']').expect("generate_handler! 没闭合");
    body[..end]
        .split([',', '\n'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.starts_with("generate_handler"))
        .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
        .collect()
}

#[test]
fn every_command_is_registered_once() {
    let mut declared = Vec::new();
    for entry in std::fs::read_dir(panel_dir()).expect("读 panel 目录") {
        let path = entry.expect("目录项").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("读源码");
        for name in declared_commands(&src) {
            declared.push((name, path.file_name().unwrap().to_string_lossy().to_string()));
        }
    }

    let mod_src = std::fs::read_to_string(panel_dir().join("mod.rs")).expect("读 mod.rs");
    let registered = registered_commands(&mod_src);

    let declared_names: BTreeSet<String> = declared.iter().map(|(n, _)| n.clone()).collect();

    let missing: Vec<&String> = declared_names.difference(&registered).collect();
    assert!(
        missing.is_empty(),
        "这些命令声明了但没进 generate_handler!，前端调用会直接失败：{missing:?}"
    );

    let extra: Vec<&String> = registered.difference(&declared_names).collect();
    assert!(
        extra.is_empty(),
        "这些命令登记了但没有 #[tauri::command] 声明：{extra:?}"
    );

    assert_eq!(
        declared.len(),
        registered.len(),
        "登记数 {} 与声明数 {} 不一致（可能重复登记）",
        registered.len(),
        declared.len()
    );
}
