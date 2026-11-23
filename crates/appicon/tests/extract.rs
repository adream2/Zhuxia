//! 端到端测试：真实文件进、图标帧出。跑在 Windows 上，不 mock 解码器。
//!
//! 合成夹具覆盖 zip/tar/ICNS/ICO/LNK 路径；PE 用系统自带的 notepad.exe（存在才跑）。

use std::io::Write;
use std::path::PathBuf;

use appicon::model::{png_bytes, Frame};
use appicon::output::{write_set, Mode, Options};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zx-appicon-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn frame(w: u32, h: u32, rgba: [u8; 4]) -> Frame {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        data.extend_from_slice(&rgba);
    }
    Frame::new(w, h, data, "test").unwrap()
}

fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    png_bytes(&frame(w, h, rgba), true).unwrap()
}

#[test]
fn ico_roundtrip_keeps_all_sizes() {
    let d = scratch("ico");
    let f = d.join("multi.ico");
    let frames = vec![
        (256u32, png(256, 256, [220, 30, 30, 255])),
        (48, png(48, 48, [30, 220, 30, 255])),
        (16, png(16, 16, [30, 30, 220, 255])),
    ];
    std::fs::write(&f, appicon::ico::build(&frames).unwrap()).unwrap();
    let set = appicon::probe::extract(&f).unwrap();
    assert_eq!(set.frames.len(), 3, "{}", set.size_list());
    assert_eq!(set.max_side(), 256);
}

#[test]
fn pe_extracts_system_exe_icon() {
    let p = PathBuf::from(r"C:\Windows\System32\notepad.exe");
    if !p.is_file() {
        eprintln!("跳过：本机没有 notepad.exe");
        return;
    }
    let set = appicon::probe::extract(&p).expect("PE 图标");
    assert!(!set.frames.is_empty());
    assert!(set.max_side() >= 32, "最大 {}（{}）", set.max_side(), set.size_list());
}

#[test]
fn icns_reads_png_chunks() {
    let d = scratch("icns");
    let f = d.join("app.icns");
    let mut blob = Vec::new();
    blob.extend_from_slice(b"icns");
    blob.extend_from_slice(&0u32.to_be_bytes()); // 长度稍后回填
    for (ty, side) in [(b"ic09", 512u32), (b"ic08", 256), (b"ic07", 128)] {
        let p = png(side, side, [10, 120, 240, 255]);
        blob.extend_from_slice(ty);
        blob.extend_from_slice(&((p.len() + 8) as u32).to_be_bytes());
        blob.extend_from_slice(&p);
    }
    let total = blob.len() as u32;
    blob[4..8].copy_from_slice(&total.to_be_bytes());
    std::fs::write(&f, &blob).unwrap();

    let set = appicon::probe::extract(&f).unwrap();
    assert_eq!(set.frames.len(), 3);
    assert_eq!(set.max_side(), 512);
}

#[test]
fn apk_falls_back_to_mipmap_files() {
    let d = scratch("apk");
    let f = d.join("demo.apk");
    let file = std::fs::File::create(&f).unwrap();
    let mut z = zip::ZipWriter::new(file);
    let opt = zip::write::SimpleFileOptions::default();
    for (dir, side) in [("mipmap-mdpi", 48u32), ("mipmap-xhdpi", 96), ("mipmap-xxxhdpi", 192)] {
        z.start_file(format!("res/{dir}/ic_launcher.png"), opt).unwrap();
        z.write_all(&png(side, side, [40, 180, 90, 255])).unwrap();
    }
    z.finish().unwrap();

    let set = appicon::probe::extract(&f).expect("APK 回退");
    assert_eq!(set.frames.len(), 3, "{}", set.size_list());
    assert_eq!(set.max_side(), 192);
}

#[test]
fn ipa_reads_cfbundleicons() {
    let d = scratch("ipa");
    let f = d.join("demo.ipa");
    let file = std::fs::File::create(&f).unwrap();
    let mut z = zip::ZipWriter::new(file);
    let opt = zip::write::SimpleFileOptions::default();
    let plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIconFiles</key><array><string>AppIcon60x60</string></array>
</dict></plist>"#;
    z.start_file("Payload/Demo.app/Info.plist", opt).unwrap();
    z.write_all(plist).unwrap();
    z.start_file("Payload/Demo.app/AppIcon60x60@2x.png", opt).unwrap();
    z.write_all(&png(120, 120, [250, 180, 30, 255])).unwrap();
    z.start_file("Payload/Demo.app/AppIcon60x60@3x.png", opt).unwrap();
    z.write_all(&png(180, 180, [250, 180, 30, 255])).unwrap();
    z.finish().unwrap();

    let set = appicon::probe::extract(&f).expect("IPA");
    assert_eq!(set.frames.len(), 2, "{}", set.size_list());
    assert_eq!(set.max_side(), 180);
}

#[test]
fn msix_reads_square_logo() {
    let d = scratch("msix");
    let f = d.join("demo.msix");
    let file = std::fs::File::create(&f).unwrap();
    let mut z = zip::ZipWriter::new(file);
    let opt = zip::write::SimpleFileOptions::default();
    let manifest = br#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
  <Applications><Application Id="App">
    <uap:VisualElements xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
      Square44x44Logo="Assets\Logo.png" Square150x150Logo="Assets\Logo150.png" />
  </Application></Applications>
</Package>"#;
    z.start_file("AppxManifest.xml", opt).unwrap();
    z.write_all(manifest).unwrap();
    z.start_file("Assets/Logo.png", opt).unwrap();
    z.write_all(&png(44, 44, [120, 40, 200, 255])).unwrap();
    z.start_file("Assets/Logo150.png", opt).unwrap();
    z.write_all(&png(150, 150, [120, 40, 200, 255])).unwrap();
    z.finish().unwrap();

    let set = appicon::probe::extract(&f).expect("MSIX");
    assert_eq!(set.max_side(), 150, "{}", set.size_list());
    assert!(set.frames.len() >= 2);
}

#[test]
fn deb_reads_hicolor_by_desktop_name() {
    let d = scratch("deb");
    let f = d.join("demo.deb");
    let mut tar_gz = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut tar_gz, flate2::Compression::fast());
        let mut t = tar::Builder::new(enc);
        let desktop = b"[Desktop Entry]\nName=Demo\nIcon=demo\nExec=demo\n";
        add_tar(&mut t, "usr/share/applications/demo.desktop", desktop);
        add_tar(&mut t, "usr/share/icons/hicolor/256x256/apps/demo.png", &png(256, 256, [20, 120, 220, 255]));
        add_tar(&mut t, "usr/share/icons/hicolor/64x64/apps/demo.png", &png(64, 64, [20, 120, 220, 255]));
        // 干扰项：另一个应用的图标，名字不匹配
        add_tar(&mut t, "usr/share/icons/hicolor/512x512/apps/other.png", &png(512, 512, [255, 0, 0, 255]));
        t.finish().unwrap();
    }
    let mut ar = Vec::new();
    ar.extend_from_slice(b"!<arch>\n");
    ar_member(&mut ar, "data.tar.gz", &tar_gz);
    std::fs::write(&f, &ar).unwrap();

    let set = appicon::probe::extract(&f).expect("deb");
    assert_eq!(set.frames.len(), 2, "{}", set.size_list());
    assert_eq!(set.max_side(), 256, "不该选 512 的 other");
}

fn add_tar(t: &mut tar::Builder<flate2::write::GzEncoder<&mut Vec<u8>>>, path: &str, data: &[u8]) {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    t.append_data(&mut h, path, data).unwrap();
}

fn ar_member(out: &mut Vec<u8>, name: &str, data: &[u8]) {
    let hdr = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        name,
        "0",
        "0",
        "0",
        "100644",
        data.len()
    );
    assert_eq!(hdr.len(), 60);
    out.extend_from_slice(hdr.as_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(b'\n');
    }
}

#[test]
fn writes_png_ico_and_all_sizes() {
    let d = scratch("out");
    let src = d.join("demo.apk");
    let file = std::fs::File::create(&src).unwrap();
    let mut z = zip::ZipWriter::new(file);
    let opt = zip::write::SimpleFileOptions::default();
    for (dir, side) in [("mipmap-hdpi", 72u32), ("mipmap-xxxhdpi", 192)] {
        z.start_file(format!("res/{dir}/ic_launcher.png"), opt).unwrap();
        z.write_all(&png(side, side, [0, 160, 255, 255])).unwrap();
    }
    z.finish().unwrap();

    let set = appicon::probe::extract(&src).unwrap();
    let out = write_set(
        &src,
        &set,
        &Options { mode: Mode::All, ..Default::default() },
    )
    .expect("写 all");
    assert!(out.outputs.iter().any(|p| p.extension().map(|e| e == "png").unwrap_or(false)));
    assert!(out.outputs.iter().any(|p| p.extension().map(|e| e == "ico").unwrap_or(false)));
    let ico = out.outputs.iter().find(|p| p.extension().map(|e| e == "ico").unwrap_or(false)).unwrap();
    let blob = std::fs::read(ico).unwrap();
    let set2 = appicon::ico::read(&blob).unwrap();
    assert!(set2.max_side() <= 192);
    let dir = out.outputs.iter().find(|p| p.extension().map(|e| e == "icons").unwrap_or(false));
    assert!(dir.map(|p| p.is_dir()).unwrap_or(false));
}

#[test]
fn unsupported_and_corrupt_have_clear_reasons() {
    let d = scratch("bad");
    let txt = d.join("notes.txt");
    std::fs::write(&txt, b"hello").unwrap();
    let e = appicon::probe::extract(&txt).unwrap_err();
    assert!(e.reason().contains("不支持"), "{}", e.reason());

    let fake = d.join("fake.apk");
    std::fs::write(&fake, b"not a zip").unwrap();
    let e = appicon::probe::extract(&fake).unwrap_err();
    assert!(e.reason().contains("损坏"), "{}", e.reason());
}

#[test]
fn lnk_parses_icon_location() {
    let d = scratch("lnk");
    let f = d.join("shortcut.lnk");
    let mut data = vec![0u8; 0x4C];
    data[0..4].copy_from_slice(&0x4Cu32.to_le_bytes());
    data[4..20].copy_from_slice(&[
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
    ]);
    data[20..24].copy_from_slice(&(0x40u32 | 0x80).to_le_bytes()); // HasIconLocation | IsUnicode
    let target = r"C:\Windows\System32\notepad.exe";
    let mut s = Vec::new();
    s.extend_from_slice(&((target.encode_utf16().count()) as u16).to_le_bytes());
    for u in target.encode_utf16() {
        s.extend_from_slice(&u.to_le_bytes());
    }
    data.extend_from_slice(&s);
    std::fs::write(&f, &data).unwrap();

    let link = appicon::lnk::parse(&data).unwrap();
    assert_eq!(link.icon_location.as_deref(), Some(target));
    assert_eq!(appicon::lnk::resolve(&link, &f), Some(PathBuf::from(target)));
}

#[test]
fn probe_prefers_magic_over_extension() {
    let d = scratch("magic");
    let f = d.join("renamed.bin");
    std::fs::write(&f, appicon::ico::build(&[(64, png(64, 64, [9, 9, 9, 255]))]).unwrap()).unwrap();
    let set = appicon::probe::extract(&f).expect("应认出 ICO");
    assert_eq!(set.max_side(), 64);
}

#[test]
fn rpm_walks_cpio_payload() {
    let d = scratch("rpm");
    let f = d.join("demo.rpm");
    let mut cpio = Vec::new();
    cpio_add(&mut cpio, "./usr/share/applications/demo.desktop", b"Icon=demo\n");
    cpio_add(&mut cpio, "./usr/share/icons/hicolor/256x256/apps/demo.png", &png(256, 256, [200, 40, 40, 255]));
    cpio_add(&mut cpio, "TRAILER!!!", b"");
    let mut gz = Vec::new();
    {
        let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
        enc.write_all(&cpio).unwrap();
        enc.finish().unwrap();
    }
    let mut rpm = vec![0u8; 96];
    rpm[..4].copy_from_slice(&[0xED, 0xAB, 0xEE, 0xDB]);
    for _ in 0..2 {
        rpm.extend_from_slice(&[0x8E, 0xAD, 0xE8, 0x01, 0, 0, 0, 0]);
        rpm.extend_from_slice(&0u32.to_be_bytes()); // nindex
        rpm.extend_from_slice(&0u32.to_be_bytes()); // hsize
    }
    rpm.extend_from_slice(&gz);
    std::fs::write(&f, &rpm).unwrap();

    let set = appicon::probe::extract(&f).expect("rpm");
    assert_eq!(set.frames.len(), 1);
    assert_eq!(set.max_side(), 256);
}

fn cpio_add(out: &mut Vec<u8>, name: &str, data: &[u8]) {
    let fields = [
        1u32,
        0o100644,
        0,
        0,
        1,
        0,
        data.len() as u32,
        0,
        0,
        0,
        0,
        (name.len() + 1) as u32,
        0,
    ];
    let mut hdr = b"070701".to_vec();
    for v in fields {
        hdr.extend_from_slice(format!("{v:08x}").as_bytes());
    }
    assert_eq!(hdr.len(), 110);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

#[test]
fn msi_scans_streams_by_content() {
    let d = scratch("msi");
    let f = d.join("demo.msi");
    let ico = appicon::ico::build(&[(64, png(64, 64, [10, 200, 200, 255]))]).unwrap();
    {
        let mut cf = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
        cf.create_storage("/Icon").unwrap();
        let mut s = cf.create_stream("/Icon/app.ico").unwrap();
        s.write_all(&ico).unwrap();
        s.flush().unwrap();
        drop(s);
        let inner = cf.into_inner();
        std::fs::write(&f, inner.into_inner()).unwrap();
    }
    let set = appicon::probe::extract(&f).expect("MSI");
    assert_eq!(set.max_side(), 64);
}

#[test]
fn app_bundle_reads_icns() {
    let d = scratch("appbundle");
    let app = d.join("Demo.app");
    std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    let plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIconFile</key><string>AppIcon</string></dict></plist>"#;
    std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
    let mut icns = Vec::new();
    icns.extend_from_slice(b"icns");
    icns.extend_from_slice(&0u32.to_be_bytes());
    let p = png(256, 256, [40, 90, 180, 255]);
    icns.extend_from_slice(b"ic08");
    icns.extend_from_slice(&((p.len() + 8) as u32).to_be_bytes());
    icns.extend_from_slice(&p);
    let total = icns.len() as u32;
    icns[4..8].copy_from_slice(&total.to_be_bytes());
    std::fs::write(app.join("Contents/Resources/AppIcon.icns"), icns).unwrap();

    let set = appicon::probe::extract(&app).expect(".app");
    assert_eq!(set.max_side(), 256);
}
