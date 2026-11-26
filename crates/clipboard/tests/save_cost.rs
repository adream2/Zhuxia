//! 量「整份历史重写」的代价，用来给 `store::MAX_PERSIST_BYTES` 定档。
//!
//! ```text
//! cargo test -p clipboard --test save_cost -- --ignored --nocapture
//! ```
//!
//! 结论（2026-11-26，本机）：DPAPI 的固定开销约 2ms，之后约 57MB/s。
//! 1000 条小文本（0.25MB）合计约 4ms，可以忽略；一条 10MB 的文本会让之后
//! **每一次复制**都付出约 576ms，而且全程握着 store 的锁。这就是上限设在
//! 1MB 的依据——不是拍脑袋定的。
//!
//! 用的是 `secure::protect` + `fs::write`，和 `Store::save` 走同一条加密路径，
//! 所以数字可以直接对照。

fn json_of(entries: usize, each_bytes: usize) -> Vec<u8> {
    let text = "x".repeat(each_bytes);
    let mut out = String::from("{\"version\":2,\"entries\":[");
    for i in 0..entries {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{i},\"kind\":\"text\",\"content\":\"{text}\",\"preview\":\"p\",\"use_count\":1}}"
        ));
    }
    out.push_str("]}");
    out.into_bytes()
}

#[test]
#[ignore]
fn measure() {
    for (entries, each) in [(50usize, 200usize), (1000, 200), (20, 512 * 1024)] {
        let plain = json_of(entries, each);
        let mb = plain.len() as f64 / 1048576.0;

        let t0 = std::time::Instant::now();
        let sealed = clipboard::secure::protect(&plain);
        let enc = t0.elapsed();

        let t1 = std::time::Instant::now();
        let p = std::env::temp_dir().join("zx-save-cost.bin");
        let _ = std::fs::write(&p, sealed.as_deref().unwrap_or(&[]));
        let wr = t1.elapsed();
        let _ = std::fs::remove_file(&p);

        println!(
            "{entries:>4} 条 x {each:>6}B = {mb:>6.2} MB | 加密 {enc:?} | 写盘 {wr:?} | 合计 {:?}",
            enc + wr
        );
    }
}
