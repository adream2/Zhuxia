//! 单文件提取耗时：`cargo run --release -p appicon --example bench -- <文件>...`

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    for a in std::env::args().skip(1) {
        let p = PathBuf::from(&a);
        let t = Instant::now();
        match appicon::probe::extract(&p) {
            Ok(set) => println!(
                "{:>6} ms  {:>4} 帧  {:<16} {}",
                t.elapsed().as_millis(),
                set.frames.len(),
                set.size_list(),
                p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            ),
            Err(e) => println!(
                "{:>6} ms  FAIL {:<30} {}",
                t.elapsed().as_millis(),
                e.tag(),
                p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            ),
        }
    }
}
