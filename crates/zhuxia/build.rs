//! 打包资源：
//! 1) Tauri 的 ACL / 能力清单（tauri-build 生成）
//! 2) 我们自己的 exe 图标与版本信息（SDK 的 rc.exe）

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Tauri 2 需要这一步来生成 capabilities 的权限清单；失败不致命（设置面板会退回原生实现）
    let _ = tauri_build::try_build(tauri_build::Attributes::new());

    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=../../assets/icons/zhuxia.ico");

    let Some(rc) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe，exe 将不带图标资源");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("zhuxia.res");
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&out)
        .arg("app.rc")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status();
    match status {
        Ok(s) if s.success() && out.exists() => {
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
        _ => println!("cargo:warning=rc.exe 编译资源失败，exe 将不带图标"),
    }
}

fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RC") {
        let p = PathBuf::from(p);
        if p.exists() { return Some(p); }
    }
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(kits).ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    for v in versions.iter().rev() {
        let rc = v.join("x64").join("rc.exe");
        if rc.exists() { return Some(rc); }
    }
    None
}
