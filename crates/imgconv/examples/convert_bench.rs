//! 手工基准：`cargo run -p imgconv --example convert_bench -- <图片> <目标格式>`
//!
//! 用来盯住单张转换的耗时（解码 + 编码），也是"右键点下去多久出文件"的实测口径。

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(src), Some(fmt)) = (args.first(), args.get(1)) else {
        eprintln!("用法：bench <图片> <目标格式>");
        std::process::exit(2);
    };
    let Some(target) = imgconv::Target::parse(fmt) else {
        eprintln!("未知格式：{fmt}");
        std::process::exit(2);
    };
    let opts = imgconv::Options::default();
    // 跑两遍：第一遍含 GDI+ 冷启动，第二遍才是稳态。
    for round in 1..=2 {
        match imgconv::convert(std::path::Path::new(src), target, &opts) {
            Ok(o) => println!(
                "第 {round} 次：{} → {}  {}×{}  {} B → {} B  {} ms",
                o.source.display(),
                o.output.display(),
                o.w,
                o.h,
                o.src_bytes,
                o.out_bytes,
                o.ms
            ),
            Err(e) => {
                eprintln!("失败：{}（{:?}）", e.reason(), e.detail());
                std::process::exit(1);
            }
        }
    }
}
