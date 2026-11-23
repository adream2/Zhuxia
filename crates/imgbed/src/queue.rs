//! 快捷上传队列（截图工具条 / 剪贴板热键）。界面里的上传页不走这里——
//! 它有自己的逐张状态，直接调命令。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use crate::clip;
use crate::toast;
use crate::ItemResult;

pub enum Job {
    Bytes { name: Option<String>, bytes: Vec<u8> },
    Files(Vec<PathBuf>),
    Clipboard,
}

struct Work {
    job: Job,
    quiet: bool,
}

static TX: OnceLock<Sender<Work>> = OnceLock::new();

pub fn start() {
    if TX.get().is_some() {
        return;
    }
    let (tx, rx) = channel::<Work>();
    if TX.set(tx).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("zx-imgbed".into())
        .spawn(move || {
            while let Ok(work) = rx.recv() {
                let results = run(work.job);
                if !work.quiet {
                    report(&results);
                }
            }
        })
        .ok();
}

pub fn submit(job: Job) {
    submit_inner(Work { job, quiet: false });
}

pub fn submit_quiet(job: Job) {
    submit_inner(Work { job, quiet: true });
}

fn submit_inner(w: Work) {
    start();
    let failed = match TX.get() {
        Some(tx) => match tx.send(w) {
            Ok(()) => return,
            Err(e) => e.0,
        },
        None => return,
    };
    std::thread::spawn(move || {
        let results = run(failed.job);
        if !failed.quiet {
            report(&results);
        }
    });
}

pub fn run(job: Job) -> Vec<ItemResult> {
    match job {
        Job::Clipboard => match clip::read_image_png() {
            Some(bytes) => crate::upload_bytes("剪贴板图片", bytes, None, "clipboard"),
            None => vec![ItemResult::err("clipboard", "剪贴板图片", "剪贴板里没有图片，先复制一张")],
        },
        Job::Bytes { name, bytes } => {
            let name = name.unwrap_or_else(|| "截图".into());
            crate::upload_bytes(&name, bytes, None, "screenshot")
        }
        Job::Files(paths) => crate::upload_files(&paths, None, "file"),
    }
}

fn report(results: &[ItemResult]) {
    let ok: Vec<&ItemResult> = results.iter().filter(|r| r.ok).collect();
    let failed: Vec<&ItemResult> = results.iter().filter(|r| !r.ok).collect();
    if ok.is_empty() {
        let msg = failed.first().map(|r| r.error.clone()).unwrap_or_else(|| "没有结果".into());
        zx_core::log::warn(format!("图床上传失败：{msg}"));
        toast::show("上传失败", &[toast::ellipsis(&msg, 90)], true);
        return;
    }
    let cfg = crate::cfg();
    let text = ok.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n");
    let copied = if cfg.auto_copy { clip::set_text(&text) } else { false };
    zx_core::log::info(format!(
        "图床上传成功 {} 张（失败 {}），平台 {}，剪贴板={copied}",
        ok.len(),
        failed.len(),
        cfg.provider
    ));
    if !failed.is_empty() {
        let msg = failed[0].error.clone();
        toast::show(
            &format!("成功 {} 张 / 失败 {} 张", ok.len(), failed.len()),
            &[toast::ellipsis(&msg, 90)],
            true,
        );
        return;
    }
    let title = if ok.len() == 1 {
        if cfg.auto_copy && copied {
            "已上传 · 链接已复制"
        } else if cfg.auto_copy {
            "已上传（剪贴板被占用，链接没复制）"
        } else {
            "已上传"
        }
    } else {
        "已全部上传"
    };
    let first = ok[0];
    let lines = vec![
        if ok.len() == 1 {
            format!("{} · {}", first.name, crate::upload::human(first.bytes))
        } else {
            format!("{} 张 · 首张 {}", ok.len(), first.name)
        },
        toast::ellipsis(&first.text, 86),
    ];
    toast::show(title, &lines, false);
}
