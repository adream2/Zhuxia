//! ZIP 系容器：APK / AAB / IPA / MSIX-APPX（以及 zip 里的 .app 包）。
//!
//! 这四种格式外表都是 zip，区别在"图标路径写在哪"：
//!
//! | 格式 | 图标声明位置 | 本模块的做法 |
//! |---|---|---|
//! | APK | 二进制 `AndroidManifest.xml` + `resources.arsc` | 解析 AXML 找 `android:icon`，再查 ARSC 得到 `res/mipmap-*/…png` |
//! | AAB | `base/manifest/AndroidManifest.xml`（protobuf）+ `resources.pb` | 不解析 protobuf，按 `base/res/mipmap-*/…` 文件名匹配 |
//! | IPA | `Payload/*.app/Info.plist`（CFBundleIcons） | plist 取图标基名，匹配 `AppIcon*.png` |
//! | MSIX | `AppxManifest.xml` 的 `Logo` / `Square*Logo` | XML 属性取路径，连同 `.scale-*` 变体一起收 |
//!
//! 共同策略：**能精确解析就精确解析，解析不了就按文件名回退，绝不猜一张无关的 PNG**。

use std::io::Read;
use std::path::Path;

use crate::axml::{self, Value as XmlValue};
use crate::error::IconError;
use crate::model::{decode_image, IconSet};

/// 单个 zip 条目读取上限（图标不会超过几 MB，AAB 里的资源表也远小于此）。
const MAX_ENTRY: u64 = 64 * 1024 * 1024;

pub struct Zip<'a> {
    archive: zip::ZipArchive<std::fs::File>,
    names: Vec<String>,
    _path: &'a Path,
}

impl<'a> Zip<'a> {
    pub fn open(path: &'a Path) -> Result<Self, IconError> {
        let file = std::fs::File::open(path).map_err(|e| crate::error::io_error(&e, path))?;
        let archive = zip::ZipArchive::new(file)
            .map_err(|e| IconError::Corrupt(format!("zip 打开失败：{e}")))?;
        let names = archive.file_names().filter_map(|s| s.ok().map(|c| c.to_string())).collect();
        Ok(Zip { archive, names, _path: path })
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    pub fn index_ci(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    pub fn read(&mut self, idx: usize) -> Result<Vec<u8>, IconError> {
        let mut f = self
            .archive
            .by_index(idx)
            .map_err(|e| IconError::Corrupt(format!("zip 条目读取失败：{e}")))?;
        let name = f.name().map(|s| s.to_string()).unwrap_or_else(|_| format!("entry#{idx}"));
        if f.encrypted() {
            return Err(IconError::Encrypted(name));
        }
        if f.size() > MAX_ENTRY {
            return Err(IconError::TooLarge(format!("{name}：{} B", f.size())));
        }
        let mut buf = Vec::with_capacity(f.size().min(8 * 1024 * 1024) as usize);
        f.read_to_end(&mut buf)
            .map_err(|e| IconError::Corrupt(format!("{name}：{e}")))?;
        if buf.len() as u64 > MAX_ENTRY {
            return Err(IconError::TooLarge(name));
        }
        Ok(buf)
    }

    pub fn read_name(&mut self, name: &str) -> Result<Vec<u8>, IconError> {
        let idx = self.index(name).or_else(|| self.index_ci(name));
        match idx {
            Some(i) => self.read(i),
            None => Err(IconError::NoIcon(format!("zip 里没有 {name}"))),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.index(name).is_some() || self.index_ci(name).is_some()
    }
}

/// ZIP 入口：先按特征分流，再交给具体解析器。
pub fn extract(path: &Path, ext: &str) -> Result<IconSet, IconError> {
    let mut z = Zip::open(path)?;
    let names = z.names().to_vec();

    if names.iter().any(|n| n == "AndroidManifest.xml") && z.has("resources.arsc") {
        return apk(&mut z);
    }
    if names.iter().any(|n| n.starts_with("base/manifest/")) {
        return aab(&mut z);
    }
    if names.iter().any(|n| n.starts_with("Payload/") && n.ends_with(".app/Info.plist")) {
        return ipa(&mut z);
    }
    if z.has("AppxManifest.xml") {
        return msix(&mut z);
    }
    if let Some(i) = names.iter().position(|n| n.ends_with(".app/Contents/Info.plist")) {
        let prefix = names[i].trim_end_matches("Contents/Info.plist").to_string();
        return mac_zip(&mut z, &prefix);
    }
    match ext {
        "apk" => apk_fallback(&mut z, "res/"),
        "aab" => aab_fallback(&mut z, "base/res/"),
        "ipa" => ipa_fallback(&mut z, ""),
        "msix" | "appx" => msix_fallback(&mut z),
        _ => Err(IconError::Unsupported(if ext.is_empty() { "zip".into() } else { ext.into() })),
    }
}

/* --------------------------------- APK --------------------------------- */

fn apk(z: &mut Zip) -> Result<IconSet, IconError> {
    let manifest = z.read_name("AndroidManifest.xml")?;
    let doc = match axml::parse(&manifest) {
        Ok(d) => d,
        Err(e) => {
            zx_core::log::warn(format!("APK Manifest 解析失败，回退文件名匹配：{}", e.reason()));
            return apk_fallback(z, "res/");
        }
    };
    let arsc = z
        .read_name("resources.arsc")
        .ok()
        .and_then(|b| crate::arsc::Arsc::parse(&b).ok());

    let icon = doc
        .elements
        .iter()
        .find(|e| e.name == "application")
        .and_then(|app| {
            ["icon", "roundIcon"]
                .iter()
                .find_map(|want| app.attrs.iter().find(|a| &a.name == want))
        });

    if let (Some(attr), Some(arsc)) = (icon, arsc.as_ref()) {
        if let Some(v) = doc.value(attr) {
            if let Some(path) = value_path(&v, arsc, 0) {
                // anydpi 的自适应图标（.xml）密度最高，会盖过 legacy PNG。有同名的
                // 位图 density 时优先用位图：那是已经合成好的完整图标，比只有前景层好。
                if path.to_ascii_lowercase().ends_with(".xml") {
                    if let Ok(set) = apk_fallback(z, "res/") {
                        return Ok(set);
                    }
                }
                match load_icon_path(z, &path, arsc, 0) {
                    Ok(set) if !set.is_empty() => {
                        return Ok(set.finish());
                    }
                    Ok(_) => {}
                    Err(e) => zx_core::log::warn(format!("APK 声明图标 {path} 读取失败：{}", e.reason())),
                }
            }
        }
    }
    // 声明缺失/自适应图标只有矢量：回退到按文件名找位图。
    match apk_fallback(z, "res/") {
        Ok(set) => Ok(set),
        Err(e) => Err(match e {
            IconError::NoIcon(_) => IconError::NoIcon(
                "APK 里没有位图图标（可能只有自适应/矢量图标）".into(),
            ),
            other => other,
        }),
    }
}

fn value_path(v: &XmlValue, arsc: &crate::arsc::Arsc, depth: u32) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match v {
        XmlValue::Str(s) => Some(s.trim().trim_start_matches('@').to_string()),
        XmlValue::Ref(id) => match arsc.resolve(*id) {
            Some(crate::arsc::Resolved::Str(s)) => Some(s.trim().to_string()),
            Some(crate::arsc::Resolved::Ref(r)) => value_path(&XmlValue::Ref(r), arsc, depth + 1),
            _ => None,
        },
        XmlValue::Int(_) => None,
    }
}

/// 路径可能是图片，也可能是自适应图标 / layer-list 的 XML；XML 再往下解一层。
fn load_icon_path(
    z: &mut Zip,
    path: &str,
    arsc: &crate::arsc::Arsc,
    depth: u32,
) -> Result<IconSet, IconError> {
    if depth > 3 {
        return Err(IconError::NoIcon("图标引用层数过深".into()));
    }
    let p = path.trim_start_matches('/');
    let Some(idx) = z.index(p).or_else(|| z.index_ci(p)) else {
        return Err(IconError::NoIcon(format!("zip 里没有 {p}")));
    };
    let data = z.read(idx)?;
    if is_image(&data) {
        let mut set = IconSet::new(format!("APK · {p}"));
        set.push(decode_image(&data, p)?);
        return Ok(set);
    }
    if !axml::is_axml(&data) && !looks_xml(&data) {
        return Err(IconError::Corrupt(format!("{p}：不是图片也不是 XML")));
    }
    let doc = axml::parse(&data)?;
    let mut refs = doc.refs_of("foreground");
    if refs.is_empty() {
        refs = doc.drawable_refs();
    }
    for r in refs {
        if let Some(next) = value_path(&XmlValue::Ref(r), arsc, depth) {
            match load_icon_path(z, &next, arsc, depth + 1) {
                Ok(set) if !set.is_empty() => return Ok(set),
                _ => continue,
            }
        }
    }
    Err(IconError::NoIcon(format!("{p}：自适应/矢量图标里没有位图层")))
}

fn is_image(d: &[u8]) -> bool {
    d.starts_with(b"\x89PNG\r\n\x1a\n")
        || d.starts_with(b"\xFF\xD8\xFF")
        || (d.len() > 12 && &d[..4] == b"RIFF" && &d[8..12] == b"WEBP")
        || d.starts_with(b"GIF8")
        || d.starts_with(b"BM")
}

fn looks_xml(d: &[u8]) -> bool {
    let s = String::from_utf8_lossy(&d[..d.len().min(64)]);
    s.trim_start().starts_with('<')
}

/// 按文件名回退：优先 `ic_launcher`，其次 `ic_launcher_round`，再其次名字里带 icon
/// 但不是 foreground/background/monochrome 的位图。同一基名的所有 density 都收。
fn apk_fallback(z: &mut Zip, prefix: &str) -> Result<IconSet, IconError> {
    let mut bases: Vec<(String, u32)> = Vec::new();
    for n in z.names() {
        if !n.starts_with(prefix) {
            continue;
        }
        let lower = n.to_ascii_lowercase();
        if !(lower.ends_with(".png") || lower.ends_with(".webp") || lower.ends_with(".jpg")) {
            continue;
        }
        if !(lower.contains("/mipmap") || lower.contains("/drawable")) {
            continue;
        }
        let Some(stem) = file_stem(n) else { continue };
        let s = stem.to_ascii_lowercase();
        if s.contains("foreground") || s.contains("background") || s.contains("monochrome") {
            continue;
        }
        let rank = if s == "ic_launcher" {
            0
        } else if s == "ic_launcher_round" {
            1
        } else if s.contains("ic_launcher") {
            2
        } else if s.contains("app_icon") || s == "icon" {
            3
        } else if s.contains("icon") {
            4
        } else {
            continue;
        };
        bases.push((stem, rank));
    }
    let Some(best_rank) = bases.iter().map(|(_, r)| *r).min() else {
        return Err(IconError::NoIcon("没有 icon/launcher 命名的位图".into()));
    };
    let best: std::collections::HashSet<&str> = bases
        .iter()
        .filter(|(_, r)| *r == best_rank)
        .map(|(stem, _)| stem.as_str())
        .collect();
    // 同一基名的所有 density 都收：APK 的 48/72/96/144/192 就是靠这一步齐的。
    let names: Vec<String> = z
        .names()
        .iter()
        .filter(|n| file_stem(n).as_deref().map(|s| best.contains(s)).unwrap_or(false))
        .cloned()
        .collect();
    let mut set = IconSet::new(format!("{} · 按文件名匹配", prefix.trim_end_matches('/')));
    for n in names {
        if let Some(i) = z.index(&n) {
            if let Ok(data) = z.read(i) {
                if let Ok(f) = decode_image(&data, &n) {
                    set.push(f);
                }
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("命中的图标文件都无法解码".into()));
    }
    set.detail = format!("APK/AAB · {} · {} 帧（{}）", set.frames[0].origin, set.frames.len(), set.size_list());
    Ok(set.finish())
}

/* --------------------------------- AAB --------------------------------- */

fn aab(z: &mut Zip) -> Result<IconSet, IconError> {
    match aab_fallback(z, "base/res/") {
        Ok(set) => Ok(set),
        Err(IconError::NoIcon(d)) => Err(IconError::NoIcon(format!(
            "AAB 未解析 resources.pb，按文件名也没找到位图图标（{d}）"
        ))),
        Err(e) => Err(e),
    }
}

fn aab_fallback(z: &mut Zip, prefix: &str) -> Result<IconSet, IconError> {
    apk_fallback(z, prefix)
}

/* --------------------------------- IPA --------------------------------- */

fn ipa(z: &mut Zip) -> Result<IconSet, IconError> {
    let Some(i) = z
        .names()
        .iter()
        .position(|n| n.starts_with("Payload/") && n.ends_with(".app/Info.plist"))
    else {
        return Err(IconError::NoIcon("IPA 里没有 .app/Info.plist".into()));
    };
    let plist_name = z.names()[i].clone();
    let app_prefix = plist_name.trim_end_matches("Info.plist").to_string();
    let bytes = z.read(i)?;
    let names = icon_base_names(&bytes);
    if !names.is_empty() {
        let mut set = IconSet::new(format!("IPA · {app_prefix}"));
        for base in &names {
            for n in z.names().to_vec() {
                if !n.starts_with(&app_prefix) || !n.to_ascii_lowercase().ends_with(".png") {
                    continue;
                }
                let Some(stem) = file_stem(&n) else { continue };
                if stem == *base || stem.starts_with(&format!("{base}@")) || stem.starts_with(&format!("{base}~")) {
                    if let Some(idx) = z.index(&n) {
                        if let Ok(data) = z.read(idx) {
                            if let Ok(f) = decode_image(&data, &n) {
                                set.push(f);
                            }
                        }
                    }
                }
            }
        }
        if !set.is_empty() {
            set.detail = format!("IPA · 图标基名 {} · {} 帧（{}）", names.join(","), set.frames.len(), set.size_list());
            return Ok(set.finish());
        }
    }
    // 回退：AppIcon*.png → iTunesArtwork
    let mut set = IconSet::new("IPA · 按文件名匹配".to_string());
    for n in z.names().to_vec() {
        if n.starts_with(&app_prefix) && n.to_ascii_lowercase().ends_with(".png") {
            let low = n.to_ascii_lowercase();
            if low.contains("appicon") || low.contains("icon") {
                if let Some(idx) = z.index(&n) {
                    if let Ok(data) = z.read(idx) {
                        if let Ok(f) = decode_image(&data, &n) {
                            set.push(f);
                        }
                    }
                }
            }
        }
    }
    for n in ["iTunesArtwork@2x", "iTunesArtwork"] {
        if let Some(idx) = z.index(n) {
            if let Ok(data) = z.read(idx) {
                if let Ok(f) = decode_image(&data, n) {
                    set.push(f);
                }
            }
        }
    }
    if set.is_empty() {
        let has_car = z.names().iter().any(|n| n.ends_with("Assets.car"));
        return Err(IconError::NoIcon(if has_car {
            "图标只在 Assets.car 里（编译后的资源目录），当前版本不解".into()
        } else {
            "IPA 里没有 PNG 图标".into()
        }));
    }
    set.detail = format!("IPA · 回退匹配 · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

/// 从 Info.plist 取图标基名（不含扩展名）。
pub(crate) fn icon_base_names(bytes: &[u8]) -> Vec<String> {
    let Ok(v) = plist::Value::from_reader(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if !s.is_empty() && !out.iter().any(|x: &String| x == s) {
            out.push(s.to_string());
        }
    };
    if let Some(d) = v.as_dictionary() {
        for key in ["CFBundleIconFile", "CFBundleIconName"] {
            if let Some(s) = d.get(key).and_then(|x| x.as_string()) {
                push(s.trim_end_matches(".png"));
            }
        }
        for key in ["CFBundleIconFiles", "CFBundleIcons", "CFBundleIcons~ipad"] {
            collect_icon_key(d.get(key), &mut push);
        }
    }
    out
}

fn collect_icon_key(v: Option<&plist::Value>, push: &mut impl FnMut(&str)) {
    let Some(v) = v else { return };
    match v {
        plist::Value::String(s) => push(s.trim_end_matches(".png")),
        plist::Value::Array(a) => {
            for x in a {
                collect_icon_key(Some(x), push);
            }
        }
        plist::Value::Dictionary(d) => {
            for (k, x) in d {
                if k.contains("Icon") || k == "CFBundlePrimaryIcon" {
                    collect_icon_key(Some(x), push);
                }
            }
        }
        _ => {}
    }
}

/* ------------------------------ MSIX / APPX ------------------------------ */

fn msix(z: &mut Zip) -> Result<IconSet, IconError> {
    let xml = z.read_name("AppxManifest.xml")?;
    let wanted = manifest_logos(&xml);
    if wanted.is_empty() {
        return Err(IconError::NoIcon("AppxManifest.xml 里没有 Logo 属性".into()));
    }
    // Square44 / Square150 是同一幅画的不同尺寸，合起来才是"多分辨率"；
    // StoreLogo 常是另一幅图（商店用），有别的 Logo 时排除。
    let order = desired_order(&wanted);
    let apps: Vec<String> = order
        .iter()
        .filter(|s| !s.to_ascii_lowercase().contains("storelogo"))
        .cloned()
        .collect();
    let list = if apps.is_empty() { order } else { apps };
    let mut set = IconSet::new("MSIX/APPX".to_string());
    for logo in list {
        if let Ok(one) = load_logo_group(z, &logo) {
            for f in one.frames {
                set.push(f);
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("Logo 指向的文件都不存在".into()));
    }
    set.detail = format!("MSIX/APPX · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

fn desired_order(v: &[String]) -> Vec<String> {
    // 优先应用磁贴主图标，其次 44×44，最后 StoreLogo
    let mut out = v.to_vec();
    out.sort_by_key(|s| {
        let l = s.to_ascii_lowercase();
        if l.contains("square150x150logo") {
            0
        } else if l.contains("square44x44logo") {
            1
        } else if l.contains("logo") {
            2
        } else {
            3
        }
    });
    out
}

#[allow(deprecated)]
fn manifest_logos(xml: &[u8]) -> Vec<String> {
    use quick_xml::events::Event;
    let text = String::from_utf8_lossy(xml);
    let mut r = quick_xml::Reader::from_str(&text);
    let mut out = Vec::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                for a in e.attributes().flatten() {
                    let key = a.key.local_name().as_ref().to_string();
                    if matches!(
                        key.as_str(),
                        "Logo" | "Square44x44Logo" | "Square150x150Logo" | "Square71x71Logo"
                            | "Square310x310Logo" | "SmallLogo" | "StoreLogo" | "Wide310x310Logo"
                    ) {
                        if let Ok(v) = a.unescape_value() {
                            let p = v.replace('\\', "/");
                            if !p.is_empty() && !out.contains(&p) {
                                out.push(p);
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn load_logo_group(z: &mut Zip, logo: &str) -> Result<IconSet, IconError> {
    let mut set = IconSet::new(format!("MSIX · {logo}"));
    // 精确路径 + 同基名的 scale/targetsize 变体
    let (dir, stem) = match logo.rsplit_once('/') {
        Some((d, s)) => (format!("{d}/"), s.to_string()),
        None => (String::new(), logo.to_string()),
    };
    let stem_base = stem.split('.').next().unwrap_or(&stem).to_string();
    let mut hit = false;
    for n in z.names().to_vec() {
        if !n.starts_with(&dir) {
            continue;
        }
        let Some(s) = file_stem(&n) else { continue };
        let low = n.to_ascii_lowercase();
        if !(low.ends_with(".png") || low.ends_with(".jpg") || low.ends_with(".jpeg")) {
            continue;
        }
        if n == logo || s == stem_base || s.starts_with(&format!("{stem_base}.")) {
            if let Some(i) = z.index(&n) {
                if let Ok(data) = z.read(i) {
                    if let Ok(f) = decode_image(&data, &n) {
                        hit = true;
                        set.push(f);
                    }
                }
            }
        }
    }
    if !hit {
        return Err(IconError::NoIcon(format!("找不到 {logo}")));
    }
    Ok(set)
}

/// 没有 Info.plist 的 IPA：iTunesArtwork / AppIcon*.png 仍然可用。
fn ipa_fallback(z: &mut Zip, prefix: &str) -> Result<IconSet, IconError> {
    let mut set = IconSet::new("IPA · 按文件名匹配".to_string());
    for n in z.names().to_vec() {
        if prefix.is_empty() || n.starts_with(prefix) {
            let low = n.to_ascii_lowercase();
            if low.ends_with(".png") && (low.contains("appicon") || low.contains("itunesartwork")) {
                if let Some(i) = z.index(&n) {
                    if let Ok(data) = z.read(i) {
                        if let Ok(f) = decode_image(&data, &n) {
                            set.push(f);
                        }
                    }
                }
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("IPA 里没有 Info.plist / AppIcon / iTunesArtwork".into()));
    }
    set.detail = format!("IPA · 回退匹配 · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

/// 没有 AppxManifest.xml 的 MSIX：Assets 下名字带 logo/icon 的位图，取最大的那组。
fn msix_fallback(z: &mut Zip) -> Result<IconSet, IconError> {
    let mut set = IconSet::new("MSIX/APPX · 按文件名匹配".to_string());
    for n in z.names().to_vec() {
        let low = n.to_ascii_lowercase();
        if !(low.ends_with(".png") || low.ends_with(".jpg") || low.ends_with(".jpeg")) {
            continue;
        }
        if !(low.contains("logo") || low.contains("icon")) {
            continue;
        }
        if let Some(i) = z.index(&n) {
            if let Ok(data) = z.read(i) {
                if let Ok(f) = decode_image(&data, &n) {
                    set.push(f);
                }
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("MSIX/APPX 里没有 Logo/Icon 位图".into()));
    }
    set.detail = format!("MSIX/APPX · 回退匹配 · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

/* ------------------------------- zip 里的 .app ------------------------------- */

fn mac_zip(z: &mut Zip, app_prefix: &str) -> Result<IconSet, IconError> {
    let plist_name = format!("{app_prefix}Contents/Info.plist");
    let bytes = z.read_name(&plist_name)?;
    let names = icon_base_names(&bytes);
    let res_prefix = format!("{app_prefix}Contents/Resources/");
    let mut set = IconSet::new(format!("App 包 · {app_prefix}"));
    for base in &names {
        for n in z.names().to_vec() {
            if n.starts_with(&res_prefix) {
                let Some(stem) = file_stem(&n) else { continue };
                if stem == *base {
                    if let Some(i) = z.index(&n) {
                        if let Ok(data) = z.read(i) {
                            if let Ok(set2) = load_icns_or_image(&data, &n) {
                                for f in set2.frames {
                                    set.push(f);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if set.is_empty() {
        // 回退：Resources 下最大的 icns
        for n in z.names().to_vec() {
            if n.starts_with(&res_prefix) && n.to_ascii_lowercase().ends_with(".icns") {
                if let Some(i) = z.index(&n) {
                    if let Ok(data) = z.read(i) {
                        if let Ok(s) = crate::icns::read(&data) {
                            for f in s.frames {
                                set.push(f);
                            }
                        }
                    }
                }
            }
        }
    }
    if set.is_empty() {
        return Err(IconError::NoIcon("zip 的 .app 包里没有图标".into()));
    }
    set.detail = format!("App 包 · {} 帧（{}）", set.frames.len(), set.size_list());
    Ok(set.finish())
}

fn load_icns_or_image(data: &[u8], name: &str) -> Result<IconSet, IconError> {
    if crate::icns::is_icns(data) {
        crate::icns::read(data)
    } else {
        let mut set = IconSet::new(name.to_string());
        set.push(decode_image(data, name)?);
        Ok(set)
    }
}

fn file_stem(name: &str) -> Option<String> {
    Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}
