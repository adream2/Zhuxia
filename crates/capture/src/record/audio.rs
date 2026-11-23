//! 音频采集：WASAPI（系统声音 loopback + 麦克风）。
//!
//! 关键决定：
//!
//! 1. **用 WASAPI 而不是 Media Foundation 采音频**。MF 的 SourceReader 能采麦克风，
//!    但拿不到**系统正在播放的声音**——loopback 只有 WASAPI 有。麦克风也要用同一套，
//!    否则两路走不同时钟，混音必然漂移。
//! 2. **统一到 48kHz / 立体声 / 16bit** 再进编码器。屏幕录制的音频源五花八门
//!    （蓝牙耳机 16k 单声道、HDMI 48k 立体声、某些虚拟声卡 44.1k 浮点），
//!    先归一化，后面所有逻辑（混音、时间戳）就只用一种格式。
//!    首选让 Windows 自己转（`AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`），
//!    不给转就退回混音格式自己线性插值。
//! 3. **靠墙上时钟推进，不靠"有数据才写"**。系统静音时 loopback 一个字节都不给，
//!    等数据就会让音轨时长比画面短，越录越不同步。所以每 10ms 无条件产出一块，
//!    取不到数据就补静音——这也是 OBS 的做法。
//! 4. **音频线程自己 `CoInitializeEx`**。WASAPI 接口有 apartment 约束，
//!    在 A 线程创建、B 线程使用会随机失败。所以打开设备这件事也在音频线程里做。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

/// 输出格式：所有音频最终都是这个样子
pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// 一次产出的帧数 = 48k × 10ms。控制条/编码器都按这个粒度追加。
const BLOCK: usize = (RATE as usize) / 100;
/// 单路环形缓冲上限（2 秒）。超过就丢最旧的——宁可丢一点也不要无限涨内存。
const RING_CAP: usize = RATE as usize * 2 * CHANNELS;
/// 一圈循环最多补救的音频（50ms）。落后太多时不试图一次追平。
const MAX_CATCHUP_FRAMES: usize = RATE as usize / 20;

// windows-rs 0.58 没导出这几个 flag，按 audioclient.h 的定义补上。
/// 让音频引擎把任意 PCM 格式转成我们要求的格式
const AUTOCONVERTPCM: u32 = 0x8000_0000;
/// 自动转换时用引擎默认的重采样质量
const SRC_DEFAULT_QUALITY: u32 = 0x0800_0000;
/// WAVE_FORMAT_PCM
const FMT_PCM: u16 = 1;
/// WAVE_FORMAT_EXTENSIBLE
const FMT_EXTENSIBLE: u16 = 0xFFFE;

const CLSID_MMDEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
/// `PKEY_Device_FriendlyName`（Devices\FunctionDiscovery 里的那个）。
/// 本地定义，省掉一个 feature。
const PKEY_FRIENDLY_NAME: windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY =
    windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };

// ---------------------------------------------------------------- 配置

/// 这次录制要采什么。
#[derive(Clone, Debug, Default)]
pub struct Spec {
    /// 系统正在播放的声音（loopback）
    pub system: bool,
    /// 麦克风
    pub mic: bool,
    /// 麦克风设备 id；空 = 系统默认
    pub mic_device: String,
    /// 增益百分比，100 = 原始
    pub system_gain: i32,
    pub mic_gain: i32,
    /// 控制条上的"静音麦克风"开关；true 时麦克风按 0 增益混音
    pub mic_mute: std::sync::Arc<AtomicBool>,
}

impl Spec {
    pub fn any(&self) -> bool {
        self.system || self.mic
    }
}

/// 设置页用：一个音频设备。
#[derive(Clone, Debug, serde::Serialize)]
pub struct Device {
    pub name: String,
    pub id: String,
}

// ---------------------------------------------------------------- 对外接口

/// 采集会话。`Drop` 会停线程。
pub struct Session {
    rx: Receiver<Vec<i16>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    ready: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// 采集统计（控制条显示"麦克风在响"、自检判断有没有声音）。
#[derive(Default)]
pub struct Stats {
    pub sys_peak: AtomicU32,
    pub mic_peak: AtomicU32,
    pub samples: AtomicU32,
    /// 打开失败的原因（0 = 正常）
    pub err: std::sync::Mutex<String>,
    /// 实际打开了哪几路，给日志用
    pub opened: std::sync::Mutex<String>,
}

impl Session {
    /// 起线程并开始采集。设备打不开时 `stats.err` 有原因，`rx` 不会有数据。
    pub fn start(spec: Spec) -> Session {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let ready = Arc::new(AtomicBool::new(false));
        let (s2, t2, st2, r2) = (stop.clone(), stats.clone(), spec.clone(), ready.clone());
        let join = std::thread::Builder::new()
            .name("zx-audio".into())
            .spawn(move || thread_main(st2, s2, t2, r2, tx))
            .ok();
        Session { rx, stop, stats, ready, join }
    }

    /// 等设备真正打开（最多 `ms` 毫秒）。
    ///
    /// **必须在开始计时之前调用**：打开 WASAPI 设备要 50~100ms，
    /// 如果这段算进录制时间里，音轨就会比画面短一截，越长越不同步。
    pub fn wait_ready(&self, ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            if self.ready.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// 取一块混好的音频（非阻塞）。返回 None = 这块还没好。
    pub fn try_recv(&self) -> Option<Vec<i16>> {
        self.rx.try_recv().ok()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn error(&self) -> String {
        self.stats.err.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn opened(&self) -> String {
        self.stats.opened.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 不 join：可能是在控制条线程里 drop 的，等几百毫秒会卡住 UI。
        // 线程看到 stop 就自己退，句柄丢掉即可。
    }
}

// ---------------------------------------------------------------- 线程主体

fn thread_main(
    spec: Spec,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    ready: Arc<AtomicBool>,
    tx: Sender<Vec<i16>>,
) {
    unsafe {
        // RPC_E_CHANGED_MODE 说明这线程已经 STA 了，不致命
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    struct Uninit;
    impl Drop for Uninit {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _uninit = Uninit;

    let mut sys = if spec.system {
        match Source::loopback("系统声音") {
            Ok(s) => {
                note(&stats, "系统声音");
                Some(s)
            }
            Err(e) => {
                set_err(&stats, format!("系统声音打不开：{e}"));
                zx_core::log::warn(format!("音频：系统声音打不开（{e}），这个文件将只有画面"));
                None
            }
        }
    } else {
        None
    };
    let mut mic = if spec.mic {
        match Source::mic(&spec.mic_device) {
            Ok(s) => {
                note(&stats, "麦克风");
                Some(s)
            }
            Err(e) => {
                set_err(&stats, format!("麦克风打不开：{e}"));
                zx_core::log::warn(format!("音频：麦克风打不开（{e}）"));
                None
            }
        }
    } else {
        None
    };

    if sys.is_none() && mic.is_none() {
        zx_core::log::warn("音频：两路都没打开，本次录制无声音");
        ready.store(true, Ordering::SeqCst);
        return;
    }
    // 先把 Start 都调了，再进入混合循环（同时起，减少两路之间的固定偏差）
    for s in [sys.as_ref(), mic.as_ref()].into_iter().flatten() {
        if let Err(e) = unsafe { s.client.Start() } {
            zx_core::log::warn(format!("音频：{} 启动失败（{e}）", s.label));
        }
    }
    // 从这一刻起音轨开始计时——调用方等这个标志再开始录画面
    let t0 = Instant::now();
    ready.store(true, Ordering::SeqCst);

    let (gs, gm) = (spec.system_gain.clamp(0, 200), spec.mic_gain.clamp(0, 200));
    let mut ring_sys: VecDeque<i16> = VecDeque::new();
    let mut ring_mic: VecDeque<i16> = VecDeque::new();
    let mut scratch: Vec<i16> = Vec::new();
    let mut produced: u64 = 0;

    while !stop.load(Ordering::SeqCst) {
        // 1) 把两路目前可用的数据全抽到环形缓冲
        if let Some(s) = sys.as_mut() {
            scratch.clear();
            s.drain(&mut scratch);
            push_ring(&mut ring_sys, &scratch);
        }
        if let Some(s) = mic.as_mut() {
            scratch.clear();
            s.drain(&mut scratch);
            push_ring(&mut ring_mic, &scratch);
        }

        // 2) 按墙上时钟决定该产出多少帧——这是"静音时也要写"的关键
        let want = (t0.elapsed().as_micros() as u64 * RATE as u64 / 1_000_000) as usize;
        let mut need = want.saturating_sub(produced as usize).min(MAX_CATCHUP_FRAMES);

        // 3) 一块一块混出来发走
        while need >= BLOCK {
            let mut block = Vec::with_capacity(BLOCK * CHANNELS);
            // 静音开关每块都读一次：按下就立刻生效，不用重开设备
            let muted = spec.mic_mute.load(Ordering::Relaxed);
            mix_into(&mut ring_sys, gs, sys.is_some(), BLOCK, &mut block);
            mix_into(&mut ring_mic, if muted { 0 } else { gm }, mic.is_some(), BLOCK, &mut block);
            produced += BLOCK as u64;
            need -= BLOCK;
            stats.samples.store(produced.min(u32::MAX as u64) as u32, Ordering::Relaxed);
            // 接收端没了（录制线程结束）= 直接退
            if tx.send(block).is_err() {
                break;
            }
        }

        // 4) 更新峰值（给控制条画"有声"指示）
        peak_into(&stats, &ring_sys, &ring_mic);

        std::thread::sleep(Duration::from_millis(4));
    }

    for s in [sys.as_ref(), mic.as_ref()].into_iter().flatten() {
        unsafe {
            let _ = s.client.Stop();
        }
    }
}

fn note(stats: &Stats, what: &str) {
    if let Ok(mut g) = stats.opened.lock() {
        if !g.is_empty() {
            g.push('+');
        }
        g.push_str(what);
    }
}

fn set_err(stats: &Stats, msg: String) {
    if let Ok(mut g) = stats.err.lock() {
        if !g.is_empty() {
            g.push('；');
        }
        g.push_str(&msg);
    }
}

fn push_ring(ring: &mut VecDeque<i16>, add: &[i16]) {
    ring.extend(add.iter().copied());
    if ring.len() > RING_CAP {
        let drop_n = ring.len() - RING_CAP;
        ring.drain(..drop_n);
    }
}

/// 从一路环形缓冲取 `frames` 帧混进 `out`（交错立体声）。
/// 数据不够的部分补静音——**不等待**，否则整条音轨会随时间变短。
fn mix_into(ring: &mut VecDeque<i16>, gain: i32, enabled: bool, frames: usize, out: &mut Vec<i16>) {
    if !enabled {
        return;
    }
    let have = (ring.len() / CHANNELS).min(frames);
    if out.is_empty() {
        out.resize(frames * CHANNELS, 0);
    }
    let g = gain.clamp(0, 200);
    for slot in out.iter_mut().take(have * CHANNELS) {
        let s = ring.pop_front().unwrap_or(0) as i32 * g / 100;
        let cur = *slot as i32 + s;
        *slot = cur.clamp(i16::MIN as i32 + 1, i16::MAX as i32) as i16;
    }
    if have < frames {
        // 缺口就在末尾补零（out 已经 resize 过）
        if out.len() < frames * CHANNELS {
            out.resize(frames * CHANNELS, 0);
        }
    }
}

fn peak_into(stats: &Stats, sys: &VecDeque<i16>, mic: &VecDeque<i16>) {
    // 环形缓冲里只留最近 2 秒，取其中一部分算峰值即可
    let p = |r: &VecDeque<i16>| -> u32 {
        let take = r.len().min(RATE as usize * CHANNELS / 4);
        let start = r.len() - take;
        r.iter().skip(start).map(|v| (*v as i32).unsigned_abs()).max().unwrap_or(0)
    };
    stats.sys_peak.store(p(sys), Ordering::Relaxed);
    stats.mic_peak.store(p(mic), Ordering::Relaxed);
}

// ---------------------------------------------------------------- 设备枚举

/// 列出活跃的采集（`capture = true`）或播放设备。
///
/// 播放设备列出来是为了告诉用户"系统声音会录哪个设备"——录屏最常见的坑是
/// 用户接了个蓝牙耳机，声音跑到耳机上，而他在扬声器上找声音。
pub fn devices(capture: bool) -> Vec<Device> {
    let mut out = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator =
            match CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL) {
                Ok(e) => e,
                Err(_) => return out,
            };
        let flow = if capture { eCapture } else { eRender };
        let Ok(col) = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else {
            return out;
        };
        let n = col.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(dev) = col.Item(i) else { continue };
            let name = friendly_name(&dev);
            let id = device_id(&dev);
            out.push(Device {
                name: if name.trim().is_empty() { format!("音频设备 {}", i + 1) } else { name },
                id,
            });
        }
    }
    out
}

/// 默认麦克风的名字与 id（空设备时 `None`）。
pub fn default_mic() -> Option<Device> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eCapture, eConsole).ok()?;
        Some(Device { name: friendly_name(&dev), id: device_id(&dev) })
    }
}

/// 默认播放设备的名字（系统声音的来源）。
pub fn default_speaker() -> Option<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        Some(friendly_name(&dev))
    }
}

unsafe fn friendly_name(dev: &IMMDevice) -> String {
    use windows::Win32::System::Com::STGM_READ;
    let Ok(store) = dev.OpenPropertyStore(STGM_READ) else {
        return String::new();
    };
    match store.GetValue(&PKEY_FRIENDLY_NAME) {
        // PROPVARIANT 实现了 Display，直接格式化就是字符串
        Ok(pv) => {
            let s = format!("{pv}");
            s.trim().to_string()
        }
        Err(_) => String::new(),
    }
}

unsafe fn device_id(dev: &IMMDevice) -> String {
    match dev.GetId() {
        Ok(p) => {
            let s = if p.is_null() { String::new() } else { p.to_string().unwrap_or_default() };
            CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
            s
        }
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------- 单路采集

struct Source {
    client: IAudioClient,
    cap: IAudioCaptureClient,
    conv: Conv,
    label: &'static str,
}

impl Source {
    /// 系统正在播放的声音。用默认播放设备的 loopback。
    fn loopback(label: &'static str) -> Result<Source, String> {
        unsafe {
            let en: IMMDeviceEnumerator =
                CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL)
                    .map_err(|e| format!("枚举设备失败：{e}"))?;
            let dev = en
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| format!("没有可用的播放设备：{e}"))?;
            Source::open(&dev, true, label)
        }
    }

    fn mic(id: &str) -> Result<Source, String> {
        unsafe {
            let en: IMMDeviceEnumerator =
                CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL)
                    .map_err(|e| format!("枚举设备失败：{e}"))?;
            let dev = if id.trim().is_empty() {
                en.GetDefaultAudioEndpoint(eCapture, eConsole)
                    .map_err(|e| format!("没有麦克风：{e}"))?
            } else {
                let w = to_wide(id);
                en.GetDevice(PCWSTR(w.as_ptr())).map_err(|e| format!("找不到指定麦克风：{e}"))?
            };
            Source::open(&dev, false, "麦克风")
        }
    }

    unsafe fn open(dev: &IMMDevice, loopback: bool, label: &'static str) -> Result<Source, String> {
        let client: IAudioClient = dev
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("打不开音频设备：{e}"))?;

        let base = if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        // 2 秒缓冲：loopback 在系统刚启动播放时会有突发，缓冲太小会丢包
        let buf_2s = 20_000_000i64;

        // 先试"让 Windows 帮我转成 48k/立体声/16bit"
        let want = WAVEFORMATEX {
            wFormatTag: FMT_PCM,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS as u32 * 2,
            nBlockAlign: (CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = base | AUTOCONVERTPCM | SRC_DEFAULT_QUALITY;
        if client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buf_2s, 0, &want, None).is_ok() {
            let cap: IAudioCaptureClient =
                client.GetService().map_err(|e| format!("取采集接口失败：{e}"))?;
            return Ok(Source { client, cap, conv: Conv::passthrough(), label });
        }

        // 退回：拿设备混音格式，自己转换
        let p = client.GetMixFormat().map_err(|e| format!("取设备格式失败：{e}"))?;
        if p.is_null() {
            return Err("设备没有返回格式".into());
        }
        let conv = Conv::from_wfx(p)?;
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, base, buf_2s, 0, p, None);
        // 混音格式的指针由 COM 分配，用完必须还回去
        CoTaskMemFree(Some(p as *const core::ffi::c_void));
        init.map_err(|e| format!("初始化音频流失败：{e}"))?;
        zx_core::log::info(format!("音频：{label} 不支持自动转换，按设备原格式自行转换"));
        let cap: IAudioCaptureClient = client.GetService().map_err(|e| format!("取采集接口失败：{e}"))?;
        Ok(Source { client, cap, conv, label })
    }

    /// 把目前能拿到的所有包追加到 `out`（已归一化为 48k 立体声 s16）。
    fn drain(&mut self, out: &mut Vec<i16>) {
        loop {
            let n = match unsafe { self.cap.GetNextPacketSize() } {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let mut p: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if unsafe { self.cap.GetBuffer(&mut p, &mut frames, &mut flags, None, None) }.is_err() {
                break;
            }
            if frames > 0 {
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || p.is_null() {
                    // 静音包：数据指针不可信，按帧数补零
                    out.resize(out.len() + frames as usize * CHANNELS, 0);
                } else {
                    let bytes = frames as usize * self.conv.block_align();
                    let raw = unsafe { std::slice::from_raw_parts(p, bytes) };
                    self.conv.feed(raw, out);
                }
            }
            let _ = unsafe { self.cap.ReleaseBuffer(frames) };
            if frames == 0 {
                break;
            }
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

// ---------------------------------------------------------------- 格式归一化

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    F32,
    I16,
    I32,
    I24,
}

/// 把设备格式转成 48k 立体声 s16。
struct Conv {
    kind: Kind,
    rate: u32,
    ch: usize,
    /// 每帧字节数（= block_align）
    align: usize,
    /// 直通：设备本来就是 48k/2ch/s16（AUTOCONVERTPCM 成功时）
    passthrough: bool,
    /// 重采样位置的小数部分，跨包保留
    pos: f64,
    /// 上一包的最后一帧（插值起点）
    prev: [f32; 2],
    frames: Vec<[f32; 2]>,
}

impl Conv {
    fn passthrough() -> Conv {
        Conv {
            kind: Kind::I16,
            rate: RATE,
            ch: CHANNELS,
            align: CHANNELS * 2,
            passthrough: true,
            pos: 0.0,
            prev: [0.0, 0.0],
            frames: Vec::new(),
        }
    }

    unsafe fn from_wfx(p: *const WAVEFORMATEX) -> Result<Conv, String> {
        let wf = &*p;
        let (bits, mut tag) = (wf.wBitsPerSample as usize, wf.wFormatTag);
        let mut kind = match (tag, bits) {
            (FMT_PCM, 16) => Kind::I16,
            (FMT_PCM, 24) => Kind::I24,
            (FMT_PCM, 32) => Kind::I32,
            (3, 32) => Kind::F32, // WAVE_FORMAT_IEEE_FLOAT
            (FMT_EXTENSIBLE, _) => {
                // 扩展格式：真正的类型在 SubFormat 里
                let ext = &*(p as *const WAVEFORMATEXTENSIBLE);
                let sub = ext.SubFormat;
                // {0000000X-0000-0010-8000-00AA00389B71}
                let is_float = sub == GUID::from_u128(0x0000_0003_0000_0010_8000_00AA_0038_9B71);
                tag = if is_float { 3 } else { FMT_PCM };
                match (is_float, bits) {
                    (true, _) => Kind::F32,
                    (false, 16) => Kind::I16,
                    (false, 24) => Kind::I24,
                    (false, 32) => Kind::I32,
                    _ => return Err(format!("不支持的位深 {bits}")),
                }
            }
            _ => return Err(format!("不支持的音频格式（tag={tag}, {bits}bit）")),
        };
        if tag != 3 && kind == Kind::F32 {
            kind = Kind::I16;
        }
        if wf.nChannels == 0 || wf.nSamplesPerSec == 0 {
            return Err("设备格式无效（通道数或采样率为 0）".into());
        }
        Ok(Conv {
            kind,
            rate: wf.nSamplesPerSec,
            ch: wf.nChannels as usize,
            align: wf.nBlockAlign as usize,
            passthrough: false,
            pos: 0.0,
            prev: [0.0, 0.0],
            frames: Vec::new(),
        })
    }

    fn block_align(&self) -> usize {
        self.align
    }

    /// 输入一包原始字节，追加 48k/2ch/s16 到 `out`。
    fn feed(&mut self, raw: &[u8], out: &mut Vec<i16>) {
        if self.passthrough {
            // 小端 i16；x86 上直接按 i16 读
            let n = raw.len() / 2;
            let src = unsafe { std::slice::from_raw_parts(raw.as_ptr() as *const i16, n) };
            out.extend_from_slice(src);
            return;
        }
        if self.align == 0 {
            return;
        }
        let n = raw.len() / self.align;
        if n == 0 {
            return;
        }
        self.frames.clear();
        self.frames.reserve(n);
        for i in 0..n {
            let off = i * self.align;
            let at = |c: usize| off + c * (self.align / self.ch).max(1);
            let s = |c: usize| -> f32 {
                let o = at(c.min(self.ch.saturating_sub(1)));
                if o >= raw.len() {
                    return 0.0;
                }
                unsafe { self.sample(raw, o) }
            };
            let (l, r) = if self.ch == 1 { let v = s(0); (v, v) } else { (s(0), s(1)) };
            self.frames.push([l, r]);
        }
        // 重采样：48k 目标，线性插值。`pos` 是"下一个输出帧落在输入的第几帧"，
        // 小数部分跨包保留，所以连续包之间不会有缝。
        let step = self.rate as f64 / RATE as f64;
        let len = self.frames.len();
        while (self.pos.floor() as isize) < len as isize {
            let idx = self.pos.floor() as isize;
            let frac = (self.pos - idx as f64) as f32;
            let a = if idx < 0 { self.prev } else { self.frames[idx as usize] };
            // 越界就保持最后一个值（不外推，避免尾部爆音）
            let b = if idx + 1 >= len as isize { a } else { self.frames[(idx + 1) as usize] };
            let mix = |c: usize| a[c] * (1.0 - frac) + b[c] * frac;
            out.push(f2i(mix(0)));
            out.push(f2i(mix(1)));
            self.pos += step;
        }
        self.pos -= len as f64;
        if self.pos < 0.0 {
            self.pos = 0.0;
        }
        self.prev = self.frames[len - 1];
    }

    unsafe fn sample(&self, raw: &[u8], o: usize) -> f32 {
        match self.kind {
            Kind::F32 => f32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]).clamp(-1.0, 1.0),
            Kind::I16 => i16::from_le_bytes([raw[o], raw[o + 1]]) as f32 / 32768.0,
            Kind::I32 => {
                i32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]) as f32 / 2_147_483_648.0
            }
            Kind::I24 => {
                let v = (raw[o] as i32) | ((raw[o + 1] as i32) << 8) | ((raw[o + 2] as i32) << 16);
                // 符号扩展到 32 位
                let v = (v << 8) >> 8;
                v as f32 / 8_388_608.0
            }
        }
    }
}

fn f2i(v: f32) -> i16 {
    (v * 32767.0).clamp(-32768.0, 32767.0) as i16
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------- 自检

/// 采 `ms` 毫秒并汇报电平。写一份 WAV 到 `out` 供人耳复核。
pub fn selftest(ms: u64, out: &std::path::Path) -> bool {
    let caps = devices(true);
    let render = default_speaker().unwrap_or_else(|| "（无）".into());
    zx_core::log::info(format!("音频设备：麦克风 {} 个，默认播放「{render}」", caps.len()));
    for d in &caps {
        zx_core::log::info(format!("  麦克风：{}", d.name));
    }

    let spec = Spec {
        system: std::env::var("ZHUXIA_AUDIO_SYS").map(|v| v != "0").unwrap_or(true),
        mic: std::env::var("ZHUXIA_AUDIO_MIC").map(|v| v == "1").unwrap_or(!caps.is_empty()),
        mic_device: String::new(),
        system_gain: 100,
        mic_gain: 100,
        mic_mute: std::sync::Arc::new(AtomicBool::new(false)),
    };
    zx_core::log::info(format!("音频自检：采集 {ms}ms（系统声音={} 麦克风={}）", spec.system, spec.mic));

    let session = Session::start(spec.clone());
    let ready = session.wait_ready(1000);
    zx_core::log::info(format!("音频自检：设备就绪={ready}"));
    let mut pcm: Vec<i16> = Vec::new();
    let t0 = Instant::now();
    let mut peak_sys = 0i32;
    let mut peak_mic = 0i32;
    while t0.elapsed() < Duration::from_millis(ms) {
        while let Some(b) = session.try_recv() {
            pcm.extend_from_slice(&b);
        }
        peak_sys = peak_sys.max(session.stats.sys_peak.load(Ordering::Relaxed) as i32);
        peak_mic = peak_mic.max(session.stats.mic_peak.load(Ordering::Relaxed) as i32);
        std::thread::sleep(Duration::from_millis(10));
    }
    while let Some(b) = session.try_recv() {
        pcm.extend_from_slice(&b);
    }
    let opened = session.opened();
    let err = session.error();
    session.stop();

    let frames = pcm.len() / CHANNELS;
    let want = (ms as usize * RATE as usize / 1000).max(1);
    // 允许 ±15%：起线程、设备启动都有开销
    let len_ok = frames + want / 8 >= want && frames <= want + want / 4;
    let rms = {
        let mut acc = 0f64;
        for s in &pcm {
            let v = *s as f64 / 32768.0;
            acc += v * v;
        }
        if pcm.is_empty() { 0.0 } else { (acc / pcm.len() as f64).sqrt() }
    };
    let wav_ok = write_wav(out, &pcm).is_ok();

    zx_core::log::info(format!(
        "音频自检结果：{} 帧 / 期望 {} 帧（{}），峰值 系统{} 麦克风{}，RMS {:.4}，打开了 [{}]{}",
        frames,
        want,
        if len_ok { "对齐" } else { "偏差大" },
        peak_sys,
        peak_mic,
        rms,
        opened,
        if err.is_empty() { String::new() } else { format!("，失败：{err}") }
    ));
    if wav_ok {
        zx_core::log::info(format!("音频自检：样本写到 {}", out.display()));
    }
    // 判据：长度对齐 + 至少有一路真的打开了 + WAV 落盘
    len_ok && !opened.is_empty() && wav_ok
}

/// 写 48k 立体声 16bit 的 WAV（只为自检，不做通用实现）。
fn write_wav(path: &std::path::Path, pcm: &[i16]) -> std::io::Result<()> {
    use std::io::Write;
    let data = pcm.len() * 2;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let byte_rate = RATE * CHANNELS as u32 * 2;
    f.write_all(b"RIFF")?;
    f.write_all(&((36 + data) as u32).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&(CHANNELS as u16).to_le_bytes())?;
    f.write_all(&RATE.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&((CHANNELS * 2) as u16).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&(data as u32).to_le_bytes())?;
    for s in pcm {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_keeps_samples() {
        let mut c = Conv::passthrough();
        let raw: Vec<u8> = [100i16, -100, 32767, -32768].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        c.feed(&raw, &mut out);
        assert_eq!(out, vec![100, -100, 32767, -32768]);
    }

    #[test]
    fn mono_16k_upsamples_to_stereo_48k() {
        // 16k 单声道 → 48k 立体声：3 倍采样，左右相同
        let mut c = Conv {
            kind: Kind::I16,
            rate: 16_000,
            ch: 1,
            align: 2,
            passthrough: false,
            pos: 0.0,
            prev: [0.0; 2],
            frames: Vec::new(),
        };
        let n = 320usize; // 20ms
        let raw: Vec<u8> = (0..n)
            .flat_map(|i| ((i as i16 * 100) % 20000 - 10000).to_le_bytes())
            .collect();
        let mut out = Vec::new();
        c.feed(&raw, &mut out);
        let frames = out.len() / 2;
        assert!((frames as i32 - 960).abs() <= 2, "got {frames}");
        // 立体声：相邻两个样本应相等（单声道复制）
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn f32_is_clamped() {
        let mut c = Conv {
            kind: Kind::F32,
            rate: RATE,
            ch: 2,
            align: 8,
            passthrough: false,
            pos: 0.0,
            prev: [0.0; 2],
            frames: Vec::new(),
        };
        let mut raw = Vec::new();
        for v in [2.0f32, -2.0] {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        c.feed(&raw, &mut out);
        // 浮点先夹到 [-1,1]，再按 32767 定标（-1.0 → -32767，而不是 -32768）
        assert_eq!(out[0], 32767);
        assert_eq!(out[1], -32767);
    }

    #[test]
    fn mixing_gain_and_silence() {
        let mut ring: VecDeque<i16> = VecDeque::new();
        ring.extend([1000i16, -1000, 2000, -2000]);
        let mut out = Vec::new();
        // 要 4 帧但只有 2 帧数据 → 后两帧静音
        mix_into(&mut ring, 50, true, 4, &mut out);
        assert_eq!(out.len(), 8);
        assert_eq!(&out[..4], &[500, -500, 1000, -1000]);
        assert_eq!(&out[4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn disabled_source_contributes_nothing() {
        let mut ring: VecDeque<i16> = VecDeque::new();
        ring.extend([1000i16, 1000]);
        let mut out = Vec::new();
        mix_into(&mut ring, 100, false, 2, &mut out);
        assert!(out.is_empty());
        assert_eq!(ring.len(), 2, "数据不该被消费");
    }
}

// ---------------------------------------------------------------- 诊断

/// 诊断 loopback：打印默认播放设备的混音格式，并用**混音格式原样**开一次采集，
/// 统计 2 秒内收到的原始包数、静音包数、转换后的峰值。
///
/// 用来区分两种"录到静音"：
/// - 包数 0 或全是静音包 → 设备根本没在出声（蓝牙耳机当默认设备、或系统静音）；
/// - 包数正常但峰值 0 → 是我们的格式转换写错了。
///
/// `ZHUXIA_AUDIO_AUTO=1` 时改用 48k/2ch/s16 + AUTOCONVERTPCM，用来对比两种路径。
pub fn diag(ms: u64) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator =
            match CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL) {
                Ok(e) => e,
                Err(e) => {
                    println!("枚举设备失败：{e}");
                    return false;
                }
            };
        let dev = match en.GetDefaultAudioEndpoint(eRender, eConsole) {
            Ok(d) => d,
            Err(e) => {
                println!("没有播放设备：{e}");
                return false;
            }
        };
        println!("默认播放设备：{}", friendly_name(&dev));

        // 1) 混音格式（只读取，不初始化）
        if let Ok(c0) = dev.Activate::<IAudioClient>(CLSCTX_ALL, None) {
            if let Ok(p) = c0.GetMixFormat() {
                let wf = &*p;
                // WAVEFORMATEX 是 packed 结构，字段必须先按值取出来再格式化
                let (tag, ch, rate, bits, align) = (
                    wf.wFormatTag,
                    wf.nChannels,
                    wf.nSamplesPerSec,
                    wf.wBitsPerSample,
                    wf.nBlockAlign,
                );
                let mut valid = 0u16;
                if tag == FMT_EXTENSIBLE {
                    let ext = &*(p as *const WAVEFORMATEXTENSIBLE);
                    valid = ext.Samples.wValidBitsPerSample;
                }
                if tag == FMT_EXTENSIBLE {
                    println!("混音格式：扩展 tag={tag} {ch}ch {rate}Hz {bits}bit align={align}（有效位深 {valid}）");
                } else {
                    println!("混音格式：tag={tag} {ch}ch {rate}Hz {bits}bit align={align}");
                }
                CoTaskMemFree(Some(p as *const core::ffi::c_void));
            }
        }

        let auto = std::env::var("ZHUXIA_AUDIO_AUTO").map(|v| v == "1").unwrap_or(false);
        let client: IAudioClient = match dev.Activate(CLSCTX_ALL, None) {
            Ok(c) => c,
            Err(e) => {
                println!("Activate 失败：{e}");
                return false;
            }
        };

        // 2) 按选定路径初始化
        let mut conv;
        if auto {
            let want = WAVEFORMATEX {
                wFormatTag: FMT_PCM,
                nChannels: CHANNELS as u16,
                nSamplesPerSec: RATE,
                nAvgBytesPerSec: RATE * CHANNELS as u32 * 2,
                nBlockAlign: (CHANNELS * 2) as u16,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUTOCONVERTPCM | SRC_DEFAULT_QUALITY;
            if let Err(e) = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 20_000_000, 0, &want, None)
            {
                println!("自动转换路径初始化失败：{e}");
                return false;
            }
            println!("路径：48k/2ch/s16 + AUTOCONVERTPCM");
            conv = Conv::passthrough();
        } else {
            let p = match client.GetMixFormat() {
                Ok(p) => p,
                Err(e) => {
                    println!("GetMixFormat 失败：{e}");
                    return false;
                }
            };
            conv = match Conv::from_wfx(p) {
                Ok(c) => c,
                Err(e) => {
                    println!("格式不支持：{e}");
                    return false;
                }
            };
            let r = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                20_000_000,
                0,
                p,
                None,
            );
            CoTaskMemFree(Some(p as *const core::ffi::c_void));
            if let Err(e) = r {
                println!("混音格式路径初始化失败：{e}");
                return false;
            }
            println!("路径：设备混音格式原样 + 自行转换");
        }

        let cap: IAudioCaptureClient = match client.GetService() {
            Ok(c) => c,
            Err(e) => {
                println!("GetService 失败：{e}");
                return false;
            }
        };
        if let Err(e) = client.Start() {
            println!("Start 失败：{e}");
            return false;
        }

        let (mut packets, mut frames_in, mut silent) = (0u32, 0u64, 0u32);
        let mut pcm: Vec<i16> = Vec::new();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            loop {
                let n = match cap.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    break;
                }
                let mut p: *mut u8 = std::ptr::null_mut();
                let mut fr = 0u32;
                let mut fl = 0u32;
                if cap.GetBuffer(&mut p, &mut fr, &mut fl, None, None).is_err() {
                    break;
                }
                packets += 1;
                frames_in += fr as u64;
                if fl & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                    silent += 1;
                } else if !p.is_null() && fr > 0 {
                    let b = fr as usize * conv.block_align();
                    let raw = std::slice::from_raw_parts(p, b);
                    conv.feed(raw, &mut pcm);
                }
                let _ = cap.ReleaseBuffer(fr);
                if fr == 0 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = client.Stop();
        let peak = pcm.iter().map(|v| (*v as i32).unsigned_abs()).max().unwrap_or(0);
        println!(
            "{ms}ms 内：{packets} 个包 / {frames_in} 帧 / {silent} 个静音包 → 转换后 {} 样本，峰值 {peak}",
            pcm.len()
        );
        packets > 0
    }
}
