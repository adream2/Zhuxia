//! 亮度探针：验证这台机器的显示器能不能用 DDC/CI 调亮度。
//!
//! `cargo run --release --example brightness_probe` → 先 −5% 再 +5%（还原），打印每一步结果。

use gesture::edges::run_action;
use zx_core::cfg::EdgeAction;

fn main() {
    for (dir, name) in [(-1, "调暗 5%"), (1, "调亮 5%")] {
        match run_action(EdgeAction::Brightness, dir) {
            Ok(()) => println!("{name}：OK"),
            Err(e) => println!("{name}：失败 —— {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
