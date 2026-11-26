//! 录屏：GIF + 外部设备（摄像头）合成。
//!
//! 方案与取舍见 `docs/RECORD.md`。这里只讲管线怎么串：
//!
//! ```text
//! overlay 框选 ──pending_record──▶ record::run(rect)
//!                                     │
//!          ┌──────────────────────────┼───────────────────────────┐
//!          ▼                          ▼                           ▼
//!   摄像头线程(zx-camera)      录制线程(zx-record)          控制条(UI 线程)
//!   IMFSourceReader 循环       sleep_until 节拍 ─▶ 抓屏     自绘 + 按钮
//!   写"最新帧槽"                ├─▶ compose 合成           暂停/继续/完成/丢弃
//!                              └─▶ GIF 脏矩形+量化+LZW
//! ```
//!
//! 三条关键约束（都是踩过的坑，别再改回去）：
//!
//! 1. 覆盖层必须在 `record::run` 之前销毁——`CAPTUREBLT` 会把它拍进每一帧。
//! 2. 抓帧不能挂在 `WM_TIMER` 上（15.6ms 粒度 + 消息队列挤压，10fps 都稳不住）。
//! 3. 摄像头只取"最新帧"，不排队——否则录制 10fps、摄像头 30fps 时延迟会一路累积。

pub mod audio;
pub mod bar;
pub mod camera;
pub mod compose;
pub mod gifenc;
pub mod grab;
pub mod mp4;
pub mod source;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use zx_core::{RecordFormat, RecordSettings};

use camera::{CamState, Feed};
use compose::{Mode, Spec};
use gifenc::GifWriter;
use grab::{Grab, Grabber};
use mp4::Mp4Writer;
use source::{FrameBuf, Rect};

/// 控制条的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Discarded,
}

/// 输出端：视频或 GIF。两种格式共用同一条抓屏/合成管线，只在这里分叉。
enum Sink {
    /// 两个都装箱：它们只在录制开始时构造一次，为省栈空间不值得纠结布局
    Gif(Box<GifWriter>),
    Mp4(Box<Mp4Writer>),
}

impl Sink {
    /// 按节拍序号写（GIF 没有时间戳，忽略序号）。
    fn push_at(&mut self, frame: &FrameBuf, tick: u32) -> std::io::Result<()> {
        match self {
            Sink::Gif(w) => w.push(frame),
            Sink::Mp4(w) => w.push_at(frame, tick),
        }
    }

    fn ticks(&self) -> u32 {
        match self {
            Sink::Gif(w) => w.ticks,
            Sink::Mp4(w) => w.ticks,
        }
    }

    /// GIF 没有音轨，这里直接忽略。
    fn push_audio(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        match self {
            Sink::Gif(_) => Ok(()),
            Sink::Mp4(w) => w.push_audio(pcm),
        }
    }

    fn has_audio(&self) -> bool {
        match self {
            Sink::Gif(_) => false,
            Sink::Mp4(w) => w.has_audio(),
        }
    }

    /// 已写入的音频秒数（GIF 恒为 0）
    fn audio_secs(&self) -> f64 {
        match self {
            Sink::Gif(_) => 0.0,
            Sink::Mp4(w) => w.audio_frames as f64 / 48_000.0,
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            Sink::Gif(w) => w.bytes.get(),
            Sink::Mp4(w) => w.bytes().get(),
        }
    }

    fn finish(self) -> std::io::Result<PathBuf> {
        match self {
            Sink::Gif(w) => w.finish(),
            Sink::Mp4(w) => w.finish(),
        }
    }

    fn abort(self) {
        match self {
            Sink::Gif(w) => w.abort(),
            Sink::Mp4(w) => w.abort(),
        }
    }
}

/// 录制线程 ↔ 控制条共享的状态。全部是原子量，不加锁排队。
pub struct UiState {
    pub paused: AtomicBool,
    pub stop: AtomicBool,
    pub discard: AtomicBool,
    /// 倒计时剩余秒数（0 = 已开始）
    pub countdown: AtomicU32,
    pub frames: AtomicU32,
    pub elapsed_ms: AtomicU64,
    pub bytes: AtomicU64,
    /// 控制栏显示用（录制期间不变）
    pub fps: AtomicU32,
    pub cam_label: Mutex<String>,
    /// 音频状态文案（"系统声音 + 麦克风" / "无声音"）
    pub audio_label: Mutex<String>,
    /// 控制条上的麦克风静音开关（音频线程每轮读取）
    pub mic_muted: Arc<AtomicBool>,
    /// 磁盘快满了（控制条把计时变成橙色，提醒用户准备停）
    pub disk_low: AtomicBool,
    /// 最近一帧缩略图（BGRA）
    pub thumb: Mutex<Option<(i32, i32, Vec<u8>)>>,
    /// GIF 先写这里，成功后 rename 到 `dst`。
    pub dst: PathBuf,
}

static STATE: OnceLock<Mutex<Option<Arc<UiState>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Arc<UiState>>> {
    STATE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn state() -> Option<Arc<UiState>> {
    slot().lock().ok().and_then(|g| g.clone())
}

/// 外部停止（热键 / 托盘）。控制条会在下一次 UI 刷新时退出。
pub fn stop() {
    if let Some(st) = state() {
        st.stop.store(true, Ordering::SeqCst);
    }
}

/// 当前是否在录制（`record::run` 期间为 true）。
pub fn is_active() -> bool {
    state().map(|s| !s.stop.load(Ordering::SeqCst)).unwrap_or(false)
}

fn virtual_bounds() -> Rect {
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn stamped_name() -> String {
    let now = zx_core::win::local_time_string();
    // 2026-11-22 03:15:07 → 2026-11-22_03-15-07
    format!("录屏_{}", now.replace(':', "-").replace(' ', "_"))
}

/// 目标目录所在卷的可用字节数。
///
/// 这是"不设时长上限"能成立的前提：不猜用户要录多久，只保证不会把磁盘写满
/// 才发现。目录必须已存在（`create_dir_all` 之后调用）。
fn free_bytes(dir: &std::path::Path) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w: Vec<u16> = dir
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut avail = 0u64;
    unsafe {
        GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut avail), None, None).ok()?;
    }
    Some(avail)
}

/// 剩余空间到这以下就正常收尾（约等于 8Mbps 再录 8 分钟）
const DISK_STOP: u64 = 512 * 1024 * 1024;
/// 到这以下开始提示（计时变橙）
const DISK_WARN: u64 = 2 * 1024 * 1024 * 1024;

/// 唯一的重名处理：`name.gif` / `name-2.gif` / …
fn unique_path(dir: &std::path::Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}.{ext}"));
    let mut n = 1;
    while p.exists() && n < 9999 {
        n += 1;
        p = dir.join(format!("{base}-{n}.{ext}"));
    }
    p
}

/// 格式优先级：显式参数（工具条 GIF 按钮）> 环境变量（自检/脚本）> 配置文件。
fn resolve_format(forced: Option<RecordFormat>, env: Option<&str>) -> Option<RecordFormat> {
    forced.or_else(|| {
        env.map(|f| if f.trim().eq_ignore_ascii_case("gif") { RecordFormat::Gif } else { RecordFormat::Video })
    })
}

/// 录一次（按配置文件里的格式）。`sel` 是屏幕物理像素，来自截图覆盖层。
///
/// 返回 true = 产出了文件（GIF 或 MP4）。**阻塞**到用户点「完成/丢弃」（或热键停止）。
pub fn run(sel: Rect) -> bool {
    run_with(sel, None)
}

/// 同 `run`，但允许调用方强制本次格式（截图工具条的「录 GIF」入口用）。
///
/// 优先级：显式参数 > `ZHUXIA_RECORD_FORMAT` > 配置文件。只影响这一次，
/// 不写 `settings.yaml`——用户点一次 GIF 不该把全局默认改成 GIF。
pub fn run_with(sel: Rect, forced: Option<RecordFormat>) -> bool {
    if sel.w < 8 || sel.h < 8 {
        zx_core::log::warn("录屏取消：选区太小（< 8px）");
        return false;
    }
    let mut cfg = zx_core::cfg::cfg().record();
    // 自检/脚本用：临时覆盖产出格式，不改配置文件
    let env_format = std::env::var("ZHUXIA_RECORD_FORMAT").ok();
    if let Some(fmt) = resolve_format(forced, env_format.as_deref()) {
        cfg.format = fmt;
        // GIF 直接按原尺寸录会大到没法贴；用户没显式设长边时按 960 收一档
        // （与 ZHUXIA_RECORD_FORMAT=gif 的旧行为一致）。
        if fmt == RecordFormat::Gif && cfg.max_width == 0 {
            cfg.max_width = 960;
        }
    }
    // 自检/脚本用：临时覆盖帧率
    if let Ok(f) = std::env::var("ZHUXIA_RECORD_FPS") {
        if let Ok(v) = f.trim().parse::<i32>() {
            cfg.fps = v.clamp(5, 60);
        }
    }
    if !cfg.enabled {
        zx_core::log::info("录屏已禁用（settings.yaml → capture.record.enabled）");
        return false;
    }
    if state().is_some() {
        zx_core::log::warn("已经有一场录制在进行，忽略这次触发");
        return false;
    }

    // 摄像头先起：倒计时的几秒正好用来打开设备、出第一帧
    let feed = if cfg.camera.enabled {
        let f = Feed::start(cfg.camera.device.clone(), cfg.camera.width, cfg.camera.height, cfg.camera.fps);
        // 最多等 1.2 秒拿尺寸；拿不到就当没有摄像头（不让录制卡在这里）
        let deadline = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < deadline && f.size().is_none() {
            if matches!(f.state(), CamState::Failed | CamState::Lost) {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Some(f)
    } else {
        None
    };
    let cam_size = feed.as_ref().and_then(|f| f.size());
    let spec = Spec::from_cfg(&cfg);
    let plan = compose::plan(&spec, (sel.w, sel.h), cam_size);

    if spec.mode == Mode::Camera && cam_size.is_none() {
        let why = feed.as_ref().map(|f| f.error()).unwrap_or_else(|| "未启用摄像头".into());
        zx_core::log::warn(format!("录屏取消：布局要求只录摄像头，但没有可用的摄像头（{why}）"));
        return false;
    }

    let dir = zx_core::cfg::cfg().capture().save_dir;
    let ext = match cfg.format {
        RecordFormat::Video => "mp4",
        RecordFormat::Gif => "gif",
    };
    let dst = match std::env::var("ZHUXIA_RECORD_OUT") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
        _ => unique_path(&dir, &stamped_name(), ext),
    };
    // 音频只对视频有意义（GIF 容器没有音轨这一说）
    let want_audio = cfg.audio.any();
    if want_audio && cfg.format == RecordFormat::Gif {
        zx_core::log::info("GIF 不含音轨：这次录制不会出现声音（要声音请用「录屏（视频）」）");
    }
    let sink = match cfg.format {
        RecordFormat::Video => {
            let bitrate = mp4::bitrate_for(plan.w, plan.h, cfg.fps, cfg.bpp);
            match Mp4Writer::create(&dst, plan.w, plan.h, cfg.fps, bitrate, want_audio) {
                Ok(w) => {
                    zx_core::log::info(format!(
                        "视频编码：H.264 {}×{} @{}fps，{} Mbps{}",
                        plan.w,
                        plan.h,
                        cfg.fps,
                        bitrate / 1_000_000,
                        if w.has_audio() { "，AAC 音轨 48kHz" } else { "，无音轨" }
                    ));
                    Sink::Mp4(Box::new(w))
                }
                Err(e) => {
                    zx_core::log::warn(format!("录屏失败：建 MP4 失败（{}）{e}", dst.display()));
                    return false;
                }
            }
        }
        RecordFormat::Gif => match GifWriter::create(&dst, plan.w, plan.h, cfg.fps) {
            Ok(w) => Sink::Gif(Box::new(w)),
            Err(e) => {
                zx_core::log::warn(format!("录屏失败：建 GIF 文件失败（{}）{e}", dst.display()));
                return false;
            }
        },
    };

    // 自检模式：`ZHUXIA_RECORD_AUTO=<毫秒>` 跳过倒计时、到点自动完成。
    let auto_ms: Option<u64> = std::env::var("ZHUXIA_RECORD_AUTO").ok().and_then(|v| v.parse().ok());
    let countdown = if auto_ms.is_some() { 0 } else { cfg.countdown };

    let st = Arc::new(UiState {
        paused: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        discard: AtomicBool::new(false),
        countdown: AtomicU32::new(if countdown > 0 { countdown as u32 } else { 0 }),
        frames: AtomicU32::new(0),
        elapsed_ms: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        fps: AtomicU32::new(cfg.fps.max(1) as u32),
        cam_label: Mutex::new(match cam_size {
            Some((w, h)) => format!("摄像头 {w}×{h}"),
            None if cfg.camera.enabled => "摄像头不可用".to_string(),
            None => "只录屏幕".to_string(),
        }),
        audio_label: Mutex::new(if want_audio {
            match (cfg.audio.system, cfg.audio.mic) {
                (true, true) => "系统声音 + 麦克风".to_string(),
                (true, false) => "系统声音".to_string(),
                (false, true) => "麦克风".to_string(),
                _ => "无声音".to_string(),
            }
        } else {
            "无声音".to_string()
        }),
        mic_muted: Arc::new(AtomicBool::new(false)),
        disk_low: AtomicBool::new(false),
        thumb: Mutex::new(None),
        dst: dst.clone(),
    });
    *slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(st.clone());

    zx_core::log::info(format!(
        "录屏开始：{}x{} @{}fps，布局 {:?}，摄像头 {:?}，时长 {}，输出 {}",
        plan.w,
        plan.h,
        cfg.fps,
        spec.mode,
        cam_size,
        if cfg.max_seconds > 0 {
            format!("上限 {} 秒", cfg.max_seconds)
        } else {
            "不限制".to_string()
        },
        dst.display()
    ));

    let (tx, rx) = std::sync::mpsc::channel::<Result<PathBuf, String>>();
    let st2 = st.clone();
    let worker = std::thread::Builder::new()
        .name("zx-record".into())
        .spawn(move || {
            let r = record_loop(sel, &cfg, &spec, &plan, feed, sink, &st2, auto_ms);
            let _ = tx.send(r);
        })
        .ok();
    if worker.is_none() {
        zx_core::log::warn("录屏失败：录制线程起不来");
        *slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
        return false;
    }

    // 控制条在调用线程（截图热键线程）跑，阻塞到用户结束
    let outcome = bar::run(st.clone(), sel, virtual_bounds());
    st.stop.store(true, Ordering::SeqCst);

    let res = rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| Err("录制线程没有在 20 秒内收尾".into()));
    if let Some(j) = worker {
        let _ = j.join();
    }
    *slot().lock().unwrap_or_else(|p| p.into_inner()) = None;

    match (&outcome, res) {
        (Outcome::Discarded, _) => {
            zx_core::log::info("录屏已丢弃，未生成文件");
            false
        }
        (_, Ok(path)) => {
            zx_core::log::info(format!("录屏完成：{}", path.display()));
            // 响一声？保持和截图一致：不弹窗、不打断，日志 + 目录里能看到文件即可。
            true
        }
        (_, Err(e)) => {
            zx_core::log::warn(format!("录屏失败：{e}"));
            false
        }
    }
}

/// 录制主循环（独立线程）。负责：倒计时 → 节拍抓帧 → 合成 → 编码 → 收尾。
#[allow(clippy::too_many_arguments)]
fn record_loop(
    sel: Rect,
    cfg: &RecordSettings,
    spec: &Spec,
    plan0: &compose::Plan,
    feed: Option<Feed>,
    mut sink: Sink,
    st: &Arc<UiState>,
    auto_ms: Option<u64>,
) -> Result<PathBuf, String> {
    // 倒计时（期间完全不抓帧，用户有时间把窗口摆好）
    let mut left = if auto_ms.is_some() { 0 } else { cfg.countdown };
    while left > 0 && !st.stop.load(Ordering::SeqCst) {
        st.countdown.store(left as u32, Ordering::SeqCst);
        let tick = Instant::now();
        while tick.elapsed() < Duration::from_millis(1000) {
            if st.stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        left -= 1;
    }
    st.countdown.store(0, Ordering::SeqCst);
    if st.stop.load(Ordering::SeqCst) {
        sink.abort();
        return Err("倒计时被取消".into());
    }

    // 把系统时钟粒度压到 1ms（默认 15.6ms，10fps 会漂 ±10%）。退出时自动还原。
    struct PeriodGuard;
    impl Drop for PeriodGuard {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::Media::timeEndPeriod(1);
            }
        }
    }
    let _period = {
        unsafe {
            windows::Win32::Media::timeBeginPeriod(1);
        }
        PeriodGuard
    };

    let mut grabber = Grabber::new(sel);
    let mut screen = FrameBuf::new(sel.w, sel.h);
    let mut cam = FrameBuf::new(1, 1);
    // 当前布局（摄像头尺寸/可用性变了要重算）
    let mut plan = plan0.clone();
    let mut out = FrameBuf::new(plan.w, plan.h);
    let mut last_cam: Option<(i32, i32)> = plan.camera.as_ref().map(|_| (cam.w, cam.h));

    // 音频先起、并等设备真正打开，**再**开始给画面计时。
    // 否则打开 WASAPI 的 50~100ms 会让音轨比画面短一截（录得越久越明显）。
    let mut audio_sess = if cfg.audio.any() {
        let spec = audio::Spec {
            system: cfg.audio.system,
            mic: cfg.audio.mic,
            mic_device: cfg.audio.mic_device.clone(),
            system_gain: cfg.audio.system_gain,
            mic_gain: cfg.audio.mic_gain,
            mic_mute: st.mic_muted.clone(),
        };
        let sess = audio::Session::start(spec);
        let ok = sess.wait_ready(900);
        zx_core::log::info(format!(
            "音频：打开 [{}]{} {}",
            sess.opened(),
            if ok { "" } else { "（等待超时）" },
            sess.error()
        ));
        Some(sess)
    } else {
        None
    };

    let fps = cfg.fps.max(1) as u32;
    // 不能写成 `Duration::from_millis(1000 / fps)`：60fps 会被整数截断成 16ms，
    // 实际跑成 62.5fps —— 名义 2.5 秒的录制只走了 2.4 秒，音轨就短 100ms。
    // 用微秒算绝对值，平均节拍才准。
    let step_us = |t: u32| Duration::from_micros(t as u64 * 1_000_000 / fps as u64);
    let mut base = Instant::now();
    // 本段（一次暂停算一段）内应处在第几个节拍
    let mut tick: u32 = 0;
    // 之前各段累计的节拍数：暂停把时间轴接上，而不是重置
    let mut tick_base: u32 = 0;
    // 实际写出的帧数（丢帧时小于时间轴节拍数）
    let mut written: u32 = 0;
    // 0 = 不限制（默认）。只有用户明确填了正数才到点自动完成。
    let max_ticks = (cfg.max_seconds.max(0) as u32).saturating_mul(fps);
    let mut last_disk_check = Instant::now() - Duration::from_secs(60);
    let disk_dir = st
        .dst
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut last_thumb_tick = 0u32;
    let mut grab_fail = 0u32;
    // 因为"音频超前画面太多"被丢掉的块数
    let mut audio_trimmed = 0u64;
    // 自检用：各段累计耗时（微秒）
    let (mut t_grab, mut t_comp, mut t_push, mut t_aud) = (0u128, 0u128, 0u128, 0u128);

    loop {
        if st.stop.load(Ordering::SeqCst) {
            break;
        }
        if st.paused.load(Ordering::SeqCst) {
            // 暂停期间既不抓帧也不计时：恢复后重新对基准，避免"补帧"。
            // 音频也要丢掉——留到恢复后再写，会变成"暂停时的声音粘在恢复后"。
            if let Some(a) = audio_sess.as_ref() {
                while a.try_recv().is_some() {}
            }
            std::thread::sleep(Duration::from_millis(40));
            tick_base = tick_base.saturating_add(tick);
            base = Instant::now();
            tick = 0;
            continue;
        }
        // 睡到绝对时间点（不在循环里累加 drift）。系统时钟粒度默认 15.6ms，
        // 这里靠 `timeBeginPeriod(1)` + 循环逼近，误差稳定在 1~2ms。
        let target = base + step_us(tick.max(1));
        if tick > 0 {
            loop {
                let now = Instant::now();
                if now >= target || st.stop.load(Ordering::SeqCst) {
                    break;
                }
                let d = target - now;
                if d > Duration::from_millis(3) {
                    std::thread::sleep(d - Duration::from_millis(2));
                } else {
                    std::thread::yield_now();
                }
            }
        }

        // DXGI 会在画面没变化时返回 Stale（`screen` 已被填成上一帧），
        // 视频必须按节拍照写，所以只把 Failed 当问题。
        let _tg = Instant::now();
        let gres = grabber.grab(&mut screen);
        if auto_ms.is_some() {
            t_grab += _tg.elapsed().as_micros();
        }
        if gres == Grab::Failed {
            grab_fail += 1;
            if grab_fail > 30 {
                sink.abort();
                return Err("连续抓不到屏幕（锁屏/分辨率变化/远程会话断开？）".into());
            }
            tick += 1;
            continue;
        }
        grab_fail = 0;

        // 摄像头：只取最新帧，不排队
        let mut cam_ok = false;
        let mut cam_state = CamState::Failed;
        if let Some(f) = feed.as_ref() {
            let (seq, s) = f.poll(&mut cam);
            cam_state = s;
            cam_ok = seq > 0 && cam.w > 1 && cam.h > 1;
        }
        let cam_size = if cam_ok { Some((cam.w, cam.h)) } else { None };
        let cam_changed = cam_size != last_cam;
        if cam_changed {
            plan = compose::plan(spec, (sel.w, sel.h), cam_size);
            if plan.w != out.w || plan.h != out.h {
                out = FrameBuf::new(plan.w, plan.h);
            }
            last_cam = cam_size;
        }

        let _tc = Instant::now();
        compose::render(&plan, Some(&screen), if cam_ok { Some(&cam) } else { None }, &mut out, 0xFF101010);
        if cfg.cursor {
            paint_cursor(&mut out, &plan, sel);
        }
        // 自检模式：往合成结果上画一个移动的方块，制造真实的脏矩形。
        // 静态桌面录 2.5 秒本来就只有 1 帧（那是对的），但这验证不了多帧路径。
        if auto_ms.is_some() {
            probe_block(&mut out, sink.ticks());
        }
        if auto_ms.is_some() {
            t_comp += _tc.elapsed().as_micros();
        }
        // 该写第几格：由**真实经过的时间**决定，不由"已经写了几帧"决定。
        // 跟不上目标帧率时这里会跳号（等于丢帧），视频时间轴仍然是真实时间。
        let should = (base.elapsed().as_secs_f64() * fps as f64).floor() as u32;
        let g_tick = tick_base.saturating_add(should.max(tick));
        tick = g_tick.saturating_sub(tick_base).saturating_add(1);
        let _tp = Instant::now();
        if let Err(e) = sink.push_at(&out, g_tick) {
            sink.abort();
            return Err(format!("写入失败：{e}"));
        }
        written += 1;
        if auto_ms.is_some() {
            t_push += _tp.elapsed().as_micros();
        }
        // 音频块是独立的 10ms 时间线，跟画面节拍无关：每轮把攒下的都排空
        if let Some(a) = audio_sess.as_ref() {
            while let Some(block) = a.try_recv() {
                // 兜底：音轨不该比画面长出半秒（真长出来说明两边时钟没对齐，
                // 继续写只会让后面的声音越来越晚——宁可丢这一块）
                let video_secs = sink.ticks() as f64 / cfg.fps.max(1) as f64;
                if sink.audio_secs() > video_secs + 0.5 {
                    audio_trimmed += 1;
                    if audio_trimmed == 1 {
                        zx_core::log::warn(format!(
                            "音频丢弃：已写 {:.2}s 但画面只有 {:.2}s（第 {audio_trimmed} 次）",
                            sink.audio_secs(),
                            video_secs
                        ));
                    }
                    continue;
                }
                let _ta = Instant::now();
                let r = sink.push_audio(&block);
                if auto_ms.is_some() {
                    t_aud += _ta.elapsed().as_micros();
                }
                if let Err(e) = r {
                    zx_core::log::warn(format!("音频写入失败：{e}"));
                    break;
                }
            }
        }

        // 统计：时长用时间轴（真实经过的时间），帧数用实际写出的帧数
        st.frames.store(written, Ordering::Relaxed);
        st.bytes.store(sink.bytes(), Ordering::Relaxed);
        st.elapsed_ms.store((g_tick as u64 + 1) * 1000 / fps as u64, Ordering::Relaxed);
        if let Ok(mut g) = st.cam_label.lock() {
            let want = match (cam_ok, cam_state) {
                (true, _) => format!("摄像头 {}×{}", cam.w, cam.h),
                (false, CamState::Lost) => "摄像头已断开".to_string(),
                (false, CamState::Failed) => "摄像头不可用".to_string(),
                (false, CamState::Starting) => "摄像头启动中…".to_string(),
                (false, CamState::Running) => "摄像头无帧".to_string(),
            };
            if *g != want {
                *g = want;
            }
        }

        // 缩略图每 5 个节拍更新一次（预览不需要跟满帧）
        if g_tick.saturating_sub(last_thumb_tick) >= 5 || last_thumb_tick == 0 {
            last_thumb_tick = g_tick;
            let (tw, th) = (168, 50);
            let t = out.thumbnail(tw, th);
            if let Ok(mut g) = st.thumb.lock() {
                *g = Some((t.w, t.h, t.buf));
            }
        }

        if max_ticks > 0 && g_tick + 1 >= max_ticks {
            zx_core::log::info(format!("录屏达到设定上限 {} 秒，自动完成", cfg.max_seconds));
            break;
        }
        // 磁盘保护：这是"不设时长上限"唯一的兜底，5 秒查一次，代价可以忽略
        if last_disk_check.elapsed() >= Duration::from_secs(5) {
            last_disk_check = Instant::now();
            if let Some(free) = free_bytes(&disk_dir) {
                if free < DISK_STOP {
                    zx_core::log::warn(format!(
                        "录屏自动完成：{} 只剩 {}MB，再录下去会写满磁盘（文件已保存）",
                        disk_dir.display(),
                        free / (1024 * 1024)
                    ));
                    st.disk_low.store(true, Ordering::Relaxed);
                    break;
                }
                let low = free < DISK_WARN;
                st.disk_low.store(low, Ordering::Relaxed);
                if low {
                    static ONCE: AtomicBool = AtomicBool::new(false);
                    if !ONCE.swap(true, Ordering::Relaxed) {
                        zx_core::log::warn(format!(
                            "磁盘剩余 {}MB，快录不下了——建议尽快停止（到 512MB 会自动保存并结束）",
                            free / (1024 * 1024)
                        ));
                    }
                }
            }
        }
        if let Some(ms) = auto_ms {
            if st.elapsed_ms.load(Ordering::Relaxed) >= ms {
                zx_core::log::info(format!("录屏自检：{ms}ms 到点，自动完成"));
                break;
            }
        }
    }

    // 循环退出有两种原因：用户点了完成（stop 已置位），或录制线程自己判定结束
    // （时长上限 / 自检到点）。后者必须在这里补上 stop，否则控制条会一直等下去。
    st.stop.store(true, Ordering::SeqCst);

    if auto_ms.is_some() {
        let cnt = written.max(1) as f64;
        zx_core::log::info(format!(
            "自检计时（每帧均值）：抓帧 {:.1}ms 合成 {:.1}ms 写入 {:.1}ms 音频 {:.1}ms；实际写 {} 帧 / 时间轴 {} 格 / 经过 {:.2}s",
            t_grab as f64 / cnt / 1000.0,
            t_comp as f64 / cnt / 1000.0,
            t_push as f64 / cnt / 1000.0,
            t_aud as f64 / cnt / 1000.0,
            written,
            written.max(sink.ticks()),
            base.elapsed().as_secs_f64(),
        ));
    }

    if let Some(a) = audio_sess.take() {
        // 收尾前把最后几块写完，否则末尾 10~30ms 会被丢掉
        while let Some(block) = a.try_recv() {
            let _ = sink.push_audio(&block);
        }
        a.stop();
        zx_core::log::info(format!(
            "音频：共写入 {:.2}s{}",
            sink.audio_secs(),
            if sink.has_audio() { "（AAC 48kHz）" } else { "（本次无音轨）" }
        ));
    }

    if st.discard.load(Ordering::SeqCst) {
        sink.abort();
        return Err("已丢弃".into());
    }
    sink.finish().map_err(|e| format!("收尾失败：{e}"))
}

// ---------------------------------------------------------------- 光标

/// 自检用：在画面上画一个随时间移动的方块（制造脏矩形，验证多帧路径）。
fn probe_block(out: &mut FrameBuf, tick: u32) {
    let size = 48i32;
    let span = (out.w - size - 40).max(1);
    let x = 20 + ((tick as i32 * 17) % span);
    let y = 20 + ((tick as i32 * 7) % (out.h - size - 40).max(1));
    let stride = out.stride();
    for yy in y..(y + size).min(out.h) {
        for xx in x..(x + size).min(out.w) {
            if yy < 0 || xx < 0 {
                continue;
            }
            let o = yy as usize * stride + xx as usize * 4;
            // 洋红，和桌面内容区分得开
            out.buf[o] = 200;
            out.buf[o + 1] = 60;
            out.buf[o + 2] = 240;
            out.buf[o + 3] = 255;
        }
    }
}

/// 把鼠标画进帧里。`BitBlt` 不含光标（`docs/CAPTURE.md` §七），录屏必须补上。
fn paint_cursor(out: &mut FrameBuf, plan: &compose::Plan, sel: Rect) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorInfo, GetCursorPos, CURSORINFO, CURSOR_SHOWING};

    let Some(screen_rect) = plan.screen else { return };
    // `cbSize` 不填的话 GetCursorInfo 直接失败（默认值是 0）——这里必须显式给。
    let mut pos = POINT::default();
    let mut ci = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetCursorPos(&mut pos).is_err() || GetCursorInfo(&mut ci).is_err() {
            return;
        }
    }
    if ci.flags != CURSOR_SHOWING || ci.hCursor.is_invalid() {
        return;
    }
    if !sel.contains(pos.x, pos.y) {
        return;
    }
    // 屏幕坐标 → 输出坐标（屏幕层可能被缩放）
    let sx = screen_rect.x + (pos.x - sel.x) * screen_rect.w / sel.w.max(1);
    let sy = screen_rect.y + (pos.y - sel.y) * screen_rect.h / sel.h.max(1);
    unsafe {
        crate::record::cursor::blit(out, windows::Win32::UI::WindowsAndMessaging::HICON(ci.hCursor.0), sx, sy);
    }
}

/// 光标位图缓存与混合。
pub(crate) mod cursor {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::HGDIOBJ;
    use windows::Win32::UI::WindowsAndMessaging::HICON;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, GetSystemMetrics, DI_NORMAL, SM_CXCURSOR, SM_CYCURSOR};

    use super::source::FrameBuf;

    /// 光标位图缓存。`dib` / `old` 只是"持有句柄别被回收"，没有读取操作。
    #[allow(dead_code)]
    struct Cache {
        w: i32,
        h: i32,
        dc: HDC,
        dib: HBITMAP,
        old: HGDIOBJ,
        bits: *mut u8,
    }

    thread_local! {
        static CACHE: std::cell::Cell<*mut Cache> = const { std::cell::Cell::new(std::ptr::null_mut()) };
        /// 借用计数，见 `zx_core::borrow`。
        static BORROW: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// 取光标缓存（一个复用的 DIB + DC，画摄像头小窗时逐帧用）。
    ///
    /// 返回借用守卫：同一个函数里嵌套取两次会产生两个 `&mut`（UB），debug 构建
    /// 由 `zx_core::borrow::Ref` 直接断言，不用再靠"消息循环一次只处理一条"的约定。
    fn cache() -> Option<zx_core::borrow::Ref<Cache>> {
        CACHE.with(|c| {
            let p = c.get();
            if p.is_null() {
                unsafe {
                    let w = GetSystemMetrics(SM_CXCURSOR).max(32);
                    let h = GetSystemMetrics(SM_CYCURSOR).max(32);
                    let screen = GetDC(None);
                    let memdc = CreateCompatibleDC(screen);
                    let bmi = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: w,
                            biHeight: -h,
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
                    let dib = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                        .unwrap_or_default();
                    ReleaseDC(None, screen);
                    if dib.is_invalid() || bits.is_null() {
                        return None;
                    }
                    let old = SelectObject(memdc, dib);
                    let boxed = Box::new(Cache { w, h, dc: memdc, dib, old, bits: bits as *mut u8 });
                    c.set(Box::into_raw(boxed));
                }
            }
            let p = c.get();
            if p.is_null() {
                None
            } else {
                Some(unsafe { zx_core::borrow::Ref::new(p, &BORROW, "录屏光标缓存") })
            }
        })
    }

    /// 把 `icon` 画到 (x, y)（输出缓冲坐标）。
    pub unsafe fn blit(out: &mut FrameBuf, icon: HICON, x: i32, y: i32) {
        unsafe {
            let Some(c) = cache() else { return };
            // 清空（全部透明黑）
            std::ptr::write_bytes(c.bits, 0, (c.w * c.h * 4) as usize);
            if DrawIconEx(c.dc, 0, 0, icon, c.w, c.h, 0, None, DI_NORMAL).is_err() {
                return;
            }
            // 检测"带 alpha"还是"老式掩码图标"：后者 alpha 全 0
            let n = (c.w * c.h) as usize;
            let mut any_alpha = false;
            for i in 0..n {
                if *c.bits.add(i * 4 + 3) != 0 {
                    any_alpha = true;
                    break;
                }
            }
            let dstride = out.stride();
            for cy in 0..c.h {
                let oy = y + cy;
                if oy < 0 || oy >= out.h {
                    continue;
                }
                for cx in 0..c.w {
                    let ox = x + cx;
                    if ox < 0 || ox >= out.w {
                        continue;
                    }
                    let s = (cy * c.w + cx) as usize * 4;
                    let d = oy as usize * dstride + ox as usize * 4;
                    let b = *c.bits.add(s);
                    let g = *c.bits.add(s + 1);
                    let r = *c.bits.add(s + 2);
                    let a = *c.bits.add(s + 3) as u32;
                    // 老式图标：DrawIconEx 已经把背景涂成黑、光标本体有颜色。
                    // 这里把"黑且 alpha=0"当透明，"有颜色"当不透明——绝大多数系统光标都吃这套。
                    let alpha = if any_alpha {
                        a
                    } else if r as u32 + g as u32 + b as u32 > 24 {
                        255
                    } else {
                        0
                    };
                    if alpha == 0 {
                        continue;
                    }
                    let inv = 255 - alpha;
                    out.buf[d] = ((b as u32 * alpha + out.buf[d] as u32 * inv) / 255) as u8;
                    out.buf[d + 1] = ((g as u32 * alpha + out.buf[d + 1] as u32 * inv) / 255) as u8;
                    out.buf[d + 2] = ((r as u32 * alpha + out.buf[d + 2] as u32 * inv) / 255) as u8;
                    out.buf[d + 3] = 255;
                }
            }
            let _ = HWND::default();
        }
    }
}

// ---------------------------------------------------------------- 自检

/// 合成自检：不需要屏幕、不需要摄像头，四种布局各出一张 PNG。
pub fn selftest_compose() -> bool {
    let dir = zx_core::cfg::run_dir();
    let screen = {
        let mut f = FrameBuf::new(800, 600);
        f.clear(0xFF2B2B2B);
        // 画几条"文字行"
        for i in 0..12 {
            let y = 40 + i * 38;
            let w = 200 + (i * 37) % 420;
            for yy in y..(y + 10) {
                for xx in 40..(40 + w) {
                    if yy < f.h && xx < f.w {
                        let o = yy as usize * f.stride() + xx as usize * 4;
                        f.buf[o] = 200;
                        f.buf[o + 1] = 200;
                        f.buf[o + 2] = 200;
                        f.buf[o + 3] = 255;
                    }
                }
            }
        }
        f
    };
    let cam = {
        let mut f = FrameBuf::new(320, 240);
        for y in 0..240 {
            for x in 0..320 {
                let o = y as usize * f.stride() + x as usize * 4;
                // 彩色棋盘 + 渐变的"人脸"感觉
                let cx = (x as f32 - 160.0) / 90.0;
                let cy = (y as f32 - 120.0) / 110.0;
                let d = cx * cx + cy * cy;
                let (r, g, b) = if d < 1.0 {
                    (230, 190, 160)
                } else {
                    (((x / 20) % 2 * 60 + 30) as u8, ((y / 20) % 2 * 60 + 40) as u8, 90)
                };
                f.buf[o] = b;
                f.buf[o + 1] = g;
                f.buf[o + 2] = r;
                f.buf[o + 3] = 255;
            }
        }
        f
    };

    let mut ok = true;
    for (name, mode) in [
        ("screen", Mode::Screen),
        ("pip", Mode::Pip),
        ("side", Mode::SideBySide),
        ("camonly", Mode::Camera),
    ] {
        let spec = Spec {
            mode,
            corner: compose::Corner::BottomRight,
            size_pct: 24,
            shape: compose::Shape::Rounded,
            mirror: true,
            border: true,
            shadow: true,
            margin: 16,
            max_width: 960,
        };
        let p = compose::plan(&spec, (800, 600), Some((320, 240)));
        let mut out = FrameBuf::new(p.w, p.h);
        compose::render(&p, Some(&screen), Some(&cam), &mut out, 0xFF101010);
        // 转成 Canvas 存 PNG（复用现有 GDI+ 编码）
        let mut cv = zx_ui::Canvas::new(out.w, out.h, 96);
        {
            let dst = cv.pixels_mut();
            let n = dst.len().min(out.buf.len());
            dst[..n].copy_from_slice(&out.buf[..n]);
        }
        let path = dir.join(format!("record-selftest-{name}.png"));
        match cv.png_bytes() {
            Some(png) => {
                if std::fs::write(&path, png).is_err() {
                    ok = false;
                } else {
                    zx_core::log::info(format!("合成自检：{} ({}x{})", path.display(), out.w, out.h));
                }
            }
            None => ok = false,
        }
    }
    ok
}

/// GIF 自检：造 20 帧递进色块，写出 GIF 再读回来断言帧数/尺寸/循环标记。
pub fn selftest_gif() -> bool {
    let dir = zx_core::cfg::run_dir();
    let path = dir.join("record-selftest.gif");
    let (w, h) = (240, 160);
    let mut writer = match GifWriter::create(&path, w, h, 10) {
        Ok(w) => w,
        Err(e) => {
            zx_core::log::warn(format!("GIF 自检：建文件失败 {e}"));
            return false;
        }
    };
    let mut frame = FrameBuf::new(w, h);
    for i in 0..20u32 {
        frame.clear(0xFF101010);
        // 一个移动的方块：前 10 帧在动，后 10 帧静止（验证脏矩形把重复帧折掉）
        let bx = if i < 10 { 10 + i as i32 * 18 } else { 172 };
        for y in 40..120 {
            for x in bx..(bx + 48).min(w) {
                if x < 0 || x >= w {
                    continue;
                }
                let o = y as usize * frame.stride() + x as usize * 4;
                frame.buf[o] = 60;
                frame.buf[o + 1] = 180;
                frame.buf[o + 2] = 240;
                frame.buf[o + 3] = 255;
            }
        }
        if let Err(e) = writer.push(&frame) {
            zx_core::log::warn(format!("GIF 自检：push 失败 {e}"));
            writer.abort();
            return false;
        }
    }
    let ticks = writer.ticks;
    let out = match writer.finish() {
        Ok(p) => p,
        Err(e) => {
            zx_core::log::warn(format!("GIF 自检：收尾失败 {e}"));
            return false;
        }
    };
    // 读回来核对
    let bytes = match std::fs::read(&out) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut dec = match opts.read_info(std::io::Cursor::new(bytes)) {
        Ok(d) => d,
        Err(e) => {
            zx_core::log::warn(format!("GIF 自检：解码失败 {e}"));
            return false;
        }
    };
    let mut n = 0;
    let mut delays = Vec::new();
    let mut total_cs = 0u32;
    while let Ok(Some(f)) = dec.read_next_frame() {
        n += 1;
        delays.push(f.delay);
        total_cs += f.delay as u32;
    }
    // 时长要对得上：20 节拍 @10fps = 2000ms = 200 厘秒
    let want_cs = ticks * 100 / 10;
    let dur_ok = (total_cs as i32 - want_cs as i32).abs() <= 2;
    let ok = n >= 2 && delays.iter().all(|d| *d > 0) && dur_ok;
    zx_core::log::info(format!(
        "GIF 自检：push {ticks} 节拍 → {n} 帧，总时长 {total_cs}cs（期望 {want_cs}cs，{}），delay 前 8 ={:?}，{} 字节 → {}",
        if dur_ok { "对齐" } else { "偏差过大" },
        &delays[..delays.len().min(8)],
        std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0),
        out.display()
    ));
    ok
}

/// 端到端自检：真的走一遍「抓屏 → 合成 → 编码 → 落盘」，2.5 秒自动完成。
///
/// 不依赖交互（不用框选、不用点按钮），所以能在无人值守的机器上跑。
/// 产出格式跟随 `ZHUXIA_RECORD_FORMAT`（video / gif）。
pub fn selftest_e2e() -> bool {
    let want_gif = std::env::var("ZHUXIA_RECORD_FORMAT")
        .map(|v| v.trim().eq_ignore_ascii_case("gif"))
        .unwrap_or_else(|_| zx_core::cfg::cfg().record().format == RecordFormat::Gif);
    let ext = if want_gif { "gif" } else { "mp4" };
    let b = virtual_bounds();
    // 用 1080p 大小的选区：这才是 60fps 的真实压力测试。
    // `ZHUXIA_SEL` 可覆盖（如 `2560x1440`），用来测更大区域的帧率。
    let (rw, rh) = std::env::var("ZHUXIA_SEL")
        .ok()
        .and_then(|v| {
            let mut it = v.split('x');
            Some((it.next()?.trim().parse::<i32>().ok()?, it.next()?.trim().parse::<i32>().ok()?))
        })
        .unwrap_or((1920, 1080));
    let w = rw.min(b.w - 40).max(160);
    let h = rh.min(b.h - 40).max(120);
    let sel = Rect::new(b.x + (b.w - w) / 2, b.y + (b.h - h) / 2, w, h);
    let out = zx_core::cfg::run_dir().join(format!("record-selftest-e2e.{ext}"));
    let _ = std::fs::remove_file(&out);
    // 把光标挪到选区中心，确保「光标也进了帧」这条被真正验证；跑完放回原处。
    let saved = {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
        let mut p = POINT::default();
        unsafe {
            let ok = GetCursorPos(&mut p).is_ok();
            let _ = SetCursorPos(sel.x + sel.w / 2, sel.y + sel.h / 2);
            if ok { Some((p.x, p.y)) } else { None }
        }
    };
    // 时长可覆盖：长时录制要验"跑十分钟音画还对齐"，2.5 秒验不出来
    let dur_ms: u64 = std::env::var("ZHUXIA_RECORD_AUTO")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &u64| *v >= 500)
        .unwrap_or(2500);
    let dur_s = dur_ms as f64 / 1000.0;
    std::env::set_var("ZHUXIA_RECORD_AUTO", dur_ms.to_string());
    std::env::set_var("ZHUXIA_RECORD_OUT", &out);
    let ok = run(sel);
    std::env::remove_var("ZHUXIA_RECORD_AUTO");
    std::env::remove_var("ZHUXIA_RECORD_OUT");
    if let Some((x, y)) = saved {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y);
        }
    }
    if !ok {
        zx_core::log::warn("端到端自检失败：run() 返回 false");
        return false;
    }
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    if size < 1024 {
        zx_core::log::warn(format!("端到端自检失败：{} 只有 {size} 字节", out.display()));
        return false;
    }
    // 读回来核对：GIF 看帧数+时长，视频用 MF 解码看帧数+尺寸
    if want_gif {
        let Ok(bytes) = std::fs::read(&out) else { return false };
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::RGBA);
        let Ok(mut dec) = opts.read_info(std::io::Cursor::new(bytes)) else { return false };
        let mut n = 0;
        let mut cs = 0u32;
        while let Ok(Some(f)) = dec.read_next_frame() {
            n += 1;
            cs += f.delay as u32;
        }
        let want_cs = (dur_s * 100.0) as i32;
        let dur_ok = (cs as i32 - want_cs).abs() <= (want_cs / 10).max(40);
        zx_core::log::info(format!(
            "端到端自检（GIF）：{}x{} 选区 → {n} 帧 / {cs}cs（期望 {want_cs}cs）/ {size} 字节{} → {}",
            w, h,
            if dur_ok { "" } else { "（时长偏差大）" },
            out.display()
        ));
        n >= 3 && dur_ok && size > 1024
    } else {
        let fps = std::env::var("ZHUXIA_RECORD_FPS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or_else(|| zx_core::cfg::cfg().record().fps.max(1) as u32);
        let Some((vf, vw, vh)) = mp4::probe_frames(&out) else {
            zx_core::log::warn("端到端自检失败：MP4 读不回来（编码器或容器有问题）");
            return false;
        };
        let want = (dur_s * fps as f64).round();
        let dur_ok = (vf as f64 - want).abs() <= want * 0.25 + 3.0;
        zx_core::log::info(format!(
            "端到端自检（视频）：{}x{} 选区 → {vf} 帧 / {vw}×{vh} / {size} 字节（期望约 {want:.0} 帧）{} → {}",
            w, h,
            if dur_ok { "" } else { "（帧数偏差大）" },
            out.display()
        ));
        // 尺寸可能被配置里的"输出宽度上限"缩小（这是预期行为），
        // 所以只要求"不超过选区、比例一致、非空"，不要求严格相等。
        let ratio_ok = vw > 0 && vh > 0 && (vw as i32) <= w && (vh as i32) <= h;

        // 音轨：配置里开了声音就必须真的有，且时长要贴住画面
        let cfg = zx_core::cfg::cfg().record();
        let mut audio_ok = true;
        if cfg.audio.any() {
            match mp4::probe_audio(&out) {
                Some((rate, ch, af, peak)) => {
                    let secs = af as f64 / rate.max(1) as f64;
                    audio_ok = (secs - dur_s).abs() <= (dur_s * 0.05).max(0.7) && ch >= 1;
                    // 静音也能过时长校验，所以峰值单独看：真有人在放声音时应该是响的
                    let loud_ok = peak > 300;
                    if std::env::var("ZHUXIA_AUDIO_EXPECT").ok().as_deref() == Some("1") {
                        audio_ok = audio_ok && loud_ok;
                    }
                    zx_core::log::info(format!(
                        "端到端自检（音轨）：{rate}Hz/{ch}ch，{} 采样帧 = {secs:.2}s（期望 {dur_s:.2}s），峰值 {peak}{}{}",
                        af,
                        if audio_ok { "" } else { "（时长或声道不对）" },
                        if loud_ok { "" } else { "（当前是静音，如需验声请先放点声音并设 ZHUXIA_AUDIO_EXPECT=1）" }
                    ));
                }
                None => {
                    audio_ok = false;
                    zx_core::log::warn("端到端自检（音轨）：MP4 里读不到音轨——AAC 编码器可能不可用");
                }
            }
        } else {
            zx_core::log::info("端到端自检：配置里声音是关的，跳过音轨校验");
        }

        (vf as f64) >= want * 0.7 && ratio_ok && audio_ok
    }
}

/// 音频自检入口（供 `--record-selftest-audio` 调用）。
pub fn selftest_audio() -> bool {
    let out = zx_core::cfg::run_dir().join("record-selftest-audio.wav");
    let ms: u64 = std::env::var("ZHUXIA_AUDIO_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    audio::selftest(ms, &out)
}

/// 摄像头自检：列设备 + 抓一帧存 PNG。没有设备时优雅失败。
pub fn selftest_camera() -> bool {
    let list = camera::list();
    zx_core::log::info(format!("摄像头 {} 个：{:?}", list.len(), list.iter().map(|d| &d.name).collect::<Vec<_>>()));
    if list.is_empty() {
        zx_core::log::warn("摄像头自检：没有找到设备（跳过抓帧）");
        return false;
    }
    let mut reader = match camera::CameraReader::open("", 640, 480, 30) {
        Ok(r) => r,
        Err(e) => {
            zx_core::log::warn(format!("摄像头自检：打开失败 {e}"));
            return false;
        }
    };
    let mut f = FrameBuf::new(reader.w, reader.h);
    // 等最多 2 秒出第一帧
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut got = false;
    while Instant::now() < deadline {
        if reader.read(&mut f) {
            got = true;
            break;
        }
    }
    if !got {
        zx_core::log::warn("摄像头自检：2 秒内没有取到帧");
        return false;
    }
    let mut cv = zx_ui::Canvas::new(f.w, f.h, 96);
    {
        let dst = cv.pixels_mut();
        let n = dst.len().min(f.buf.len());
        dst[..n].copy_from_slice(&f.buf[..n]);
    }
    let path = zx_core::cfg::run_dir().join("record-selftest-camera.png");
    match cv.png_bytes() {
        Some(png) => match std::fs::write(&path, png) {
            Ok(()) => {
                zx_core::log::info(format!("摄像头自检：{}x{} → {}", f.w, f.h, path.display()));
                true
            }
            Err(_) => false,
        },
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamped_name_has_no_colons() {
        let n = stamped_name();
        assert!(!n.contains(':'), "{n}");
        assert!(n.starts_with("录屏_"));
    }

    #[test]
    fn explicit_gif_wins_over_env_and_config() {
        assert_eq!(resolve_format(Some(RecordFormat::Gif), Some("video")), Some(RecordFormat::Gif));
        assert_eq!(resolve_format(None, Some("gif")), Some(RecordFormat::Gif));
        assert_eq!(resolve_format(None, Some("VIDEO")), Some(RecordFormat::Video));
        assert_eq!(resolve_format(None, None), None);
    }

    #[test]
    fn unique_path_counts_up() {
        let dir = std::env::temp_dir().join("zx-record-test");
        let _ = std::fs::create_dir_all(&dir);
        let base = "zz-unique-probe";
        let p1 = unique_path(&dir, base, "mp4");
        assert_eq!(p1.file_name().unwrap(), format!("{base}.mp4").as_str());
        std::fs::write(&p1, b"x").unwrap();
        let p2 = unique_path(&dir, base, "mp4");
        assert_eq!(p2.file_name().unwrap(), format!("{base}-2.mp4").as_str());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
