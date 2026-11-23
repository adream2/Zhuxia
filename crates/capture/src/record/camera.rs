//! 摄像头（外部设备）采集：Media Foundation `IMFSourceReader`。
//!
//! 为什么不引 `nokhwa` / OpenCV：它们为"跨平台 + 全功能"付的代价（bindings crates、
//! mozjpeg、几十 MB 的 native 库）在这个只需要"取一帧 BGRA"的场景里是纯成本。
//! `windows` 0.58 已经把 MF 绑定全带上了，加一行 feature 就能用。
//!
//! 两条硬规则：
//!
//! 1. **`ReadSample` 是阻塞的**，所以采集必须在自己的线程里，结果放进"最新帧槽"。
//! 2. **合成侧只取最新帧，绝不排队**。摄像头 30fps、录制 10fps 时逐帧消费会越积越多，
//!    最后画面延迟十几秒；丢中间帧才是正确做法（OBS 同理）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use windows::core::{GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

use super::source::FrameBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CamState {
    /// 线程在打开设备
    Starting,
    Running,
    /// 设备掉了（拔出/被占用），最后一帧会保留
    Lost,
    Failed,
}

/// 进程级 `MFStartup`。MF 内部是引用计数的，这里只 start 一次、进程退出由系统回收
/// （常驻进程没有"干净卸载"的必要，强行 shutdown 反而会让后续枚举失败）。
fn ensure_mf() -> Result<(), String> {
    static INITED: OnceLock<Result<(), String>> = OnceLock::new();
    INITED
        .get_or_init(|| unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("MFStartup 失败：{e}"))
        })
        .clone()
}

/// 枚举视频输入设备。
pub fn list() -> Vec<Device> {
    let mut out = Vec::new();
    if ensure_mf().is_err() {
        return out;
    }
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        if MFCreateAttributes(&mut attrs, 1).is_err() {
            return out;
        }
        let Some(attrs) = attrs else { return out };
        if attrs
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .is_err()
        {
            return out;
        }
        let mut raw: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        if MFEnumDeviceSources(&attrs, &mut raw, &mut count).is_err() || raw.is_null() {
            return out;
        }
        let slice = std::slice::from_raw_parts(raw, count as usize);
        for act in slice.iter().flatten() {
            let name = friendly_name(act);
            if !name.trim().is_empty() {
                out.push(Device { name });
            }
        }
        CoTaskMemFree(Some(raw as *const core::ffi::c_void));
    }
    out
}

unsafe fn friendly_name(act: &IMFActivate) -> String {
    let mut p = PWSTR(std::ptr::null_mut());
    let mut len = 0u32;
    if act
        .GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut p, &mut len)
        .is_ok()
        && !p.is_null()
    {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        return s;
    }
    String::new()
}

/// 一个已打开的摄像头。只在一个线程里用（`IMFSourceReader` 不是 `Send` 的通用对象，
/// 这里通过"线程独占 + 通道"绕开，别把它跨线程传）。
pub struct CameraReader {
    reader: IMFSourceReader,
    pub w: i32,
    pub h: i32,
}

impl CameraReader {
    /// 打开设备。`device` 为空 = 第一个可用设备；名字按 `list()` 的结果匹配。
    pub fn open(device: &str, req_w: i32, req_h: i32, fps: i32) -> Result<Self, String> {
        ensure_mf()?;
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1).map_err(|e| format!("MFCreateAttributes 失败：{e}"))?;
            let attrs = attrs.ok_or("MFCreateAttributes 返回空")?;
            let _ = attrs.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            );
            let mut raw: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFEnumDeviceSources(&attrs, &mut raw, &mut count)
                .map_err(|e| format!("枚举摄像头失败：{e}"))?;
            if raw.is_null() || count == 0 {
                return Err("没有找到摄像头".into());
            }
            let slice = std::slice::from_raw_parts(raw, count as usize);
            let want = device.trim();
            let mut chosen: Option<IMFActivate> = None;
            let mut names = Vec::new();
            for act in slice.iter().flatten() {
                let n = friendly_name(act);
                let hit = want.is_empty() || n == want || n.contains(want);
                if hit && chosen.is_none() {
                    chosen = Some(act.clone());
                }
                names.push(n);
            }
            CoTaskMemFree(Some(raw as *const core::ffi::c_void));
            let Some(act) = chosen else {
                return Err(format!("没找到摄像头「{want}」（可用：{}）", names.join(" / ")));
            };

            let source: IMFMediaSource = act
                .ActivateObject()
                .map_err(|e| format!("激活摄像头失败：{e}"))?;
            // 让 MF 自动插入色彩转换器（USB 摄像头常常只给 YUY2/NV12）
            let mut ratts: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut ratts, 2).map_err(|e| format!("MFCreateAttributes 失败：{e}"))?;
            let ratts = ratts.ok_or("MFCreateAttributes 返回空")?;
            let _ = ratts.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1);
            let reader = MFCreateSourceReaderFromMediaSource(&source, &ratts)
                .map_err(|e| format!("创建 SourceReader 失败：{e}"))?;

            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            // 先按请求的分辨率要，失败再退"只要 RGB32，尺寸由驱动选"
            let mut ok = false;
            for with_size in [true, false] {
                let Ok(mt) = MFCreateMediaType() else { continue };
                if mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).is_err() {
                    continue;
                }
                if mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).is_err() {
                    continue;
                }
                if with_size {
                    let _ = mt.SetUINT64(
                        &MF_MT_FRAME_SIZE,
                        ((req_w.max(16) as u64) << 32) | req_h.max(16) as u64,
                    );
                    if fps > 0 {
                        // 帧率是 (分子<<32)|分母
                        let _ = mt.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1);
                    }
                }
                if reader.SetCurrentMediaType(stream, None, &mt).is_ok() {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return Err("摄像头不支持 RGB32 输出".into());
            }

            let (mut w, mut h) = (req_w.max(16), req_h.max(16));
            if let Ok(cur) = reader.GetCurrentMediaType(stream) {
                if let Ok(packed) = cur.GetUINT64(&MF_MT_FRAME_SIZE) {
                    w = (packed >> 32) as i32;
                    h = (packed & 0xFFFF_FFFF) as i32;
                }
            }
            if w <= 0 || h <= 0 {
                return Err("读不到摄像头分辨率".into());
            }
            zx_core::log::info(format!("摄像头已打开：{w}x{h}（请求 {req_w}x{req_h}@{fps}）"));
            Ok(CameraReader { reader, w, h })
        }
    }

    /// 取一帧到 `out`（尺寸自动对到设备分辨率）。返回 false = 这一轮没有新帧。
    pub fn read(&mut self, out: &mut FrameBuf) -> bool {
        if out.w != self.w || out.h != self.h {
            *out = FrameBuf::new(self.w, self.h);
        }
        unsafe {
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            if self
                .reader
                .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                .is_err()
            {
                return false;
            }
            // ENDOFSTREAM / 没拿到样本：这一轮跳过（录制继续，不卡住）
            let Some(sample) = sample else { return false };
            let Ok(buf) = sample.ConvertToContiguousBuffer() else { return false };
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut cur = 0u32;
            if buf.Lock(&mut ptr, None, Some(&mut cur)).is_err() || ptr.is_null() {
                return false;
            }
            // RGB32 的实际 stride 可能带 padding：用 cur / h 反推最稳
            let h = self.h as usize;
            let stride = (cur as usize / h).max(self.w as usize * 4);
            let copy_w = (self.w as usize * 4).min(stride);
            let dstride = out.stride();
            for y in 0..h {
                let s = y * stride;
                let d = y * dstride;
                if s + copy_w > cur as usize || d + dstride > out.buf.len() {
                    break;
                }
                out.buf[d..d + copy_w].copy_from_slice(std::slice::from_raw_parts(ptr.add(s), copy_w));
                // RGB32 的 alpha 字节未定义，统一刷成不透明
                for x in (d + 3..d + copy_w).step_by(4) {
                    out.buf[x] = 255;
                }
            }
            let _ = buf.Unlock();
            true
        }
    }
}

/// 最新帧槽。
pub struct Slot {
    pub w: i32,
    pub h: i32,
    pub buf: Vec<u8>,
    pub seq: u64,
    pub state: CamState,
    pub name: String,
    pub error: String,
}

impl Slot {
    fn blank() -> Self {
        Slot {
            w: 0,
            h: 0,
            buf: Vec::new(),
            seq: 0,
            state: CamState::Starting,
            name: String::new(),
            error: String::new(),
        }
    }
}

/// 摄像头采集线程 + 最新帧。`Drop` 会停线程。
pub struct Feed {
    slot: Arc<Mutex<Slot>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Feed {
    /// 立刻返回，设备在后台打开（倒计时那几秒正好用来启动）。
    pub fn start(device: String, w: i32, h: i32, fps: i32) -> Self {
        let slot = Arc::new(Mutex::new(Slot::blank()));
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = slot.clone();
        let st2 = stop.clone();
        let join = std::thread::Builder::new()
            .name("zx-camera".into())
            .spawn(move || {
                let mut reader = match CameraReader::open(&device, w, h, fps) {
                    Ok(r) => r,
                    Err(e) => {
                        zx_core::log::warn(format!("摄像头启动失败：{e}"));
                        let mut g = s2.lock().unwrap_or_else(|p| p.into_inner());
                        g.state = CamState::Failed;
                        g.error = e;
                        return;
                    }
                };
                {
                    let mut g = s2.lock().unwrap_or_else(|p| p.into_inner());
                    g.w = reader.w;
                    g.h = reader.h;
                    g.buf = vec![0u8; reader.w as usize * 4 * reader.h as usize];
                    g.state = CamState::Running;
                    g.name = device.clone();
                }
                let mut frame = FrameBuf::new(reader.w, reader.h);
                let mut misses = 0u32;
                while !st2.load(Ordering::Relaxed) {
                    if reader.read(&mut frame) {
                        misses = 0;
                        let mut g = s2.lock().unwrap_or_else(|p| p.into_inner());
                        if g.buf.len() == frame.buf.len() {
                            g.buf.copy_from_slice(&frame.buf);
                            g.seq = g.seq.wrapping_add(1);
                            g.state = CamState::Running;
                        }
                    } else {
                        misses += 1;
                        // 连续拿不到帧 = 设备掉了/被别的程序抢了：标记 Lost，线程退出
                        if misses > 60 {
                            let mut g = s2.lock().unwrap_or_else(|p| p.into_inner());
                            g.state = CamState::Lost;
                            zx_core::log::warn("摄像头连续取不到帧，标记为断开");
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            })
            .ok();
        Feed { slot, stop, join }
    }

    /// 取最新一帧（复制到 `out`）。返回 `(seq, state)`。
    pub fn poll(&self, out: &mut FrameBuf) -> (u64, CamState) {
        let g = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if g.buf.is_empty() || g.w <= 0 || g.h <= 0 {
            return (g.seq, g.state);
        }
        if out.w != g.w || out.h != g.h {
            *out = FrameBuf::new(g.w, g.h);
        }
        if out.buf.len() == g.buf.len() {
            out.buf.copy_from_slice(&g.buf);
        }
        (g.seq, g.state)
    }

    pub fn state(&self) -> CamState {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).state
    }

    pub fn error(&self) -> String {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).error.clone()
    }

    pub fn size(&self) -> Option<(i32, i32)> {
        let g = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if g.w > 0 && g.h > 0 && !g.buf.is_empty() {
            Some((g.w, g.h))
        } else {
            None
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 设备名 → 给人看的短名（去掉 " (VID_xxxx&PID_xxxx)" 之后的部分太长，这里只截断）。
pub fn short_name(name: &str) -> String {
    let n = name.trim();
    if n.chars().count() <= 28 {
        return n.to_string();
    }
    let s: String = n.chars().take(27).collect();
    format!("{s}…")
}

#[allow(dead_code)]
fn _assert_guid_usable(_g: GUID) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_name_truncates() {
        assert_eq!(short_name("cam"), "cam");
        let long = "a".repeat(40);
        let s = short_name(&long);
        assert_eq!(s.chars().count(), 28);
        assert!(s.ends_with('…'));
    }
}
