//! Win32 剪贴板读写：文本 / HTML / 图片 / 文件 / 私有标记。
//!
//! 对照参考仓库的 `infrastructure/windows_api/win_clipboard.rs`：
//! - 读取顺序、`HTML Format` 解析、`CF_HDROP` 文件读取一致；
//! - `CF_DIB → PNG` 用 GDI+ 编码（参考实现用 image crate），保留原图格式时优先抓
//!   `PNG` / `GIF` / `JFIF` / `WebP` 这些注册格式的原始字节（动图不会被压平）；
//! - 写剪贴板时带 `own_write` 标记，避免自己写回去的内容被当成新复制。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HANDLE, HGLOBAL};
use windows::Win32::Graphics::GdiPlus::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::rules;

pub const CF_UNICODETEXT: u32 = 13;
pub const CF_BITMAP: u32 = 2;
pub const CF_DIB: u32 = 8;
pub const CF_DIBV5: u32 = 17;
pub const CF_HDROP: u32 = 15;

// 参考实现里的隐私/自写标记格式名。
const FMT_HTML: &str = "HTML Format";
const FMT_EXCLUDE_MONITOR: &str = "ExcludeClipboardContentFromMonitorProcessing";
const FMT_VIEWER_IGNORE: &str = "Clipboard Viewer Ignore";
const FMT_INCLUDE_HISTORY: &str = "CanIncludeInClipboardHistory";
const FMT_INCLUDE_CLOUD: &str = "CanUploadToCloudClipboard";

static OWN_WRITE_UNTIL_MS: AtomicI64 = AtomicI64::new(0);
static LAST_WRITE_HASH: AtomicU64 = AtomicU64::new(0);
static LAST_WRITE_HASH_ALT: AtomicU64 = AtomicU64::new(0);
static PAUSED: AtomicBool = AtomicBool::new(false);

pub fn set_paused(p: bool) {
    PAUSED.store(p, Ordering::Relaxed);
}
pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn now_ms() -> i64 {
    crate::model::now_ms()
}

/// 标记「接下来这几百毫秒内的变化是我自己写的」。
pub fn begin_own_write(hash: u64, alt: u64) {
    OWN_WRITE_UNTIL_MS.store(now_ms() + 2500, Ordering::SeqCst);
    LAST_WRITE_HASH.store(hash, Ordering::SeqCst);
    LAST_WRITE_HASH_ALT.store(alt, Ordering::SeqCst);
}

/// 当前剪贴板内容是不是我们刚写进去的（含 Office/WPS 回写的一次）。
pub fn is_own_write(hash: Option<u64>) -> bool {
    let until = OWN_WRITE_UNTIL_MS.load(Ordering::SeqCst);
    if now_ms() > until {
        return false;
    }
    match hash {
        None => true,
        Some(h) => {
            h != 0 && (h == LAST_WRITE_HASH.load(Ordering::SeqCst)
                || h == LAST_WRITE_HASH_ALT.load(Ordering::SeqCst))
        }
    }
}

pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// 剪贴板被密码管理器标记为「不要记录」。
pub fn is_marked_private() -> bool {
    for name in [FMT_EXCLUDE_MONITOR, FMT_VIEWER_IGNORE] {
        if raw_format(name).is_some() {
            return true;
        }
    }
    for name in [FMT_INCLUDE_HISTORY, FMT_INCLUDE_CLOUD] {
        if let Some(bytes) = raw_format(name) {
            if bytes.len() >= 4 && u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == 0 {
                return true;
            }
        }
    }
    false
}

fn format_id(name: &str) -> u32 {
    let n = wide(name);
    unsafe { RegisterClipboardFormatW(PCWSTR(n.as_ptr())) }
}

/// 带重试地打开剪贴板：别的程序（尤其 Office）会短暂占着。
fn with_clipboard<T>(f: impl FnOnce() -> T) -> Option<T> {
    for _ in 0..12 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            let out = f();
            let _ = unsafe { CloseClipboard() };
            return Some(out);
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    None
}

fn hglobal_bytes(h: HANDLE) -> Option<Vec<u8>> {
    if h.is_invalid() {
        return None;
    }
    let hg = HGLOBAL(h.0);
    unsafe {
        let size = GlobalSize(hg);
        if size == 0 {
            return None;
        }
        let p = GlobalLock(hg) as *const u8;
        if p.is_null() {
            return None;
        }
        let mut out = vec![0u8; size];
        std::ptr::copy_nonoverlapping(p, out.as_mut_ptr(), size);
        let _ = GlobalUnlock(hg);
        Some(out)
    }
}

fn read_format_bytes(id: u32) -> Option<Vec<u8>> {
    if id == 0 {
        return None;
    }
    let h = unsafe { GetClipboardData(id) }.ok()?;
    hglobal_bytes(h)
}

/// 按注册名读原始字节（`HTML Format` / `PNG` / `GIF` …）。
pub fn raw_format(name: &str) -> Option<Vec<u8>> {
    let id = format_id(name);
    if id == 0 {
        return None;
    }
    with_clipboard(|| read_format_bytes(id)).flatten()
}

pub fn read_text() -> Option<String> {
    // 句柄只在剪贴板打开期间有效：必须关掉之前把字符串拷出来。
    with_clipboard(|| unsafe {
        let h = GetClipboardData(CF_UNICODETEXT).ok()?;
        if h.is_invalid() {
            return None;
        }
        let hg = HGLOBAL(h.0);
        let size = GlobalSize(hg);
        if size < 2 {
            return None;
        }
        let p = GlobalLock(hg) as *const u16;
        if p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while len * 2 + 1 < size {
            if *p.add(len) == 0 {
                break;
            }
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        let _ = GlobalUnlock(hg);
        Some(s)
    })
    .flatten()
}

pub fn read_html() -> Option<String> {
    let raw = raw_format(FMT_HTML)?;
    rules::parse_cf_html(&raw).or_else(|| Some(String::from_utf8_lossy(&raw).to_string()))
}

pub fn read_files() -> Option<Vec<String>> {
    // 同 read_text：HDROP 句柄在关闭剪贴板后不再有效。
    with_clipboard(|| unsafe {
        let h = GetClipboardData(CF_HDROP).ok()?;
        if h.is_invalid() {
            return None;
        }
        let hdrop = HDROP(h.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        if count == 0 {
            return None;
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = DragQueryFileW(hdrop, i, Some(&mut buf));
            if n > 0 {
                out.push(String::from_utf16_lossy(&buf[..n as usize]));
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    })
    .flatten()
}

/// 当前剪贴板里那张图的原始编码（GIF/PNG/JPEG/WebP），保留动图。
pub fn read_native_image() -> Option<(Vec<u8>, &'static str, u64)> {
    for (name, ext) in [
        ("GIF", "gif"),
        ("Animated GIF", "gif"),
        ("image/gif", "gif"),
        ("PNG", "png"),
        ("image/png", "png"),
        ("JFIF", "jpg"),
        ("JPEG", "jpg"),
        ("image/jpeg", "jpg"),
        ("WebP", "webp"),
        ("image/webp", "webp"),
    ] {
        let Some(raw) = raw_format(name) else { continue };
        let valid = match ext {
            "gif" => raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a"),
            "png" => raw.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            "jpg" => raw.starts_with(&[0xFF, 0xD8]),
            "webp" => raw.starts_with(b"RIFF") && raw.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !valid {
            continue;
        }
        let hash = rules::bytes_hash(&raw);
        return Some((raw, ext, hash));
    }
    None
}

/// 剪贴板位图 → PNG 字节（CF_DIB / CF_DIBV5）。
pub fn read_dib_png() -> Option<(Vec<u8>, u64)> {
    let dib = with_clipboard(|| {
        read_format_bytes(CF_DIB).or_else(|| read_format_bytes(CF_DIBV5))
    })
    .flatten()?;
    let png = dib_to_png(&dib)?;
    let hash = rules::bytes_hash(&png);
    Some((png, hash))
}

// ---------------------------------------------------------------- DIB → PNG

/// 把 CF_DIB 的字节（BITMAPINFOHEADER + 像素）用 GDI+ 编成 PNG。
///
/// 只处理 BI_RGB 的 24/32 位（覆盖截图工具、浏览器、Office、聊天软件），
/// 32 位全透明 alpha（很多程序写 0）按不透明处理，与系统看图器表现一致。
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    zx_ui::painter::init_gdiplus();
    if dib.len() < 40 {
        return None;
    }
    let u32_at = |o: usize| -> u32 {
        u32::from_le_bytes([dib[o], dib[o + 1], dib[o + 2], dib[o + 3]])
    };
    let header = u32_at(0) as usize;
    if header < 40 || dib.len() < header {
        return None;
    }
    let w = u32_at(4) as i32;
    let h_raw = u32_at(8) as i32;
    let bpp = u16::from_le_bytes([dib[14], dib[15]]) as usize;
    let compression = u32_at(16);
    let clr_used = u32_at(32) as usize;
    if w <= 0 || h_raw == 0 || compression != 0 || !(bpp == 24 || bpp == 32) {
        return None;
    }
    let h = h_raw.unsigned_abs() as i32;
    let top_down = h_raw < 0;
    let palette = if bpp <= 8 { clr_used.max(1 << bpp) } else { 0 } * 4;
    let offset = header + palette;
    let stride = (w as usize * bpp).div_ceil(32) * 4;
    let need = offset + stride * h as usize;
    if dib.len() < need {
        return None;
    }

    let mut pixels = vec![0u8; stride * h as usize];
    for row in 0..h as usize {
        let src_row = if top_down { row } else { h as usize - 1 - row };
        let s = offset + src_row * stride;
        let d = row * stride;
        pixels[d..d + stride].copy_from_slice(&dib[s..s + stride]);
    }

    if bpp == 32 {
        // 全 0 的 alpha 通道当成不透明（CF_DIB 的 alpha 常常是垃圾值）。
        let all_zero = pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
        if all_zero {
            for p in pixels.as_chunks_mut::<4>().0.iter_mut() {
                p[3] = 255;
            }
        }
    }

    let format = if bpp == 32 { 0x0026_200Au32 as i32 } else { 0x0002_1808 };
    unsafe {
        let mut bmp: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(w, h, stride as i32, format, Some(pixels.as_ptr()), &mut bmp).0
            != 0
        {
            return None;
        }
        let clsid = png_encoder_clsid()?;
        // 用内存流编码：不往磁盘写临时 PNG（截图里可能有机密内容）。
        let stream = match windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal(
            HGLOBAL(std::ptr::null_mut()),
            BOOL(1),
        ) {
            std::result::Result::Ok(s) => s,
            std::result::Result::Err(_) => {
                GdipDisposeImage(bmp as *mut GpImage);
                return None;
            }
        };
        let st = GdipSaveImageToStream(bmp as *mut GpImage, &stream, &clsid, std::ptr::null());
        GdipDisposeImage(bmp as *mut GpImage);
        if st.0 != 0 {
            return None;
        }
        let hg = windows::Win32::System::Com::StructuredStorage::GetHGlobalFromStream(&stream).ok()?;
        let size = GlobalSize(hg);
        if size == 0 {
            return None;
        }
        let p = GlobalLock(hg) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(hg);
        Some(bytes)
    }
}

unsafe fn png_encoder_clsid() -> Option<windows::core::GUID> {
    let mut num = 0u32;
    let mut size = 0u32;
    if GdipGetImageEncodersSize(&mut num, &mut size).0 != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let encoders = buf.as_mut_ptr() as *mut ImageCodecInfo;
    if GdipGetImageEncoders(num, size, encoders).0 != 0 {
        return None;
    }
    for i in 0..num as usize {
        let info = &*encoders.add(i);
        if info.MimeType.is_null() {
            continue;
        }
        let mime = pcwstr_to_string(info.MimeType);
        if mime.eq_ignore_ascii_case("image/png") {
            return Some(info.Clsid);
        }
    }
    None
}

unsafe fn pcwstr_to_string(p: PCWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.0.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
}

// ---------------------------------------------------------------- 写入

fn set_bytes(id: u32, bytes: &[u8]) -> bool {
    unsafe {
        let hmem = match GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) {
            std::result::Result::Ok(h) => h,
            _ => return false,
        };
        let p = GlobalLock(hmem) as *mut u8;
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(hmem);
        if SetClipboardData(id, HANDLE(hmem.0)).is_err() {
            let _ = windows::Win32::Foundation::GlobalFree(HGLOBAL(hmem.0));
            return false;
        }
        true
    }
}

fn set_wide(id: u32, text: &str) -> bool {
    let mut bytes: Vec<u8> = Vec::with_capacity((text.len() + 1) * 2);
    for u in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    set_bytes(id, &bytes)
}

fn set_files(paths: &[String]) -> bool {
    // DROPFILES + 双 \0 结尾的宽字符路径表
    let mut payload: Vec<u8> = Vec::new();
    let header_size = std::mem::size_of::<DROPFILES>();
    payload.resize(header_size, 0);
    for p in paths {
        for u in p.encode_utf16().chain(std::iter::once(0)) {
            payload.extend_from_slice(&u.to_le_bytes());
        }
    }
    payload.extend_from_slice(&[0, 0]);
    unsafe {
        let df = payload.as_mut_ptr() as *mut DROPFILES;
        (*df).pFiles = header_size as u32;
        (*df).fWide = BOOL(1);
    }
    set_bytes(CF_HDROP, &payload)
}

/// 写纯文本（只放 `CF_UNICODETEXT`；系统会自动为 Chromium 系提供 UTF-8 转换）。
pub fn write_text(text: &str) -> bool {
    begin_own_write(rules::text_hash(text), 0);
    with_clipboard(|| {
        unsafe { let _ = EmptyClipboard(); }
        set_wide(CF_UNICODETEXT, text)
    })
    .unwrap_or(false)
}

pub fn write_text_html(text: &str, html: &str) -> bool {
    let cf = rules::encode_cf_html(html);
    begin_own_write(rules::text_hash(text), rules::text_hash(&cf));
    with_clipboard(|| {
        unsafe { let _ = EmptyClipboard(); }
        let a = set_wide(CF_UNICODETEXT, text);
        let b = set_bytes(format_id(FMT_HTML), cf.as_bytes());
        a && b
    })
    .unwrap_or(false)
}

pub fn write_files(paths: &[String]) -> bool {
    // 采集端对文件算的是「路径按行拼接」的文本指纹，这里必须一致，
    // 否则粘贴文件会被当成一次新的复制再记一遍。
    begin_own_write(rules::text_hash(&paths.join("
")), 0);
    with_clipboard(|| {
        unsafe { let _ = EmptyClipboard(); }
        set_files(paths)
    })
    .unwrap_or(false)
}

/// 用内存里的图片字节建 GDI+ 位图。
///
/// 附件在盘上是 DPAPI 密文，不能再走 `GdipCreateBitmapFromFile`；
/// 先解密到内存，再用 `SHCreateMemStream` 喂给 GDI+（不落临时明文文件）。
pub fn image_from_bytes(bytes: &[u8]) -> Option<*mut GpImage> {
    zx_ui::painter::init_gdiplus();
    unsafe {
        let stream = SHCreateMemStream(Some(bytes))?;
        let mut bmp: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromStream(&stream, &mut bmp).0 != 0 {
            return None;
        }
        Some(bmp as *mut GpImage)
    }
}

/// 把内存里的图片字节写回剪贴板：注册格式（原字节，保动画）+ CF_BITMAP。
/// `files` 非空时追加 CF_HDROP（只有明文文件才适合给资源管理器）。
pub fn write_image_bytes(bytes: &[u8], ext: &str, files: &[String]) -> bool {
    zx_ui::painter::init_gdiplus();
    let fmt = match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => Some("PNG"),
        "gif" => Some("GIF"),
        "jpg" | "jpeg" => Some("JFIF"),
        "webp" => Some("WebP"),
        "bmp" => Some("BMP"),
        _ => None,
    };
    begin_own_write(rules::bytes_hash(bytes), 0);
    let mut hbmp = windows::Win32::Graphics::Gdi::HBITMAP::default();
    unsafe {
        if let Some(stream) = SHCreateMemStream(Some(bytes)) {
            let mut img: *mut GpBitmap = std::ptr::null_mut();
            if GdipCreateBitmapFromStream(&stream, &mut img).0 == 0 {
                let _ = GdipCreateHBITMAPFromBitmap(img, &mut hbmp, 0xFFFF_FFFF);
                GdipDisposeImage(img as *mut GpImage);
            }
        }
    }
    let (ok, bitmap_owned) = with_clipboard(|| {
        unsafe { let _ = EmptyClipboard(); }
        let mut ok = false;
        if let Some(name) = fmt {
            ok |= set_bytes(format_id(name), bytes);
        }
        let mut bitmap_owned = false;
        if !hbmp.is_invalid() {
            unsafe {
                if SetClipboardData(CF_BITMAP, HANDLE(hbmp.0)).is_ok() {
                    ok = true;
                    // 成功时所有权归剪贴板，不能再删。
                    bitmap_owned = true;
                }
            }
        }
        if !files.is_empty() {
            ok |= set_files(files);
        }
        (ok, bitmap_owned)
    })
    .unwrap_or((false, false));
    if !bitmap_owned && !hbmp.is_invalid() {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(hbmp.0));
        }
    }
    ok
}

/// 把图片文件写回剪贴板。附件已加密时先解密，且不再写 CF_HDROP
/// （资源管理器拿到的是密文，塞给它没意义）。
pub fn write_image_file(path: &str) -> bool {
    let raw = match std::fs::read(path) {
        std::result::Result::Ok(b) => b,
        _ => return false,
    };
    let sealed = crate::secure::is_protected(&raw);
    let bytes = if sealed {
        match crate::secure::unprotect(&raw) {
            Some(b) => b,
            None => return false,
        }
    } else {
        raw
    };
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_string();
    let files = if sealed { Vec::new() } else { vec![path.to_string()] };
    write_image_bytes(&bytes, &ext, &files)
}

/// 写文本并标记「不要被剪贴板历史/云同步收集」（复制密码时用）。
pub fn write_private_text(text: &str) -> bool {
    begin_own_write(rules::text_hash(text), 0);
    with_clipboard(|| {
        unsafe { let _ = EmptyClipboard(); }
        let a = set_wide(CF_UNICODETEXT, text);
        let _ = set_bytes(format_id(FMT_EXCLUDE_MONITOR), &[0u8; 4]);
        let _ = set_bytes(format_id(FMT_VIEWER_IGNORE), &[0u8; 4]);
        a
    })
    .unwrap_or(false)
}

/// 清空系统剪贴板（粘贴后「清除」可选）。
pub fn clear_clipboard() {
    with_clipboard(|| unsafe { let _ = EmptyClipboard(); });
}

// ---------------------------------------------------------------- 快照/恢复（瞬时粘贴）

/// 粘贴前把用户原来的剪贴板内容留住，粘贴完再放回去。
pub enum Snapshot {
    Text(String),
    TextHtml(String, String),
    Image(Vec<u8>, String),
    Files(Vec<String>),
    Empty,
}

pub fn snapshot() -> Snapshot {
    if let Some(files) = read_files() {
        return Snapshot::Files(files);
    }
    if let Some((bytes, ext, _)) = read_native_image() {
        return Snapshot::Image(bytes, ext.to_string());
    }
    let text = read_text();
    let html = read_html();
    match (text, html) {
        (Some(t), Some(h)) => Snapshot::TextHtml(t, h),
        (Some(t), None) => Snapshot::Text(t),
        _ => Snapshot::Empty,
    }
}

pub fn restore(snap: Snapshot) -> bool {
    match snap {
        Snapshot::Text(t) => write_text(&t),
        Snapshot::TextHtml(t, h) => write_text_html(&t, &h),
        Snapshot::Image(bytes, ext) => {
            // 直接从内存写回：临时文件删掉后 CF_HDROP 会指向不存在的路径。
            write_image_bytes(&bytes, &ext, &[])
        }
        Snapshot::Files(paths) => write_files(&paths),
        Snapshot::Empty => {
            clear_clipboard();
            true
        }
    }
}

/// 打开一个文件/链接（列表里的「打开」）。
pub fn shell_open(path: &str) {
    let op = wide("open");
    let file = wide(path);
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张 2×2 的 24 位 DIB，验证「内存流编码 PNG」这条新路径。
    #[test]
    fn dib_encodes_to_png_in_memory() {
        let (w, h, bpp) = (2usize, 2usize, 24usize);
        let stride = (w * bpp).div_ceil(32) * 4;
        let mut dib = vec![0u8; 40 + stride * h];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&(w as i32).to_le_bytes());
        dib[8..12].copy_from_slice(&(h as i32).to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&(bpp as u16).to_le_bytes());
        dib[20..24].copy_from_slice(&((stride * h) as u32).to_le_bytes());
        for row in 0..h {
            for col in 0..w {
                let o = 40 + row * stride + col * 3;
                dib[o] = (col * 120) as u8;
                dib[o + 1] = (row * 200) as u8;
                dib[o + 2] = 0x40;
            }
        }
        let png = dib_to_png(&dib).expect("png bytes");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
        assert!(png.len() > 40);

        let img = image_from_bytes(&png).expect("decode png");
        unsafe {
            let mut iw = 0u32;
            let mut ih = 0u32;
            GdipGetImageWidth(img, &mut iw);
            GdipGetImageHeight(img, &mut ih);
            GdipDisposeImage(img);
            assert_eq!((iw, ih), (w as u32, h as u32));
        }
    }
}
