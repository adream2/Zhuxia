//! MP4（H.264）输出探针。
//!
//! 验证「录屏 = 视频」这条路在这台机器上成立：用系统自带的 Media Foundation
//! SinkWriter 写一个 H.264 MP4，再用 SourceReader 读回来核对帧数与尺寸。
//! **不依赖 ffmpeg.exe、不引第三方编码库**——这是本项目能"零新增依赖"出视频的前提。
//!
//! ```text
//! cargo run --release --example h264_probe -- 0          # 默认 1080p30 / 8Mbps
//! cargo run --release --example h264_probe -- 1          # 1440p60 / 20Mbps
//! ```

use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::*;

fn pack(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run(w: u32, h: u32, fps: u32, bitrate: u32) -> Result<(), String> {
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("MFStartup: {e}"))?;
        let path = std::env::temp_dir().join(format!("zx-h264-probe-{w}x{h}@{fps}.mp4"));
        let _ = std::fs::remove_file(&path);
        let wpath = wide(&path.to_string_lossy());

        let t0 = Instant::now();
        // 允许使用显卡自带的编码器（Intel QuickSync / NVIDIA NVENC / AMD VCE）。
        // 不设这个 attribute 时 MF 只用 CPU 软件编码，速度差 3~4 倍。
        let hw = std::env::var("ZX_HW").ok().as_deref() == Some("1");
        let mut attrs: Option<IMFAttributes> = None;
        if hw {
            MFCreateAttributes(&mut attrs, 1).map_err(|e| format!("{e}"))?;
            if let Some(a) = attrs.as_ref() {
                let _ = a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1);
            }
        }
        let writer = MFCreateSinkWriterFromURL(PCWSTR(wpath.as_ptr()), None, attrs.as_ref())
            .map_err(|e| format!("MFCreateSinkWriterFromURL: {e}"))?;

        // ---- 输出类型：H.264 ----
        let out_mt = MFCreateMediaType().map_err(|e| format!("{e}"))?;
        out_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| format!("major: {e}"))?;
        out_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(|e| format!("h264 subtype: {e}"))?;
        out_mt.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(|e| format!("bitrate: {e}"))?;
        out_mt.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).map_err(|e| format!("size: {e}"))?;
        out_mt.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(|e| format!("fps: {e}"))?;
        out_mt
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| format!("interlace: {e}"))?;
        let stream = writer.AddStream(&out_mt).map_err(|e| format!("AddStream: {e}"))?;

        // ---- 输入类型：RGB32（和我们合成出来的 BGRA 布局一致）----
        let in_mt = MFCreateMediaType().map_err(|e| format!("{e}"))?;
        in_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| format!("{e}"))?;
        in_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| format!("rgb32: {e}"))?;
        in_mt.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).map_err(|e| format!("{e}"))?;
        in_mt.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(|e| format!("{e}"))?;
        // RGB32 的 stride 必须显式声明：MF 的 DIB 转换器默认按 bottom-up 理解，
        // 我们给的是 top-down（BitBlt 的 DIB 就是自上而下），所以这里必须是正数。
        let stride_mode = std::env::var("ZX_STRIDE").unwrap_or_else(|_| "pos".into());
        match stride_mode.as_str() {
            "none" => {}
            "neg" => { let _ = in_mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, (-(w as i32 * 4)) as u32); }
            _ => { let _ = in_mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4); }
        }
        in_mt
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| format!("{e}"))?;
        writer
            .SetInputMediaType(stream, &in_mt, None)
            .map_err(|e| format!("SetInputMediaType: {e}（H.264 编码器可能不可用）"))?;
        // 显式开始：虽然文档说第一次 WriteSample 会自动调，但实测不调就报
        // MF_E_INVALIDREQUEST（0xC00D36B2），第 0 帧就挂。
        writer.BeginWriting().map_err(|e| format!("BeginWriting: {e}"))?;

        // ---- 写 2 秒：一个移动的色块，逼编码器真的编码 ----
        let frames = fps * 2;
        let stride = (w * 4) as usize;
        let bytes = stride * h as usize;
        let dur = 10_000_000i64 / fps as i64;
        let mut px = vec![0u8; bytes];
        // 背景只填一次：否则测出来的是"我画图的速度"，不是编码速度
        for y in 0..h as usize {
            for x in 0..w as usize {
                let o = y * stride + x * 4;
                px[o] = 32;
                px[o + 1] = 32;
                px[o + 2] = 40;
                px[o + 3] = 255;
            }
        }
        let bw = 160usize;
        let mut prev_bx: Option<usize> = None;
        let encode_start = Instant::now();
        for i in 0..frames {
            // 只擦上一帧的块、画这一帧的块 —— 开销恒定且很小
            if let Some(pb) = prev_bx {
                for y in (h as usize / 3)..(h as usize * 2 / 3) {
                    for x in pb..(pb + bw).min(w as usize) {
                        let o = y * stride + x * 4;
                        px[o] = 32;
                        px[o + 1] = 32;
                        px[o + 2] = 40;
                    }
                }
            }
            let bx = ((i as usize * 23) % (w as usize - bw)).max(1);
            for y in (h as usize / 3)..(h as usize * 2 / 3) {
                for x in bx..(bx + bw).min(w as usize) {
                    let o = y * stride + x * 4;
                    px[o] = 200;
                    px[o + 1] = 60;
                    px[o + 2] = 240;
                }
            }
            prev_bx = Some(bx);
            let buf = MFCreateMemoryBuffer(bytes as u32).map_err(|e| format!("{e}"))?;
            let mut p: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| format!("lock: {e}"))?;
            std::ptr::copy_nonoverlapping(px.as_ptr(), p, bytes);
            let _ = buf.Unlock();
            buf.SetCurrentLength(bytes as u32).map_err(|e| format!("{e}"))?;

            let sample = MFCreateSample().map_err(|e| format!("{e}"))?;
            sample.AddBuffer(&buf).map_err(|e| format!("{e}"))?;
            sample.SetSampleTime(i as i64 * dur).map_err(|e| format!("{e}"))?;
            sample.SetSampleDuration(dur).map_err(|e| format!("{e}"))?;
            writer
                .WriteSample(stream, &sample)
                .map_err(|e| format!("WriteSample 第 {i} 帧失败：{e}"))?;
        }
        writer.Finalize().map_err(|e| format!("Finalize: {e}"))?;
        let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;

        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        println!(
            "\n✓ {w}×{h}@{fps} {:.1}Mbps：写 {frames} 帧用 {encode_ms:.0}ms（{:.1}ms/帧，{}），文件 {}",
            bitrate as f64 / 1e6,
            encode_ms / frames as f64,
            if encode_ms / frames as f64 <= 1000.0 / fps as f64 { "够实时" } else { "⚠ 慢于实时" },
            humans(size)
        );
        println!("  {}", path.display());
        println!("  建立 SinkWriter 到出第一帧前，总耗时 {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);

        // ---- 读回来核对 ----
        match read_back(&wpath) {
            Ok((n, rw, rh)) => {
                println!("  读回：{n} 帧，{rw}×{rh}");
                if n < frames / 2 {
                    eprintln!("  ⚠ 读回帧数明显偏少，可能有丢帧");
                }
            }
            Err(e) => eprintln!("  读回失败：{e}"),
        }
        Ok(())
    }
}

fn humans(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / 1048576.0)
    } else {
        format!("{} KB", b / 1024)
    }
}

fn read_back(wpath: &[u16]) -> Result<(u32, u32, u32), String> {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wpath.as_ptr()), None)
            .map_err(|e| format!("SourceReader: {e}"))?;
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
            .map_err(|e| format!("{e}"))?;
        reader
            .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
            .map_err(|e| format!("{e}"))?;
        let mut w = 0u32;
        let mut h = 0u32;
        if let Ok(mt) = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
            if let Ok(p) = mt.GetUINT64(&MF_MT_FRAME_SIZE) {
                w = (p >> 32) as u32;
                h = (p & 0xFFFF_FFFF) as u32;
            }
        }
        let mut n = 0;
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
                break; // ENDOFSTREAM
            }
            if sample.is_some() {
                n += 1;
            }
            if n > 10_000 {
                break;
            }
        }
        Ok((n, w, h))
    }
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    let cases: Vec<(u32, u32, u32, u32)> = match arg.as_str() {
        "1" => vec![(2560, 1440, 60, 20_000_000)],
        "2" => vec![(1920, 1080, 60, 16_000_000)],
        "3" => vec![(1920, 1080, 30, 4_000_000)],
        _ => vec![(1920, 1080, 30, 8_000_000)],
    };
    for (w, h, fps, br) in cases {
        if let Err(e) = run(w, h, fps, br) {
            eprintln!("✗ {w}×{h}@{fps} 失败：{e}");
        }
    }
}
