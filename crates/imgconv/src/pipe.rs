//! 收件口：Explorer 右键菜单把任务投给常驻进程（帧格式见 `zx_core::pipe`）。
//!
//! 短命进程只负责投递，真正的转换在常驻进程里排队做——批量 20 张就只花 20 次进程启动，
//! 而不是 20 个 GDI+ 实例各自冷启动。

use std::path::PathBuf;

use zx_core::pipe::{self, Job};

use crate::fmt::Target;

const PIPE_NAME: &str = r"\\.\pipe\zhuxia-imgconv";

/// 一条任务 =（目标格式, 文件路径）。
///
/// 用 newtype 而不是直接给 `(Target, PathBuf)` 实现：元组不是本 crate 的类型，
/// 孤儿规则不允许。对外仍然是 `(Target, PathBuf)`，转换只发生在这一层。
pub struct Conv(pub Target, pub PathBuf);

impl Job for Conv {
    fn encode(&self) -> String {
        pipe::encode_pair(self.0.id(), &self.1)
    }

    fn decode(rec: &str) -> Option<Self> {
        let (id, path) = pipe::decode_pair(rec)?;
        Some(Conv(Target::parse(&id)?, path))
    }
}

/// 客户端：把一批任务交给常驻进程。成功 = 对方收下了。
pub fn send(jobs: &[(Target, PathBuf)]) -> bool {
    let jobs: Vec<Conv> = jobs.iter().map(|(t, p)| Conv(*t, p.clone())).collect();
    pipe::send(PIPE_NAME, &jobs)
}

/// 服务端：常驻进程里跑一条线程，收到任务就交给 `on_job`。
pub fn serve(on_job: impl Fn(Target, PathBuf) + Send + 'static) {
    pipe::serve(PIPE_NAME, "zx-imgconv-pipe", move |c: Conv| on_job(c.0, c.1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip() {
        let job = Conv(Target::WebP, PathBuf::from(r"C:\图片 目录\a b.png"));
        let rec = job.encode();
        let back = <Conv as Job>::decode(&rec).expect("应能解回");
        assert_eq!(back.0, job.0);
        assert_eq!(back.1, job.1);
        // 路径里带空格与中文都不能破坏协议
        assert!(rec.contains('\u{1f}'));
    }

    #[test]
    fn bad_records_are_skipped() {
        assert!(<Conv as Job>::decode("").is_none());
        assert!(<Conv as Job>::decode("png").is_none());
        assert!(<Conv as Job>::decode("nope\u{1f}C:\\a.png").is_none());
        assert!(<Conv as Job>::decode("png\u{1f}").is_none());
    }
}
