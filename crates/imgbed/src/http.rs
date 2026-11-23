//! 最小可用的 WinHTTP 客户端：GitHub REST 只需要 GET / POST / PUT / PATCH + JSON。
//!
//! 为什么不用 reqwest：常驻进程 + 单 exe 的取向里，TLS 栈（rustls/openssl）与异步运行时
//! 是纯成本；WinHTTP 是系统组件，零依赖、走系统 TLS、自动跟随代理设置。
//!
//! 代理是这里最容易被忽略的一项：WinHTTP 的 `AUTOMATIC_PROXY` 读的是 **WinHTTP** 的
//! 代理配置，不是 IE/WinINET 的那份（Clash 的"系统代理"改的是后者）。所以设置里留了
//! 手动代理；只支持 HTTP(S) CONNECT，SOCKS5 明确报错——WinHTTP 本来就不支持。

use windows::core::PCWSTR;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Networking::WinHttp::*;

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// `x-ratelimit-remaining`（403 限流时要靠它区分"权限不足"和"被限流"）
    pub rate_remaining: Option<u32>,
    /// `x-ratelimit-reset`（Unix 秒）
    pub rate_reset: Option<u64>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    proxy: &str,
    timeout_ms: u32,
) -> Result<Response, String> {
    let (secure, host, port, path) = split_url(url)?;
    let proxy = proxy.trim();
    if proxy.starts_with("socks") {
        return Err("WinHTTP 不支持 SOCKS 代理；请填 HTTP 代理地址（例如 http://127.0.0.1:7890）".into());
    }

    unsafe {
        let agent = zx_core::win::wide("Zhuxia/0.1 (imgbed)");
        let (access, proxy_w) = if proxy.is_empty() {
            (WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None)
        } else {
            // WinHttpOpen 的代理参数要 `host:port`，带 scheme 会被当成主机名。
            let p = proxy.trim_start_matches("http://").trim_start_matches("https://");
            (WINHTTP_ACCESS_TYPE_NAMED_PROXY, Some(zx_core::win::wide(p)))
        };
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            access,
            proxy_w.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr())),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err(format!("WinHttpOpen 失败（{}）", last_error()));
        }

        let _ = WinHttpSetTimeouts(session, 8_000, 8_000, timeout_ms as i32, timeout_ms as i32);

        let host_w = zx_core::win::wide(&host);
        let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), port, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return Err(format!("连接 {host} 失败（{}）", last_error()));
        }

        let verb = zx_core::win::wide(method);
        let path_w = zx_core::win::wide(&path);
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let req = WinHttpOpenRequest(
            connect,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        );
        if req.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return Err(format!("WinHttpOpenRequest 失败（{}）", last_error()));
        }

        let mut header_block: Vec<u16> = Vec::new();
        for (k, v) in headers {
            header_block.extend(format!("{k}: {v}").encode_utf16());
            header_block.extend([13u16, 10u16]);
        }
        // 结尾必须再补一个空行，否则最后一个头会被 WinHTTP 当成残缺
        header_block.extend([13u16, 10u16]);
        let hdr = Some(header_block.as_slice());

        let body_len = body.map(|b| b.len()).unwrap_or(0) as u32;
        let sent = WinHttpSendRequest(
            req,
            hdr,
            body.map(|b| b.as_ptr() as *const _),
            body_len,
            body_len,
            0,
        );

        let mut out = Err(format!("发送请求失败（{}）", last_error()));
        if sent.is_ok() && WinHttpReceiveResponse(req, std::ptr::null_mut()).is_ok() {
            let status = status_code(req).unwrap_or(0);
            let mut body: Vec<u8> = Vec::new();
            loop {
                let mut buf = [0u8; 16 * 1024];
                let mut got = 0u32;
                if WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut got).is_err() {
                    break;
                }
                if got == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..got as usize]);
                // GitHub 的错误响应都很小；这个上限只是防止对端吐无限数据。
                if body.len() > 8 * 1024 * 1024 {
                    break;
                }
            }
            out = Ok(Response {
                status,
                body,
                rate_remaining: header_num(req, "x-ratelimit-remaining"),
                rate_reset: header_num(req, "x-ratelimit-reset").map(u64::from),
            });
        }

        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);
        out
    }
}

fn status_code(req: *mut core::ffi::c_void) -> Option<u16> {
    unsafe {
        let mut code = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            req,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut _ as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .ok()?;
        Some(code as u16)
    }
}

/// 读一个响应头（`WINHTTP_QUERY_CUSTOM` + 头名）。不存在时返回 None。
fn header_num(req: *mut core::ffi::c_void, name: &str) -> Option<u32> {
    let name_w = zx_core::win::wide(name);
    unsafe {
        let mut len = 0u32;
        // 第一次只问长度：必然返回 ERROR_INSUFFICIENT_BUFFER，长度里是所需字节数。
        let _ = WinHttpQueryHeaders(
            req,
            WINHTTP_QUERY_CUSTOM,
            PCWSTR(name_w.as_ptr()),
            None,
            &mut len,
            std::ptr::null_mut(),
        );
        if len == 0 || len > 4096 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize / 2) + 1];
        let mut len2 = (buf.len() * 2) as u32;
        WinHttpQueryHeaders(
            req,
            WINHTTP_QUERY_CUSTOM,
            PCWSTR(name_w.as_ptr()),
            Some(buf.as_mut_ptr() as *mut _),
            &mut len2,
            std::ptr::null_mut(),
        )
        .ok()?;
        let used = (len2 as usize / 2).min(buf.len());
        let text: String = String::from_utf16_lossy(&buf[..used]).trim().to_string();
        text.parse::<u32>().ok()
    }
}

fn last_error() -> String {
    unsafe {
        let e = GetLastError();
        if e.0 == 0 {
            "系统未给出错误码".into()
        } else {
            format!("Win32 {}", e.0)
        }
    }
}

fn split_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("不支持的 URL：{url}"));
    };
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        // 只处理纯数字端口；IPv6 字面量在这种场景用不到
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().map_err(|_| "端口不合法".to_string())?)
        }
        _ => (host_port.to_string(), if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("URL 缺少主机名：{url}"));
    }
    Ok((secure, host, port, path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::split_url;

    #[test]
    fn splits_https_with_path_and_port() {
        let (s, h, p, path) = split_url("https://api.github.com/repos/a/b/contents/x.png").unwrap();
        assert!(s);
        assert_eq!(h, "api.github.com");
        assert_eq!(p, 443);
        assert_eq!(path, "/repos/a/b/contents/x.png");

        let (s, h, p, _) = split_url("http://127.0.0.1:7890/x").unwrap();
        assert!(!s);
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 7890);
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(split_url("ftp://x/y").is_err());
    }
}
