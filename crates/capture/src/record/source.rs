//! 像素缓冲与屏幕抓帧。
//!
//! 录屏的"一帧"不经过 `zx_ui::Canvas`：Canvas 是 GDI+ 的画布，每次构造要建 DIB、
//! 建 Graphics，适合"画一帧给人看"，不适合"每秒 10~25 次、每次都拷一块内存"。
//! 这里只有一个 `Vec<u8>`（BGRA，自上而下，stride = w*4），抓到即用、用完即弃。
//!
//! 屏幕侧仍然走 `crate::screen::Frame::grab_rect`——和长截图同一个函数，
//! `BitBlt(SRCCOPY | CAPTUREBLT)` 一次拷一块区域。

use windows::Win32::Foundation::RECT;

/// 一块像素：BGRA、自上而下、行间无 padding（`stride == w * 4`）。
#[derive(Clone)]
pub struct FrameBuf {
    pub w: i32,
    pub h: i32,
    pub buf: Vec<u8>,
}

impl FrameBuf {
    pub fn new(w: i32, h: i32) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        FrameBuf { w, h, buf: vec![0u8; w as usize * 4 * h as usize] }
    }

    #[inline]
    pub fn stride(&self) -> usize {
        self.w as usize * 4
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// `y` 行的字节切片。
    #[inline]
    pub fn row(&self, y: usize) -> &[u8] {
        let s = self.stride();
        &self.buf[y * s..(y + 1) * s]
    }

    /// 屏幕坐标下的 ARGB（0xAARRGGBB）。越界返回 0。
    #[inline]
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0;
        }
        let o = y as usize * self.stride() + x as usize * 4;
        let b = self.buf[o] as u32;
        let g = self.buf[o + 1] as u32;
        let r = self.buf[o + 2] as u32;
        0xFF00_0000 | (r << 16) | (g << 8) | b
    }

    /// 填成不透明纯色（ARGB）。
    pub fn clear(&mut self, argb: u32) {
        let (b, g, r) = ((argb & 0xFF) as u8, ((argb >> 8) & 0xFF) as u8, ((argb >> 16) & 0xFF) as u8);
        let a = if (argb >> 24) & 0xFF == 0 { 0xFF } else { ((argb >> 24) & 0xFF) as u8 };
        // 与 capture 其它模块一致用 chunks_exact_mut（as_chunks_mut 需要更新的 MSRV）
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for px in self.buf.chunks_exact_mut(4) {
            px[0] = b;
            px[1] = g;
            px[2] = r;
            px[3] = a;
        }
    }

    /// 整帧缩放到 `dst`（最近邻，够做缩略图预览）。
    pub fn thumbnail(&self, dw: i32, dh: i32) -> FrameBuf {
        let (dw, dh) = (dw.max(1), dh.max(1));
        let mut out = FrameBuf::new(dw, dh);
        let ds = out.stride();
        for y in 0..dh as usize {
            let sy = (y * self.h as usize / dh as usize).min(self.h as usize - 1);
            let srow = self.row(sy);
            for x in 0..dw as usize {
                let sx = (x * self.w as usize / dw as usize).min(self.w as usize - 1);
                let so = sx * 4;
                let dof = y * ds + x * 4;
                out.buf[dof..dof + 4].copy_from_slice(&srow[so..so + 4]);
                out.buf[dof + 3] = 255;
            }
        }
        out
    }
}

/// 从屏幕抓一块区域到 `out`（尺寸必须已经对好）。
///
/// 返回 false 表示这次没抓到（锁屏、UAC 安全桌面、分辨率变了、远程会话断开）。
/// **不要**把它当致命错误：录制要能跳帧继续。
pub fn grab_screen(rect: RECT, out: &mut FrameBuf) -> bool {
    let want_w = (rect.right - rect.left).max(1);
    let want_h = (rect.bottom - rect.top).max(1);
    if out.w != want_w || out.h != want_h {
        *out = FrameBuf::new(want_w, want_h);
    }
    let Some(f) = crate::screen::Frame::grab_rect(rect) else { return false };
    if f.w != out.w || f.h != out.h {
        return false;
    }
    let src = f.pixels_ptr();
    if src.is_null() {
        return false;
    }
    let len = f.pixels_len();
    if len < out.buf.len() {
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(src, out.buf.as_mut_ptr(), out.buf.len());
    }
    true
}

/// 自定义矩形（屏幕物理像素）。用结构体而不是 `RECT` 是为了能 `Copy` + `Eq` + 直接算宽高。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w: w.max(0), h: h.max(0) }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// 两个矩形的并集（脏矩形用）。
    pub fn union(&self, o: Rect) -> Rect {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
    }
}

impl From<Rect> for RECT {
    fn from(r: Rect) -> Self {
        RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
    }
}

/// 等比缩放到"装进"`box_w × box_h`（letterbox，不裁切、不变形）。
pub fn fit(src_w: i32, src_h: i32, box_w: i32, box_h: i32) -> (i32, i32) {
    if src_w <= 0 || src_h <= 0 || box_w <= 0 || box_h <= 0 {
        return (1, 1);
    }
    let sw = box_w as i64 * src_h as i64;
    let sh = box_h as i64 * src_w as i64;
    if sw <= sh {
        // 宽度是限制因素：先占满宽，再按比例算高
        let w = box_w;
        let h = ((src_h as i64 * w as i64 + src_w as i64 / 2) / src_w as i64) as i32;
        (w, h.max(1).min(box_h))
    } else {
        // 高度是限制因素
        let h = box_h;
        let w = ((src_w as i64 * h as i64 + src_h as i64 / 2) / src_h as i64) as i32;
        (w.max(1).min(box_w), h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect() {
        assert_eq!(fit(1920, 1080, 960, 960), (960, 540));
        assert_eq!(fit(1080, 1920, 960, 960), (540, 960));
        assert_eq!(fit(640, 480, 320, 240), (320, 240));
        // 极窄的源也要给出至少 1px
        assert_eq!(fit(1, 1000, 100, 100).0, 1);
    }

    #[test]
    fn rect_union_and_contains() {
        let a = Rect::new(10, 10, 5, 5);
        let b = Rect::new(20, 20, 5, 5);
        let u = a.union(b);
        assert_eq!((u.x, u.y, u.w, u.h), (10, 10, 15, 15));
        assert!(u.contains(24, 24));
        assert!(!u.contains(25, 25));
        assert_eq!(a.union(Rect::default()), a);
        assert!(Rect::default().is_empty());
    }

    #[test]
    fn thumbnail_is_opaque_and_sized() {
        let mut src = FrameBuf::new(100, 50);
        src.clear(0xFF102030);
        let t = src.thumbnail(10, 5);
        assert_eq!((t.w, t.h), (10, 5));
        assert_eq!(t.pixel(3, 3), 0xFF102030);
    }
}
