//! 批量任务队列：N 条工人线程 + 1 条汇总线程。
//!
//! 为什么要队列而不是"收到即做"：Explorer 多选 20 个文件会连开 20 支短命进程，
//! 几乎同时把活投进来。如果每条都单独弹提示，屏幕上会刷 20 张卡片；排队后合并成
//! 一条「已转换 20 张」。
//!
//! 这里只提供机械结构（线程、channel、350ms/60 条的合并窗口），
//! 做什么任务、卡片上写什么，都由调用方用闭包决定——两个模块的文案本来就不同
//! （「已转换 N 张」/「已提取 N 个」），没必要为统一而统一。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// 合并窗口：安静这么久就出一张卡片。
const QUIET_MS: u64 = 350;
/// 一批最多攒这么多条，防止一口气选几百个文件时卡片等太久。
const MAX_BATCH: usize = 60;

/// 一个通用队列。每个模块自己持有一份 `static`，任务类型由泛型决定。
pub struct Queue<J: Send + 'static> {
    tx: OnceLock<mpsc::Sender<J>>,
}

impl<J: Send + 'static> Queue<J> {
    pub const fn new() -> Self {
        Self { tx: OnceLock::new() }
    }

    /// 投一条任务。队列没起来就直接忽略（调用方本来就有兜底）。
    pub fn submit(&self, job: J) {
        if let Some(tx) = self.tx.get() {
            let _ = tx.send(job);
        }
    }

    /// 起工人与汇总线程。只调一次（重复调用会多出一批空转线程）。
    ///
    /// - `run(job)`：做完一条任务，返回 `(路径, 结果)`；日志由它自己写（各自字段不同）。
    /// - `summarise(batch)`：一批任务安静下来后调用，负责出卡片。
    pub fn start<R, C, S>(&self, workers: usize, prefix: &str, run: C, summarise: S)
    where
        R: Send + 'static,
        C: Fn(J) -> (PathBuf, R) + Send + Sync + 'static,
        S: Fn(Vec<(PathBuf, R)>) + Send + 'static,
    {
        let (job_tx, job_rx) = mpsc::channel::<J>();
        let (done_tx, done_rx) = mpsc::channel::<(PathBuf, R)>();
        if self.tx.set(job_tx).is_err() {
            return; // 已经起过了
        }

        let run = Arc::new(run);
        let rx = Arc::new(std::sync::Mutex::new(job_rx));
        for i in 0..workers {
            let rx = Arc::clone(&rx);
            let done_tx = done_tx.clone();
            let run = Arc::clone(&run);
            std::thread::Builder::new()
                .name(format!("{prefix}-{i}"))
                .spawn(move || loop {
                    let job = {
                        // 锁中毒不当回事：拿回内部值继续，别让一条坏任务带走整队
                        let guard = match rx.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        guard.recv()
                    };
                    let Ok(job) = job else { return };
                    let done = run(job);
                    if done_tx.send(done).is_err() {
                        return; // 汇总线程没了，收工
                    }
                })
                .ok();
        }
        drop(done_tx);

        let _ = std::thread::Builder::new()
            .name(format!("{prefix}-summary"))
            .spawn(move || summarise_loop(done_rx, summarise));
    }
}

impl<J: Send + 'static> Default for Queue<J> {
    fn default() -> Self {
        Self::new()
    }
}

/// 汇总循环：先阻塞等第一条，然后攒 `QUIET_MS` 内到达的（最多 `MAX_BATCH` 条）。
fn summarise_loop<R, S>(rx: Receiver<(PathBuf, R)>, summarise: S)
where
    S: Fn(Vec<(PathBuf, R)>),
{
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match rx.recv_timeout(Duration::from_millis(QUIET_MS)) {
                Ok(v) => batch.push(v),
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        summarise(batch);
    }
}

/// 卡片上最多列几条明细（成功与失败各自用这个上限）。
pub const MAX_LINES: usize = 3;
