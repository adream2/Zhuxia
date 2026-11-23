//! 网络：GetIfTable2 选网卡 + 累计字节增量算实时速率（TrafficMonitor 的选卡规则）。
//! 连接详情（连接类型 / 协商速率 / SSID / 标准 / 本机与公网 IP / 延迟）在后台线程轮询。

use std::collections::VecDeque;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

use crate::snapshot::NetInfo;

const MAX_HIST: usize = 60;
const SILENT_RESELECT_SECS: f64 = 30.0;
const PREFERRED_CHECK_TICKS: u32 = 30;

const VIRTUAL_KEYWORDS: &[&str] = &[
    "vmware", "virtualbox", "vbox", "hyper-v", "vethernet", "wsl", "docker",
    "tunnel", "teredo", "isatap", "6to4", "loopback", "pseudo", "tap", "virtual", "vpn",
    "环回", "隧道", "虚拟",
];

#[derive(Clone, Default)]
pub struct Adapter {
    pub luid: u64,
    pub id: String,
    pub name: String,
    pub alias: String,
    pub link_tx: u64,
    pub link_rx: u64,
    pub in_octets: u64,
    pub out_octets: u64,
    pub is_wifi: bool,
}

pub struct NetSample {
    pub up: i64,
    pub down: i64,
    pub name: String,
    pub up_hist: Vec<i64>,
    pub down_hist: Vec<i64>,
    pub adapter: Option<Adapter>,
}

pub struct NetSampler {
    selected: Option<Adapter>,
    preferred: Option<String>,
    has_prev: bool,
    prev_in: u64,
    prev_out: u64,
    last: Option<std::time::Instant>,
    silent_secs: f64,
    ticks_since_pref: u32,
    up_hist: VecDeque<i64>,
    down_hist: VecDeque<i64>,
}

impl NetSampler {
    pub fn new() -> NetSampler {
        let mut s = NetSampler {
            selected: None, preferred: None, has_prev: false,
            prev_in: 0, prev_out: 0, last: None, silent_secs: 0.0,
            ticks_since_pref: PREFERRED_CHECK_TICKS,
            up_hist: VecDeque::with_capacity(MAX_HIST), down_hist: VecDeque::with_capacity(MAX_HIST),
        };
        s.select();
        s
    }

    pub fn current(&self) -> Option<Adapter> { self.selected.clone() }

    fn select(&mut self) {
        let all = enumerate();
        if let Some(pref) = self.preferred.clone() {
            if let Some(a) = all.iter().find(|a| a.id == pref) {
                self.apply(a.clone());
                self.ticks_since_pref = 0;
                return;
            }
        }
        let best = all.into_iter()
            .filter(|a| !a.is_wifi || true)
            .filter(|a| !is_virtual(a))
            .max_by_key(|a| a.in_octets + a.out_octets);
        match best {
            Some(a) => self.apply(a),
            None => { self.selected = None; self.has_prev = false; }
        }
    }

    fn apply(&mut self, a: Adapter) {
        self.selected = Some(a);
        self.has_prev = false;
        self.silent_secs = 0.0;
        self.up_hist.clear();
        self.down_hist.clear();
    }

    pub fn sample(&mut self, preferred: Option<String>) -> NetSample {
        let pref = preferred.filter(|s| !s.is_empty());
        if pref != self.preferred {
            self.preferred = pref;
            self.selected = None;
            self.has_prev = false;
        }
        if self.selected.is_none() {
            self.select();
            if self.selected.is_none() {
                return NetSample {
                    up: 0, down: 0, name: "未连接".into(),
                    up_hist: self.up_hist.iter().copied().collect(),
                    down_hist: self.down_hist.iter().copied().collect(),
                    adapter: None,
                };
            }
        } else if self.preferred.is_some() {
            let pinned = self.preferred.clone().unwrap_or_default();
            if self.selected.as_ref().map(|a| a.id == pinned).unwrap_or(false) {
                // 钉住的网卡拔了/禁用 → 回落到自动
                if !self.selected.as_ref().map(a_link_up_today).unwrap_or(false) {
                    self.select();
                }
            } else {
                self.ticks_since_pref += 1;
                if self.ticks_since_pref >= PREFERRED_CHECK_TICKS {
                    self.ticks_since_pref = 0;
                    self.select();
                }
            }
        }

        // 只读所选网卡那一行（GetIfEntry2），不要每 tick 重建整张表。
        let Some(cur) = self.selected.clone() else {
            return NetSample { up: 0, down: 0, name: "未连接".into(), up_hist: vec![], down_hist: vec![], adapter: None };
        };
        let fresh = read_one(cur.luid).or_else(|| enumerate().into_iter().find(|a| a.id == cur.id));
        let Some(now_a) = fresh else {
            self.select();
            return NetSample {
                up: 0, down: 0, name: self.selected.as_ref().map(|a| a.name.clone()).unwrap_or("未连接".into()),
                up_hist: self.up_hist.iter().copied().collect(),
                down_hist: self.down_hist.iter().copied().collect(),
                adapter: self.selected.clone(),
            };
        };
        self.selected = Some(now_a.clone());

        if !self.has_prev {
            self.prev_in = now_a.in_octets;
            self.prev_out = now_a.out_octets;
            self.has_prev = true;
            self.last = Some(std::time::Instant::now());
            return NetSample {
                up: 0, down: 0, name: now_a.name.clone(),
                up_hist: self.up_hist.iter().copied().collect(),
                down_hist: self.down_hist.iter().copied().collect(),
                adapter: Some(now_a),
            };
        }

        let now = std::time::Instant::now();
        let dt = self.last.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        self.last = Some(now);
        if dt <= 0.0 {
            return NetSample { up: 0, down: 0, name: now_a.name.clone(), up_hist: vec![], down_hist: vec![], adapter: Some(now_a) };
        }
        let d_in = now_a.in_octets.saturating_sub(self.prev_in);
        let d_out = now_a.out_octets.saturating_sub(self.prev_out);
        self.prev_in = now_a.in_octets;
        self.prev_out = now_a.out_octets;
        let up = (d_out as f64 / dt) as i64;
        let down = (d_in as f64 / dt) as i64;

        if d_in == 0 && d_out == 0 {
            self.silent_secs += dt;
            if self.preferred.is_none() && self.silent_secs >= SILENT_RESELECT_SECS {
                self.select();
                return NetSample {
                    up: 0, down: 0, name: self.selected.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                    up_hist: self.up_hist.iter().copied().collect(),
                    down_hist: self.down_hist.iter().copied().collect(),
                    adapter: self.selected.clone(),
                };
            }
        } else {
            self.silent_secs = 0.0;
        }

        if self.up_hist.len() >= MAX_HIST { self.up_hist.pop_front(); }
        if self.down_hist.len() >= MAX_HIST { self.down_hist.pop_front(); }
        self.up_hist.push_back(up);
        self.down_hist.push_back(down);

        NetSample {
            up, down, name: now_a.name.clone(),
            up_hist: self.up_hist.iter().copied().collect(),
            down_hist: self.down_hist.iter().copied().collect(),
            adapter: Some(now_a),
        }
    }
}

impl Default for NetSampler {
    fn default() -> Self {
        Self::new()
    }
}

fn a_link_up_today(a: &Adapter) -> bool {
    a.id.is_empty() || enumerate().iter().any(|x| x.id == a.id)
}

fn read_one(luid: u64) -> Option<Adapter> {
    use windows::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
    unsafe {
        let mut row = MIB_IF_ROW2::default();
        row.InterfaceLuid.Value = luid;
        if GetIfEntry2(&mut row).is_err() { return None; }
        Some(Adapter {
            luid,
            id: guid_string(&row.InterfaceGuid),
            name: wide_str(&row.Description),
            alias: wide_str(&row.Alias),
            link_tx: row.TransmitLinkSpeed,
            link_rx: row.ReceiveLinkSpeed,
            in_octets: row.InOctets,
            out_octets: row.OutOctets,
            is_wifi: row.Type == 71,
        })
    }
}

pub fn enumerate() -> Vec<Adapter> {
    let mut out = Vec::new();
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() { return out; }
        let n = (*table).NumEntries as usize;
        let rows = std::ptr::addr_of!((*table).Table) as *const windows::Win32::NetworkManagement::IpHelper::MIB_IF_ROW2;
        for i in 0..n {
            let r = &*rows.add(i);
            if r.OperStatus != IfOperStatusUp { continue; }
            if r.Type == 24 { continue; } // IF_TYPE_SOFTWARE_LOOPBACK
            // 只要真实硬件网卡：否则 Hyper-V/WFP 的 Filter 接口也会混进适配器下拉里
            if r.InterfaceAndOperStatusFlags._bitfield & 0x01 == 0 { continue; }
            out.push(Adapter {
                luid: r.InterfaceLuid.Value,
                id: guid_string(&r.InterfaceGuid),
                name: wide_str(&r.Description),
                alias: wide_str(&r.Alias),
                link_tx: r.TransmitLinkSpeed,
                link_rx: r.ReceiveLinkSpeed,
                in_octets: r.InOctets,
                out_octets: r.OutOctets,
                is_wifi: r.Type == 71,
            });
        }
        FreeMibTable(table as *const _);
    }
    out
}

fn is_virtual(a: &Adapter) -> bool {
    let d = a.name.to_lowercase();
    let n = a.alias.to_lowercase();
    VIRTUAL_KEYWORDS.iter().any(|k| d.contains(k) || n.contains(k))
}

fn wide_str(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

pub fn guid_string(g: &windows::core::GUID) -> String {
    format!("{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3],
        g.data4[4], g.data4[5], g.data4[6], g.data4[7])
}

/// 后台线程：连接信息（类型/速率/SSID/IP/延迟/公网 IP）。1Hz 轮询，绝不阻塞采样线程。
pub struct NetInfoPoller {
    pub latest: NetInfo,
    inner: std::sync::Arc<std::sync::Mutex<NetInfo>>,
}

impl NetInfoPoller {
    pub fn start() -> NetInfoPoller {
        let inner = std::sync::Arc::new(std::sync::Mutex::new(NetInfo::default()));
        let slot = inner.clone();
        std::thread::Builder::new().name("zx-netinfo".into()).spawn(move || {
            let mut tick: u32 = 0;
            let mut public_v4: Option<String> = None;
            let mut public_v6: Option<String> = None;
            let mut last_public: u32 = 0;
            let mut last_http_ok = 0u32;
            let mut cached_adapter: Option<Adapter> = None;
            let mut last_gw: i64 = -1;
            let mut last_pub: i64 = -1;
            // Wi-Fi 详情缓存：(适配器 Id, 结果, 抓取时的 tick)。
            // wlanapi 的调用在 Win10+ 是位置相关的（会点亮定位图标），
            // 面板开着也不该 1Hz 去问——上游是 5 分钟一次，这里照做。
            let mut wifi_cache: Option<(String, WifiDetails, u32)> = None;
            const WIFI_CACHE_TICKS: u32 = 300;
            loop {
                tick = tick.wrapping_add(1);
                // 网卡枚举每 5 秒一次；其余 tick 复用上一轮结果
                if tick % 5 == 1 || cached_adapter.is_none() {
                    cached_adapter = enumerate().into_iter().max_by_key(|a| a.in_octets + a.out_octets);
                }
                let adapter = cached_adapter.clone();

                // 1) 快速字段先发布：类型 / 协商速率 / 本机 IPv4（不等 HTTP 与 ping）
                let mut info = NetInfo::default();
                if let Some(a) = &adapter {
                    info.connection_type = if a.is_wifi { "WLAN".into() } else { "有线".into() };
                    info.link_rx_bps = a.link_rx as i64;
                    info.link_tx_bps = a.link_tx as i64;
                    info.local_ip = local_ipv4(a);
                }
                info.public_ip = public_v4.clone();
                info.public_ip_v6 = public_v6.clone();
                if let Ok(mut g) = slot.lock() { *g = info.clone(); }

                // 2) Wi-Fi 详情（wlanapi，较慢，5 分钟内复用缓存）
                if let Some(a) = &adapter {
                    if a.is_wifi {
                        let fresh = matches!(&wifi_cache,
                            Some((id, _, t)) if id == &a.id && tick.wrapping_sub(*t) < WIFI_CACHE_TICKS);
                        if !fresh {
                            let w = wifi_details(a);
                            wifi_cache = Some((a.id.clone(), w, tick));
                        }
                        if let Some((_, w, _)) = &wifi_cache {
                            info.ssid = w.ssid.clone();
                            info.standard = w.standard.clone();
                            info.band = w.band.clone();
                            info.width = w.width.clone();
                        }
                    }
                }

                // 3) 公网 IP：5 分钟一次，失败 60 秒后重试
                if tick == 1 || tick.wrapping_sub(last_public) > 300 || (public_v4.is_none() && tick.wrapping_sub(last_http_ok) > 60) {
                    last_public = tick;
                    // 端点按大陆可达性排序，逐个回退（它们都只回一行纯文本）。
                    for url in ["https://ipv4.icanhazip.com", "https://4.ipw.cn", "https://api.ipify.org"] {
                        if let Some(v) = crate::http::get_text(url, 3000) {
                            let v = v.trim().to_string();
                            if !v.is_empty() && !v.contains('<') && v.contains('.') {
                                public_v4 = Some(v);
                                last_http_ok = tick;
                                break;
                            }
                        }
                    }
                    for url in ["https://ipv6.icanhazip.com", "https://6.ipw.cn", "https://api6.ipify.org"] {
                        if let Some(v) = crate::http::get_text(url, 2500) {
                            let v = v.trim().to_string();
                            if v.contains(':') { public_v6 = Some(v); break; }
                        }
                    }
                    info.public_ip = public_v4.clone();
                    info.public_ip_v6 = public_v6.clone();
                }

                // 4) 延迟：本地网关 + 公网
                // 延迟每 3 秒测一次，其余 tick 复用（ping 是纯等待，但没必要 1Hz）
                if tick % 3 == 1 {
                    last_gw = gateway_ip().and_then(|g| crate::icmp::ping_ms(&g, 800)).unwrap_or(-1);
                    last_pub = crate::icmp::ping_ms("www.baidu.com", 1000).unwrap_or(-1);
                }
                info.gateway_rtt_ms = last_gw;
                info.public_rtt_ms = last_pub;

                if let Ok(mut g) = slot.lock() { *g = info; }
                std::thread::sleep(std::time::Duration::from_millis(1000));
            }
        }).ok();
        NetInfoPoller { latest: NetInfo::default(), inner }
    }

    pub fn get(&self) -> NetInfo { self.inner.lock().map(|g| g.clone()).unwrap_or_default() }
}

/// 本机 IPv4 + 默认网关（同一次 GetAdaptersAddresses）。
pub fn adapter_addrs(desc: &str) -> (Option<String>, Option<String>) {
    use windows::Win32::NetworkManagement::IpHelper::{GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS};
    use windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_ADDRESSES_LH;
    let mut size: u32 = 32 * 1024;
    for _ in 0..3 {
        let mut buf = vec![0u8; size as usize];
        let rc = unsafe {
            GetAdaptersAddresses(2 /* AF_INET */, GAA_FLAG_INCLUDE_GATEWAYS,
                                 None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size)
        };
        if rc == 111 { continue; } // ERROR_BUFFER_OVERFLOW
        if rc != 0 { return (None, None); }
        unsafe {
            let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
            let mut best: (Option<String>, Option<String>) = (None, None);
            while !p.is_null() {
                let cur = &*p;
                let name = if cur.Description.0.is_null() {
                    String::new()
                } else {
                    wide_str(std::slice::from_raw_parts(cur.Description.0, 256))
                };
                let mut ip = None;
                let mut gw = None;
                let mut ua = cur.FirstUnicastAddress;
                while !ua.is_null() {
                    let u = &*ua;
                    if let Some(a) = sockaddr_v4(u.Address.lpSockaddr) { ip = Some(a); break; }
                    ua = u.Next;
                }
                let mut ga = cur.FirstGatewayAddress;
                while !ga.is_null() {
                    let g = &*ga;
                    if let Some(a) = sockaddr_v4(g.Address.lpSockaddr) { gw = Some(a); break; }
                    ga = g.Next;
                }
                if name == desc { return (ip, gw); }
                if best.0.is_none() && (ip.is_some() || gw.is_some()) { best = (ip, gw); }
                p = cur.Next;
            }
            return best;
        }
    }
    (None, None)
}

fn local_ipv4(a: &Adapter) -> Option<String> { adapter_addrs(&a.name).0 }

fn gateway_ip() -> Option<String> {
    let a = enumerate().into_iter().max_by_key(|x| x.in_octets + x.out_octets)?;
    adapter_addrs(&a.name).1
}

unsafe fn sockaddr_v4(sa: *const windows::Win32::Networking::WinSock::SOCKADDR) -> Option<String> {
    if sa.is_null() { return None; }
    let family = (*sa).sa_family;
    if family.0 != windows::Win32::Networking::WinSock::AF_INET.0 { return None; }
    let p = sa as *const u8;
    Some(format!("{}.{}.{}.{}", *p.add(4), *p.add(5), *p.add(6), *p.add(7)))
}

/// Wi-Fi 详情：SSID / 802.11 标准 / 频段 / 信道宽度。
#[derive(Clone, Default)]
pub struct WifiDetails {
    pub ssid: Option<String>,
    pub standard: Option<String>,
    pub band: Option<String>,
    pub width: Option<String>,
}

/// 仅在网络详情面板打开期间置位。
///
/// `wlanapi` 的这几个调用**在 Win10+ 上是位置相关的** —— 一调就会点亮任务栏的定位图标。
/// 上游因此把整组查询挪到"面板打开时"，空闲轮询一次都不碰。这里照做：面板关着走缓存，
/// 绝不触发定位。
static WIFI_WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_wifi_wanted(on: bool) {
    WIFI_WANTED.store(on, std::sync::atomic::Ordering::Relaxed);
}
fn wifi_wanted() -> bool { WIFI_WANTED.load(std::sync::atomic::Ordering::Relaxed) }

fn wifi_details(a: &Adapter) -> WifiDetails {
    use windows::Win32::NetworkManagement::WiFi::*;
    let mut out = WifiDetails::default();
    if !wifi_wanted() { return out; }
    unsafe {
        let mut negotiated = 0u32;
        let mut handle = windows::Win32::Foundation::HANDLE::default();
        if WlanOpenHandle(2, None, &mut negotiated, &mut handle) != 0 { return out; }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
            let _ = WlanCloseHandle(handle, None);
            return out;
        }
        let n = (*list).dwNumberOfItems as usize;
        let items = std::ptr::addr_of!((*list).InterfaceInfo) as *const WLAN_INTERFACE_INFO;
        for i in 0..n {
            let it = &*items.add(i);
            if guid_string(&it.InterfaceGuid) != a.id { continue; }
            let mut size = 0u32;
            let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut opcode_type = WLAN_OPCODE_VALUE_TYPE(0);
            let rc = WlanQueryInterface(handle, &it.InterfaceGuid, wlan_intf_opcode_current_connection,
                                        None, &mut size, &mut data, Some(&mut opcode_type));
            if rc == 0 && !data.is_null() {
                let attr = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                let s = &attr.wlanAssociationAttributes.dot11Ssid;
                let len = s.uSSIDLength.min(32) as usize;
                out.ssid = Some(String::from_utf8_lossy(&s.ucSSID[..len]).to_string());
                out.standard = Some(match attr.wlanAssociationAttributes.dot11PhyType.0 {
                    4 => "Wi-Fi 4 (802.11n)",
                    5 => "Wi-Fi 5 (802.11ac)",
                    8 => "Wi-Fi 6 (802.11ax)",
                    9 => "Wi-Fi 7 (802.11be)",
                    6 => "802.11ad",
                    7 => "802.11ax",
                    _ => "—",
                }.to_string());
                // 频段 / 信道宽度只能从所连 BSS 的条目里拿（连接属性里没有）
                let bssid = attr.wlanAssociationAttributes.dot11Bssid;
                let (band, width) = bss_band_width(handle, &it.InterfaceGuid, bssid);
                out.band = band;
                out.width = width;
                WlanFreeMemory(data);
            }
        }
        WlanFreeMemory(list as *const _);
        let _ = WlanCloseHandle(handle, None);
    }
    out
}

/// 在驱动缓存的 BSS 列表里找到"当前连着的那个 AP"，读出中心频率与信道宽度。
///
/// 坑：`dot11_BSS_type` 的 any 是 3 —— 传 0 得到 ERROR_INVALID_PARAMETER，而且失败是静默的
/// （上游踩过，这里沿用结论）。
fn bss_band_width(
    handle: windows::Win32::Foundation::HANDLE,
    guid: &windows::core::GUID,
    bssid: [u8; 6],
) -> (Option<String>, Option<String>) {
    use windows::Win32::NetworkManagement::WiFi::*;
    let mut band = None;
    let mut width = None;
    unsafe {
        let mut list: *mut WLAN_BSS_LIST = std::ptr::null_mut();
        let rc = WlanGetNetworkBssList(handle, guid, None, DOT11_BSS_TYPE(3), false, None, &mut list);
        if rc != 0 || list.is_null() { return (band, width); }
        let count = (*list).dwNumberOfItems as usize;
        let base = std::ptr::addr_of!((*list).wlanBssEntries) as *const WLAN_BSS_ENTRY;
        for i in 0..count {
            let e = &*base.add(i);
            if e.dot11Bssid != bssid { continue; }
            band = Some(band_of(e.ulChCenterFrequency).to_string());
            // IE 偏移相对**条目起点**，从条目指针加偏移就能摸到信标里的信息元素
            let ie_off = e.ulIeOffset as usize;
            let ie_size = e.ulIeSize as usize;
            if ie_off >= std::mem::size_of::<WLAN_BSS_ENTRY>() && ie_size > 0 && ie_size <= 4096 {
                let blob = std::slice::from_raw_parts(
                    (e as *const WLAN_BSS_ENTRY as *const u8).add(ie_off), ie_size);
                if let Some(w) = channel_width(blob) { width = Some(w); }
            }
            break;
        }
        WlanFreeMemory(list as *const _);
    }
    (band, width)
}

fn band_of(center_khz: u32) -> &'static str {
    if center_khz == 0 { return "—"; }
    if center_khz < 3_000_000 { "2.4 GHz" }
    else if center_khz < 5_925_000 { "5 GHz" }
    else { "6 GHz" }
}

/// 从信标/探测响应的信息元素里解析信道宽度。
///
/// `HT Operation`(61) 只说 20/40，`VHT Operation`(192) 才给出 80/160/80+80。
/// 两者都没有（纯 802.11b/g）就是 20 MHz。
fn channel_width(ies: &[u8]) -> Option<String> {
    let mut ht_40 = false;
    let mut i = 0usize;
    while i + 2 <= ies.len() {
        let id = ies[i];
        let len = ies[i + 1] as usize;
        let body = &ies[i + 2..(i + 2 + len).min(ies.len())];
        match id {
            61 if body.len() >= 2 => {
                // STA channel width: bit1；secondary offset: bits2-3（0=no, 1=above, 3=below）
                let overall = body[0] & 0x02 != 0;
                let offset = (body[0] >> 2) & 0x03;
                ht_40 = overall && offset != 0;
            }
            192 if !body.is_empty() => {
                return Some(match body[0] {
                    1 => "80 MHz".to_string(),
                    2 => "160 MHz".to_string(),
                    3 => "80+80 MHz".to_string(),
                    _ => if ht_40 { "40 MHz".to_string() } else { "20 MHz".to_string() },
                });
            }
            _ => {}
        }
        i += 2 + len;
    }
    Some(if ht_40 { "40 MHz" } else { "20 MHz" }.to_string())
}
