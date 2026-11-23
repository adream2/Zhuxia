//! 手工探针：`cargo run -p appicon --example probe -- <文件>...`
//! 输出到临时目录，不改源文件。

use std::path::PathBuf;

use appicon::output::{write_set, Mode, Options};

fn main() {
    let out_dir = std::env::temp_dir().join("zx-appicon-probe");
    let _ = std::fs::create_dir_all(&out_dir);
    for a in std::env::args().skip(1) {
        let p = PathBuf::from(&a);
        print!("{} ... ", p.display());
        match appicon::probe::extract(&p) {
            Ok(set) => {
                let opts = Options {
                    mode: Mode::All,
                    out_dir: Some(out_dir.clone()),
                    ..Default::default()
                };
                match write_set(&p, &set, &opts) {
                    Ok(o) => println!(
                        "OK {} 帧（{}）· {} → {}",
                        set.frames.len(),
                        set.size_list(),
                        set.detail,
                        o.outputs.iter().map(|x| x.display().to_string()).collect::<Vec<_>>().join(", ")
                    ),
                    Err(e) => println!("写盘失败：{}", e.reason()),
                }
            }
            Err(e) => println!("FAIL {}（{:?}）", e.reason(), e.detail()),
        }
    }
}
