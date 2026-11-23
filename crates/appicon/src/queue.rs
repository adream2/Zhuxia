//! 批量队列：2 条工人线程 + 1 条汇总线程（350ms 合并成一张卡片）。
//!
//! 线程与合并窗口的机制在 `zx_core::batch`，这里只描述"做什么"和"卡片上写什么"。

use std::path::{Path, PathBuf};

use crate::error::IconError;
use crate::output::{self, Mode, Options, Outcome};
use crate::toast;

type Job = (Mode, PathBuf);
type Done = (PathBuf, Result<Outcome, IconError>);

static QUEUE: zx_core::batch::Queue<Job> = zx_core::batch::Queue::new();

pub fn submit(mode: Mode, path: PathBuf) {
    QUEUE.submit((mode, path));
}

pub fn start() {
    QUEUE.start(2, "zx-appicon", run_one, summarise);
}

fn run_one(job: Job) -> Done {
    let (mode, path) = job;
    let opts = options(mode);
    let res = run(&path, &opts);
    match &res {
        Ok(o) => zx_core::log::info(format!(
            "图标提取：{} → {}（{} 帧，最大 {}×{}，{} ms）",
            o.source.display(),
            o.outputs.first().map(|p| p.display().to_string()).unwrap_or_default(),
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
    (path, res)
}

fn run(path: &Path, opts: &Options) -> Result<Outcome, IconError> {
    let set = crate::probe::extract(path)?;
    output::write_set(path, &set, opts)
}

fn summarise(batch: Vec<Done>) {
    if crate::quiet() {
        return;
    }
    let ok: Vec<&Outcome> = batch.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
    let bad: Vec<(&PathBuf, &IconError)> = batch
        .iter()
        .filter_map(|(p, r)| r.as_ref().err().map(|e| (p, e)))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if bad.is_empty() {
        for o in ok.iter().take(zx_core::batch::MAX_LINES) {
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
        if ok.len() > zx_core::batch::MAX_LINES {
            lines.push(format!("……另有 {} 个", ok.len() - zx_core::batch::MAX_LINES));
        }
        toast::show(&format!("已提取 {} 个图标", ok.len()), &lines, false);
        return;
    }

    for (p, e) in bad.iter().take(zx_core::batch::MAX_LINES) {
        lines.push(format!("{}：{}", file_name(p), e.reason()));
    }
    if bad.len() > zx_core::batch::MAX_LINES {
        lines.push(format!("……另有 {} 个失败", bad.len() - zx_core::batch::MAX_LINES));
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

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
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
