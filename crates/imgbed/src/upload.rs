//! 上传管线：准备（校验 / 转 WebP）→ 调后端 → 出链接 → 记录。
//!
//! 转换放在"准备"这一步，意味着上传页可以在发送前就告诉用户"420 KB → 168 KB"；
//! 后端拿到的永远是最终字节，不用关心格式。

use zx_core::ImgBedSettings;

use crate::history::{self, Record};
use crate::link;
use crate::path;
use crate::provider::{Backend, Fail};

#[derive(Debug)]
pub struct Prepared {
    pub name: String,
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub content_type: &'static str,
    pub width: u32,
    pub height: u32,
    pub original_bytes: usize,
    /// 转换相关的提示（"1.2 MB → 168 KB（WebP q82）"这类）
    pub note: Option<String>,
}

pub struct Outcome {
    pub record: Record,
    /// 已按设置格式化好的文本（Markdown / HTML / BBCode / URL）
    pub text: String,
    pub note: Option<String>,
}

/// 校验 + 可选转 WebP。
pub fn prepare(
    cfg: &ImgBedSettings,
    original_name: Option<&str>,
    bytes: Vec<u8>,
    _source: &str,
) -> Result<Prepared, Fail> {
    if bytes.is_empty() {
        return Err(Fail::new("图片是空的", "0 字节"));
    }
    let max = cfg.max_mb as usize * 1024 * 1024;
    if bytes.len() > max {
        return Err(Fail::new(
            "图片太大",
            format!("{} 超过上限 {} MB（设置里可调）", human(bytes.len() as u64), cfg.max_mb),
        ));
    }
    let (mut ext, mut content_type) = path::kind_of(&bytes)?;
    let (mut width, mut height) = zx_media::dimensions(&bytes).unwrap_or((0, 0));
    let original_bytes = bytes.len();
    let mut out = bytes;
    let mut note = None;

    if cfg.convert == "webp" && can_convert(ext) && !(ext == "webp" && is_animated_webp(&out)) {
        match zx_media::decode(&out) {
            Ok(img) => {
                // max_side == 0 = 不限制（原尺寸）；>0 且超出则按长边等比缩小
                let sized = zx_media::resize(&img, cfg.max_side);
                match zx_media::encode_webp(&sized, cfg.quality) {
                    Ok(webp) => {
                        // 选了 WebP 就用 WebP：不再比体积、不回退原格式，
                        // 行为可预测（"我选了 WebP，传上去就是 WebP"）。
                        note = Some(format!(
                            "{} → {}（WebP q{}{}）",
                            human(original_bytes as u64),
                            human(webp.len() as u64),
                            cfg.quality,
                            if sized.width != img.width {
                                format!("，{}×{} → {}×{}", img.width, img.height, sized.width, sized.height)
                            } else {
                                String::new()
                            }
                        ));
                        out = webp;
                        ext = "webp";
                        content_type = "image/webp";
                        // 记录的是"真正传上去的那张图"的尺寸，缩过就跟着改
                        width = sized.width;
                        height = sized.height;
                    }
                    Err(e) => note = Some(e.text()),
                }
            }
            Err(e) => note = Some(e.text()),
        }
    } else if cfg.convert == "webp" && ext == "webp" && is_animated_webp(&out) {
        note = Some("动图保留原格式".into());
    }

    let stem = path::file_stem(cfg, original_name, &out);
    let name = format!("{stem}.{ext}");
    Ok(Prepared {
        name,
        bytes: out,
        ext,
        content_type,
        width,
        height,
        original_bytes,
        note,
    })
}

pub fn upload_one(
    backend: &dyn Backend,
    cfg: &ImgBedSettings,
    dir: &str,
    prep: Prepared,
    source: &str,
) -> Result<Outcome, Fail> {
    let caps = backend.caps();
    let mb = prep.bytes.len() as f64 / 1048576.0;
    if mb > caps.max_mb as f64 {
        return Err(Fail::new(
            "超过平台上限",
            format!("{} 的平台单文件上限是 {} MB", cfg.provider, caps.max_mb),
        ));
    }
    let req = crate::provider::UploadRequest {
        dir: dir.to_string(),
        name: prep.name.clone(),
        bytes: prep.bytes,
        content_type: prep.content_type,
    };
    let uploaded = backend.upload(&req)?;
    let rel = if uploaded.path.is_empty() { req.path() } else { uploaded.path.clone() };
    let url = if uploaded.url.is_empty() { backend.url(&rel)? } else { uploaded.url };
    let text = link::render(&cfg.copy_format, &url, &req.name);
    let record = Record {
        id: String::new(),
        provider: cfg.provider.clone(),
        path: rel,
        url,
        name: req.name.clone(),
        bytes: req.bytes.len() as u64,
        width: prep.width,
        height: prep.height,
        format: prep.ext.to_string(),
        source: source.to_string(),
        created_at: history::now_secs(),
    };
    history::add(record.clone());
    Ok(Outcome { record, text, note: prep.note })
}

fn can_convert(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "webp" | "bmp" | "tiff")
}

/// VP8X chunk 的 flags 第 1 位是"有动画"。
fn is_animated_webp(bytes: &[u8]) -> bool {
    if bytes.len() < 21 || &bytes[12..16] != b"VP8X" {
        return false;
    }
    bytes[20] & 0x02 != 0
}

pub fn human(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zx_core::{ImgBedCfg, ImgBedUploadCfg};

    fn cfg(convert: &str) -> ImgBedSettings {
        zx_core::Settings {
            imgbed: Some(ImgBedCfg {
                upload: ImgBedUploadCfg {
                    convert: Some(convert.into()),
                    max_side: Some(2560),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }
        .imgbed()
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&vec![200u8; (w * h * 4) as usize]).unwrap();
        }
        out
    }

    #[test]
    fn converts_png_to_webp_and_renames() {
        let bytes = png(64, 32);
        let p = prepare(&cfg("webp"), Some("a.png"), bytes, "file").unwrap();
        assert_eq!(p.ext, "webp");
        assert!(p.name.ends_with(".webp"));
        assert!(&p.bytes[..4] == b"RIFF");
        assert!(p.note.unwrap().contains("WebP"));
    }

    #[test]
    fn keep_mode_does_not_touch_bytes() {
        let bytes = png(16, 16);
        let p = prepare(&cfg("keep"), Some("a.png"), bytes.clone(), "file").unwrap();
        assert_eq!(p.ext, "png");
        assert_eq!(p.bytes, bytes);
    }

    #[test]
    fn max_side_zero_is_unlimited() {
        let bytes = png(200, 100);
        let mut c = cfg("webp");
        c.max_side = 0;
        let p = prepare(&c, Some("a.png"), bytes, "file").unwrap();
        assert_eq!((p.width, p.height), (200, 100));
        let img = zx_media::decode(&p.bytes).unwrap();
        assert_eq!((img.width, img.height), (200, 100), "0 表示不限制，不能缩");
        assert!(p.note.unwrap().contains("WebP"));
    }

    #[test]
    fn max_side_scales_proportionally_and_records_real_size() {
        let bytes = png(200, 100);
        let mut c = cfg("webp");
        c.max_side = 50;
        let p = prepare(&c, Some("a.png"), bytes, "file").unwrap();
        let img = zx_media::decode(&p.bytes).unwrap();
        assert_eq!((img.width, img.height), (50, 25), "长边按 50 等比缩");
        // 记录的是实际上传那张的尺寸，不是原图
        assert_eq!((p.width, p.height), (50, 25));
        assert!(p.note.unwrap().contains("200×100 → 50×25"));
    }

    #[test]
    fn smaller_than_limit_stays_untouched_in_size() {
        let bytes = png(40, 20);
        let mut c = cfg("webp");
        c.max_side = 2560;
        let p = prepare(&c, Some("a.png"), bytes, "file").unwrap();
        let img = zx_media::decode(&p.bytes).unwrap();
        assert_eq!((img.width, img.height), (40, 20), "只缩不放");
    }

    #[test]
    fn refuses_oversize_and_non_image() {
        let mut c = cfg("webp");
        c.max_mb = 1;
        let e = prepare(&c, None, vec![0; 1024 * 1024 + 1], "file").unwrap_err();
        assert_eq!(e.stage, "图片太大");
        let e = prepare(&c, None, b"hello".to_vec(), "file").unwrap_err();
        assert_eq!(e.stage, "不是图片");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2 KB");
        assert_eq!(human(3 * 1024 * 1024), "3.0 MB");
    }
}
