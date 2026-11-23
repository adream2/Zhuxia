//! 音频链路探针：枚举设备、采 3 秒、打印电平、写 WAV。
//!
//! 用法：`cargo run --release --example audio_probe`
//! 想验证"系统声音能不能录到"，跑之前先放点音乐/视频。
//! `ZHUXIA_AUDIO_SYS=0` 只测麦克风，`ZHUXIA_AUDIO_MIC=1` 也测麦克风。

fn main() {
    if std::env::args().any(|a| a == "--diag") {
        let ok = capture::record::audio::diag(2500);
        std::process::exit(if ok { 0 } else { 1 });
    }
    let out = std::env::current_dir().unwrap_or_default().join("audio-probe.wav");
    let ms: u64 = std::env::var("ZX_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let ok = capture::record::audio::selftest(ms, &out);
    println!("音频探针：{}（{}）", if ok { "✓ 通过" } else { "✗ 失败" }, out.display());
    std::process::exit(if ok { 0 } else { 1 });
}
