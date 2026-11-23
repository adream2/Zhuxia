//! 多平台密钥：`<exe>/imgbed/secrets.dat`（DPAPI 加密的 JSON）。
//!
//! 一个文件放所有平台的凭据，key 规则是 `<provider>.<field>`：
//! `github.token`、`s3.secretAccessKey`、`webdav.password`、`custom.token`。
//! 之所以不放 `settings.yaml`：那份文件经常被同步、备份、贴进聊天，而 token 等价于
//! 仓库/桶的写权限。DPAPI 绑定当前 Windows 用户，换账号就解不开。
//!
//! v0 的 `token.dat`（单 token）会在首次读取时迁移成 `github.token` 并删除。

use std::collections::BTreeMap;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

const MAGIC: &[u8; 8] = b"ZXIMGB02";
const ENTROPY: &[u8] = b"zhuxia.imgbed.v2";
/// v0 单 token 文件的魔数与熵（迁移用）
const LEGACY_MAGIC: &[u8; 8] = b"ZXIMGB01";
const LEGACY_ENTROPY: &[u8] = b"zhuxia.imgbed.v1";

fn dir() -> PathBuf {
    zx_core::cfg::run_dir().join("imgbed")
}

fn path() -> PathBuf {
    dir().join("secrets.dat")
}

fn legacy_path() -> PathBuf {
    dir().join("token.dat")
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    unsafe {
        let src = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let v = src.to_vec();
        for b in std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) {
            std::ptr::write_volatile(b, 0);
        }
        let _ = LocalFree(HLOCAL(out.pbData as *mut core::ffi::c_void));
        v
    }
}

fn zero(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

fn protect(plain: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    let input = blob(plain);
    let ent = blob(entropy);
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, PCWSTR::null(), Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            .ok()?;
    }
    if out.pbData.is_null() {
        return None;
    }
    Some(take(out))
}

fn unprotect(bytes: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    let input = blob(bytes);
    let ent = blob(entropy);
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
    }
    if out.pbData.is_null() {
        return None;
    }
    Some(take(out))
}

/// 读全部密钥。解不开/损坏时返回空表并记一条日志（不让整个模块崩）。
pub fn all() -> BTreeMap<String, String> {
    migrate_legacy();
    let Ok(bytes) = std::fs::read(path()) else {
        return BTreeMap::new();
    };
    let Some(body) = bytes.strip_prefix(MAGIC.as_slice()) else {
        zx_core::log::warn("图床：secrets.dat 魔数不对，按空处理（需要重新填密钥）");
        return BTreeMap::new();
    };
    let Some(mut plain) = unprotect(body, ENTROPY) else {
        zx_core::log::warn("图床：secrets.dat 解不开（换了 Windows 账号？），请重新填密钥");
        return BTreeMap::new();
    };
    let text = String::from_utf8_lossy(&plain).to_string();
    zero(&mut plain);
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn get(key: &str) -> Option<String> {
    all().get(key).cloned().filter(|v| !v.trim().is_empty())
}

/// 只回"哪些 key 有值"，永不回原文。
pub fn set_keys() -> Vec<String> {
    all().keys().cloned().collect()
}

pub fn has(key: &str) -> bool {
    get(key).is_some()
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let mut map = all();
    let value = value.trim();
    if value.is_empty() {
        map.remove(key);
    } else {
        map.insert(key.to_string(), value.to_string());
    }
    write(&map)
}

pub fn clear(key: &str) -> Result<(), String> {
    let mut map = all();
    map.remove(key);
    write(&map)
}

fn write(map: &BTreeMap<String, String>) -> Result<(), String> {
    let _ = std::fs::create_dir_all(dir());
    let mut plain = serde_json::to_vec(map).map_err(|e| format!("序列化密钥失败：{e}"))?;
    let body = protect(&plain, ENTROPY).ok_or_else(|| "DPAPI 加密失败，密钥未保存".to_string())?;
    zero(&mut plain);
    let mut sealed = Vec::with_capacity(MAGIC.len() + body.len());
    sealed.extend_from_slice(MAGIC);
    sealed.extend_from_slice(&body);
    let tmp = path().with_extension("dat.tmp");
    std::fs::write(&tmp, &sealed).map_err(|e| format!("写密钥失败：{e}"))?;
    std::fs::rename(&tmp, path()).map_err(|e| format!("保存密钥失败：{e}"))?;
    Ok(())
}

/// v0 单 token → `github.token`。只做一次；成功就删旧文件。
fn migrate_legacy() {
    let legacy = legacy_path();
    if !legacy.exists() {
        return;
    }
    let Ok(bytes) = std::fs::read(&legacy) else { return };
    let Some(body) = bytes.strip_prefix(LEGACY_MAGIC.as_slice()) else { return };
    let Some(mut plain) = unprotect(body, LEGACY_ENTROPY) else { return };
    let token = String::from_utf8_lossy(&plain).trim().to_string();
    zero(&mut plain);
    if token.is_empty() {
        let _ = std::fs::remove_file(&legacy);
        return;
    }
    let mut map = {
        let Ok(bytes) = std::fs::read(path()) else { return };
        let Some(body) = bytes.strip_prefix(MAGIC.as_slice()) else { return };
        let Some(mut p) = unprotect(body, ENTROPY) else { return };
        let text = String::from_utf8_lossy(&p).to_string();
        zero(&mut p);
        serde_json::from_str::<BTreeMap<String, String>>(&text).unwrap_or_default()
    };
    map.entry("github.token".into()).or_insert(token);
    if write(&map).is_ok() {
        let _ = std::fs::remove_file(&legacy);
        zx_core::log::info("图床：已把旧 token.dat 迁移到 secrets.dat（github.token）");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_versions_are_stable() {
        assert_eq!(MAGIC, b"ZXIMGB02");
        assert_eq!(LEGACY_MAGIC, b"ZXIMGB01");
    }

    #[test]
    fn map_roundtrip_through_json() {
        let mut m = BTreeMap::new();
        m.insert("github.token".to_string(), "ghp_x".to_string());
        let text = serde_json::to_string(&m).unwrap();
        let back: BTreeMap<String, String> = serde_json::from_str(&text).unwrap();
        assert_eq!(back.get("github.token").map(String::as_str), Some("ghp_x"));
    }
}
