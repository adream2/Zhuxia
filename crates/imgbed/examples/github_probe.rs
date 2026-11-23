//! 网络探针：确认 WinHTTP 能连上 api.github.com（TLS / 代理 / DNS 任一环节坏了都能看出来）。
//!
//! ```bash
//! cargo run -p imgbed --example github_probe
//! ZHUXIA_IMGBED_PROXY=http://127.0.0.1:7890 cargo run -p imgbed --example github_probe
//! ```

fn main() {
    let proxy = std::env::var("ZHUXIA_IMGBED_PROXY").unwrap_or_default();
    let url = std::env::var("ZHUXIA_IMGBED_URL")
        .unwrap_or_else(|_| "https://api.github.com/zen".to_string());
    println!("GET {url}  proxy={}", if proxy.is_empty() { "<系统>" } else { &proxy });
    match imgbed::http::request("GET", &url, &[("User-Agent", "Zhuxia-ImgBed")], None, &proxy, 15_000) {
        Ok(r) => {
            println!("HTTP {}", r.status);
            println!("rate-remaining={:?} reset={:?}", r.rate_remaining, r.rate_reset);
            println!("{}", r.text().trim());
            if (200..300).contains(&r.status) { std::process::exit(0); }
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("失败：{e}");
            std::process::exit(1);
        }
    }
}
