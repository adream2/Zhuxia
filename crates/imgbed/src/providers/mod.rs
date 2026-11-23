//! 四个平台后端。每个文件只实现自己的协议差异；共同行为（命名、转换、历史）
//! 都在上层，不在这里重复。

pub mod custom;
pub mod github;
pub mod s3;
pub mod webdav;

pub use github::Github;
