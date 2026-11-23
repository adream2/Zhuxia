//! 极简 WinHTTP 客户端：只做 GET 文本，用来取公网 IP 与 Clash 的 /connections。
//! 不走任何异步运行时（常驻工具不需要），单请求 2–3 秒超时。

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::*;

pub fn get_text(url: &str, timeout_ms: u32) -> Option<String> { get_with_headers(url, timeout_ms, &[]) }

pub fn get_with_headers(url: &str, timeout_ms: u32, headers: &[(&str, &str)]) -> Option<String> {
    let (secure, host, path) = split_url(url)?;
    unsafe {
        let agent = zx_core::win::wide("Zhuxia/0.1");
        let session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                                 PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() { return None; }
        let _ = WinHttpSetTimeouts(session, 1500, 1500, timeout_ms as i32, timeout_ms as i32);

        let host_w = zx_core::win::wide(&host);
        let port = if secure { 443 } else { 80 };
        let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), port, 0);
        if connect.is_null() { let _ = WinHttpCloseHandle(session); return None; }
        let verb = zx_core::win::wide("GET");
        let path_w = zx_core::win::wide(&path);
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let req = WinHttpOpenRequest(connect, PCWSTR(verb.as_ptr()), PCWSTR(path_w.as_ptr()),
                                     PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags);
        if req.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return None;
        }

        let mut header_block: Vec<u16> = Vec::new();
        for (k, v) in headers {
            header_block.extend(format!("{k}: {v}").encode_utf16());
            header_block.extend([13u16, 10u16]);
        }
        let hdr = if header_block.is_empty() { None } else { Some(header_block.as_slice()) };

        let mut out = None;
        let sent = WinHttpSendRequest(req, hdr, Some(std::ptr::null()), 0, 0, 0).is_ok();
        if sent && WinHttpReceiveResponse(req, std::ptr::null_mut()).is_ok() {
            let mut body: Vec<u8> = Vec::new();
            loop {
                let mut buf = [0u8; 8192];
                let mut got = 0u32;
                if WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut got).is_err() { break; }
                if got == 0 { break; }
                body.extend_from_slice(&buf[..got as usize]);
                if body.len() > 4 * 1024 * 1024 { break; }
            }
            if !body.is_empty() { out = Some(String::from_utf8_lossy(&body).to_string()); }
        }
        if out.is_none() && !url.contains("127.0.0.1") && !url.contains("localhost") {
            zx_core::log::warn_once(Box::leak(format!("http:{url}").into_boxed_str()), format!("WinHTTP GET 失败：{url} err={:?}", windows::Win32::Foundation::GetLastError()));
        }
        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);
        out
    }
}

fn split_url(url: &str) -> Option<(bool, String, String)> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        (false, url)
    };
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() { return None; }
    Some((secure, host.to_string(), path.to_string()))
}
