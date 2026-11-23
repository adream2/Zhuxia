//! 单张转换：校验 → 解码（含定向/缩放）→ 定输出名 → 临时文件编码 → 改名落盘。
//!
//! 参考 Filee 的 `MagickImageConverter.ConvertAsync` + `OutputPathResolver`：
//! - **源文件永不覆盖**：目标名与源同名时改叫 `名字 (2).ext`；
//! - 先写临时文件再 `rename`，中途失败不会留下半张图；
//! - 转换失败只影响这一张，批量里的其余文件照常继续（错误按类别汇总）。

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::decode::Bitmap;
use crate::encode::Quality;
use crate::error::ConvertError;
use crate::fmt::{self, Target};

/// 转换策略（来自 `settings.yaml` 的 `imgconv` 段）。
#[derive(Clone, Debug)]
pub struct Options {
    pub quality: u8,
    pub webp_lossless: bool,
    pub background: [u8; 3],
    /// true = 同名时直接覆盖；false = 自动改叫 `名字 (2).ext`。
    pub overwrite: bool,
    /// Some = 统一放到这个目录；None = 与源文件同目录。
    pub out_dir: Option<PathBuf>,
    /// 输出 ICO 时塞哪几个规格（8–256）。其余目标格式忽略这个字段。
    pub ico_sizes: Vec<u32>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            quality: 90,
            webp_lossless: false,
            background: [255, 255, 255],
            overwrite: false,
            out_dir: None,
            ico_sizes: vec![256, 48, 32, 16],
        }
    }
}

impl Options {
    pub fn quality(&self) -> Quality {
        Quality {
            quality: self.quality.clamp(1, 100),
            webp_lossless: self.webp_lossless,
            background: self.background,
        }
    }
}

/// 一张转换结果。
#[derive(Debug)]
pub struct Outcome {
    pub source: PathBuf,
    pub output: PathBuf,
    pub src_bytes: u64,
    pub out_bytes: u64,
    pub w: u32,
    pub h: u32,
    pub ms: u128,
    /// 转成了但值得说一句（例如 PNG 比原 JPG 大了一倍）。
    pub notice: Option<String>,
}

/// 单张转换入口。
pub fn convert(source: &Path, target: Target, opts: &Options) -> Result<Outcome, ConvertError> {
    let started = Instant::now();

    let meta = std::fs::metadata(source).map_err(|e| crate::error::io_error(&e, source))?;
    if !meta.is_file() {
        return Err(ConvertError::NotFound(source.display().to_string()));
    }
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext.is_empty() || !fmt::is_input(&ext) {
        return Err(ConvertError::Unsupported(if ext.is_empty() { "未知".into() } else { ext }));
    }
    if fmt::same_format(&ext, target) {
        return Err(ConvertError::AlreadyThat(ext));
    }

    // 解码（顺带按目标格式的边上限缩放）。
    // ICO 例外：直接在最大的那个规格上解码，小规格再逐级缩，矢量源也不会糊。
    let decode_hint = if target == Target::Ico {
        opts.ico_sizes.iter().copied().max().unwrap_or(256).min(256)
    } else {
        target.max_side()
    };
    let img: Bitmap = crate::decode::load(source, decode_hint)?;

    let output = plan_output(source, target, opts)?;
    let tmp = temp_beside(&output);

    // 编码到临时文件；任何一步失败都把临时文件删掉，别留垃圾。
    let encoded = crate::encode::write(&img, target, opts.quality(), &opts.ico_sizes, &tmp);
    if let Err(e) = encoded {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if !opts.overwrite && output.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConvertError::WriteFailed(format!("同名文件已存在：{}", output.display())));
    }
    // std::fs::rename 在 Windows 上是 MoveFileEx(REPLACE_EXISTING)，同卷原子替换。
    if let Err(e) = std::fs::rename(&tmp, &output) {
        let _ = std::fs::remove_file(&tmp);
        return Err(crate::error::io_error(&e, &output));
    }

    let out_bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    let src_bytes = meta.len();
    let notice = if out_bytes > src_bytes && src_bytes > 0 {
        Some(format!(
            "输出比原文件大 {}%",
            (out_bytes as f64 / src_bytes as f64 * 100.0).round() as i64 - 100
        ))
    } else {
        None
    };

    Ok(Outcome {
        source: source.to_path_buf(),
        output,
        src_bytes,
        out_bytes,
        w: img.w,
        h: img.h,
        ms: started.elapsed().as_millis(),
        notice,
    })
}

/// 定输出路径：同目录（或指定目录）+ 同名换扩展名；冲突时 `名字 (2).ext`。
///
/// 与 Filee `OutputPathResolver` 的差别：这里只有"原目录 / 固定目录"两种落点，
/// 不做文件名模板——右键菜单要的是零配置，模板留给设置页以后加。
fn plan_output(source: &Path, target: Target, opts: &Options) -> Result<PathBuf, ConvertError> {
    let dir = match &opts.out_dir {
        Some(d) => {
            std::fs::create_dir_all(d).map_err(|e| crate::error::io_error(&e, d))?;
            d.clone()
        }
        None => source.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")),
    };
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| ConvertError::Unsupported("文件名无法解析".into()))?;

    let candidate = dir.join(format!("{stem}.{}", target.ext()));
    if opts.overwrite {
        return Ok(candidate);
    }
    if !candidate.exists() && candidate != source {
        return Ok(candidate);
    }
    for n in 2..=9999 {
        let p = dir.join(format!("{stem} ({n}).{}", target.ext()));
        if !p.exists() && p != source {
            return Ok(p);
        }
    }
    Err(ConvertError::WriteFailed("同名文件太多，放弃自动改名".into()))
}

/// 同目录的临时文件名：编码器按 CLSID 认格式，不依赖扩展名。
fn temp_beside(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    let dir = output.parent().unwrap_or(Path::new("."));
    dir.join(format!(".{name}.zx-{}.tmp", std::process::id()))
}
