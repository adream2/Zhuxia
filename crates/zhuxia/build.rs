//! 打包资源：
//! 1) Tauri 的 ACL / 能力清单（tauri-build 生成）
//! 2) 我们自己的 exe 图标与版本信息（SDK 的 rc.exe）
//!
//! 版本号只有一个真源：`Cargo.toml` 的 `[workspace.package] version`。
//! 这里用 `CARGO_PKG_VERSION`（继承自 workspace）替换 `app.rc` 里的占位符，
//! 免得发布时改了 Cargo.toml 却忘了 exe 的 VERSIONINFO，产物和标签对不上。

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
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // 不能 canonicalize：Windows 上它会返回 `\?\E:\...` 这种 verbatim 前缀，rc.exe 不认。
    // 正斜杠照收，也就省掉了字符串里反斜杠被当转义符处理的问题。
    let icon = manifest
        .join("../../assets/icons/zhuxia.ico")
        .to_string_lossy()
        .replace('\\', "/");
    let version = env!("CARGO_PKG_VERSION");

    // 图标路径是相对 app.rc 写的，而生成的 rc 落在 OUT_DIR，所以换成绝对路径
    let src = std::fs::read_to_string(manifest.join("app.rc")).unwrap_or_default();
    let generated = src
        .replace("@ICON@", &icon)
        .replace("@VERSION4@", &format!("{version},0"))
        .replace("@VERSION@", version);
    let rc_file = out_dir.join("zhuxia.rc");
    if std::fs::write(&rc_file, generated).is_err() {
        println!("cargo:warning=写 zhuxia.rc 失败，exe 将不带图标");
        return;
    }

    let out = out_dir.join("zhuxia.res");
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&out)
        .arg(&rc_file)
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
