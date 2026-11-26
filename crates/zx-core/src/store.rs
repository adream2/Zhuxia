//! 落盘工具：一份文件的"要么全有、要么全无"写入。
//!
//! 这个仓库里有四份状态要持久化——`settings.yaml`（配置）、`imgbed/secrets.dat`
//! （DPAPI 密钥）、`clipboard/history.dat`、`netstat.json`（流量与封禁）。
//! 其中三份手写了 `tmp + rename`，第四份（`netstat.json`）直接 `fs::write`——
//! 于是"断电/被强杀时恰好写到一半"只会在第四份身上丢整份数据，
//! 而且丢的是**用户看不到也补不回来的历史累计流量**。
//!
//! 与其让每处各写一遍（并且迟早再漏一处），不如在这里收口。

use std::path::{Path, PathBuf};

/// 同目录的临时文件名。必须同目录：跨卷 `rename` 会失败（EXDEV）。
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// 原子写：先写 `*.tmp`，再 `rename` 覆盖目标。
///
/// `rename` 在 Windows 上是替换语义（`MoveFileEx` + `REPLACE_EXISTING`），
/// 所以读者要么看到旧文件、要么看到新文件，不会看到半截。
///
/// `rename` 失败（目标被别的进程以不给共享的方式打开、或跨卷）时退回 `copy`：
/// 这条路不是原子的，但比"直接失败、数据全丢"好，且失败会如实返回。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = tmp_path(path);
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            std::fs::copy(&tmp, path).map_err(|e2| format!("改名失败（{e}），复制也失败：{e2}"))?;
            let _ = std::fs::remove_file(&tmp);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zx-store-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn tmp_file_sits_next_to_the_target() {
        let p = Path::new(r"C:\a\b\netstat.json");
        assert_eq!(tmp_path(p), Path::new(r"C:\a\b\netstat.json.tmp"));
    }

    #[test]
    fn overwrites_atomically_and_leaves_no_tmp() {
        let d = tmpdir("atomic");
        let f = d.join("state.json");
        std::fs::write(&f, b"old").unwrap();
        write_atomic(&f, b"new").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"new");
        assert!(!tmp_path(&f).exists(), "临时文件必须被消费掉");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fails_loudly_when_parent_is_missing() {
        // 父目录不存在时必须报错，而不是静默丢数据（否则用户以为存上了）
        let d = std::env::temp_dir().join(format!("zx-store-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("nested.json");
        assert!(write_atomic(&f, b"x").is_err());
    }
}
