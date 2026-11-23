//! macOS：`.app` 目录与 `.dmg` 磁盘镜像。
//!
//! `.app` 是目录包，图标在 `Contents/Info.plist` 的 `CFBundleIconFile` 指向的
//! `Contents/Resources/*.icns` 里。`.dmg` 则是 UDIF 容器，里面通常是一个 HFS+ 分区
//! ——把分区解到临时文件后，用纯 Rust 的 HFS+ 读取器按路径取 icns。
//!
//! 新版 macOS 安装包越来越多用 APFS，APFS 的目录树比 HFS+ 复杂一个量级，本版明确
//! 报"只支持 HFS+"，而不是假装支持然后给个空结果。

use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::error::IconError;
use crate::model::{decode_image, IconSet};

/* --------------------------------- .app --------------------------------- */

pub fn app_bundle(path: &Path) -> Result<IconSet, IconError> {
    let contents = if path.file_name().map(|n| n == "Contents").unwrap_or(false) {
        path.to_path_buf()
    } else {
        path.join("Contents")
    };
    let plist = contents.join("Info.plist");
    if !plist.is_file() {
        return Err(IconError::NoIcon("不是 .app 包（没有 Contents/Info.plist）".into()));
    }
    let bytes = std::fs::read(&plist).map_err(|e| crate::error::io_error(&e, &plist))?;
    let names = crate::zipk::icon_base_names(&bytes);
    let res = contents.join("Resources");
    let mut set = IconSet::new(format!("App 包 · {}", path.display()));

    for base in &names {
        for cand in candidates(&res, base) {
            if let Ok(data) = std::fs::read(&cand) {
                if let Ok(one) = load_icns_or_image(&data, &cand.to_string_lossy()) {
                    for f in one.frames {
                        set.push(f);
                    }
                }
            }
        }
    }
    if set.is_empty() {
        // 回退：Resources 下最大的 icns
        if let Ok(rd) = std::fs::read_dir(&res) {
            let mut list: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("icns")).unwrap_or(false))
                .collect();
            list.sort();
            for p in list {
                if let Ok(data) = std::fs::read(&p) {
                    if let Ok(one) = crate::icns::read(&data) {
                        for f in one.frames {
                            set.push(f);
                        }
                    }
                }
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("Contents/Resources 里没有 icns/png 图标".into()));
    }
    set.detail = format!("App 包 · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

fn candidates(res: &Path, base: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for ext in ["icns", "png"] {
        let p = res.join(format!("{base}.{ext}"));
        if p.is_file() {
            out.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(res) {
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let is_img = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("icns") || e.eq_ignore_ascii_case("png"))
                    .unwrap_or(false);
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                is_img && (stem == base || stem.starts_with(&format!("{base}@")))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out.sort();
    out.dedup();
    out
}

fn load_icns_or_image(data: &[u8], name: &str) -> Result<IconSet, IconError> {
    if crate::icns::is_icns(data) {
        crate::icns::read(data)
    } else {
        let mut set = IconSet::new(name.to_string());
        set.push(decode_image(data, name)?);
        Ok(set)
    }
}

/* --------------------------------- DMG --------------------------------- */

pub fn dmg(path: &Path) -> Result<IconSet, IconError> {
    let mut arch = udif::DmgArchive::open(path)
        .map_err(|e| IconError::Corrupt(format!("DMG 打开失败：{e}")))?;
    let parts = arch.partitions();
    let hfs = parts.iter().find(|p| p.partition_type.is_hfs_compatible());
    if hfs.is_none() && parts.iter().any(|p| p.partition_type == udif::PartitionType::Apfs) {
        return Err(IconError::NeedExternal(
            "DMG 是 APFS 分区，本版只读 HFS+".into(),
        ));
    }

    let tmp = std::env::temp_dir().join(format!(
        ".zx-dmg-{}-{}.hfs",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = (|| -> Result<IconSet, IconError> {
        match hfs {
            Some(p) => {
                let id = p.id;
                arch.extract_partition_to_file(id, &tmp)
                    .map_err(|e| IconError::Corrupt(format!("DMG 分区解压失败：{e}")))?;
            }
            None => {
                arch.extract_main_partition_to_file(&tmp)
                    .map_err(|e| IconError::Corrupt(format!("DMG 主分区解压失败：{e}")))?;
            }
        }
        let f = std::fs::File::open(&tmp).map_err(|e| crate::error::io_error(&e, &tmp))?;
        let mut vol = hfsplus::HfsVolume::open(BufReader::new(f))
            .map_err(|e| IconError::NeedExternal(format!("分区不是可读的 HFS+：{e}")))?;
        let entries = vol.walk().map_err(|e| IconError::Corrupt(format!("HFS+ 遍历失败：{e}")))?;

        // 候选：.VolumeIcon.icns、*.app/Contents/Info.plist、*.app/Contents/Resources/*.icns
        let mut plists: Vec<String> = Vec::new();
        let mut icons: Vec<String> = Vec::new();
        for e in &entries {
            if e.entry.kind != hfsplus::EntryKind::File {
                continue;
            }
            let p = e.path.trim_start_matches('/').to_string();
            let low = p.to_ascii_lowercase();
            if low.ends_with(".volumeicon.icns") {
                icons.push(e.path.clone());
            } else if low.ends_with(".app/contents/info.plist") {
                plists.push(e.path.clone());
            } else if low.ends_with(".icns") && low.contains(".app/contents/resources/") {
                icons.push(e.path.clone());
            }
        }

        let mut set = IconSet::new("DMG · HFS+".to_string());
        // 优先第一个 .app 的 Info.plist 指定图标
        for pl in plists.iter().take(4) {
            let Ok(bytes) = vol.read_file(pl) else { continue };
            let names = crate::zipk::icon_base_names(&bytes);
            let app_root = pl.trim_end_matches("/Contents/Info.plist").trim_end_matches("/contents/Info.plist");
            for base in names {
                for e in &entries {
                    if e.entry.kind != hfsplus::EntryKind::File {
                        continue;
                    }
                    let p = e.path.replace('\\', "/");
                    let low = p.to_ascii_lowercase();
                    if !low.starts_with(&format!("{}/contents/resources/", app_root.to_ascii_lowercase())) {
                        continue;
                    }
                    let stem = Path::new(&p)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    if stem == base || stem.starts_with(&format!("{base}@")) {
                        if let Ok(data) = vol.read_file(&e.path) {
                            if let Ok(one) = load_icns_or_image(&data, &p) {
                                for f in one.frames {
                                    set.push(f);
                                }
                            }
                        }
                    }
                }
            }
            if !set.is_empty() {
                break;
            }
        }
        if set.is_empty() {
            for p in icons.iter().take(8) {
                if let Ok(data) = vol.read_file(p) {
                    if let Ok(one) = load_icns_or_image(&data, p) {
                        for f in one.frames {
                            set.push(f);
                        }
                    }
                }
            }
        }
        if set.is_empty() {
            return Err(IconError::NoIcon("DMG 里没有 .app 图标或 VolumeIcon".into()));
        }
        set.detail = format!("DMG · HFS+ · {} 帧（{}）", set.frames.len(), set.size_list());
        Ok(set.finish())
    })();

    let _ = std::fs::remove_file(&tmp);
    result
}
