//! 收件口：Explorer 右键菜单把任务投给常驻进程（帧格式见 `zx_core::pipe`）。
//!
//! `.app` 之类只能走命令行的目标也走这条路：多选时每个文件一支短命进程，
//! 投完就退出，提取在常驻进程里排队。

use std::path::PathBuf;

use zx_core::pipe::{self, Job};

use crate::output::Mode;

const PIPE_NAME: &str = r"\\.\pipe\zhuxia-appicon";

/// 一条任务 =（输出形态, 文件路径）。
///
/// 用 newtype 而不是直接给 `(Mode, PathBuf)` 实现：元组不是本 crate 的类型，
/// 孤儿规则不允许。对外仍然是 `(Mode, PathBuf)`。
pub struct Conv(pub Mode, pub PathBuf);

impl Job for Conv {
    fn encode(&self) -> String {
        pipe::encode_pair(self.0.id(), &self.1)
    }

    fn decode(rec: &str) -> Option<Self> {
        let (id, path) = pipe::decode_pair(rec)?;
        Some(Conv(Mode::parse(&id)?, path))
    }
}

/// 客户端：把一批任务交给常驻进程。
pub fn send(jobs: &[(Mode, PathBuf)]) -> bool {
    let jobs: Vec<Conv> = jobs.iter().map(|(m, p)| Conv(*m, p.clone())).collect();
    pipe::send(PIPE_NAME, &jobs)
}

/// 服务端：常驻进程里跑一条线程，收到任务就交给 `on_job`。
pub fn serve(on_job: impl Fn(Mode, PathBuf) + Send + 'static) {
    pipe::serve(PIPE_NAME, "zx-appicon-pipe", move |c: Conv| on_job(c.0, c.1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip() {
        let job = Conv(Mode::Ico, PathBuf::from(r"D:\安装包\微信 Setup.exe"));
        let back = <Conv as Job>::decode(&job.encode()).expect("应能解回");
        assert_eq!(back.0, job.0);
        assert_eq!(back.1, job.1);
    }

    #[test]
    fn bad_records_are_skipped() {
        assert!(<Conv as Job>::decode("").is_none());
        assert!(<Conv as Job>::decode("nope\u{1f}C:\\a.exe").is_none());
        assert!(<Conv as Job>::decode("png\u{1f}").is_none());
    }
}
