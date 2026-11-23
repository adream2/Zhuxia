//! 图片格式转换（第 5 个模块）：资源管理器右键 → 子菜单选格式 → 就地转好。
//!
//! 与参考项目 Filee 的对应关系（实现细节见 `docs/CONVERT.md`）：
//!
//! | Filee | 这里 |
//! |---|---|
//! | Magick.NET（ImageMagick）读所有格式 | GDI+ 读常见格式（CMYK/ICC 靠系统），WebP 走 libwebp |
//! | `ImageEncoder.ApplyOptions`（AutoOrient → Resize → Depth 8 → 拍平 alpha） | `decode::load` + `encode::write` 同序 |
//! | `MaxSide`（JPEG 65500 / WebP 16383） | `fmt::Target::max_side` |
//! | `OutputPathResolver`（永不复写源文件、`名字 (2).ext`） | `convert::plan_output` |
//! | `error.*` 错误码 | `error::ConvertError`（带中文可执行提示） |
//! | 任务队列 + 单文件结果 | `queue`：2 工人 + 350ms 汇总一张卡片 |
//!
//! 模块边界沿用宿主约定：只暴露 `start()` 和命令行入口，不 `use` 其它模块。

pub mod convert;
pub mod decode;
pub mod encode;
pub mod error;
pub mod fmt;
pub mod pipe;
pub mod queue;
pub mod shell;
pub mod toast;

use std::path::PathBuf;

pub use convert::{convert, Options, Outcome};
pub use error::ConvertError;
pub use fmt::Target;

/// 宿主启动时调用（`modules::install`）。只登记，不改宿主。
pub fn start() {
    zx_ui::painter::init_gdiplus();

    // 右键菜单跟着配置走：启用但没装 → 装；关掉了但还装着 → 卸。
    let c = zx_core::cfg::cfg().imgconv();
    if c.enabled {
        // 装过但指向别的 exe（dev 构建换成发布版、或整个目录搬了家）也要重写，
        // 否则菜单会一直调用一个不存在的路径。
        let current = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let stale = shell::registered_exe()
            .map(|old| !old.eq_ignore_ascii_case(&current))
            .unwrap_or(true);
        if !shell::installed() || stale {
            match shell::install() {
                Ok(n) => zx_core::log::info(format!("图片转换：已注册右键菜单（{n} 个扩展名）")),
                Err(e) => zx_core::log::warn(format!("图片转换：注册右键菜单失败：{e}")),
            }
        }
    } else if shell::installed() {
        let _ = shell::remove();
        zx_core::log::info("图片转换：已按配置移除右键菜单");
    }

    queue::start();
    pipe::serve(queue::submit);
    zx_core::log::info("图片转换模块已就绪（右键菜单 + 管道队列）");
}

/// 装/卸右键菜单（设置页与命令行共用）。
pub fn install_menu() -> Result<usize, String> {
    shell::install()
}

pub fn remove_menu() -> Result<(), String> {
    shell::remove()
}

pub fn menu_installed() -> bool {
    shell::installed()
}

/// 自检用：把 `files` 转成 `target`，返回每条结果。不弹卡片、不走管道。
pub fn convert_batch(files: &[PathBuf], target: Target) -> Vec<(PathBuf, Result<Outcome, ConvertError>)> {
    zx_ui::painter::init_gdiplus();
    let opts = queue::options();
    files
        .iter()
        .map(|p| (p.clone(), convert::convert(p, target, &opts)))
        .collect()
}

/// 命令行入口。返回 `Some(exit_code)` 表示这条命令被本模块接管，宿主应当直接退出。
///
/// 为什么放在提权闸门之前：Explorer 起的是普通权限进程，不该为了转一张图弹 UAC。
pub fn handle_cli(args: &[String]) -> Option<i32> {
    if let Some(i) = args.iter().position(|a| a == "--img-convert") {
        return Some(cli_convert(args, i));
    }
    if let Some(i) = args.iter().position(|a| a == "--imgconv-menu") {
        let action = args.get(i + 1).map(|s| s.as_str()).unwrap_or("status");
        return Some(cli_menu(action));
    }
    None
}

fn cli_convert(args: &[String], i: usize) -> i32 {
    let Some(fmt) = args.get(i + 1).and_then(|s| Target::parse(s)) else {
        notify("图片转换", &["命令行缺少目标格式（png / jpg / webp / gif / bmp / tiff）".to_string()], true);
        return 2;
    };
    let files: Vec<PathBuf> = args[i + 2..]
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if files.is_empty() {
        notify("图片转换", &["没有收到文件路径".to_string()], true);
        return 2;
    }

    // 常驻进程在跑：交给它排队，本进程立刻退出（不弹窗、不占内存）。
    let jobs: Vec<(Target, PathBuf)> = files.iter().map(|p| (fmt, p.clone())).collect();
    if pipe::send(&jobs) {
        zx_core::log::info(format!("图片转换：已交给常驻进程（{} 张 → {}）", files.len(), fmt.label()));
        return 0;
    }

    // 没在跑：本进程自己转，转完弹一张卡片（阻塞到卡片消失，进程才退出）。
    zx_ui::painter::init_gdiplus();
    let opts = queue::options();
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut lines: Vec<String> = Vec::new();
    for p in &files {
        match convert::convert(p, fmt, &opts) {
            Ok(o) => {
                ok += 1;
                if lines.len() < 3 {
                    let name = o
                        .output
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    lines.push(format!("{name} · {} ms", o.ms));
                }
            }
            Err(e) => {
                failed += 1;
                if lines.len() < 4 {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.display().to_string());
                    lines.push(format!("{name}：{}", e.reason()));
                }
            }
        }
    }
    if quiet() {
        return if failed == 0 { 0 } else { 1 };
    }
    if failed == 0 {
        toast::show_blocking(&format!("已转换 {ok} 张图片"), &lines, false);
        0
    } else if ok == 0 {
        toast::show_blocking(&format!("转换失败（{failed} 张）"), &lines, true);
        1
    } else {
        lines.push(format!("成功 {ok} 张 · 失败 {failed} 张"));
        toast::show_blocking("转换完成（部分失败）", &lines, true);
        1
    }
}

fn cli_menu(action: &str) -> i32 {
    match action {
        "install" => match shell::install() {
            Ok(n) => {
                notify("图片转换", &[format!("右键菜单已安装（{n} 个图片扩展名）")], false);
                0
            }
            Err(e) => {
                notify("图片转换", &[format!("安装失败：{e}")], true);
                1
            }
        },
        "remove" => match shell::remove() {
            Ok(()) => {
                notify("图片转换", &["右键菜单已移除".to_string()], false);
                0
            }
            Err(e) => {
                notify("图片转换", &[format!("移除失败：{e}")], true);
                1
            }
        },
        _ => {
            let on = shell::installed();
            notify(
                "图片转换",
                &[if on { "右键菜单：已安装".into() } else { "右键菜单：未安装".into() }],
                false,
            );
            if on { 0 } else { 1 }
        }
    }
}

/// 命令行场景的提示：`ZHUXIA_IMGCONV_QUIET=1` 时完全静默（给脚本/测试用）。
fn notify(title: &str, lines: &[String], error: bool) {
    if quiet() {
        return;
    }
    toast::show_blocking(title, lines, error);
}

pub(crate) fn quiet() -> bool {
    std::env::var("ZHUXIA_IMGCONV_QUIET").ok().as_deref() == Some("1")
}
