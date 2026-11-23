//! 显示器 / 任务栏拓扑探针。
//!
//! 用法：cargo run --release --example displays_probe -p zx-core

fn main() {
    let list = zx_core::monitor::displays();
    println!("displays = {}", list.len());
    for d in &list {
        println!("  id={}", d.id);
        println!("     name={:?} device={:?} primary={} taskbar=0x{:X} bounds={:?}",
                 d.name, d.device_name, d.primary, d.taskbar, d.bounds);
    }
}
