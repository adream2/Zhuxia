//! 把整块虚拟桌面抓成 BMP（不依赖截图模块，纯 GDI）。
//!
//! 用途：验证"轨迹到底有没有画到屏幕上"——不靠肉眼、不靠截图 API 的历史状态，
//! 抓一帧存盘就能比对。
//!
//! ```bash
//! cargo run -p gesture --example screen_dump -- shot.bmp
//! ```

use std::fs::File;
use std::io::Write;

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "shot.bmp".into());
    unsafe {
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let (w, h) = (w.max(1), h.max(1));

        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .expect("CreateDIBSection");
        let old = SelectObject(mem, dib);
        let _ = BitBlt(mem, 0, 0, w, h, screen, vx, vy, SRCCOPY | CAPTUREBLT);
        let _ = GdiFlush();

        let pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        let mut file = File::create(&path).expect("create bmp");
        let file_size = 54 + pixels.len() as u32;
        let mut header = Vec::with_capacity(54);
        header.extend_from_slice(b"BM");
        header.extend_from_slice(&file_size.to_le_bytes());
        header.extend_from_slice(&[0, 0, 0, 0]);
        header.extend_from_slice(&54u32.to_le_bytes());
        header.extend_from_slice(&40u32.to_le_bytes());
        header.extend_from_slice(&w.to_le_bytes());
        header.extend_from_slice(&(-h).to_le_bytes()); // 负高度 = 自上而下，和 DIB 行序一致
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&32u16.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&pixels.len().to_le_bytes());
        header.extend_from_slice(&2835i32.to_le_bytes());
        header.extend_from_slice(&2835i32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        file.write_all(&header).expect("header");
        file.write_all(pixels).expect("pixels");

        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        println!("已保存 {path}（{w}x{h} @ {vx},{vy}）");
    }
}
