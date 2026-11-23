//! 贴图：把截图钉在屏幕最上层。
//!
//! 交互只有四个：拖动、滚轮缩放、双击关闭、右键菜单（复制/保存/关闭）。
//! 贴图窗口与选区在同一线程创建，共享表用 Mutex 保护即可。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromScan0, GdipDisposeImage, GpBitmap, GpImage,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use zx_ui::menu::{self, Item};
use zx_ui::window::create_window;
use zx_ui::Canvas;

const CLASS: &str = "ZhuxiaCapturePin";

struct Pin {
    hwnd: HWND,
    canvas: Canvas,
    img: *mut GpImage,
    /// GDI+ 位图直接引用这块像素，必须与 `img` 同生命周期。
    _pixels: Vec<u8>,
    w: i32,
    h: i32,
    scale: f32,
    drag: Option<(POINT, RECT)>,
}

unsafe impl Send for Pin {}

fn pins() -> &'static Mutex<HashMap<isize, Box<Pin>>> {
    static PINS: OnceLock<Mutex<HashMap<isize, Box<Pin>>>> = OnceLock::new();
    PINS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register() {
    crate::register_class_dbl(CLASS, wndproc);
}

pub fn create(canvas: &mut Canvas, cursor: POINT) -> Option<HWND> {
    let (w, h) = (canvas.w, canvas.h);
    if w <= 0 || h <= 0 {
        return None;
    }

    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    let mut scale = 1.0f32;
    if w > vw * 72 / 100 || h > vh * 72 / 100 {
        scale = (vw as f32 * 0.72 / w as f32).min(vh as f32 * 0.72 / h as f32);
    }
    let nw = (w as f32 * scale).round() as i32;
    let nh = (h as f32 * scale).round() as i32;

    let hwnd = create_window(
        CLASS,
        "驻匣 · 贴图",
        WS_POPUP,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        1,
        1,
    )?;

    let stride = canvas.stride();
    let pixels = canvas.pixels().to_vec();
    let mut img: *mut GpBitmap = std::ptr::null_mut();
    unsafe {
        if GdipCreateBitmapFromScan0(
            w,
            h,
            stride as i32,
            0x000E_200B,
            Some(pixels.as_ptr() as *mut u8),
            &mut img,
        )
        .0
            != 0
        {
            let _ = DestroyWindow(hwnd);
            return None;
        }
    }

    let mut pin = Box::new(Pin {
        hwnd,
        canvas: Canvas::new(nw, nh, 96),
        img: img as *mut GpImage,
        _pixels: pixels,
        w,
        h,
        scale,
        drag: None,
    });

    let x = (cursor.x - nw / 2).clamp(vx, vx + vw - nw);
    let y = (cursor.y - nh / 2).clamp(vy, vy + vh - nh);
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, nw, nh, SWP_SHOWWINDOW);
    }
    pin.render();
    pins().lock().unwrap_or_else(|p| p.into_inner()).insert(hwnd.0 as isize, pin);
    zx_core::log::info(format!("贴图已创建 {w}x{h} 显示 {nw}x{nh} at ({x},{y})"));
    Some(hwnd)
}

/// 截屏前隐藏所有贴图，返回需要恢复的窗口。
pub fn hide_all() -> Vec<HWND> {
    let map = pins().lock().unwrap_or_else(|p| p.into_inner());
    let mut hidden = Vec::new();
    for pin in map.values() {
        unsafe {
            if IsWindowVisible(pin.hwnd).as_bool() {
                let _ = ShowWindow(pin.hwnd, SW_HIDE);
                hidden.push(pin.hwnd);
            }
        }
    }
    hidden
}

pub fn restore(list: Vec<HWND>) {
    for hwnd in list {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

impl Pin {
    fn original(&self) -> Canvas {
        let mut c = Canvas::new(self.w, self.h, 96);
        c.draw_image(self.img, 0.0, 0.0, self.w as f32, self.h as f32);
        c
    }

    fn render(&mut self) {
        let nw = (self.w as f32 * self.scale).round() as i32;
        let nh = (self.h as f32 * self.scale).round() as i32;
        self.canvas.resize(nw, nh);
        self.canvas.clear(0x00000000);
        self.canvas.draw_image(self.img, 0.0, 0.0, nw as f32, nh as f32);

        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        self.canvas.present(self.hwnd, r.left, r.top);
    }

    fn zoom(&mut self, delta: i32) {
        let step = if delta > 0 { 1.12 } else { 1.0 / 1.12 };
        let next = (self.scale * step).clamp(0.15, 4.0);
        let max_w = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) } as f32 * 1.5;
        let max_h = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) } as f32 * 1.5;
        let fit = (max_w / self.w as f32).min(max_h / self.h as f32);
        self.scale = next.min(fit.max(0.15));
        self.render();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut map = pins().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pin) = map.get_mut(&(hwnd.0 as isize)) {
                if let Some((start, rect)) = pin.drag {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let (dx, dy) = (pt.x - start.x, pt.y - start.y);
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        rect.left + dx,
                        rect.top + dy,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOSIZE,
                    );
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut map = pins().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pin) = map.get_mut(&(hwnd.0 as isize)) {
                let mut pt = POINT::default();
                let mut rect = RECT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = GetWindowRect(hwnd, &mut rect);
                pin.drag = Some((pt, rect));
                let _ = SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let mut map = pins().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pin) = map.get_mut(&(hwnd.0 as isize)) {
                pin.drag = None;
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((w.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut map = pins().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pin) = map.get_mut(&(hwnd.0 as isize)) {
                pin.zoom(delta);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let cmd = menu::popup(
                hwnd,
                pt.x,
                pt.y,
                &[
                    Item::new(1, "复制"),
                    Item::new(2, "保存到截图目录"),
                    Item::sep(),
                    Item::new(3, "关闭贴图"),
                ],
            );
            match cmd {
                1 => {
                    let map = pins().lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(pin) = map.get(&(hwnd.0 as isize)) {
                        let orig = pin.original();
                        let ok = crate::output::copy_to_clipboard(&orig);
                        zx_core::log::info(format!("贴图复制（原图 {}x{}）：{ok}", pin.w, pin.h));
                    }
                }
                2 => {
                    let map = pins().lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(pin) = map.get(&(hwnd.0 as isize)) {
                        let dir = crate::cfg().save_dir;
                        let orig = pin.original();
                        if let Some(path) = crate::output::save_png(&orig, &dir) {
                            zx_core::log::info(format!("贴图已保存 {}", path.display()));
                        }
                    }
                }
                3 => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            unsafe {
                let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).unwrap_or_default());
            }
            LRESULT(1)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let mut map = pins().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pin) = map.remove(&(hwnd.0 as isize)) {
                unsafe {
                    let _ = GdipDisposeImage(pin.img);
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
