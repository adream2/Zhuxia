//! 落盘加密：剪贴板里可能有密码、令牌、私钥，历史与图片附件一律用
//! DPAPI（用户态）加密后再写盘，不落明文。
//!
//! - 作用域用当前用户：换用户/换机器都解不开，拷贝走文件也没用；
//! - 每次 `save()` 整份加密（默认上限 1000 条，毫秒级），实现简单、没有
//!   「某条忘了加密」的漏网可能；
//! - 加密结果带 8 字节魔数，便于区分旧版明文文件并做一次性迁移。
//!
//! DPAPI 失败时**不写盘**（fail closed），宁可这一轮不保存，也不退回明文。

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// 加密文件的开头。旧版明文 JSON / PNG 不会以它开头，迁移时据此判断。
pub const MAGIC: &[u8; 8] = b"ZXCLIP01";

/// 附加熵：把密钥用途绑到本模块，别的程序拿到同一用户的 DPAPI 也解不了。
fn entropy() -> &'static [u8] {
    b"zhuxia.clipboard.v1"
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    unsafe {
        let src = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let v = src.to_vec();
        // DPAPI 返回的明文缓冲先清零再释放。
        for b in std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) {
            std::ptr::write_volatile(b, 0);
        }
        let _ = LocalFree(HLOCAL(out.pbData as *mut core::ffi::c_void));
        v
    }
}

/// 用 volatile 写清零，避免优化器把「没人读的写」删掉。
pub fn zero(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

/// 明文 → 加密字节（带魔数）。失败返回 `None`，调用方必须放弃写盘。
pub fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    let input = blob(plain);
    let ent = blob(entropy());
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&ent),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
    }
    if out.pbData.is_null() {
        return None;
    }
    let body = take(out);
    let mut v = Vec::with_capacity(MAGIC.len() + body.len());
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&body);
    Some(v)
}

/// 加密字节 → 明文。没有魔数（旧版明文文件）或解密失败返回 `None`。
pub fn unprotect(blob_bytes: &[u8]) -> Option<Vec<u8>> {
    let body = blob_bytes.strip_prefix(MAGIC)?;
    let input = blob(body);
    let ent = blob(entropy());
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            .ok()?;
    }
    if out.pbData.is_null() {
        return None;
    }
    Some(take(out))
}

/// 文件内容是不是本模块写出的密文。
pub fn is_protected(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// 原地加密一个旧版明文文件（附件迁移用）。已经是密文就返回 `true`。
pub fn protect_file_in_place(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    if is_protected(&bytes) {
        return true;
    }
    let Some(sealed) = protect(&bytes) else { return false };
    crate::store::write_atomic(path, &sealed)
}

/// 安全删除：先覆写再删，避免旧明文留在空闲簇里被捡到。
pub fn shred(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() {
            let len = meta.len().min(16 * 1024 * 1024) as usize;
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
                use std::io::Write;
                let zeros = vec![0u8; len.max(1)];
                let _ = f.write_all(&zeros);
                let _ = f.sync_all();
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_utf8() {
        let plain = "密码 P@ssw0rd / 令牌 ghp_123".as_bytes();
        let sealed = protect(plain).expect("dpapi protect");
        assert!(is_protected(&sealed));
        assert_ne!(&sealed[MAGIC.len()..], plain);
        assert_eq!(unprotect(&sealed).as_deref(), Some(plain));
    }

    #[test]
    fn plaintext_is_rejected() {
        assert!(unprotect(b"{\"entries\":[]}").is_none());
        assert!(!is_protected(b"hello"));
    }
}
