//! PaddleOCR 模型包：下载 / 导入 / 删除 / 状态。
//!
//! 模型不进 exe（15MB 会毁掉"单文件小工具"的定位），改为**按需获取**，三条路都支持：
//!
//! 1. **一键下载**：默认走 `hf-mirror.com`（国内可用），失败自动退 `huggingface.co`；
//! 2. **用户自行下载**：把 `det.onnx` / `rec.onnx` / `keys.txt` 放进任意目录，点"导入"复制进来；
//! 3. **GitHub 镜像加速**：内置一组 ghproxy 前缀（`ghfast.top` / `gh-proxy.com` / `ghproxy.net`），
//!    字典文件（GitHub raw）和用户填的 GitHub 地址都会自动套前缀，直连失败再退回直连。
//!
//! 目录：`<exe>/ocr/`（与 `settings.yaml` 同级，卸载时删掉即可，不写注册表）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::*;
use zx_core::win::wide;

/// 主源（RapidOCR 的 PP-OCRv4 中文模型）。
const HF_MIRROR: &str = "https://hf-mirror.com/SWHL/RapidOCR/resolve/main";
const HF: &str = "https://huggingface.co/SWHL/RapidOCR/resolve/main";
/// GitHub raw 的字典；走镜像前缀加速。
const KEYS_GH: &str = "https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/v2.7.0/ppocr/utils/ppocr_keys_v1.txt";
const GH_PROXIES: [&str; 3] = ["https://ghfast.top/", "https://gh-proxy.com/", "https://ghproxy.net/"];

const DET_FILE: &str = "det.onnx";
const REC_FILE: &str = "rec.onnx";
const KEYS_FILE: &str = "keys.txt";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// 镜像优先，失败退官方
    Auto,
    HfMirror,
    HuggingFace,
    /// 自定义 base（可指向内网/自建镜像）；字典同目录取不到再走 GitHub 镜像
    Custom,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub installed: bool,
    pub dir: PathBuf,
    pub bytes: u64,
    pub det: bool,
    pub rec: bool,
    pub keys: bool,
}

pub fn dir() -> PathBuf {
    zx_core::cfg::run_dir().join("ocr")
}

pub fn status() -> Status {
    let d = dir();
    let mut st = Status { dir: d.clone(), ..Default::default() };
    let mut total = 0u64;
    for (name, field) in [(DET_FILE, 0), (REC_FILE, 1), (KEYS_FILE, 2)] {
        let p = d.join(name);
        if let Ok(m) = std::fs::metadata(&p) {
            total += m.len();
            match field {
                0 => st.det = m.len() > 1_000_000,
                1 => st.rec = m.len() > 1_000_000,
                _ => st.keys = m.len() > 1000,
            }
        }
    }
    st.bytes = total;
    st.installed = st.det && st.rec && st.keys;
    st
}

pub fn remove() -> std::io::Result<()> {
    crate::ppocr::unload();
    let d = dir();
    for name in [DET_FILE, REC_FILE, KEYS_FILE] {
        let _ = std::fs::remove_file(d.join(name));
    }
    let _ = std::fs::remove_dir(&d);
    Ok(())
}

// ---------------------------------------------------------------- 进度

static DONE: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);
static STAGE: Mutex<String> = Mutex::new(String::new());
static ERROR: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub running: bool,
    pub done: u64,
    pub total: u64,
    pub stage: String,
    pub error: Option<String>,
}

pub fn progress() -> Progress {
    Progress {
        running: RUNNING.load(Ordering::SeqCst),
        done: DONE.load(Ordering::SeqCst),
        total: TOTAL.load(Ordering::SeqCst),
        stage: STAGE.lock().map(|s| s.clone()).unwrap_or_default(),
        error: ERROR.lock().ok().and_then(|e| e.clone()),
    }
}

fn stage(s: impl Into<String>) {
    if let Ok(mut g) = STAGE.lock() {
        *g = s.into();
    }
}

// ---------------------------------------------------------------- 安装

/// 一键下载。`source` 决定 base；`custom_base` 只在 `Source::Custom` 时使用。
pub fn download(source: Source, custom_base: &str) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("已经在下载了".into());
    }
    if let Ok(mut e) = ERROR.lock() {
        *e = None;
    }
    DONE.store(0, Ordering::SeqCst);
    TOTAL.store(0, Ordering::SeqCst);
    let result = download_inner(source, custom_base);
    RUNNING.store(false, Ordering::SeqCst);
    if let Err(e) = &result {
        if let Ok(mut g) = ERROR.lock() {
            *g = Some(e.clone());
        }
        zx_core::log::warn(format!("OCR 模型下载失败：{e}"));
    } else {
        zx_core::log::info("OCR 模型下载完成");
    }
    result
}

fn download_inner(source: Source, custom_base: &str) -> Result<(), String> {
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let bases: Vec<String> = match source {
        Source::Auto => vec![HF_MIRROR.to_string(), HF.to_string()],
        Source::HfMirror => vec![HF_MIRROR.to_string()],
        Source::HuggingFace => vec![HF.to_string()],
        Source::Custom => {
            let base = custom_base.trim().trim_end_matches('/').to_string();
            let mut v = Vec::new();
            // 用户填 GitHub 地址时自动套镜像前缀（国内直连 github 基本不可用）
            if base.contains("github.com") || base.contains("githubusercontent.com") {
                for proxy in GH_PROXIES {
                    v.push(format!("{proxy}{base}"));
                }
            }
            v.push(base);
            v
        }
    };
    // det / rec：按 base 顺序试
    for (name, rel) in [(DET_FILE, "/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx"),
                        (REC_FILE, "/PP-OCRv4/ch_PP-OCRv4_rec_infer.onnx")] {
        let dest = dir.join(name);
        if valid_model(&dest) {
            continue;
        }
        let mut last_err = String::new();
        let mut ok = false;
        for base in &bases {
            let url = format!("{base}{rel}");
            match fetch_to_file(&url, &dest, name) {
                Ok(()) => {
                    if valid_model(&dest) {
                        ok = true;
                        break;
                    }
                    last_err = format!("{name} 下载后校验失败");
                }
                Err(e) => last_err = e,
            }
        }
        if !ok {
            return Err(format!("{name} 下载失败：{last_err}"));
        }
    }
    // 字典：自定义 base 先找同目录 keys.txt，再走 GitHub 镜像链
    let keys_dest = dir.join(KEYS_FILE);
    if !valid_keys(&keys_dest) {
        let mut candidates: Vec<String> = Vec::new();
        if source == Source::Custom {
            candidates.push(format!("{}/keys.txt", custom_base.trim().trim_end_matches('/')));
        }
        for proxy in GH_PROXIES {
            candidates.push(format!("{proxy}{KEYS_GH}"));
        }
        candidates.push(KEYS_GH.to_string());
        let mut last_err = String::new();
        let mut ok = false;
        for url in &candidates {
            match fetch_to_file(url, &keys_dest, KEYS_FILE) {
                Ok(()) if valid_keys(&keys_dest) => {
                    ok = true;
                    break;
                }
                Ok(()) => last_err = "字典下载后内容异常".into(),
                Err(e) => last_err = e,
            }
        }
        if !ok {
            return Err(format!("字典下载失败：{last_err}（也可以手动导入）"));
        }
    }
    let st = status();
    if !st.installed {
        return Err("下载完成但文件不完整，请改用本地导入".into());
    }
    Ok(())
}

fn valid_model(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 1_000_000).unwrap_or(false)
}

fn valid_keys(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 1000).unwrap_or(false)
}

/// 从目录导入（用户自行下载的三个文件）。
pub fn install_from_dir(src: &Path) -> Result<(), String> {
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let mut missing = Vec::new();
    for name in [DET_FILE, REC_FILE, KEYS_FILE] {
        let from = src.join(name);
        if !from.exists() {
            missing.push(name);
            continue;
        }
        std::fs::copy(&from, dir.join(name)).map_err(|e| format!("复制 {name} 失败：{e}"))?;
    }
    // 允许用户起别的名字：在目录里按大小猜
    if !missing.is_empty() {
        if let Ok(rd) = std::fs::read_dir(src) {
            for entry in rd.flatten() {
                let p = entry.path();
                let is_onnx = p.extension().map(|e| e.eq_ignore_ascii_case("onnx")).unwrap_or(false);
                if !is_onnx {
                    continue;
                }
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let target = if (3_000_000..7_000_000).contains(&len) { DET_FILE }
                    else if len > 7_000_000 { REC_FILE } else { continue };
                if missing.contains(&target) {
                    std::fs::copy(&p, dir.join(target)).map_err(|e| format!("复制失败：{e}"))?;
                    missing.retain(|m| *m != target);
                }
            }
        }
    }
    if !missing.is_empty() {
        return Err(format!("缺少文件：{}", missing.join(" / ")));
    }
    let st = status();
    if st.installed { Ok(()) } else { Err("文件大小不对，可能不是 PP-OCRv4 模型".into()) }
}

// ---------------------------------------------------------------- HTTP

/// 下载到 `.part` 再改名：中途断掉不会留下半个"看起来已安装"的模型。
fn fetch_to_file(url: &str, dest: &Path, label: &str) -> Result<(), String> {
    let part = dest.with_extension("part");
    stage(format!("下载 {label}"));
    DONE.store(0, Ordering::SeqCst);
    TOTAL.store(0, Ordering::SeqCst);
    let f = std::fs::File::create(&part).map_err(|e| format!("建文件失败：{e}"))?;
    let mut w = std::io::BufWriter::new(f);
    let mut written: u64 = 0;
    let mut total: u64 = 0;
    let res = http_get(url, |chunk, t| {
        use std::io::Write;
        if w.write_all(chunk).is_err() {
            return false;
        }
        written += chunk.len() as u64;
        DONE.store(written, Ordering::SeqCst);
        if t > 0 {
            total = t;
            TOTAL.store(t, Ordering::SeqCst);
        }
        true
    });
    {
        use std::io::Write;
        let _ = w.flush();
    }
    drop(w);
    let cleanup = |e: String| {
        let _ = std::fs::remove_file(&part);
        e
    };
    res.map_err(cleanup)?;
    // 长度对不上（连接被掐断最常见）就别改名——否则会留下"看起来装好了"的半截模型
    if total > 0 && written < total {
        return Err(cleanup(format!("{label} 下载不完整：{written}/{total} 字节")));
    }
    if written == 0 {
        return Err(cleanup(format!("{label} 下载到 0 字节")));
    }
    std::fs::rename(&part, dest).map_err(|e| cleanup(format!("改名失败：{e}")))?;
    Ok(())
}

/// 拆 `https://host[:port]/path`。
fn split_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (host_port.to_string(), 443u16),
    };
    Some((host, port, path.to_string()))
}

fn http_get(url: &str, mut on_data: impl FnMut(&[u8], u64) -> bool) -> Result<(), String> {
    let (host, port, path) = split_url(url).ok_or_else(|| format!("不支持的地址：{url}"))?;
    let agent = wide("Zhuxia/0.1 (OCR model download)");
    let host_w = wide(&host);
    let path_w = wide(&path);
    unsafe {
        let session = WinHttpOpen(PCWSTR(agent.as_ptr()),
                                  WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                                  PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err("WinHTTP 初始化失败".into());
        }
        let close = |h: *mut core::ffi::c_void| {
            if !h.is_null() { let _ = WinHttpCloseHandle(h); }
        };
        let _ = WinHttpSetTimeouts(session, 4000, 4000, 15000, 30000);
        let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), port, 0);
        if connect.is_null() {
            close(session);
            return Err(format!("连接 {host} 失败"));
        }
        let req = WinHttpOpenRequest(connect, windows::core::w!("GET"), PCWSTR(path_w.as_ptr()),
                                     PCWSTR::null(), PCWSTR::null(), std::ptr::null(),
                                     WINHTTP_FLAG_SECURE);
        if req.is_null() {
            close(connect);
            close(session);
            return Err("创建请求失败".into());
        }
        // 跟随重定向（HuggingFace 会 302 到 CDN）
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS.to_le_bytes();
        let _ = WinHttpSetOption(Some(req as *const core::ffi::c_void),
                                 WINHTTP_OPTION_REDIRECT_POLICY, Some(&policy));
        let sent = WinHttpSendRequest(req, None, None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(req, std::ptr::null_mut()).is_ok();
        if !sent {
            let err = windows::Win32::Foundation::GetLastError();
            close(req);
            close(connect);
            close(session);
            return Err(format!("请求失败：{}", err.0));
        }
        // 状态码
        let mut code: u32 = 0;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(req,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(), Some(&mut code as *mut u32 as *mut _), &mut len, std::ptr::null_mut());
        if code != 200 {
            close(req);
            close(connect);
            close(session);
            return Err(format!("HTTP {code}"));
        }
        // 总长度（拿不到就 0）
        let mut total: u32 = 0;
        let mut tl = 4u32;
        let _ = WinHttpQueryHeaders(req,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(), Some(&mut total as *mut u32 as *mut _), &mut tl, std::ptr::null_mut());
        let total = total as u64;
        let mut buf = vec![0u8; 64 * 1024];
        let mut failed: Option<String> = None;
        loop {
            let mut avail: u32 = 0;
            if let Err(e) = WinHttpQueryDataAvailable(req, &mut avail) {
                failed = Some(format!("读取失败：{e}"));
                break;
            }
            if avail == 0 {
                break;
            }
            let want = (avail as usize).min(buf.len());
            let mut got: u32 = 0;
            if let Err(e) = WinHttpReadData(req, buf.as_mut_ptr() as *mut _, want as u32, &mut got) {
                failed = Some(format!("接收失败：{e}"));
                break;
            }
            if got == 0 {
                break;
            }
            if !on_data(&buf[..got as usize], total) {
                failed = Some("写入本地文件失败".into());
                break;
            }
        }
        close(req);
        close(connect);
        close(session);
        if let Some(e) = failed {
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_https_urls() {
        let (h, p, path) = split_url("https://hf-mirror.com/a/b.onnx").unwrap();
        assert_eq!((h.as_str(), p, path.as_str()), ("hf-mirror.com", 443, "/a/b.onnx"));
        let (h, p, _) = split_url("https://example.com:8443/x").unwrap();
        assert_eq!((h.as_str(), p), ("example.com", 8443));
        assert!(split_url("ftp://x/y").is_none());
    }

    #[test]
    fn status_is_empty_on_fresh_dir() {
        // 不触碰真实目录：只验证构造逻辑
        let st = Status::default();
        assert!(!st.installed);
    }
}
