//! 批量队列：常驻进程里 2 条工人线程 + 1 条汇总线程。
//!
//! 线程与合并窗口的机制在 `zx_core::batch`，这里只描述"做什么"和"卡片上写什么"
//! （两个模块的文案本来就不同，没必要为统一而统一）。

use std::path::{Path, PathBuf};

use crate::convert::{self, Options, Outcome};
use crate::error::ConvertError;
use crate::fmt::Target;
use crate::toast;

type Job = (Target, PathBuf);
type Done = (PathBuf, Result<Outcome, ConvertError>);

static QUEUE: zx_core::batch::Queue<Job> = zx_core::batch::Queue::new();

/// 投一条任务（管道线程调）。队列没起来就直接忽略。
pub fn submit(target: Target, path: PathBuf) {
    QUEUE.submit((target, path));
}

/// 起工人与汇总线程。只调一次。
pub fn start() {
    QUEUE.start(2, "zx-imgconv", run_one, summarise);
}

fn run_one(job: Job) -> Done {
    let (target, path) = job;
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
    (path, res)
}

fn summarise(batch: Vec<Done>) {
    if crate::quiet() {
        return;
    }
    let ok: Vec<&Outcome> = batch.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
    let bad: Vec<(&PathBuf, &ConvertError)> = batch
        .iter()
        .filter_map(|(p, r)| r.as_ref().err().map(|e| (p, e)))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if bad.is_empty() {
        // 成功：最多列 3 条，其余合并
        for o in ok.iter().take(zx_core::batch::MAX_LINES) {
            let name = file_name(&o.output);
            let size = zx_core::fmt::bytes(o.out_bytes as i64);
            lines.push(match &o.notice {
                Some(n) => format!("{name} · {size} · {n}"),
                None => format!("{name} · {size} · {} ms", o.ms),
            });
        }
        if ok.len() > zx_core::batch::MAX_LINES {
            lines.push(format!("……另有 {} 张", ok.len() - zx_core::batch::MAX_LINES));
        }
        toast::show(&format!("已转换 {} 张图片", ok.len()), &lines, false);
        return;
    }

    // 有失败：先列失败（最多 3 条），再报成功数
    for (p, e) in bad.iter().take(zx_core::batch::MAX_LINES) {
        lines.push(format!("{}：{}", file_name(p), e.reason()));
    }
    if bad.len() > zx_core::batch::MAX_LINES {
        lines.push(format!("……另有 {} 张失败", bad.len() - zx_core::batch::MAX_LINES));
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

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
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
