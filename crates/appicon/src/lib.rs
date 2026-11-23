//! 图标提取（第七模块）：右键 exe / dll / lnk / msi / apk / ipa / dmg / deb / rpm /
//! AppImage 等 → 就地生成 PNG / ICO / 全尺寸图集。
//!
//! 覆盖范围与提取方式见 `docs/APPICON.md`：
//!
//! | 平台 | 格式 | 图标在哪 |
//! |---|---|---|
//! | Windows | exe/dll/ocx/cpl/scr/sys/mui | PE 的 `RT_GROUP_ICON` + `RT_ICON` |
//! | Windows | lnk | StringData 的 `ICON_LOCATION` → 目标程序 |
//! | Windows | msi | OLE 复合文档的 `Icon` 存储 |
//! | Windows | msix/appx | zip + `AppxManifest.xml` 的 `Square*Logo` |
//! | macOS | app / dmg / icns | `Info.plist` → `Resources/*.icns`；DMG 走 UDIF + HFS+ |
//! | iOS | ipa | `Info.plist` 的 `CFBundleIcons` → `AppIcon*.png` |
//! | Android | apk | 二进制 Manifest + `resources.arsc` → `res/mipmap-*` |
//! | Android | aab | `base/res/mipmap-*`（不解析 resources.pb） |
//! | Linux | deb/rpm/AppImage | `.desktop` 的 `Icon=` → hicolor/pixmaps（含 SVG） |
//!
//! 模块边界同其它模块：只暴露 `start()` 与命令行入口，不 `use` 其它业务模块。

pub mod arsc;
pub mod axml;
pub mod error;
pub mod ico;
pub mod icns;
pub mod linux;
pub mod lnk;
pub mod macos;
pub mod menu;
pub mod model;
pub mod msi;
pub mod output;
pub mod pe;
pub mod pipe;
pub mod probe;
pub mod queue;
pub mod toast;
pub mod zipk;

use std::path::PathBuf;

pub use error::IconError;
pub use output::{Mode, Options, Outcome};

/// 宿主启动时调用（`modules::install`）。
pub fn start() {
    zx_ui::painter::init_gdiplus();

    let c = zx_core::cfg::cfg().appicon();
    if c.enabled {
        let current = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let stale = menu::registered_exe()
            .map(|old| !old.eq_ignore_ascii_case(&current))
            .unwrap_or(true);
        if !menu::installed() || stale {
            match menu::install() {
                Ok(n) => zx_core::log::info(format!("图标提取：已注册右键菜单（{n} 个扩展名）")),
                Err(e) => zx_core::log::warn(format!("图标提取：注册右键菜单失败：{e}")),
            }
        }
    } else if menu::installed() {
        let _ = menu::remove();
        zx_core::log::info("图标提取：已按配置移除右键菜单");
    }

    queue::start();
    pipe::serve(queue::submit);
    zx_core::log::info("图标提取模块已就绪（右键菜单 + 管道队列）");
}

pub fn install_menu() -> Result<usize, String> {
    menu::install()
}
pub fn remove_menu() -> Result<(), String> {
    menu::remove()
}
pub fn menu_installed() -> bool {
    menu::installed()
}

/// 自检/脚本用：直接提取，不弹卡片、不走管道。
pub fn extract_batch(files: &[PathBuf], mode: Mode) -> Vec<(PathBuf, Result<Outcome, IconError>)> {
    zx_ui::painter::init_gdiplus();
    let opts = queue::options(mode);
    files
        .iter()
        .map(|p| {
            let res = probe::extract(p).and_then(|set| output::write_set(p, &set, &opts));
            (p.clone(), res)
        })
        .collect()
}

/// 命令行入口。`Some(exit_code)` 表示本模块接管了这条命令。
pub fn handle_cli(args: &[String]) -> Option<i32> {
    if let Some(i) = args.iter().position(|a| a == "--appicon-selftest") {
        return Some(cli_extract(args, i, true));
    }
    if let Some(i) = args.iter().position(|a| a == "--appicon") {
        return Some(cli_extract(args, i, false));
    }
    if let Some(i) = args.iter().position(|a| a == "--appicon-menu") {
        let action = args.get(i + 1).map(|s| s.as_str()).unwrap_or("status");
        return Some(cli_menu(action));
    }
    None
}

fn cli_extract(args: &[String], i: usize, selftest: bool) -> i32 {
    let Some(mode) = args.get(i + 1).and_then(|s| Mode::parse(s)) else {
        notify("图标提取", &["命令行缺少输出形态（png / ico / all）".to_string()], true, selftest);
        return 2;
    };
    let files: Vec<PathBuf> = args[i + 2..]
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if files.is_empty() {
        notify("图标提取", &["没有收到文件路径".to_string()], true, selftest);
        return 2;
    }

    if !selftest {
        let jobs: Vec<(Mode, PathBuf)> = files.iter().map(|p| (mode, p.clone())).collect();
        if pipe::send(&jobs) {
            zx_core::log::info(format!("图标提取：已交给常驻进程（{} 个 → {}）", files.len(), mode.label()));
            return 0;
        }
    }

    zx_ui::painter::init_gdiplus();
    let opts = queue::options(mode);
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut lines: Vec<String> = Vec::new();
    for p in &files {
        match probe::extract(p).and_then(|set| output::write_set(p, &set, &opts)) {
            Ok(o) => {
                ok += 1;
                let out = o
                    .outputs
                    .first()
                    .map(|x| x.display().to_string())
                    .unwrap_or_default();
                let line = format!(
                    "{} → {}（{} 帧，最大 {}×{}，{} ms）",
                    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    out,
                    o.frames,
                    o.largest.0,
                    o.largest.1,
                    o.ms
                );
                if selftest {
                    println!("OK  {line}");
                }
                if lines.len() < 3 {
                    lines.push(format!(
                        "{} · {} 帧 · {}×{}",
                        p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        o.frames,
                        o.largest.0,
                        o.largest.1
                    ));
                }
            }
            Err(e) => {
                failed += 1;
                let line = format!(
                    "{}：{}（{}）",
                    p.display(),
                    e.reason(),
                    e.detail().unwrap_or("无细节")
                );
                if selftest {
                    println!("ERR {line}");
                }
                if lines.len() < 4 {
                    lines.push(format!(
                        "{}：{}",
                        p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        e.reason()
                    ));
                }
            }
        }
    }

    if selftest {
        println!("RESULT ok={ok} failed={failed}");
        return if failed == 0 { 0 } else { 1 };
    }
    if quiet() {
        return if failed == 0 { 0 } else { 1 };
    }
    if failed == 0 {
        toast::show_blocking(&format!("已提取 {ok} 个图标"), &lines, false);
        0
    } else if ok == 0 {
        toast::show_blocking(&format!("提取失败（{failed} 个）"), &lines, true);
        1
    } else {
        lines.push(format!("成功 {ok} 个 · 失败 {failed} 个"));
        toast::show_blocking("提取完成（部分失败）", &lines, true);
        1
    }
}

fn cli_menu(action: &str) -> i32 {
    match action {
        "install" => match menu::install() {
            Ok(n) => {
                notify("图标提取", &[format!("右键菜单已安装（{n} 个扩展名）")], false, false);
                0
            }
            Err(e) => {
                notify("图标提取", &[format!("安装失败：{e}")], true, false);
                1
            }
        },
        "remove" => match menu::remove() {
            Ok(()) => {
                notify("图标提取", &["右键菜单已移除".to_string()], false, false);
                0
            }
            Err(e) => {
                notify("图标提取", &[format!("移除失败：{e}")], true, false);
                1
            }
        },
        _ => {
            let on = menu::installed();
            notify(
                "图标提取",
                &[if on { "右键菜单：已安装".into() } else { "右键菜单：未安装".into() }],
                false,
                false,
            );
            if on { 0 } else { 1 }
        }
    }
}

fn notify(title: &str, lines: &[String], error: bool, selftest: bool) {
    if selftest || quiet() {
        for l in lines {
            println!("{l}");
        }
        return;
    }
    toast::show_blocking(title, lines, error);
}

pub(crate) fn quiet() -> bool {
    std::env::var("ZHUXIA_APPICON_QUIET").ok().as_deref() == Some("1")
}
