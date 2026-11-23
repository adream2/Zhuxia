//! 格式探测与分派：**先看内容，再看扩展名**。
//!
//! 用户把 `.apk` 改名成 `.zip`、把 `.dll` 改名成 `.bin` 都很常见。magic 能认出来的
//! 就按内容走；认不出来才退回扩展名，扩展名也没有就明确报"认不出"，不做无根据的猜测。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::IconError;
use crate::model::IconSet;

const MAX_SMALL: u64 = 64 * 1024 * 1024;
const MAX_BIG: u64 = 256 * 1024 * 1024;

pub fn extract(path: &Path) -> Result<IconSet, IconError> {
    extract_depth(path, 0)
}

fn extract_depth(path: &Path, depth: u32) -> Result<IconSet, IconError> {
    if depth > 4 {
        return Err(IconError::NoIcon("快捷方式/嵌套引用层数过深".into()));
    }
    let meta = std::fs::metadata(path).map_err(|e| crate::error::io_error(&e, path))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if meta.is_dir() {
        if ext == "app" || path.to_string_lossy().to_ascii_lowercase().ends_with(".app") {
            return crate::macos::app_bundle(path);
        }
        return Err(IconError::Unsupported("目录".into()));
    }
    if !meta.is_file() {
        return Err(IconError::NotFound(path.display().to_string()));
    }

    let mut head = [0u8; 16];
    {
        let mut f = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
        let _ = f.read(&mut head);
    }
    let tail_koly = {
        let mut f = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
        let len = meta.len();
        let mut ok = false;
        if len >= 512
            && f.seek(SeekFrom::End(-512)).is_ok() {
                let mut t = [0u8; 4];
                if f.read_exact(&mut t).is_ok() {
                    ok = &t == b"koly";
                }
            }
        ok
    };

    if ext == "lnk" || looks_lnk(&head) {
        return via_lnk(path, depth);
    }
    if ext == "ico" || (head[0] == 0 && head[1] == 0 && head[2] == 1 && head[3] == 0) {
        let data = read_capped(path, MAX_SMALL)?;
        return crate::ico::read(&data);
    }
    if ext == "icns" || &head[..4] == b"icns" {
        let data = read_capped(path, MAX_SMALL)?;
        return crate::icns::read(&data);
    }
    if &head[..2] == b"MZ" {
        let data = read_capped(path, MAX_BIG)?;
        return crate::pe::extract(&data);
    }
    if crate::msi::is_cfb(&head) {
        let data = read_capped(path, MAX_BIG)?;
        return crate::msi::extract(&data);
    }
    if is_zip(&head) {
        return crate::zipk::extract(path, &ext);
    }
    if crate::linux::is_ar(&head) {
        return crate::linux::deb(path);
    }
    if crate::linux::is_rpm(&head) {
        return crate::linux::rpm(path);
    }
    if &head[..4] == b"\x7fELF" {
        return crate::linux::appimage(path);
    }
    if tail_koly || ext == "dmg" {
        return crate::macos::dmg(path);
    }

    // 内容认不出来，退回扩展名（并给出对应的明确错误，而不是"未知格式"）。
    match ext.as_str() {
        "exe" | "dll" | "ocx" | "cpl" | "scr" | "sys" | "mui" => Err(IconError::Corrupt(
            "扩展名是 PE，但文件头不是 MZ".into(),
        )),
        "msi" => Err(IconError::Corrupt("扩展名是 MSI，但不是 OLE 复合文档".into())),
        "apk" | "aab" | "ipa" | "msix" | "appx" | "zip" => {
            Err(IconError::Corrupt("扩展名是 zip 系，但文件头不是 PK".into()))
        }
        "deb" => Err(IconError::Corrupt("扩展名是 deb，但不是 ar 归档".into())),
        "rpm" => Err(IconError::Corrupt("扩展名是 rpm，但魔法数不匹配".into())),
        "appimage" => Err(IconError::Corrupt("扩展名是 AppImage，但不是 ELF".into())),
        "dmg" => Err(IconError::Corrupt("扩展名是 DMG，但找不到 koly 尾标记".into())),
        "lnk" => Err(IconError::Corrupt("扩展名是快捷方式，但文件头无效".into())),
        "" => Err(IconError::Unknown(path.display().to_string())),
        other => Err(IconError::Unsupported(other.to_string())),
    }
}

fn looks_lnk(head: &[u8]) -> bool {
    head.len() >= 4 && head[..4] == [0x4C, 0x00, 0x00, 0x00]
}

fn is_zip(head: &[u8]) -> bool {
    head.len() >= 4 && &head[..2] == b"PK" && matches!(head[2], 3 | 5 | 7)
}

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, IconError> {
    let meta = std::fs::metadata(path).map_err(|e| crate::error::io_error(&e, path))?;
    if meta.len() > cap {
        return Err(IconError::TooLarge(format!("{} B", meta.len())));
    }
    std::fs::read(path).map_err(|e| crate::error::io_error(&e, path))
}

/// 快捷方式：解析出图标/目标路径，递归提取，并在 detail 里标出来源。
fn via_lnk(path: &Path, depth: u32) -> Result<IconSet, IconError> {
    let data = read_capped(path, 4 * 1024 * 1024)?;
    let link = crate::lnk::parse(&data)?;
    let Some(target) = crate::lnk::resolve(&link, path) else {
        return Err(IconError::NoIcon(
            "快捷方式没有图标路径，指向的目标也不存在".into(),
        ));
    };
    let mut set = extract_depth(&target, depth + 1)?;
    set.detail = format!("LNK → {} · {}", target.display(), set.detail);
    Ok(set)
}
