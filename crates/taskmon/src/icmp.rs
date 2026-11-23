//! ICMP 延迟探测（本地网关 / 公网）。

use std::net::ToSocketAddrs;
use windows::Win32::NetworkManagement::IpHelper::{IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho};

pub fn ping_ms(host: &str, timeout_ms: u32) -> Option<i64> {
    let addr = (host, 0).to_socket_addrs().ok()?.next()?;
    let ip = match addr.ip() {
        std::net::IpAddr::V4(v4) => v4,
        _ => return None,
    };
    unsafe {
        let h = IcmpCreateFile().ok()?;
        let payload = [0u8; 32];
        let mut reply = vec![0u8; 32 + 8 + std::mem::size_of::<usize>() * 8 + 64];
        let n = IcmpSendEcho(h, u32::from_ne_bytes(ip.octets()), payload.as_ptr() as *const _, payload.len() as u16,
                             None, reply.as_mut_ptr() as *mut _, reply.len() as u32, timeout_ms);
        let _ = IcmpCloseHandle(h);
        if n == 0 { return None; }
        // ICMP_ECHO_REPLY（x64 自然对齐）：Address@0, Status@4, RoundTripTime@8
        let status = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]);
        if status != 0 { return None; }
        let rtt = u32::from_le_bytes([reply[8], reply[9], reply[10], reply[11]]);
        Some(rtt as i64)
    }
}
