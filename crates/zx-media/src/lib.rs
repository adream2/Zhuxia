//! 图像解码 / 缩放 / WebP 编码 / 缩略图。
//!
//! 为什么单独一层：图床要"上传前转 WebP"，图片转换（imgconv）要"右键转格式"，
//! 两边都需要同一套逐像素代码；模块之间不许互相 `use`，所以沉到独立 crate。
//!
//! 只解码 PNG / JPEG / WebP：
//! - 截图与网页图片基本都是这三种；
//! - GIF/动画 WebP 不转（保留动画比"省体积"重要），SVG 不栅格化（矢量原样更小）；
//! - BMP/TIFF 少见到不值得为它拉解码器。
//!
//! 缩放是 Lanczos3（分离两趟，u8 中间缓冲）。截图里的文字对"最近邻/双线性"很敏感，
//! 三次卷积在 2560 长边下仍然干净，代价是纯 Rust 的逐像素循环 —— release 开 opt-level=3。

pub mod image;
pub mod webp;

pub use image::{dimensions, decode, resize, Image, MediaError};
pub use webp::{encode_webp, thumbnail_webp};
