//! 多平台图床（第八模块）：设置面板里的一页，页内三 tab（上传 / 图片 / 设置），
//! 后端可切 GitHub、S3、WebDAV、自定义。
//!
//! 常驻进程只负责"快捷上传"（截图工具条、剪贴板热键、托盘）；界面只活在设置面板
//! 进程里，不额外开窗口。设计见 `docs/IMGBED.md`。
//!
//! 平台差异被 `provider::Backend` 收口；这里只做编排：读配置 → 准备字节（转 WebP）→
//! 调后端 → 出链接 → 记历史。

pub mod clip;
pub mod history;
pub mod hotkey;
pub mod http;
pub mod link;
pub mod path;
pub mod provider;
pub mod providers;
pub mod queue;
pub mod registry;
pub mod secrets;
pub mod toast;
pub mod upload;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use zx_core::ImgBedSettings;

use provider::Fail;

/// 一次上传的结果，给界面/命令行共用。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ItemResult {
    pub ok: bool,
    pub source: String,
    pub provider: String,
    pub name: String,
    pub path: String,
    pub url: String,
    pub text: String,
    pub note: String,
    pub error: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub ms: u64,
}

impl ItemResult {
    pub fn err(source: &str, name: &str, error: impl Into<String>) -> ItemResult {
        ItemResult {
            source: source.into(),
            name: name.into(),
            error: error.into(),
            ..Default::default()
        }
    }
}

/// 当前配置（每次现读，设置页改完立刻生效）。
pub fn cfg() -> ImgBedSettings {
    zx_core::cfg::cfg().imgbed()
}

/// 宿主在 `modules::install()` 里调一次。
pub fn start() {
    zx_ui::painter::init_gdiplus();
    let c = cfg();
    queue::start();
    hotkey::start();
    maintenance();
    if !c.enabled {
        zx_core::log::info("图床模块已禁用（settings.yaml → imgbed.enabled）");
        return;
    }
    let configured = registry::build(&c).is_ok();
    zx_core::log::info(format!(
        "图床模块已就绪：平台 {}，路径 {}，{}，热键 {}",
        c.provider,
        registry::default_path(&c, &c.provider),
        if configured { "已配置" } else { "未配置" },
        if c.hotkey.is_empty() { "<关闭>" } else { &c.hotkey }
    ));
}

// ---------------------------------------------------------------- 上传入口

/// 截图工具条的落点：PNG 字节。
pub fn upload_png(bytes: Vec<u8>) {
    queue::submit(queue::Job::Bytes { name: None, bytes });
}

/// 热键/托盘：剪贴板图片。
pub fn upload_clipboard() {
    hotkey::trigger();
}

/// 设置页的"上传测试图"（同步，1–3 秒）。
pub fn upload_test_now() -> Result<String, String> {
    let png = clip::test_png();
    let r = upload_bytes("zhuxia-test.png", png, None, "test")
        .into_iter()
        .next()
        .ok_or_else(|| "没有结果".to_string())?;
    if !r.ok {
        return Err(r.error);
    }
    if cfg().auto_copy {
        clip::set_text(&r.text);
    }
    Ok(r.url)
}

/// 批量上传本地文件。`dir_override` 为空 = 用当前平台的默认路径。
pub fn upload_files(paths: &[PathBuf], dir_override: Option<&str>, source: &str) -> Vec<ItemResult> {
    let cfg = cfg();
    let backend = match registry::build(&cfg) {
        Ok(b) => b,
        Err(e) => {
            return paths
                .iter()
                .map(|p| {
                    ItemResult::err(source, &file_label(p), e.text())
                })
                .collect()
        }
    };
    let dir = match resolve_dir(&cfg, dir_override) {
        Ok(d) => d,
        Err(e) => {
            return paths
                .iter()
                .map(|p| ItemResult::err(source, &file_label(p), e.text()))
                .collect()
        }
    };
    paths
        .iter()
        .map(|p| {
            let name = file_label(p);
            let started = Instant::now();
            let bytes = match std::fs::read(p) {
                Ok(b) => b,
                Err(e) => return ItemResult::err(source, &name, format!("读文件失败：{e}")),
            };
            let prep = match upload::prepare(&cfg, Some(&name), bytes, source) {
                Ok(p) => p,
                Err(e) => return ItemResult::err(source, &name, e.text()),
            };
            match upload::upload_one(backend.as_ref(), &cfg, &dir, prep, source) {
                Ok(o) => to_result(&cfg, o, started, source),
                Err(e) => ItemResult {
                    ms: started.elapsed().as_millis() as u64,
                    ..ItemResult::err(source, &name, e.text())
                },
            }
        })
        .collect()
}

/// 单张内存图（截图 / 剪贴板 / 测试）。
pub fn upload_bytes(
    name: &str,
    bytes: Vec<u8>,
    dir_override: Option<&str>,
    source: &str,
) -> Vec<ItemResult> {
    let cfg = cfg();
    let started = Instant::now();
    let backend = match registry::build(&cfg) {
        Ok(b) => b,
        Err(e) => return vec![ItemResult::err(source, name, e.text())],
    };
    let dir = match resolve_dir(&cfg, dir_override) {
        Ok(d) => d,
        Err(e) => return vec![ItemResult::err(source, name, e.text())],
    };
    let prep = match upload::prepare(&cfg, Some(name), bytes, source) {
        Ok(p) => p,
        Err(e) => return vec![ItemResult::err(source, name, e.text())],
    };
    match upload::upload_one(backend.as_ref(), &cfg, &dir, prep, source) {
        Ok(o) => vec![to_result(&cfg, o, started, source)],
        Err(e) => vec![ItemResult {
            ms: started.elapsed().as_millis() as u64,
            ..ItemResult::err(source, name, e.text())
        }],
    }
}

fn to_result(cfg: &ImgBedSettings, o: upload::Outcome, started: Instant, source: &str) -> ItemResult {
    ItemResult {
        ok: true,
        source: source.into(),
        provider: cfg.provider.clone(),
        name: o.record.name.clone(),
        path: o.record.path.clone(),
        url: o.record.url.clone(),
        text: o.text,
        note: o.note.unwrap_or_default(),
        error: String::new(),
        bytes: o.record.bytes,
        width: o.record.width,
        height: o.record.height,
        ms: started.elapsed().as_millis() as u64,
    }
}

/// 上传页实时预览"最终写到哪"。
pub fn resolve_dir(cfg: &ImgBedSettings, dir_override: Option<&str>) -> Result<String, Fail> {
    let raw = match dir_override {
        Some(d) => d.trim().to_string(),
        None => registry::default_path(cfg, &cfg.provider),
    };
    path::render(&raw)
}

fn file_label(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

// ---------------------------------------------------------------- 平台 / 设置

pub fn metas() -> Vec<provider::ProviderMeta> {
    provider::metas()
}

/// 当前平台配置里的默认上传路径（未渲染）。
pub fn default_path() -> String {
    let c = cfg();
    registry::default_path(&c, &c.provider)
}

/// 当前平台是否已经能上传（配置 + 密钥齐）。
pub fn configured() -> bool {
    registry::build(&cfg()).is_ok()
}

/// 平台连接测试。
pub fn test() -> Result<Vec<String>, String> {
    let cfg = cfg();
    let backend = registry::build(&cfg).map_err(|e| e.text())?;
    backend.test().map(|r| r.lines).map_err(|e| e.text())
}

/// 只回哪些密钥有值，永不回原文。
pub fn secret_keys() -> Vec<String> {
    secrets::set_keys()
}

pub fn set_secret(key: &str, value: &str) -> Result<(), String> {
    secrets::set(key, value)
}

pub fn clear_secret(key: &str) -> Result<(), String> {
    secrets::clear(key)
}

pub fn hotkey_state() -> (String, String, String) {
    hotkey::state()
}

// ---------------------------------------------------------------- 图片页

pub fn history_query(q: &str, provider: &str, sort: &str, offset: usize, limit: usize) -> (Vec<history::Record>, usize) {
    history::query(q, provider, sort, offset, limit)
}

pub fn history_count() -> usize {
    history::len()
}

pub fn history_remove(id: &str) -> bool {
    history::remove(id)
}

pub fn history_clear() {
    history::clear()
}

pub fn remote_dirs(dir: &str) -> Result<Vec<String>, String> {
    let cfg = cfg();
    let backend = registry::build(&cfg).map_err(|e| e.text())?;
    if !backend.caps().list_dirs {
        return Err("该平台不支持浏览目录".into());
    }
    backend.list_dirs(dir).map_err(|e| e.text())
}

pub fn remote_images(dir: &str) -> Result<Vec<provider::RemoteImage>, String> {
    let cfg = cfg();
    let backend = registry::build(&cfg).map_err(|e| e.text())?;
    if !backend.caps().list_images {
        return Err("该平台不支持远端浏览".into());
    }
    backend.list_images(dir).map_err(|e| e.text())
}

pub fn delete_remote(path: &str) -> Result<(), String> {
    let cfg = cfg();
    let backend = registry::build(&cfg).map_err(|e| e.text())?;
    if !backend.caps().delete {
        return Err("该平台不支持删除".into());
    }
    backend.delete(path).map_err(|e| e.text())
}

/// 生成/读取缩略图（WebP 字节）。界面用 data URL 显示，避免放开 CSP。
pub fn thumbnail(path: &str, provider_id: &str, url: &str) -> Result<Vec<u8>, String> {
    thumb_cache(path, provider_id, url)
}

fn thumb_cache(path: &str, provider_id: &str, url: &str) -> Result<Vec<u8>, String> {
    let dir = zx_core::cfg::run_dir().join("imgbed").join("thumbs");
    let _ = std::fs::create_dir_all(&dir);
    let key = path::hash16(format!("{provider_id}:{path}:{url}").as_bytes());
    let file = dir.join(format!("{key}.webp"));
    if let Ok(b) = std::fs::read(&file) {
        if !b.is_empty() {
            return Ok(b);
        }
    }
    // 只有绝对路径才当本地文件读；记录里的 path 是仓库/桶内路径，不能按相对路径误读
    let local = std::path::Path::new(path);
    let bytes = if local.is_absolute() && local.is_file() {
        std::fs::read(local).ok()
    } else {
        None
    }
    .or_else(|| fetch_url(url, &cfg().proxy))
    .ok_or_else(|| "取缩略图失败（链接不可访问？）".to_string())?;
    let img = zx_media::decode(&bytes).map_err(|e| e.text())?;
    let webp = zx_media::thumbnail_webp(&img, 320).map_err(|e| e.text())?;
    let _ = std::fs::write(&file, &webp);
    Ok(webp)
}

/// 灯箱取原图（公开链接；私有仓库请用缩略图/本地记录）。
pub fn image_bytes(url: &str) -> Result<Vec<u8>, String> {
    fetch_url(url, &cfg().proxy).ok_or_else(|| "取原图失败（链接不可访问或需要鉴权）".to_string())
}

fn fetch_url(url: &str, proxy: &str) -> Option<Vec<u8>> {
    if url.is_empty() || !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    let resp = http::request("GET", url, &[("User-Agent", "Zhuxia-ImgBed")], None, proxy, 20_000).ok()?;
    if !(200..300).contains(&resp.status) || resp.body.is_empty() {
        return None;
    }
    Some(resp.body)
}

// ---------------------------------------------------------------- 维护

fn maintenance() {
    std::thread::Builder::new()
        .name("zx-imgbed-maint".into())
        .spawn(|| {
            let mut last = settings_mtime();
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                let mtime = settings_mtime();
                if mtime != last {
                    last = mtime;
                    zx_core::cfg::reload();
                    hotkey::reload();
                    zx_core::log::info("图床配置已重载");
                }
            }
        })
        .ok();
}

fn settings_mtime() -> Option<std::time::SystemTime> {
    zx_core::cfg::settings_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

// ---------------------------------------------------------------- 命令行

pub fn handle_cli(args: &[String]) -> Option<i32> {
    if args.iter().any(|a| a == "--imgbed-selftest") {
        return Some(selftest());
    }
    if args.iter().any(|a| a == "--imgbed-backends") {
        return Some(cli_backends());
    }
    if let Some(i) = args.iter().position(|a| a == "--imgbed-dirs") {
        let dir = args.get(i + 1).filter(|a| !a.starts_with("--")).cloned().unwrap_or_default();
        return Some(match remote_dirs(&dir) {
            Ok(list) => {
                for d in list { println!("{d}"); }
                0
            }
            Err(e) => { eprintln!("{e}"); 1 }
        });
    }
    if let Some(i) = args.iter().position(|a| a == "--imgbed-ls") {
        let dir = args.get(i + 1).filter(|a| !a.starts_with("--")).cloned().unwrap_or_default();
        return Some(match remote_images(&dir) {
            Ok(list) => {
                for r in list { println!("{}	{}	{}", r.name, r.bytes, r.url); }
                0
            }
            Err(e) => { eprintln!("{e}"); 1 }
        });
    }
    if args.iter().any(|a| a == "--imgbed-test") {
        return Some(cli_test());
    }
    if let Some(i) = args.iter().position(|a| a == "--imgbed-token") {
        return Some(cli_token(args.get(i + 1)));
    }
    if let Some(i) = args.iter().position(|a| a == "--imgbed-secret") {
        let key = args.get(i + 1).cloned().unwrap_or_default();
        let value = args.get(i + 2).cloned().unwrap_or_default();
        if key.is_empty() || value.is_empty() {
            eprintln!("用法：--imgbed-secret <provider.field> <value>");
            return Some(2);
        }
        return Some(match set_secret(&key, &value) {
            Ok(()) => {
                println!("{key} 已加密保存");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        });
    }
    if args.iter().any(|a| a == "--imgbed-token-clear") {
        return Some(match clear_secret("github.token") {
            Ok(()) => {
                println!("已删除 github.token");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        });
    }
    if let Some(i) = args.iter().position(|a| a == "--imgbed") {
        return Some(cli_upload(args, i));
    }
    None
}

fn quiet() -> bool {
    std::env::var("ZHUXIA_IMGBED_QUIET").ok().as_deref() == Some("1")
}

fn cli_upload(args: &[String], i: usize) -> i32 {
    zx_ui::painter::init_gdiplus();
    let files: Vec<PathBuf> = args[i + 1..]
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if files.is_empty() {
        if !quiet() {
            toast::show_blocking("图床", &["命令行没给图片路径".into()], true);
        }
        return 2;
    }
    let results = upload_files(&files, None, "file");
    let mut ok = 0;
    for r in &results {
        if r.ok {
            ok += 1;
            println!("{}\t{}\t{}", r.path, upload::human(r.bytes), r.url);
            if !r.note.is_empty() {
                println!("\t{}", r.note);
            }
        } else {
            eprintln!("{}：{}", r.name, r.error);
        }
    }
    if ok > 0 && cfg().auto_copy {
        let text = results
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        clip::set_text(&text);
    }
    if !quiet() {
        if ok == results.len() {
            toast::show_blocking(
                &format!("已上传 {ok} 张"),
                &[toast::ellipsis(&results[0].text, 86)],
                false,
            );
        } else {
            let msg = results.iter().find(|r| !r.ok).map(|r| r.error.clone()).unwrap_or_default();
            toast::show_blocking("上传失败", &[toast::ellipsis(&msg, 90)], true);
        }
    }
    if ok == results.len() { 0 } else { 1 }
}

fn cli_test() -> i32 {
    match test() {
        Ok(lines) => {
            println!("连接正常（{}）", cfg().provider);
            for l in &lines {
                println!("  {l}");
            }
            0
        }
        Err(e) => {
            eprintln!("连接失败：{e}");
            1
        }
    }
}

fn cli_backends() -> i32 {
    for m in metas() {
        println!(
            "{}\t{}\t目录={} 列图={} 删除={} 上限={}MB",
            m.id,
            m.name,
            m.caps.list_dirs,
            m.caps.list_images,
            m.caps.delete,
            m.caps.max_mb
        );
        for f in &m.fields {
            println!(
                "  {:<14} {:<10} {}{}{}",
                f.key,
                f.label,
                if f.required { "必填 " } else { "" },
                if f.secret { "密钥 " } else { "" },
                f.hint
            );
        }
    }
    0
}

fn cli_token(arg: Option<&String>) -> i32 {
    let Some(t) = arg.filter(|t| !t.starts_with("--")) else {
        eprintln!("用法：--imgbed-token < token >（写到 secrets.dat 的 github.token）");
        return 2;
    };
    match set_secret("github.token", t) {
        Ok(()) => {
            println!("github.token 已加密保存");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

// ---------------------------------------------------------------- 离线自检

pub fn selftest() -> i32 {
    let mut pass = 0;
    let mut fail = 0;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            pass += 1;
            println!("PASS  {name}");
        } else {
            fail += 1;
            println!("FAIL  {name}  {detail}");
        }
    };

    let cfg = zx_core::Settings {
        imgbed: Some(zx_core::ImgBedCfg {
            upload: zx_core::ImgBedUploadCfg {
                naming: Some("timestamp".into()),
                convert: Some("webp".into()),
                quality: Some(82),
                max_side: Some(2560),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
    .imgbed();

    let p = path::render("images/{yyyy}/{MM}").unwrap_or_default();
    check(
        "路径：日期变量渲染",
        p.starts_with("images/") && p.split('/').count() == 3,
        &p,
    );

    let rejected = path::render("a/../b").is_err()
        && path::render("a//b").is_err()
        && path::render("a\\b").is_err();
    check("路径：非法段被拒", rejected, "");

    let png = clip::test_png();
    let kind = path::kind_of(&png).map(|(e, _)| e).unwrap_or("");
    check("类型：PNG 魔数", kind == "png", kind);

    let prep = upload::prepare(&cfg, Some("a.png"), png.clone(), "test");
    match prep {
        Ok(x) => {
            let decoded = zx_media::decode(&x.bytes).is_ok();
            check(
                "WebP：转换后仍是有效图片",
                x.ext == "webp" && decoded && x.name.ends_with(".webp"),
                &x.name,
            );
        }
        Err(e) => check("WebP：转换", false, &e.text()),
    }

    // 长边：0 = 不限制；>0 超出则等比缩小（自检图是 64×64）
    let unlimited = zx_core::Settings {
        imgbed: Some(zx_core::ImgBedCfg {
            upload: zx_core::ImgBedUploadCfg {
                convert: Some("webp".into()),
                max_side: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
    .imgbed();
    let kept = upload::prepare(&unlimited, Some("a.png"), png.clone(), "test");
    check(
        "长边：0 = 不限制",
        matches!(&kept, Ok(x) if x.width == 64 && x.height == 64),
        "",
    );

    let shrunk = upload::prepare(&cfg, Some("a.png"), png.clone(), "test");
    let small_cfg = zx_core::Settings {
        imgbed: Some(zx_core::ImgBedCfg {
            upload: zx_core::ImgBedUploadCfg {
                convert: Some("webp".into()),
                max_side: Some(32),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
    .imgbed();
    let half = upload::prepare(&small_cfg, Some("a.png"), png.clone(), "test");
    check(
        "长边：超出按比例缩",
        matches!(&half, Ok(x) if x.width == 32 && x.height == 32)
            && matches!(&shrunk, Ok(x) if x.width == 64),
        "",
    );

    let tpl = link::template_for("jsdelivr", "").unwrap_or_default();
    let url = link::resolve(&tpl, "me", "imgs", "main", "images/中文 1.webp");
    check(
        "链接：jsDelivr + 中文编码",
        url.contains("%E4%B8%AD%E6%96%87%201.webp"),
        &url,
    );

    let metas = metas();
    check("平台：四家元数据齐全", metas.len() == 4, &format!("{}", metas.len()));
    check(
        "平台：自定义接口无列目录能力",
        metas.iter().any(|m| m.id == "custom" && !m.caps.list_dirs && !m.caps.list_images),
        "",
    );

    let each_default = metas
        .iter()
        .filter(|m| m.id != "custom")
        .all(|m| !m.default_path.is_empty());
    check("平台：默认可写路径", each_default, "");

    // 配置落到四个平台都能反序列化（不联网）
    let mut config_ok = true;
    for m in &metas {
        let mut map = serde_yaml::Mapping::new();
        for f in &m.fields {
            let v = match f.kind {
                provider::FieldKind::Bool => serde_yaml::Value::Bool(false),
                provider::FieldKind::Kv => serde_yaml::Value::Mapping(Default::default()),
                _ => serde_yaml::Value::String(String::new()),
            };
            map.insert(serde_yaml::Value::String(f.key.into()), v);
        }
        let s = zx_core::Settings {
            imgbed: Some(zx_core::ImgBedCfg {
                provider: Some(m.id.into()),
                providers: [(m.id.to_string(), serde_yaml::Value::Mapping(map))].into_iter().collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c = s.imgbed();
        if c.provider != m.id || c.provider_value().is_none() {
            config_ok = false;
        }
    }
    check("平台：配置结构可往返", config_ok, "");

    let sizes = upload::human(1536) == "1 KB" && upload::human(2 * 1024 * 1024) == "2.0 MB";
    check("体积显示", sizes, "");

    println!("\n{pass} 项通过，{fail} 项失败");
    if fail == 0 { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selftest_passes() {
        assert_eq!(selftest(), 0);
    }

    #[test]
    fn cli_routes() {
        let a = vec!["zhuxia".to_string(), "--imgbed-selftest".to_string()];
        assert!(handle_cli(&a).is_some());
        let a = vec!["zhuxia".to_string(), "--imgconv".to_string()];
        assert!(handle_cli(&a).is_none());
    }

    #[test]
    fn dir_override_semantics() {
        let c = zx_core::Settings::default().imgbed();
        assert_eq!(resolve_dir(&c, Some("")).unwrap(), "");
        assert!(resolve_dir(&c, Some("a/{yyyy}")).unwrap().starts_with("a/"));
    }
}
