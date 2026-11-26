//! 整屏抓取：把虚拟桌面一次 BitBlt 到 32bpp DIB。
//!
//! 抓屏采用 GDI：虚拟桌面坐标天然是物理像素，多屏不需要自己拼 tile；
//! `CAPTUREBLT` 会把分层窗口与硬件加速层一起拷进来。
//! 代价是比 DXGI 多几毫秒，但只发生在按下截图热键的瞬间。

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};
use zx_ui::Canvas;

/// 冻结的一帧屏幕；坐标是虚拟桌面物理像素。
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    stride: usize,
    dib: HBITMAP,
    memdc: HDC,
    old: HGDIOBJ,
    bits: *mut u8,
}

// 只在抓屏线程内使用，不跨线程共享。
// SAFETY: `Frame` 里是 HDC + 一块 DIB 像素指针。它只在抓屏的那一个线程里
// 创建、读取、释放（`grab_*` 返回后由调用方用完即弃）。`Send` 是为了能放进
// 需要 `Send` 的容器；**不要**把同一个 `Frame` 挪到另一个线程去读——DC 是
// 创建线程的，`DeleteDC` 也在 Drop 里。
unsafe impl Send for Frame {}

struct Dib {
    dib: HBITMAP,
    memdc: HDC,
    old: HGDIOBJ,
    bits: *mut u8,
    stride: usize,
}

fn make_dib(screen: HDC, w: i32, h: i32) -> Option<Dib> {
    let memdc = unsafe { CreateCompatibleDC(screen) };
    if memdc.is_invalid() {
        return None;
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let dib = unsafe {
        CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?
    };
    if bits.is_null() {
        unsafe {
            let _ = DeleteDC(memdc);
        }
        return None;
    }
    let old = unsafe { SelectObject(memdc, dib) };
    Some(Dib {
        dib,
        memdc,
        old,
        bits: bits as *mut u8,
        stride: w as usize * 4,
    })
}

fn capture(rect: RECT) -> Option<Frame> {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let Some(dib) = make_dib(screen, w, h) else {
            ReleaseDC(None, screen);
            return None;
        };
        let rop = ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0);
        let ok = BitBlt(dib.memdc, 0, 0, w, h, screen, rect.left, rect.top, rop).is_ok();
        ReleaseDC(None, screen);
        if !ok {
            SelectObject(dib.memdc, dib.old);
            let _ = DeleteObject(dib.dib);
            let _ = DeleteDC(dib.memdc);
            return None;
        }
        force_opaque(dib.bits, w as usize * 4 * h as usize);
        Some(Frame {
            x: rect.left,
            y: rect.top,
            w,
            h,
            stride: dib.stride,
            dib: dib.dib,
            memdc: dib.memdc,
            old: dib.old,
            bits: dib.bits,
        })
    }
}

impl Frame {
    pub fn grab() -> Option<Frame> {
        unsafe {
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            capture(RECT { left: x, top: y, right: x + w, bottom: y + h })
        }
    }

    pub fn grab_rect(r: RECT) -> Option<Frame> {
        capture(r)
    }

    /// 屏幕坐标 → ARGB；屏外返回 0。
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        if !self.contains(x, y) {
            return 0;
        }
        let (lx, ly) = ((x - self.x) as usize, (y - self.y) as usize);
        unsafe {
            let p = self.bits.add(ly * self.stride + lx * 4);
            let b = *p as u32;
            let g = *p.add(1) as u32;
            let r = *p.add(2) as u32;
            0xFF00_0000 | (r << 16) | (g << 8) | b
        }
    }

    /// 整帧拷贝进同尺寸画布。
    pub fn blit_into(&self, canvas: &mut Canvas) {
        if canvas.w != self.w || canvas.h != self.h {
            return;
        }
        let dst = canvas.pixels_mut();
        unsafe {
            std::ptr::copy_nonoverlapping(self.bits, dst.as_mut_ptr(), dst.len());
        }
    }

    pub fn pixels_ptr(&self) -> *const u8 {
        self.bits
    }

    pub fn pixels_len(&self) -> usize {
        self.stride * self.h as usize
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn bounds(&self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memdc, self.old);
            let _ = DeleteObject(self.dib);
            let _ = DeleteDC(self.memdc);
        }
    }
}

/// BitBlt 不写 alpha；统一刷成 255，避免分层窗口/PNG 变全透明。
unsafe fn force_opaque(bits: *mut u8, len: usize) {
    let mut i = 3;
    while i < len {
        *bits.add(i) = 0xFF;
        i += 4;
    }
}

/// 把矩形裁到帧内。
pub fn clamp_rect(r: RECT, bounds: RECT) -> RECT {
    RECT {
        left: r.left.max(bounds.left),
        top: r.top.max(bounds.top),
        right: r.right.min(bounds.right),
        bottom: r.bottom.min(bounds.bottom),
    }
}
