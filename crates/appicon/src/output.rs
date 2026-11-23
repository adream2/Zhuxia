//! 输出：把一组帧写成 PNG / ICO，或"全部尺寸"（PNG + ICO + 每个原生分辨率的单独文件）。
//!
//! 尺寸策略是三平台通用的：
//! - **只缩不放**：原生最大 32px 的图标不会给你生成 256px 的糊图；
//! - ICO 里优先复用原生帧（PE 的 48×48、APK 的 192×192），缺的档位从最大帧缩；
//! - PNG 输出取最大帧，长边超过 `maxSize` 才缩（默认 512，图标一般不需要 1024）。

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::error::IconError;
use crate::model::{fit_long, png_bytes, resize, square_pad, IconSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 只出一张 PNG（最大帧）。
    Png,
    /// 只出一个多尺寸 ICO。
    Ico,
    /// PNG + ICO + 每个原生分辨率的 PNG（放进 `<名字>.icons/`）。
    All,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "png" => Some(Mode::Png),
            "ico" => Some(Mode::Ico),
            "all" => Some(Mode::All),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Mode::Png => "png",
            Mode::Ico => "ico",
            Mode::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Png => "PNG 图片",
            Mode::Ico => "ICO 多尺寸",
            Mode::All => "全部尺寸",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub mode: Mode,
    /// PNG 最长边上限（16–4096），只缩不放。
    pub max_size: u32,
    /// ICO 里塞哪几档（8–256）。
    pub ico_sizes: Vec<u32>,
    pub out_dir: Option<PathBuf>,
    pub overwrite: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Png,
            max_size: 512,
            ico_sizes: vec![256, 128, 64, 48, 32, 16],
            out_dir: None,
            overwrite: false,
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub source: PathBuf,
    pub outputs: Vec<PathBuf>,
    pub detail: String,
    pub frames: usize,
    pub largest: (u32, u32),
    pub src_bytes: u64,
    pub ms: u128,
    pub notice: Option<String>,
}

/// 按模式落盘。`source` 只用来定名字与目录，不再读它。
pub fn write_set(source: &Path, set: &IconSet, opts: &Options) -> Result<Outcome, IconError> {
    let started = Instant::now();
    let src_bytes = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);
    if set.is_empty() {
        return Err(IconError::NoIcon("提取结果为空".into()));
    }
    let largest = set.largest().cloned().ok_or_else(|| IconError::NoIcon("没有最大帧".into()))?;
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
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| IconError::Unsupported("文件名无法解析".into()))?;

    let mut outputs: Vec<PathBuf> = Vec::new();
    let mut notice = None;

    // 目标名冲突：一次把这一组输出都要用的基名定下来，避免 PNG 叫 (2)、ICO 叫 (3)。
    let base = pick_base(&dir, &stem, opts)?;
    let png_path = dir.join(format!("{base}.png"));
    let ico_path = dir.join(format!("{base}.ico"));
    let all_dir = dir.join(format!("{base}.icons"));

    match opts.mode {
        Mode::Png => {
            let f = fit_long(&largest, opts.max_size)?;
            if f.w < 48 || f.h < 48 {
                notice = Some(format!("原生只有 {}×{}，未放大", f.w, f.h));
            }
            write_atomic(&png_path, &png_bytes(&f, false)?)?;
            outputs.push(png_path);
        }
        Mode::Ico => {
            let blob = build_ico(set, &opts.ico_sizes)?;
            write_atomic(&ico_path, &blob)?;
            outputs.push(ico_path);
        }
        Mode::All => {
            let f = fit_long(&largest, opts.max_size)?;
            write_atomic(&png_path, &png_bytes(&f, false)?)?;
            outputs.push(png_path);

            let blob = build_ico(set, &opts.ico_sizes)?;
            write_atomic(&ico_path, &blob)?;
            outputs.push(ico_path);

            std::fs::create_dir_all(&all_dir).map_err(|e| crate::error::io_error(&e, &all_dir))?;
            write_atomic(&all_dir.join("manifest.txt"), manifest(source, set).as_bytes())?;
            for (i, frame) in set.frames.iter().enumerate() {
                let name = format!("icon-{}x{}.png", frame.w, frame.h);
                let mut path = all_dir.join(&name);
                if path.exists() {
                    path = all_dir.join(format!("icon-{}x{}-{}.png", frame.w, frame.h, i + 1));
                }
                write_atomic(&path, &png_bytes(frame, false)?)?;
                outputs.push(path);
            }
            outputs.push(all_dir);
        }
    }

    if notice.is_none() && set.frames.len() == 1 && largest.long_side() <= 32 {
        notice = Some(format!("只找到 {}×{} 一帧", largest.w, largest.h));
    }

    Ok(Outcome {
        source: source.to_path_buf(),
        outputs,
        detail: set.detail.clone(),
        frames: set.frames.len(),
        largest: (largest.w, largest.h),
        src_bytes,
        ms: started.elapsed().as_millis(),
        notice,
    })
}

/// 生成 ICO 字节。尺寸清单见 `ico::plan`：只缩不放，原生帧优先。
pub fn build_ico(set: &IconSet, sizes: &[u32]) -> Result<Vec<u8>, IconError> {
    let largest = set.largest().ok_or_else(|| IconError::NoIcon("没有可写帧".into()))?;
    let square = square_pad(largest);
    let plan = crate::ico::plan(&set.frames, sizes);
    let mut frames: Vec<(u32, Vec<u8>)> = Vec::with_capacity(plan.len());
    for side in plan {
        let exact = set
            .frames
            .iter()
            .find(|f| f.w == side && f.h == side)
            .cloned();
        let png = match exact {
            Some(f) => match f.png {
                Some(b) => b,
                None => png_bytes(&f, true)?,
            },
            None => {
                let small = resize(&square, side, side)?;
                png_bytes(&small, true)?
            }
        };
        frames.push((side, png));
    }
    crate::ico::build(&frames)
}

/// 选一个不冲突的基名（`照片` → `照片`、`照片 (2)`…）。
fn pick_base(dir: &Path, stem: &str, opts: &Options) -> Result<String, IconError> {
    let taken = |base: &str| -> bool {
        dir.join(format!("{base}.png")).exists()
            || dir.join(format!("{base}.ico")).exists()
            || dir.join(format!("{base}.icons")).exists()
    };
    if opts.overwrite || !taken(stem) {
        return Ok(stem.to_string());
    }
    for n in 2..=9999 {
        let base = format!("{stem} ({n})");
        if !taken(&base) {
            return Ok(base);
        }
    }
    Err(IconError::WriteFailed("同名文件太多，放弃自动改名".into()))
}

/// 先写同目录临时文件再改名：中途失败不留半张图。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), IconError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "icon".into());
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".{name}.zx-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(crate::error::io_error(&e, path));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(crate::error::io_error(&e, path));
    }
    Ok(())
}

fn manifest(source: &Path, set: &IconSet) -> String {
    let mut s = String::new();
    s.push_str("驻匣 · 图标提取\n");
    s.push_str(&format!("源文件：{}\n", source.display()));
    s.push_str(&format!("来源：{}\n", set.detail));
    s.push_str(&format!("帧数：{}\n\n", set.frames.len()));
    for f in &set.frames {
        s.push_str(&format!(
            "icon-{}x{}.png\t{}×{}\t{}\n",
            f.w,
            f.h,
            f.w,
            f.h,
            f.origin
        ));
    }
    s
}
