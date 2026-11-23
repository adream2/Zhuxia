//! 批量队列：常驻进程里 2 条工人线程 + 1 条汇总线程。
//!
//! 为什么要队列而不是"收到即转"：Explorer 多选 20 张会连开 20 个进程、几乎同时写管道，
//! 如果每条都单独弹提示，屏幕上会刷 20 张卡片。这里合并成一条：「已转换 20 张」。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::convert::{self, Options, Outcome};
use crate::error::ConvertError;
use crate::fmt::Target;
use crate::toast;

type Job = (Target, PathBuf);
type Done = (PathBuf, Result<Outcome, ConvertError>);

static JOB_TX: std::sync::OnceLock<mpsc::Sender<Job>> = std::sync::OnceLock::new();

/// 投一条任务（管道线程调）。队列没起来就直接忽略。
pub fn submit(target: Target, path: PathBuf) {
    if let Some(tx) = JOB_TX.get() {
        let _ = tx.send((target, path));
    }
}

/// 起工人与汇总线程。只调一次。
pub fn start() {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    let _ = JOB_TX.set(job_tx);

    let rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    for i in 0..2 {
        let rx = rx.clone();
        let done_tx = done_tx.clone();
        std::thread::Builder::new()
            .name(format!("zx-imgconv-{i}"))
            .spawn(move || loop {
                let job = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    guard.recv()
                };
                let Ok((target, path)) = job else { return };
                let opts = options();
                let res = convert::convert(&path, target, &opts);
                match &res {
                    Ok(o) => zx_core::log::info(format!(
                        "图片转换：{} → {}（{}×{}，{} → {} B，{} ms）",
                        o.source.display(),
                        o.output.display(),
                        o.w,
                        o.h,
                        o.src_bytes,
                        o.out_bytes,
                        o.ms
                    )),
                    Err(e) => zx_core::log::warn(format!(
                        "图片转换失败：{}（{}：{:?}）",
                        path.display(),
                        e.reason(),
                        e.detail()
                    )),
                }
                let _ = done_tx.send((path, res));
            })
            .ok();
    }
    drop(done_tx);

    std::thread::Builder::new()
        .name("zx-imgconv-summary".into())
        .spawn(move || summarise(done_rx))
        .ok();
}

/// 汇总线程：安静 350ms 或攒够 60 条就出一张卡片。
fn summarise(rx: Receiver<Done>) {
    loop {
        let first = match rx.recv() {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut batch = vec![first];
        while batch.len() < 60 {
            match rx.recv_timeout(Duration::from_millis(350)) {
                Ok(v) => batch.push(v),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if crate::quiet() {
            continue;
        }
        show_summary(&batch);
    }
}

fn show_summary(batch: &[Done]) {
    let ok: Vec<&Outcome> = batch.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
    let bad: Vec<(&PathBuf, &ConvertError)> = batch
        .iter()
        .filter_map(|(p, r)| r.as_ref().err().map(|e| (p, e)))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if bad.is_empty() {
        // 成功：最多列 3 条，其余合并
        for o in ok.iter().take(3) {
            let name = o
                .output
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let size = human_size(o.out_bytes);
            lines.push(match &o.notice {
                Some(n) => format!("{name} · {size} · {n}"),
                None => format!("{name} · {size} · {} ms", o.ms),
            });
        }
        if ok.len() > 3 {
            lines.push(format!("……另有 {} 张", ok.len() - 3));
        }
        toast::show(&format!("已转换 {} 张图片", ok.len()), &lines, false);
        return;
    }

    // 有失败：先列失败（最多 3 条），再报成功数
    for (p, e) in bad.iter().take(3) {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.display().to_string());
        lines.push(format!("{name}：{}", e.reason()));
    }
    if bad.len() > 3 {
        lines.push(format!("……另有 {} 张失败", bad.len() - 3));
    }
    if !ok.is_empty() {
        lines.push(format!("成功 {} 张，失败 {} 张", ok.len(), bad.len()));
    }
    let title = if ok.is_empty() {
        format!("转换失败（{} 张）", bad.len())
    } else {
        format!("转换完成：{} 成功 · {} 失败", ok.len(), bad.len())
    };
    toast::show(&title, &lines, ok.is_empty());
}

fn human_size(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}

/// 从配置取当前策略（每次现读：设置页改完立刻生效）。
pub fn options() -> Options {
    let c = zx_core::cfg::cfg().imgconv();
    Options {
        quality: c.quality,
        webp_lossless: c.webp_lossless,
        background: [255, 255, 255],
        overwrite: c.overwrite,
        out_dir: c.out_dir,
        ico_sizes: c.ico_sizes,
    }
}
