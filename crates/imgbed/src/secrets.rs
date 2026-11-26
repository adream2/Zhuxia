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

/// 缓存项：内容 + 它对应的磁盘戳（mtime, 长度）。
struct Cached {
    map: BTreeMap<String, String>,
    stamp: Option<(std::time::SystemTime, u64)>,
}

/// 进程内缓存。
///
/// 以前每次 `get()` 都要**读整份文件 + DPAPI 解密 + JSON 反序列化**。面板每次返回
/// 状态都会调 `set_keys()`，`write()` 又先 `all()` 再加密整份——
/// "改一个字段"= 解密全表 + 加密全表。缓存之后这些全变成一次内存查询。
///
/// **必须带磁盘戳**：常驻进程和设置面板是两个进程，都读写同一个 `secrets.dat`。
/// 纯内存缓存会让"面板改了 token、常驻进程还在用旧 token"变成必然——上传 401，
/// 用户只能重启程序。`stat` 比 DPAPI 解密便宜几个数量级，每次比对完全划算。
/// （`cfg` 有 `reload()` 做同样的事，这里以前没有。）
static CACHE: std::sync::Mutex<Option<Cached>> = std::sync::Mutex::new(None);

fn lock_cache() -> std::sync::MutexGuard<'static, Option<Cached>> {
    CACHE.lock().unwrap_or_else(|p| p.into_inner())
}

/// 磁盘戳。文件不存在时返回 None（与"存在但为空"区分开）。
fn stamp() -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path()).ok()?;
    let t = m.modified().ok()?;
    Some((t, m.len()))
}

/// 读全部密钥。解不开/损坏时返回空表并记一条日志（不让整个模块崩）。
pub fn all() -> BTreeMap<String, String> {
    let now = stamp();
    {
        let g = lock_cache();
        if let Some(c) = g.as_ref() {
            if c.stamp == now {
                return c.map.clone();
            }
        }
    }
    // 戳变了（别的进程写过）或还没读过：重新解密
    let map = load_from_disk();
    *lock_cache() = Some(Cached { map: map.clone(), stamp: stamp() });
    map
}

/// 真正的读盘 + 解密。只在缓存未命中时走一次。
fn load_from_disk() -> BTreeMap<String, String> {
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
    // 走 &[u8] 直接反序列化，不经 String：`from_utf8_lossy` 会复制出一份**不会被清零**
    // 的副本，明文在堆上多留一份到它自然释放为止。
    let out = serde_json::from_slice::<BTreeMap<String, String>>(&plain).unwrap_or_default();
    zero(&mut plain);
    out
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
    zx_core::store::write_atomic(&path(), &sealed).map_err(|e| format!("保存密钥失败：{e}"))?;
    // 落盘成功才更新缓存：失败时内存里不能有一份"其实没存上"的值
    *lock_cache() = Some(Cached { map: map.clone(), stamp: stamp() });
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
        let m = serde_json::from_slice::<BTreeMap<String, String>>(&p).unwrap_or_default();
        zero(&mut p);
        m
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
