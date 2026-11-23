//! 采集/粘贴音效：运行时合成一小段 WAV（无外部音频资源，也不依赖系统提示音）。
//!
//! 参考实现打包 `copy.wav` / `paste.wav` 并用音量设置播放；这里保持同样的
//! 「响一下」语义，波形由代码生成：复制是一声上扬的短音，粘贴是一声下行的短音。
//! 音量共 21 档（0%–100%，5% 一档），首次用到时生成并缓存。

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

fn wav(notes: &[(f32, f32)], volume: f64) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let mut samples: Vec<i16> = Vec::new();
    for &(freq, dur) in notes {
        let n = (RATE as f32 * dur) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            // 5ms 淡入、20ms 淡出，避免爆音
            let rise = (t / 0.005).min(1.0);
            let fall = ((dur - t) / 0.02).clamp(0.0, 1.0);
            let v = (t * freq * std::f32::consts::TAU).sin() * rise * fall * 0.28 * volume as f32;
            samples.push((v * i16::MAX as f32) as i16);
        }
    }
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// 21 档缓冲（copy / paste 各一组）。
type Bank = Vec<Vec<u8>>;

fn banks() -> &'static (Bank, Bank) {
    static B: OnceLock<(Bank, Bank)> = OnceLock::new();
    B.get_or_init(|| {
        let mut copy = Vec::with_capacity(21);
        let mut paste = Vec::with_capacity(21);
        for i in 0..=20 {
            let v = i as f64 / 20.0;
            copy.push(wav(&[(880.0, 0.045), (1320.0, 0.05)], v));
            paste.push(wav(&[(660.0, 0.05), (440.0, 0.05)], v));
        }
        (copy, paste)
    })
}

pub fn play(which: &str, volume: f64) {
    if volume <= 0.01 {
        return;
    }
    let idx = ((volume.clamp(0.0, 1.0) * 20.0).round() as usize).min(20);
    let (copy, paste) = banks();
    let data = if which == "copy" { &copy[idx] } else { &paste[idx] };
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(data.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}
