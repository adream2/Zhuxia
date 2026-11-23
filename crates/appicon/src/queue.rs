//! 批量队列：2 条工人线程 + 1 条汇总线程（350ms 合并成一张卡片）。
//!
//! 多选 10 个安装包时 Explorer 会起 10 支短命进程，全部投到常驻侧排队；
//! 卡片只出一张，避免屏幕被刷满。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::error::IconError;
use crate::output::{self, Mode, Options, Outcome};
use crate::toast;

type Job = (Mode, PathBuf);
type Done = (PathBuf, Result<Outcome, IconError>);

static JOB_TX: std::sync::OnceLock<mpsc::Sender<Job>> = std::sync::OnceLock::new();

pub fn submit(mode: Mode, path: PathBuf) {
    if let Some(tx) = JOB_TX.get() {
        let _ = tx.send((mode, path));
    }
}

pub fn start() {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    let _ = JOB_TX.set(job_tx);

    let rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    for i in 0..2 {
        let rx = rx.clone();
        let done_tx = done_tx.clone();
        std::thread::Builder::new()
            .name(format!("zx-appicon-{i}"))
            .spawn(move || loop {
                let job = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    guard.recv()
                };
                let Ok((mode, path)) = job else { return };
                let opts = options(mode);
                let res = run(&path, &opts);
                match &res {
                    Ok(o) => zx_core::log::info(format!(
                        "图标提取：{} → {}（{} 帧，最大 {}×{}，{} ms）",
                        o.source.display(),
                        o.outputs
                            .first()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        o.frames,
                        o.largest.0,
                        o.largest.1,
                        o.ms
                    )),
                    Err(e) => zx_core::log::warn(format!(
                        "图标提取失败：{}（{}：{:?}）",
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
        .name("zx-appicon-summary".into())
        .spawn(move || summarise(done_rx))
        .ok();
}

fn run(path: &std::path::Path, opts: &Options) -> Result<Outcome, IconError> {
    let set = crate::probe::extract(path)?;
    output::write_set(path, &set, opts)
}

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
    let bad: Vec<(&PathBuf, &IconError)> = batch
        .iter()
        .filter_map(|(p, r)| r.as_ref().err().map(|e| (p, e)))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if bad.is_empty() {
        for o in ok.iter().take(3) {
            let name = o
                .outputs
                .first()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let spec = if o.largest.0 > 0 {
                format!(" · {}×{} · {} 帧", o.largest.0, o.largest.1, o.frames)
            } else {
                String::new()
            };
            lines.push(format!("{name}{spec}"));
        }
        if ok.len() > 3 {
            lines.push(format!("……另有 {} 个", ok.len() - 3));
        }
        toast::show(&format!("已提取 {} 个图标", ok.len()), &lines, false);
        return;
    }

    for (p, e) in bad.iter().take(3) {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.display().to_string());
        lines.push(format!("{name}：{}", e.reason()));
    }
    if bad.len() > 3 {
        lines.push(format!("……另有 {} 个失败", bad.len() - 3));
    }
    if !ok.is_empty() {
        lines.push(format!("成功 {} 个，失败 {} 个", ok.len(), bad.len()));
    }
    let title = if ok.is_empty() {
        format!("提取失败（{} 个）", bad.len())
    } else {
        format!("提取完成：{} 成功 · {} 失败", ok.len(), bad.len())
    };
    toast::show(&title, &lines, ok.is_empty());
}

/// 每次现读配置：设置页改完立刻生效。
pub fn options(mode: Mode) -> Options {
    let c = zx_core::cfg::cfg().appicon();
    Options {
        mode,
        max_size: c.max_size,
        ico_sizes: c.ico_sizes,
        out_dir: c.out_dir,
        overwrite: c.overwrite,
    }
}
