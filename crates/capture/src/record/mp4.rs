//! MP4（H.264）输出：系统自带的 Media Foundation SinkWriter。
//!
//! 零外部依赖——不需要 ffmpeg.exe、不需要装编码器。实测编码 **2.0ms/帧**
//! （1080p60），远不是瓶颈（见 `examples/h264_probe.rs`）。
//!
//! 两个必须记住的坑（都在探针里踩过）：
//!
//! 1. **必须显式 `BeginWriting()`**。文档说第一次 `WriteSample` 会自动调，
//!    实测不调就直接 `0xC00D36B2`（MF_E_INVALIDREQUEST），第 0 帧就挂。
//! 2. **RGB32 输入要给 `MF_MT_DEFAULT_STRIDE` 正数**（top-down）。我们的像素
//!    来自 BitBlt/DXGI，都是自上而下；MF 的 DIB 转换器默认按 bottom-up 理解。
//!
//! 第三条针对音轨：**AAC 输出类型必须显式声明 payload type 与 profile**。
//! 不写 `MF_MT_AAC_PAYLOAD_TYPE=0` 时有些系统会把 ADTS 头也塞进 MP4 样本，
//! 播放器能放但时长算不准。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::*;

use super::source::FrameBuf;
use zx_core::win::wide;

fn pack(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

fn mf_err(what: &str, e: windows::core::Error) -> std::io::Error {
    std::io::Error::other(format!("{what}：{e}"))
}

#[derive(Clone, Default)]
pub struct ByteCounter(Arc<AtomicU64>);

// IMFSinkWriter 是 free-threaded 的 MTA 对象，可以换线程用（我们只有一个线程在写）。
// windows-rs 不给 COM 接口自动 impl Send，这里显式声明。
unsafe impl Send for Mp4Writer {}

impl ByteCounter {
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

pub struct Mp4Writer {
    writer: Option<IMFSinkWriter>,
    stream: u32,
    /// 音轨流索引；None = 这个文件没有声音
    audio_stream: Option<u32>,
    pub w: i32,
    pub h: i32,
    fps: i32,
    /// 已写入的帧数（也用作时间戳刻度）
    pub ticks: u32,
    /// 已写入的音频采样帧数（48k 下每帧 1/48000 秒）
    pub audio_frames: u64,
    tmp: PathBuf,
    dst: PathBuf,
    bytes: ByteCounter,
    last_probe: u32,
}

impl Mp4Writer {
    /// `bitrate` 单位 bps。`fps` 与尺寸会写进输出类型，必须与实际写入的帧一致。
    /// `with_audio` 为真时同时建一条 AAC 音轨（48k/立体声/16bit 输入）。
    pub fn create(
        path: &Path,
        w: i32,
        h: i32,
        fps: i32,
        bitrate: u32,
        with_audio: bool,
    ) -> std::io::Result<Self> {
        unsafe {
            // MFStartup 是引用计数，重复调用安全；失败也不致命（可能已由摄像头初始化）
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));

            // 注意：**不要**预先创建这个文件。SinkWriter 看到已存在的空文件会报
            // 0xC00D36D5（它要自己创建并写 moov 头）。
            // 临时名必须保留 `.mp4` 扩展名：SinkWriter 靠扩展名决定封装格式，
            // 写成 `xxx.mp4.part` 会被判成未知类型（0xC00D36D5）。
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "record".into());
            let tmp = path.with_file_name(format!("{stem}.part.mp4"));
            let _ = std::fs::remove_file(&tmp);
            let wpath = wide(&tmp.to_string_lossy());
            let writer = MFCreateSinkWriterFromURL(PCWSTR(wpath.as_ptr()), None, None)
                .map_err(|e| mf_err("创建 SinkWriter 失败", e))?;

            let out_mt = MFCreateMediaType().map_err(|e| mf_err("MFCreateMediaType", e))?;
            out_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| mf_err("major", e))?;
            out_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(|e| mf_err("h264", e))?;
            out_mt.SetUINT32(&MF_MT_AVG_BITRATE, bitrate.max(500_000)).map_err(|e| mf_err("bitrate", e))?;
            out_mt.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32)).map_err(|e| mf_err("size", e))?;
            out_mt.SetUINT64(&MF_MT_FRAME_RATE, pack(fps as u32, 1)).map_err(|e| mf_err("fps", e))?;
            out_mt
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| mf_err("interlace", e))?;
            let stream = writer.AddStream(&out_mt).map_err(|e| mf_err("AddStream", e))?;

            let in_mt = MFCreateMediaType().map_err(|e| mf_err("MFCreateMediaType", e))?;
            in_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| mf_err("major", e))?;
            in_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| mf_err("rgb32", e))?;
            in_mt.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32)).map_err(|e| mf_err("size", e))?;
            in_mt.SetUINT64(&MF_MT_FRAME_RATE, pack(fps as u32, 1)).map_err(|e| mf_err("fps", e))?;
            in_mt
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| mf_err("interlace", e))?;
            // top-down：我们的像素是自上而下的，必须显式声明
            let _ = in_mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, (w * 4) as u32);
            writer
                .SetInputMediaType(stream, &in_mt, None)
                .map_err(|e| mf_err("SetInputMediaType（H.264 编码器是否可用？）", e))?;

            let audio_stream = if with_audio {
                Some(add_audio_stream(&writer)?)
            } else {
                None
            };

            writer.BeginWriting().map_err(|e| mf_err("BeginWriting", e))?;

            Ok(Mp4Writer {
                writer: Some(writer),
                stream,
                audio_stream,
                w,
                h,
                fps: fps.max(1),
                ticks: 0,
                audio_frames: 0,
                tmp,
                dst: path.to_path_buf(),
                bytes: ByteCounter::default(),
                last_probe: 0,
            })
        }
    }

    /// 写入一帧，时间戳由**节拍序号**决定。
    ///
    /// 为什么不用"第几个写入的帧"：机器跟不上目标帧率时（临时卡顿、抓屏变慢），
    /// 按写入计数会让视频时间轴比真实时间短——画面变成快放，而音轨是实时的，
    /// 于是越录越不同步。按节拍序号写，丢帧表现为"这一格画面晚出现"，
    /// 和音轨始终对齐。
    pub fn push_at(&mut self, frame: &FrameBuf, tick: u32) -> std::io::Result<()> {
        self.write_frame(frame, tick)
    }

    fn write_frame(&mut self, frame: &FrameBuf, tick: u32) -> std::io::Result<()> {
        let Some(writer) = self.writer.as_ref() else {
            return Err(std::io::Error::other("编码器已关闭"));
        };
        let bytes = self.w as usize * 4 * self.h as usize;
        if frame.buf.len() < bytes {
            return Err(std::io::Error::other("帧尺寸与编码器不符"));
        }
        unsafe {
            let buf = MFCreateMemoryBuffer(bytes as u32).map_err(|e| mf_err("MFCreateMemoryBuffer", e))?;
            let mut p: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| mf_err("Lock", e))?;
            // 尺寸严格等于 w*4*h：SinkWriter 会校验输入缓冲长度
            std::ptr::copy_nonoverlapping(frame.buf.as_ptr(), p, bytes);
            let _ = buf.Unlock();
            buf.SetCurrentLength(bytes as u32).map_err(|e| mf_err("SetCurrentLength", e))?;

            let sample = MFCreateSample().map_err(|e| mf_err("MFCreateSample", e))?;
            sample.AddBuffer(&buf).map_err(|e| mf_err("AddBuffer", e))?;
            // 时间戳单位 100ns
            let dur = 10_000_000i64 / self.fps as i64;
            sample
                .SetSampleTime(tick as i64 * dur)
                .map_err(|e| mf_err("SetSampleTime", e))?;
            sample.SetSampleDuration(dur).map_err(|e| mf_err("SetSampleDuration", e))?;
            writer.WriteSample(self.stream, &sample).map_err(|e| mf_err("WriteSample", e))?;
        }
        self.ticks = self.ticks.max(tick).saturating_add(1);
        // 每 15 帧看一眼文件大小（给控制条显示体积，不能每帧 stat）
        if self.ticks.saturating_sub(self.last_probe) >= 15 {
            self.last_probe = self.ticks;
            if let Ok(m) = std::fs::metadata(&self.tmp) {
                self.bytes.0.store(m.len(), Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// 写入一段音频（48kHz 立体声 16bit 交错）。
    ///
    /// 时间戳按**累计采样帧数**算，不看墙上时钟——这样即使某次写入晚了，
    /// 音轨内部也永远连续，不会出现"越录越飘"。
    pub fn push_audio(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        let Some(astream) = self.audio_stream else { return Ok(()) };
        let Some(writer) = self.writer.as_ref() else {
            return Err(std::io::Error::other("编码器已关闭"));
        };
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes = pcm.len() * 2;
        let frames = (pcm.len() / 2) as u64;
        unsafe {
            let buf = MFCreateMemoryBuffer(bytes as u32).map_err(|e| mf_err("音频缓冲", e))?;
            let mut p: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| mf_err("音频 Lock", e))?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, p, bytes);
            let _ = buf.Unlock();
            buf.SetCurrentLength(bytes as u32).map_err(|e| mf_err("音频长度", e))?;

            let sample = MFCreateSample().map_err(|e| mf_err("音频样本", e))?;
            sample.AddBuffer(&buf).map_err(|e| mf_err("音频 AddBuffer", e))?;
            let t = |f: u64| (f * 10_000_000 / 48_000) as i64;
            sample.SetSampleTime(t(self.audio_frames)).map_err(|e| mf_err("音频时间戳", e))?;
            sample.SetSampleDuration(t(frames)).map_err(|e| mf_err("音频时长", e))?;
            writer.WriteSample(astream, &sample).map_err(|e| mf_err("写音频", e))?;
        }
        self.audio_frames += frames;
        Ok(())
    }

    pub fn has_audio(&self) -> bool {
        self.audio_stream.is_some()
    }

    pub fn bytes(&self) -> ByteCounter {
        self.bytes.clone()
    }

    /// 收尾：Finalize 写索引，再 rename 到目标路径。
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        if let Some(w) = self.writer.take() {
            unsafe {
                w.Finalize().map_err(|e| mf_err("Finalize", e))?;
            }
        }
        if let Ok(m) = std::fs::metadata(&self.tmp) {
            self.bytes.0.store(m.len(), Ordering::Relaxed);
        }
        std::fs::rename(&self.tmp, &self.dst)?;
        Ok(self.dst)
    }

    pub fn abort(mut self) {
        // 不 Finalize：半截文件没有索引，播放器会拒绝；直接删掉最干净
        self.writer.take();
        let _ = std::fs::remove_file(&self.tmp);
    }
}

/// 音频采样率与码率：屏幕录制里 48kHz/160kbps 已经听不出压缩感，
/// 再高只是让文件变大（讲解人声的有效带宽本来就只有一半）。
const AUDIO_RATE: u32 = 48_000;
const AUDIO_BPS: u32 = 160_000;

/// 给 SinkWriter 挂一条 AAC 音轨，输入声明为 PCM。
unsafe fn add_audio_stream(writer: &IMFSinkWriter) -> std::io::Result<u32> {
    let out_mt = MFCreateMediaType().map_err(|e| mf_err("MFCreateMediaType(audio)", e))?;
    out_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| mf_err("audio major", e))?;
    out_mt.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).map_err(|e| mf_err("aac", e))?;
    out_mt.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(|e| mf_err("aac bits", e))?;
    out_mt.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE).map_err(|e| mf_err("aac rate", e))?;
    out_mt.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).map_err(|e| mf_err("aac ch", e))?;
    out_mt
        .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BPS / 8)
        .map_err(|e| mf_err("aac bps", e))?;
    // 裸 AAC（不带 ADTS 头）+ AAC-LC
    out_mt.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0).map_err(|e| mf_err("aac payload", e))?;
    out_mt
        .SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)
        .map_err(|e| mf_err("aac profile", e))?;
    let stream = writer.AddStream(&out_mt).map_err(|e| mf_err("AddStream(audio)", e))?;

    let in_mt = MFCreateMediaType().map_err(|e| mf_err("MFCreateMediaType(pcm)", e))?;
    in_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| mf_err("pcm major", e))?;
    in_mt.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(|e| mf_err("pcm", e))?;
    in_mt.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(|e| mf_err("pcm bits", e))?;
    in_mt.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE).map_err(|e| mf_err("pcm rate", e))?;
    in_mt.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).map_err(|e| mf_err("pcm ch", e))?;
    in_mt.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).map_err(|e| mf_err("pcm align", e))?;
    in_mt
        .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_RATE * 4)
        .map_err(|e| mf_err("pcm bps", e))?;
    let _ = in_mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1);
    writer
        .SetInputMediaType(stream, &in_mt, None)
        .map_err(|e| mf_err("SetInputMediaType(audio)（AAC 编码器是否可用？）", e))?;
    Ok(stream)
}

/// 读回 MP4 的音轨：返回 (采样率, 声道数, 采样帧数, 峰值)。
///
/// 峰值是判断"真的录到声音了"的唯一可靠依据——只看时长，静音也能过。
pub fn probe_audio(path: &Path) -> Option<(u32, u32, u64, i32)> {
    unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        let wpath = wide(&path.to_string_lossy());
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wpath.as_ptr()), None).ok()?;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        reader
            .SetStreamSelection(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, true)
            .ok()?;
        // 要求解码成 PCM，否则读到的是压缩包，长度换算没有意义
        let pcm = MFCreateMediaType().ok()?;
        pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
        pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ok()?;
        let _ = reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, None, &pcm);
        let mt = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32).ok()?;
        let rate = mt.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(AUDIO_RATE);
        let ch = mt.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).max(1);
        let align = (ch * 2) as u64;
        let mut frames = 0u64;
        let mut peak = 0i32;
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            if reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                )
                .is_err()
            {
                break;
            }
            if flags & 0x2 != 0 {
                break;
            }
            if let Some(s) = sample {
                if let Ok(b) = s.ConvertToContiguousBuffer() {
                    let len = b.GetCurrentLength().unwrap_or(0);
                    frames += len as u64 / align;
                    let mut p: *mut u8 = std::ptr::null_mut();
                    if b.Lock(&mut p, None, None).is_ok() {
                        if !p.is_null() {
                            let n = (len as usize / 2).min(1 << 20);
                            let pcm = std::slice::from_raw_parts(p as *const i16, n);
                            for v in pcm {
                                peak = peak.max((*v as i32).abs());
                            }
                        }
                        let _ = b.Unlock();
                    }
                }
            }
            if frames > (AUDIO_RATE as u64) * 3600 {
                break;
            }
        }
        Some((rate, ch, frames, peak))
    }
}

/// 读回 MP4：返回 (帧数, 宽, 高)。自检用来验证"写出去的文件真的能解"。
pub fn probe_frames(path: &Path) -> Option<(u32, u32, u32)> {
    unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        let wpath = wide(&path.to_string_lossy());
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wpath.as_ptr()), None).ok()?;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true);
        let mut w = 0u32;
        let mut h = 0u32;
        if let Ok(mt) = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
            if let Ok(p) = mt.GetUINT64(&MF_MT_FRAME_SIZE) {
                w = (p >> 32) as u32;
                h = (p & 0xFFFF_FFFF) as u32;
            }
        }
        let mut n = 0u32;
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            if reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                )
                .is_err()
            {
                break;
            }
            if flags & 0x2 != 0 {
                break;
            }
            if sample.is_some() {
                n += 1;
            }
            if n > 100_000 {
                break;
            }
        }
        Some((n, w, h))
    }
}

/// 由 bpp（每像素每帧比特）换算码率。屏幕内容用固定 Mbps 会一边浪费一边糊。
pub fn bitrate_for(w: i32, h: i32, fps: i32, bpp: f64) -> u32 {
    let v = (w as f64) * (h as f64) * (fps as f64) * bpp;
    (v as u32).clamp(800_000, 120_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_scales_with_pixels_and_fps() {
        // 1080p30 0.13bpp ≈ 8Mbps
        let b = bitrate_for(1920, 1080, 30, 0.13);
        assert!((7_000_000..9_000_000).contains(&b), "{b}");
        // 60fps 应该翻倍
        let b2 = bitrate_for(1920, 1080, 60, 0.13);
        assert!((b2 as f64 / b as f64 - 2.0).abs() < 0.05);
        // 下限保护
        assert!(bitrate_for(160, 120, 10, 0.06) >= 800_000);
    }
}
