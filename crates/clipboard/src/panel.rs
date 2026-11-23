//! 剪贴板面板：单列列表 + 悬停详情卡（Hover Card），点一下即粘贴。
//!
//! 设计取向（与上一版相比）：
//! - 去掉左右分栏，面板窄而高（默认 460×580），和系统剪贴板浮窗一个体量；
//! - 列表只留「类型徽标 + 一行预览 + 来源/时间」，详情交给悬停卡，避免列表拥挤；
//! - 分类用顶部胶囊页签：全部 / 文本 / 链接 / 图片 / 文件 / 收藏；
//! - 收藏 = 置顶：收藏的条目固定在最前，行尾有书签标记，重复点一下取消。

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::GdiPlus::{GdipDisposeImage, GdipGetImageHeight, GdipGetImageWidth, GpImage};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_MENU,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use zx_core::win::wide;
use zx_ui::painter::Canvas;
use zx_ui::theme;
use zx_ui::window;
use zx_ui::{with_alpha, TextAlign, VAlign};

use crate::model::{relative_time, Entry, Kind};
use crate::rules;
use crate::store::{self, Filter, Query, Sort};
use crate::win_clip;

const CLASS: &str = "ZhuxiaClipboardPanel";
const CLASS_HOVER: &str = "ZhuxiaClipboardHover";
const PROMPT_CLASS: &str = "ZhuxiaClipboardPrompt";
const WM_REFRESH: u32 = WM_APP + 31;
const WM_TOAST: u32 = WM_APP + 32;
const ID_EDIT: i32 = 101;
const ID_OK: i32 = 102;
const ID_CANCEL: i32 = 103;

// DIP 布局
const PAD: f32 = 12.0;
const HEADER_H: f32 = 46.0;
const SEARCH_Y: f32 = 44.0;
const SEARCH_H: f32 = 32.0;
const SEARCH_ICON: f32 = 28.0;
const TABS_Y: f32 = 84.0;
const TABS_H: f32 = 26.0;
const BODY_Y: f32 = 118.0;
const FOOT_H: f32 = 24.0;
const ROW_H: f32 = 54.0;
const CARD_W: f32 = 380.0;

const TAB_LABELS: [(&str, Tab); 7] = [
    ("全部", Tab::All),
    ("文本", Tab::Texts),
    ("链接", Tab::Urls),
    ("图片", Tab::Images),
    ("文件", Tab::Files),
    ("收藏", Tab::Pinned),
    ("密钥", Tab::Secrets),
];

thread_local! {
    static PANEL: RefCell<*mut Panel> = const { RefCell::new(std::ptr::null_mut()) };
}

// 跨线程只靠 HWND 通信：采集线程/热键线程不能碰 UI 线程的 thread_local。
static PANEL_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static TARGET_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static DISPATCHER: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

const WM_CLIP_SHOW: u32 = WM_APP + 51;
const WM_CLIP_TOGGLE: u32 = WM_APP + 52;
const WM_CLIP_SEARCH: u32 = WM_APP + 53;
const WM_CLIP_HIDE: u32 = WM_APP + 54;
const CLASS_DISPATCH: &str = "ZhuxiaClipboardDispatch";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Texts,
    Urls,
    Images,
    Files,
    Pinned,
    Secrets,
}

impl Tab {
    fn filter(self) -> Filter {
        match self {
            Tab::All => Filter::All,
            Tab::Texts => Filter::Texts,
            Tab::Urls => Filter::Urls,
            Tab::Images => Filter::Images,
            Tab::Files => Filter::Files,
            Tab::Pinned => Filter::Pinned,
            Tab::Secrets => Filter::Secrets,
        }
    }
}

struct Panel {
    hwnd: HWND,
    edit: HWND,
    dpi: u32,
    w: i32,
    h: i32,
    canvas: Canvas,
    light: bool,
    text: String,
    tab: Tab,
    sort: Sort,
    entries: Vec<Entry>,
    sel: usize,
    scroll: usize,
    hover: i32,
    /// 鼠标是否压在行尾的收藏按钮上
    hover_pin: i32,
    target: HWND,
    pinned: bool,
    toast: Option<(String, i64)>,
    in_dialog: bool,
    font_edit: HFONT,
    /// 列表里的图片缩略图（按条目 id 缓存，最多 24 张）
    thumbs: HashMap<i64, *mut GpImage>,
    /// 搜索框里显示的是不是占位提示（原生 EDIT 没有 cue banner，自己造一个）
    placeholder: bool,
    /// 刚显示的时间：焦点切换过程中的 WM_ACTIVATE(失活) 不能立刻把面板关掉
    shown_at: i64,
    /// 悬停卡：独立的分层窗口，不抢焦点、鼠标穿透
    card: HWND,
    card_canvas: Canvas,
    card_img: Option<(*mut GpImage, i64)>,
    /// 准备弹卡的条目（等计时器到点）
    pending_card: i32,
    /// DWM 圆角不可用、改用窗口区域时，resize 要重设区域
    fallback_region: bool,
}

const PLACEHOLDER: &str = "搜索内容 / 备注 / 标签 / 来源";

impl Panel {
    fn dip(&self, v: f32) -> f32 {
        v * self.dpi as f32 / 96.0
    }
    fn px(&self, v: f32) -> i32 {
        (v * self.dpi as f32 / 96.0).round() as i32
    }
    fn body_h(&self) -> f32 {
        self.h as f32 - self.dip(BODY_Y) - self.dip(FOOT_H)
    }
    fn row_h(&self) -> f32 {
        self.dip(ROW_H)
    }
    fn rows_visible(&self) -> usize {
        ((self.body_h() / self.row_h()).floor() as usize).max(1)
    }
    fn list_x(&self) -> f32 {
        self.dip(PAD) - self.dip(4.0)
    }
    fn list_w(&self) -> f32 {
        self.w as f32 - 2.0 * self.list_x()
    }

    /// 搜索带的底色：原生 EDIT 也要用同一个颜色，否则输入区会露出一个浅色方块。
    fn search_bg(&self, p: &theme::Palette) -> u32 {
        blend_on(p.card_solid, p.text, 0x06)
    }

    /// 原生 EDIT 的位置与大小。
    ///
    /// 单行 EDIT 永远把文字贴着控件顶部画，控件比字高很多时就会明显偏上。
    /// 所以控件高度不是直接取搜索带高度，而是「字体行高 + 上下内边距」，
    /// 再在搜索带里垂直居中——字号、搜索带高度怎么改都还是居中的。
    fn edit_rect(&self) -> (i32, i32, i32, i32) {
        let band_x = self.px(PAD + SEARCH_ICON);
        let band_w = self.w - self.px(PAD) * 2 - self.px(SEARCH_ICON) - self.px(2.0);
        let band_y = self.px(SEARCH_Y + 1.0);
        let band_h = (self.px(SEARCH_H) - self.px(2.0)).max(1);
        let text_h = font_text_height(self.font_edit);
        // 单行 EDIT 的文字贴着控件顶部画，所以控件只能比行高高 2px；
        // 多出来的高度会全部落在字下面，看起来就是「字偏上」。
        let pad_v = self.px(1.0);
        let h = (text_h + pad_v * 2).min(band_h);
        let y = band_y + (band_h - h) / 2;
        (band_x, y, band_w, h)
    }

    fn reload(&mut self) {
        let q = Query {
            text: self.text.clone(),
            filter: Some(self.tab.filter()),
            sort: self.sort,
        };
        self.entries = store::store().lock().unwrap_or_else(|p| p.into_inner()).list(&q);
        if self.sel >= self.entries.len() {
            self.sel = self.entries.len().saturating_sub(1);
        }
        if self.scroll > self.sel {
            self.scroll = self.sel;
        }
        let max_scroll = self.entries.len().saturating_sub(self.rows_visible());
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        self.pending_card = -1;
        self.hide_card();
        self.invalidate();
    }

    fn set_placeholder(&mut self, on: bool) {
        self.placeholder = on;
        self.text.clear();
        unsafe {
            let text = wide(if on { PLACEHOLDER } else { "" });
            let _ = SetWindowTextW(self.edit, PCWSTR(text.as_ptr()));
        }
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn selected(&self) -> Option<Entry> {
        self.entries.get(self.sel).cloned()
    }

    // ------------------------------------------------------------ 绘制

    fn draw(&mut self) {
        let hdc = unsafe { GetDC(self.hwnd) };
        self.draw_to(hdc);
        unsafe {
            ReleaseDC(self.hwnd, hdc);
            // 搜索框是子窗口，父窗口整块 BitBlt 会把它盖住：画完立刻让它自己重绘。
            let _ = InvalidateRect(self.edit, None, true);
            let _ = UpdateWindow(self.edit);
        }
    }

    /// 把整块界面画到指定 DC（WM_PAINT 用 BeginPaint 的 DC，其余用 GetDC）。
    fn draw_to(&mut self, hdc: HDC) {
        let p = theme::for_window(self.light);
        self.canvas.clear(0);
        self.canvas.fill_rect(0.0, 0.0, self.w as f32, self.h as f32, p.card_solid);

        self.draw_header(&p);
        self.draw_search(&p);
        self.draw_tabs(&p);
        self.draw_list(&p);
        self.draw_footer(&p);

        if let Some((msg, at)) = self.toast.clone() {
            if crate::model::now_ms() - at < 1600 {
                let w = self.dip(240.0);
                let x = (self.w as f32 - w) / 2.0;
                let y = self.h as f32 - self.dip(FOOT_H) - self.dip(44.0);
                self.canvas.fill_round(x, y, w, self.dip(30.0), self.dip(8.0), 0xE6202020);
                self.canvas.text(&msg, x, y, w, self.dip(30.0), self.dip(12.5), 0xFFFFFFFF, TextAlign::Center, VAlign::Center);
            }
        }

        self.canvas.blit(hdc, 0, 0);
    }

    fn draw_header(&mut self, p: &theme::Palette) {
        let pad = self.dip(PAD);
        self.canvas.text_ex(
            "剪贴板", pad, 0.0, self.dip(120.0), self.dip(HEADER_H),
            self.dip(16.5), p.text, TextAlign::Near, VAlign::Center, true, false,
        );
        let count = format!("{} 条", self.entries.len());
        self.canvas.text(
            &count, pad + self.dip(66.0), 0.0, self.dip(80.0), self.dip(HEADER_H),
            self.dip(11.0), p.subtle, TextAlign::Near, VAlign::Center,
        );

        // 排序（点一下循环）
        let sw = self.dip(56.0);
        let sx = self.w as f32 - pad - self.dip(52.0) - self.dip(8.0) - sw;
        if self.hover == 810 {
            self.canvas.fill_round(sx, self.dip(11.0), sw, self.dip(24.0), self.dip(6.0), p.hover);
        }
        self.canvas.text(
            self.sort.label(), sx, self.dip(11.0), sw, self.dip(24.0),
            self.dip(11.0), p.subtle, TextAlign::Center, VAlign::Center,
        );

        // 设置
        let bw = self.dip(52.0);
        let bx = self.w as f32 - pad - bw;
        let by = self.dip(11.0);
        let bh = self.dip(24.0);
        if self.hover == 900 {
            self.canvas.fill_round(bx, by, bw, bh, self.dip(6.0), p.hover);
        } else {
            self.canvas.fill_round(bx, by, bw, bh, self.dip(6.0), with_alpha(p.text, 0x08));
        }
        self.canvas.text("设置", bx, by, bw, bh, self.dip(11.5), p.text, TextAlign::Center, VAlign::Center);
    }

    /// 搜索框的外框与放大镜：原生 EDIT 只有经典边框，自己画一圈底，输入框才看得出是个框。
    fn draw_search(&mut self, p: &theme::Palette) {
        let pad = self.dip(PAD);
        let y = self.dip(SEARCH_Y);
        let h = self.dip(SEARCH_H);
        let w = self.w as f32 - pad * 2.0;
        self.canvas.fill_round(pad, y, w, h, self.dip(9.0), self.search_bg(p));
        self.canvas.stroke_round(pad, y, w, h, self.dip(9.0), 1.0, p.separator);
        // 放大镜：圆 + 斜手柄。手柄往右下伸，整枚图标的墨迹会比圆心低，
        // 所以圆心要往上提一点，才能和输入文字的光学中心对齐。
        let cx = pad + self.dip(13.2);
        let cy = y + h / 2.0 - self.dip(1.6);
        let r = self.dip(4.6);
        self.canvas.stroke_round(cx - r, cy - r, r * 2.0, r * 2.0, r, 1.4, p.subtle);
        let d = r * 0.72;
        self.canvas.line(
            cx + d,
            cy + d,
            cx + d + self.dip(3.2),
            cy + d + self.dip(3.2),
            1.5,
            p.subtle,
        );
    }

    fn draw_tabs(&mut self, p: &theme::Palette) {
        let pad = self.dip(PAD);
        let y = self.dip(TABS_Y);
        let h = self.dip(TABS_H);
        let mut x = pad;
        for (i, (label, tab)) in TAB_LABELS.iter().enumerate() {
            let w = self.dip(42.0);
            let active = self.tab == *tab;
            if active {
                self.canvas.fill_round(x, y, w, h, self.dip(6.0), with_alpha(p.accent, 0x1F));
            } else if self.hover == 800 + i as i32 {
                self.canvas.fill_round(x, y, w, h, self.dip(6.0), p.hover);
            }
            let color = if active { p.accent } else { p.subtle };
            self.canvas.text_ex(
                label, x, y, w, h, self.dip(11.5), color,
                TextAlign::Center, VAlign::Center, active, false,
            );
            x += w + self.dip(4.0);
        }
    }

    fn draw_list(&mut self, p: &theme::Palette) {
        let x0 = self.list_x();
        let lw = self.list_w();
        let y0 = self.dip(BODY_Y);
        let bh = self.body_h();
        if self.entries.is_empty() {
            let msg = if !self.text.trim().is_empty() {
                "没有匹配的记录"
            } else if self.tab == Tab::Secrets {
                "还没有密钥：右键任意记录 → 收进密钥（Ctrl+K）"
            } else {
                "暂无记录"
            };
            self.canvas.text(msg, x0, y0, lw, bh, self.dip(12.5), p.subtle, TextAlign::Center, VAlign::Center);
            return;
        }

        let row_h = self.row_h();
        let visible = self.rows_visible();
        let end = (self.scroll + visible).min(self.entries.len());
        let entries: Vec<Entry> = self.entries[self.scroll..end].to_vec();
        let pinned_total = self.entries.iter().take_while(|e| e.pinned).count();
        let privacy = crate::cfg().privacy;

        for (i, e) in entries.iter().enumerate() {
            let idx = self.scroll + i;
            let y = y0 + i as f32 * row_h;
            let row = (x0, y + self.dip(2.0), lw, row_h - self.dip(4.0));
            let selected = idx == self.sel;
            let hovered = self.hover == idx as i32;

            if selected {
                self.canvas.fill_round(row.0, row.1, row.2, row.3, self.dip(9.0), with_alpha(p.accent, 0x20));
                self.canvas.fill_round(row.0, row.1 + self.dip(8.0), self.dip(3.0), row.3 - self.dip(16.0), self.dip(1.5), p.accent);
            } else if hovered {
                self.canvas.fill_round(row.0, row.1, row.2, row.3, self.dip(9.0), p.hover);
            }

            // 徽标 / 缩略图
            let bs = self.dip(30.0);
            let bx = row.0 + self.dip(10.0);
            let by = row.1 + (row.3 - bs) / 2.0;
            let (bg, fg) = if e.secret {
                (with_alpha(p.accent, if self.light { 0x22 } else { 0x33 }), p.accent)
            } else {
                badge_colors(e.kind, self.light)
            };
            self.canvas.fill_round(bx, by, bs, bs, self.dip(8.0), bg);
            if e.secret {
                // 密钥分类用锁形徽标，一眼区别于普通文本。
                lock_glyph(&mut self.canvas, bx, by, bs, bs, fg, bg);
            } else {
                let mut drew_thumb = false;
                if e.kind == Kind::Image {
                    if let Some(img) = self.thumb(e) {
                        unsafe {
                            let mut iw = 0u32;
                            let mut ih = 0u32;
                            GdipGetImageWidth(img, &mut iw);
                            GdipGetImageHeight(img, &mut ih);
                            if iw > 0 && ih > 0 {
                                let scale = (bs / iw as f32).min(bs / ih as f32);
                                let dw = iw as f32 * scale;
                                let dh = ih as f32 * scale;
                                self.canvas.draw_image(img, bx + (bs - dw) / 2.0, by + (bs - dh) / 2.0, dw, dh);
                                drew_thumb = true;
                            }
                        }
                    }
                }
                if !drew_thumb {
                    self.canvas.text(e.kind.badge(), bx, by, bs, bs, self.dip(11.0), fg, TextAlign::Center, VAlign::Center);
                }
            }

            // 文本
            let tx = bx + bs + self.dip(10.0);
            let reserve = self.dip(34.0);
            let tw = row.0 + row.2 - reserve - tx;
            let title = preview_line(e);
            let masked = privacy && e.tags.iter().any(|t| t == "sensitive");
            let title_color = if masked { p.subtle } else { p.text };
            self.canvas.text_ex(
                &title, tx, row.1 + self.dip(5.0), tw, self.dip(20.0),
                self.dip(12.5), title_color, TextAlign::Near, VAlign::Center, false, true,
            );

            let mut meta = format!(
                "{} · {}",
                if e.source_app.is_empty() { "未知" } else { &e.source_app },
                relative_time(e.sort_at)
            );
            if e.secret {
                meta = format!("密钥 · {meta}");
            }
            if e.pinned {
                meta.push_str(" · 已收藏");
            }
            if !e.note.trim().is_empty() {
                if e.secret {
                    // 密钥的「描述」直接露一小段，列表框里就能分辨用途。
                    let desc = e.note.trim();
                    let brief: String = desc.chars().take(14).collect();
                    let ell = if desc.chars().count() > 14 { "…" } else { "" };
                    meta.push_str(&format!(" · {brief}{ell}"));
                } else {
                    meta.push_str(" · 有备注");
                }
            }
            if masked {
                meta.push_str(" · 已脱敏");
            }
            self.canvas.text_ex(
                &meta, tx, row.1 + self.dip(24.0), tw, self.dip(16.0),
                self.dip(10.5), p.subtle, TextAlign::Near, VAlign::Center, false, true,
            );

            // 行尾：收藏（置顶）标记 / 悬停按钮
            let (px_, py_, pw_, ph_) = self.pin_rect_at(idx);
            let bm_w = self.dip(8.0);
            let bm_h = self.dip(10.0);
            let bm_dy = self.dip(4.0);
            if e.pinned || hovered {
                if e.pinned {
                    self.canvas.fill_round(px_, py_, pw_, ph_, self.dip(5.0), with_alpha(p.accent, 0x18));
                } else if self.hover_pin == idx as i32 {
                    self.canvas.fill_round(px_, py_, pw_, ph_, self.dip(5.0), p.hover);
                }
                bookmark(
                    &mut self.canvas,
                    px_ + (pw_ - bm_w) / 2.0,
                    py_ + bm_dy,
                    bm_w,
                    bm_h,
                    if e.pinned { p.accent } else { p.subtle },
                );
            }

            // 收藏区与其余记录之间的分隔线
            if pinned_total > 0 && idx + 1 == pinned_total {
                let sep_y = y + row_h - self.dip(1.0);
                self.canvas.fill_rect(x0 + self.dip(10.0), sep_y, lw - self.dip(20.0), 1.0, p.separator);
            }
        }

        // 滚动条
        if self.entries.len() > visible {
            let track_x = self.w as f32 - self.dip(5.0);
            let track_y = y0;
            let track_h = bh;
            let thumb_h = (track_h * visible as f32 / self.entries.len() as f32).max(self.dip(22.0));
            let max_scroll = self.entries.len().saturating_sub(visible) as f32;
            let t = if max_scroll > 0.0 { self.scroll as f32 / max_scroll } else { 0.0 };
            let thumb_y = track_y + (track_h - thumb_h) * t;
            self.canvas.fill_round(track_x, thumb_y, self.dip(3.0), thumb_h, self.dip(1.5), p.separator);
        }
    }

    fn draw_footer(&mut self, p: &theme::Palette) {
        let y = self.h as f32 - self.dip(FOOT_H);
        self.canvas.fill_rect(0.0, y, self.w as f32, self.dip(FOOT_H), with_alpha(p.text, 0x07));
        let hints = "↑↓ 选择   Enter 粘贴   Ctrl+Enter 纯文本   Del 删除   右键更多";
        self.canvas.text(
            hints, self.dip(PAD), y, self.w as f32 - self.dip(PAD) * 2.0, self.dip(FOOT_H),
            self.dip(10.5), p.subtle, TextAlign::Near, VAlign::Center,
        );
    }

    fn thumb(&mut self, e: &Entry) -> Option<*mut GpImage> {
        if let Some(&p) = self.thumbs.get(&e.id) {
            return Some(p);
        }
        if e.content.starts_with("data:") || !std::path::Path::new(&e.content).exists() {
            return None;
        }
        // 附件在盘上是密文：解密到内存再交给 GDI+。
        let bytes = store::Store::read_attachment(&e.content)?;
        let img = win_clip::image_from_bytes(&bytes)?;
        if self.thumbs.len() >= 24 {
            for (_, old) in self.thumbs.drain() {
                unsafe { GdipDisposeImage(old) };
            }
        }
        self.thumbs.insert(e.id, img);
        Some(img)
    }

    // ------------------------------------------------------------ 悬停卡

    fn schedule_card(&mut self, idx: usize) {
        self.pending_card = idx as i32;
        unsafe {
            let _ = SetTimer(self.hwnd, 2, 220, None);
        }
    }

    fn hide_card(&mut self) {
        self.pending_card = -1;
        unsafe {
            let _ = KillTimer(self.hwnd, 2);
            if !self.card.is_invalid() {
                let _ = ShowWindow(self.card, SW_HIDE);
            }
        }
    }

    fn show_card(&mut self, idx: usize) {
        let Some(e) = self.entries.get(idx).cloned() else { return };
        if self.card.is_invalid() {
            return;
        }
        let p = theme::for_window(self.light);
        let pad = self.dip(14.0);
        let w = self.dip(CARD_W);
        let is_secret = e.secret;
        let desc = if is_secret { e.note.trim().to_string() } else { String::new() };
        // 卡片高度跟着内容走，避免一行文本拖出一个大空盒子。
        let body_text: String = if e.kind.is_text() && !is_secret {
            let masked = crate::cfg().privacy && e.tags.iter().any(|t| t == "sensitive");
            if masked {
                rules::mask_preview(&e.content)
            } else {
                e.content.chars().take(1500).collect()
            }
        } else {
            String::new()
        };
        let bottom_reserve = self.dip(if e.tags.is_empty() { 0.0 } else { 20.0 }
            + if is_secret || e.note.trim().is_empty() { 0.0 } else { 20.0 });
        let desc_lines = if desc.is_empty() {
            0.0
        } else {
            let cpl = ((w - pad * 2.0) / self.dip(7.0)).max(8.0);
            (desc.chars().count() as f32 / cpl).ceil().clamp(1.0, 8.0)
        };
        let h = if is_secret {
            // 头部 + 遮蔽框 + 一行提示 + 描述 + 内边距
            self.dip(42.0 + 10.0 + 30.0 + 6.0 + 17.0 + 12.0) + self.dip(18.0) * desc_lines + bottom_reserve
        } else {
            match e.kind {
                Kind::Image => self.dip(340.0),
                Kind::File | Kind::Video => {
                    let n = e.file_paths().len().min(5) as f32;
                    self.dip(42.0 + 20.0) + self.dip(21.0) * n + self.dip(12.0) + bottom_reserve
                }
                _ => {
                    let cpl = ((w - pad * 2.0) / self.dip(7.0)).max(8.0);
                    let lines = (body_text.chars().count() as f32 / cpl).ceil().clamp(1.0, 14.0);
                    self.dip(42.0 + 20.0) + self.dip(18.0) * lines + self.dip(14.0) + bottom_reserve
                }
            }
        };
        // 图片先加载好：画布借用期间不能再借 self 去取图。
        let (img, iw, ih) = if e.kind == Kind::Image && !is_secret {
            let (iw, ih) = self.card_image(&e);
            let img = self.card_img.map(|(p, _)| p);
            (img, iw, ih)
        } else {
            (None, 0.0, 0.0)
        };

        self.card_canvas.resize(w as i32, h as i32);
        self.card_canvas.clear(0);
        let bg = if self.light { 0xFAFFFFFF } else { 0xFA202020 };
        self.card_canvas.fill_round(0.0, 0.0, w, h, self.dip(12.0), bg);
        self.card_canvas.stroke_round(0.5, 0.5, w - 1.0, h - 1.0, self.dip(12.0), 1.0, p.separator);

        // 头部：类型 / 密钥标题 + 来源/时间
        let head = if is_secret { e.secret_title() } else { e.kind.label().to_string() };
        self.card_canvas.text_ex(
            &head, pad, pad, w - pad * 2.0, self.dip(20.0),
            self.dip(13.0), p.text, TextAlign::Near, VAlign::Center, true, true,
        );
        let mut meta = format!(
            "{} · {}",
            if e.source_app.is_empty() { "未知" } else { &e.source_app },
            relative_time(e.created_at)
        );
        if is_secret {
            meta = format!("密钥 · {meta}");
        }
        self.card_canvas.text_ex(
            &meta, pad, pad + self.dip(19.0), w - pad * 2.0, self.dip(16.0),
            self.dip(10.5), p.subtle, TextAlign::Near, VAlign::Center, false, true,
        );
        let mut y = pad + self.dip(42.0);
        self.card_canvas.fill_rect(pad, y, w - pad * 2.0, 1.0, p.separator);
        y += self.dip(10.0);

        // 底部：标签 / 备注（密钥的描述画在正文区，这里不再重复）
        let mut bottom = 0.0;
        if !e.tags.is_empty() {
            bottom += self.dip(20.0);
        }
        if !is_secret && !e.note.trim().is_empty() {
            bottom += self.dip(20.0);
        }
        let bottom_h = bottom;
        let body_h = (h - y - pad - bottom_h - self.dip(4.0)).max(self.dip(40.0));

        if is_secret {
            // 遮蔽框：表示「这里有内容」，但绝不把原文摆到屏幕上。
            let box_h = self.dip(30.0);
            let box_bg = blend_on(p.card_solid, p.text, 0x06);
            self.card_canvas.fill_round(pad, y, w - pad * 2.0, box_h, self.dip(6.0), box_bg);
            let lw = self.dip(13.0);
            let lh = self.dip(15.0);
            let lx = pad + self.dip(9.0);
            let ly = y + (box_h - lh) / 2.0;
            lock_glyph(&mut self.card_canvas, lx, ly, lw, lh, p.subtle, 0);
            self.card_canvas.text_ex(
                &e.masked_content(),
                pad + self.dip(28.0),
                y,
                w - pad * 2.0 - self.dip(36.0),
                box_h,
                self.dip(11.5),
                p.text,
                TextAlign::Near,
                VAlign::Center,
                false,
                true,
            );
            y += box_h + self.dip(6.0);
            self.card_canvas.text_ex(
                "粘贴时只写入上面的密钥内容，标题与描述不会带上",
                pad,
                y,
                w - pad * 2.0,
                self.dip(15.0),
                self.dip(10.0),
                p.subtle,
                TextAlign::Near,
                VAlign::Center,
                false,
                true,
            );
            y += self.dip(17.0);
            if !desc.is_empty() {
                self.card_canvas.text_wrapped(
                    &desc, pad, y, w - pad * 2.0, self.dip(18.0) * desc_lines,
                    self.dip(11.5), p.label, 0,
                );
            }
        } else {
            match e.kind {
                Kind::Image => {
                    if let Some(img) = img {
                        if iw > 0.0 && ih > 0.0 {
                            let scale = ((w - pad * 2.0) / iw).min(body_h / ih).min(1.4);
                            let dw = iw * scale;
                            let dh = ih * scale;
                            let dx = (w - dw) / 2.0;
                            let dy = y + (body_h - dh) / 2.0;
                            self.card_canvas.fill_round(dx, dy, dw, dh, self.dip(6.0), p.track);
                            self.card_canvas.draw_image(img, dx, dy, dw, dh);
                        }
                    }
                    let mut info = e.display_line();
                    if let Ok(meta) = std::fs::metadata(&e.content) {
                        let mb = meta.len() as f64 / 1048576.0;
                        let size = if mb >= 1.0 {
                            format!("  ·  {mb:.1} MB")
                        } else {
                            format!("  ·  {:.0} KB", meta.len() as f64 / 1024.0)
                        };
                        info.push_str(&size);
                    }
                    self.card_canvas.text_ex(
                        &info, pad, y + body_h + self.dip(2.0), w - pad * 2.0, self.dip(16.0),
                        self.dip(10.0), p.subtle, TextAlign::Center, VAlign::Center, false, true,
                    );
                }
                Kind::File | Kind::Video => {
                    let paths = e.file_paths();
                    let mut line = y;
                    for (i, path) in paths.iter().take(5).enumerate() {
                        let name = std::path::Path::new(path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.clone());
                        self.card_canvas.text_ex(
                            &format!("{}. {name}", i + 1), pad, line, w - pad * 2.0 - self.dip(52.0), self.dip(20.0),
                            self.dip(11.5), p.text, TextAlign::Near, VAlign::Center, false, true,
                        );
                        let exists = std::path::Path::new(path).exists();
                        self.card_canvas.text(
                            if exists { "存在" } else { "已删除" },
                            w - pad - self.dip(50.0), line, self.dip(50.0), self.dip(20.0),
                            self.dip(10.0), if exists { p.subtle } else { p.danger }, TextAlign::Far, VAlign::Center,
                        );
                        line += self.dip(21.0);
                    }
                    if paths.len() > 5 {
                        self.card_canvas.text(
                            &format!("…共 {} 项", paths.len()), pad, line, w - pad * 2.0, self.dip(18.0),
                            self.dip(10.5), p.subtle, TextAlign::Near, VAlign::Center,
                        );
                    }
                }
                _ => {
                    self.card_canvas.text_wrapped(&body_text, pad, y, w - pad * 2.0, body_h, self.dip(12.0), p.text, 0);
                }
            }
        }

        // 标签 / 备注
        let mut by = h - pad - bottom_h;
        if !e.tags.is_empty() {
            let tags = e.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join("  ");
            self.card_canvas.text_ex(&tags, pad, by, w - pad * 2.0, self.dip(18.0), self.dip(10.5),
                                     with_alpha(p.accent, 0xE0), TextAlign::Near, VAlign::Center, false, true);
            by += self.dip(20.0);
        }
        if !is_secret && !e.note.trim().is_empty() {
            self.card_canvas.text_ex(&e.note, pad, by, w - pad * 2.0, self.dip(18.0), self.dip(10.5),
                                     p.label, TextAlign::Near, VAlign::Center, false, true);
        }

        // 摆到面板右侧（放不下就翻到左侧），纵向跟着行
        let (_, row_y, _, _) = self.row_screen_rect(idx);
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        let work = {
            let mon = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            unsafe {
                if GetMonitorInfoW(mon, &mut info).as_bool() {
                    info.rcWork
                } else {
                    RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
                }
            }
        };
        let gap = self.dip(8.0) as i32;
        let mut x = r.right + gap;
        if x + w as i32 > work.right - 8 {
            x = r.left - w as i32 - gap;
        }
        if x < work.left + 8 {
            x = work.right - w as i32 - 8;
        }
        let mut ypos = row_y - self.dip(10.0) as i32;
        if ypos + h as i32 > work.bottom - 8 {
            ypos = work.bottom - h as i32 - 8;
        }
        if ypos < work.top + 8 {
            ypos = work.top + 8;
        }
        unsafe {
            let _ = ShowWindow(self.card, SW_SHOWNOACTIVATE);
        }
        self.card_canvas.present(self.card, x, ypos);
    }

    fn card_image(&mut self, e: &Entry) -> (f32, f32) {
        if let Some((img, id)) = self.card_img {
            if id == e.id {
                let mut iw = 0u32;
                let mut ih = 0u32;
                unsafe {
                    GdipGetImageWidth(img, &mut iw);
                    GdipGetImageHeight(img, &mut ih);
                }
                return (iw as f32, ih as f32);
            }
        }
        if let Some((img, _)) = self.card_img.take() {
            unsafe {
                GdipDisposeImage(img);
            }
        }
        if e.content.starts_with("data:") || !std::path::Path::new(&e.content).exists() {
            return (0.0, 0.0);
        }
        let Some(bytes) = store::Store::read_attachment(&e.content) else {
            return (0.0, 0.0);
        };
        let Some(p) = win_clip::image_from_bytes(&bytes) else {
            return (0.0, 0.0);
        };
        self.card_img = Some((p, e.id));
        let mut iw = 0u32;
        let mut ih = 0u32;
        unsafe {
            GdipGetImageWidth(p, &mut iw);
            GdipGetImageHeight(p, &mut ih);
        }
        (iw as f32, ih as f32)
    }

    // ------------------------------------------------------------ 命中与交互

    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let y0 = self.dip(BODY_Y);
        if y < y0 || x < self.list_x() || x > self.list_x() + self.list_w() {
            return None;
        }
        let idx = ((y - y0) / self.row_h()).floor() as usize + self.scroll;
        if idx < self.entries.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn pin_rect_at(&self, idx: usize) -> (f32, f32, f32, f32) {
        let y0 = self.dip(BODY_Y);
        let row_h = self.row_h();
        let y = y0 + (idx - self.scroll) as f32 * row_h;
        let x = self.list_x() + self.list_w() - self.dip(34.0);
        (x, y + self.dip(14.0), self.dip(24.0), self.dip(26.0))
    }

    fn hit_pin(&self, x: f32, y: f32, idx: usize) -> bool {
        let (px, py, pw, ph) = self.pin_rect_at(idx);
        x >= px && x <= px + pw && y >= py && y <= py + ph
    }

    fn tab_at(&self, x: f32, y: f32) -> Option<Tab> {
        if y < self.dip(TABS_Y) || y > self.dip(TABS_Y + TABS_H) {
            return None;
        }
        let mut tx = self.dip(PAD);
        for (label, tab) in TAB_LABELS.iter() {
            let _ = label;
            let w = self.dip(42.0);
            if x >= tx && x <= tx + w {
                return Some(*tab);
            }
            tx += w + self.dip(4.0);
        }
        None
    }

    fn hit_chrome(&self, x: f32, y: f32) -> Option<i32> {
        let pad = self.dip(PAD);
        if y >= self.dip(11.0) && y <= self.dip(35.0) && x >= self.w as f32 - pad - self.dip(52.0) {
            return Some(900);
        }
        if y >= self.dip(11.0) && y <= self.dip(35.0) {
            let sx = self.w as f32 - pad - self.dip(52.0) - self.dip(8.0) - self.dip(56.0);
            if x >= sx && x <= sx + self.dip(56.0) {
                return Some(810);
            }
        }
        None
    }

    fn click(&mut self, x: f32, y: f32) {
        if let Some(hit) = self.hit_chrome(x, y) {
            match hit {
                900 => {
                    // 面板是 TOPMOST，先把面板收起来，设置窗口才看得见。
                    hide();
                    crate::open_settings();
                }
                810 => {
                    self.sort = self.sort.next();
                    self.reload();
                }
                _ => {}
            }
            return;
        }
        if let Some(tab) = self.tab_at(x, y) {
            self.tab = tab;
            self.scroll = 0;
            self.sel = 0;
            self.reload();
            return;
        }
        if let Some(idx) = self.row_at(x, y) {
            self.sel = idx;
            if self.hit_pin(x, y, idx) {
                self.toggle_pin(idx);
                return;
            }
            if let Some(e) = self.selected() {
                self.paste(e, false);
            }
        }
    }

    fn toggle_pin(&mut self, idx: usize) {
        let Some(e) = self.entries.get(idx).cloned() else { return };
        let pinned = store::store().lock().unwrap_or_else(|p| p.into_inner()).toggle_pin(e.id);
        self.toast(if pinned { "已收藏并置顶" } else { "已取消收藏" });
        // 保持选中同一条（置顶后位置会变）
        self.reload();
        if let Some(pos) = self.entries.iter().position(|x| x.id == e.id) {
            self.sel = pos;
            self.ensure_visible();
        }
    }

    fn ensure_visible(&mut self) {
        let visible = self.rows_visible();
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + visible {
            self.scroll = (self.sel + 1).saturating_sub(visible);
        }
    }

    fn paste(&mut self, entry: Entry, plain: bool) {
        let id = entry.id;
        self.hide_card();
        hide();
        std::thread::sleep(std::time::Duration::from_millis(55));
        if !crate::paste::paste_entry(id, plain, false, Some(self.target)) {
            show(true);
            if let Some(p) = panel_of(self.hwnd) {
                p.toast("粘贴失败：内容已不可用");
            }
        }
    }

    fn on_key(&mut self, vk: u16, ctrl: bool, shift: bool, alt: bool) -> bool {
        let page = self.rows_visible().saturating_sub(1).max(1);
        match vk {
            0x28 => {
                if self.sel + 1 < self.entries.len() {
                    self.sel += 1;
                    self.ensure_visible();
                    self.follow_selection();
                }
                true
            }
            0x26 => {
                if self.sel > 0 {
                    self.sel -= 1;
                    self.ensure_visible();
                    self.follow_selection();
                }
                true
            }
            0x21 | 0x22 => {
                if vk == 0x21 {
                    self.sel = self.sel.saturating_sub(page);
                } else {
                    self.sel = (self.sel + page).min(self.entries.len().saturating_sub(1));
                }
                self.ensure_visible();
                self.follow_selection();
                true
            }
            0x24 | 0x23 => {
                self.sel = if vk == 0x24 { 0 } else { self.entries.len().saturating_sub(1) };
                self.ensure_visible();
                self.follow_selection();
                true
            }
            0x0D => {
                if let Some(e) = self.selected() {
                    self.paste(e, ctrl);
                }
                true
            }
            0x2E => {
                if let Some(e) = self.selected() {
                    store::store().lock().unwrap_or_else(|p| p.into_inner()).delete(e.id);
                    self.reload();
                }
                true
            }
            0x50 if ctrl => {
                let idx = self.sel;
                self.toggle_pin(idx);
                true
            }
            0x43 if ctrl => {
                if let Some(e) = self.selected() {
                    crate::paste::write_entry(&e, false);
                    store::store().lock().unwrap_or_else(|p| p.into_inner()).touch(e.id);
                    self.toast("已复制");
                }
                true
            }
            0x4B if ctrl => {
                // 和右键「收进密钥 / 移出密钥」同一个动作。
                if let Some(e) = self.selected() {
                    if e.secret {
                        store::store().lock().unwrap_or_else(|p| p.into_inner()).set_secret(e.id, false, "");
                        self.toast("已移出密钥");
                        self.reload();
                    } else {
                        self.edit_secret();
                    }
                }
                true
            }
            0x54 if ctrl => {
                self.edit_tags();
                true
            }
            0x4E if ctrl => {
                self.edit_note();
                true
            }
            0x45 if ctrl => {
                self.edit_external();
                true
            }
            0x1B => {
                hide();
                true
            }
            _ => {
                let modifier = match crate::cfg().quick_paste_modifier.as_str() {
                    "alt" => alt,
                    "ctrl" => ctrl,
                    "shift" => shift,
                    _ => false,
                };
                if modifier && (0x31..=0x39).contains(&vk) {
                    let idx = (vk - 0x31) as usize;
                    if let Some(e) = self.entries.get(idx).cloned() {
                        self.sel = idx;
                        self.paste(e, false);
                    }
                    return true;
                }
                false
            }
        }
    }

    /// 键盘移动后：选中项始终可见，并把悬停卡切到这一条。
    fn follow_selection(&mut self) {
        self.hover = self.sel as i32;
        self.invalidate();
        if self.entries.is_empty() {
            self.hide_card();
        } else if is_visible() {
            self.schedule_card(self.sel);
        }
    }

    fn context_menu(&mut self, x: i32, y: i32, idx: usize) {
        use zx_ui::menu::{self, Item};
        self.sel = idx;
        let Some(e) = self.selected() else { return };
        let pin_label = if e.pinned { "取消收藏" } else { "收藏并置顶" };
        let secret_label = if e.secret { "移出密钥" } else { "收进密钥…" };
        let items = [
            Item::new(1, "粘贴"),
            Item::new(2, "纯文本粘贴"),
            Item::new(3, "复制到剪贴板"),
            Item::sep(),
            Item::new(4, pin_label),
            Item::new(14, secret_label),
            Item::new(15, "编辑密钥标题与描述…").enabled(e.secret),
            Item::new(5, "标签…"),
            Item::new(6, "备注…"),
            Item::sep(),
            Item::new(7, "打开"),
            Item::new(8, "在资源管理器中显示").enabled(e.is_external()),
            Item::new(9, "用外部编辑器编辑…").enabled(e.kind.is_text()),
            Item::new(10, "设置这条的快捷键…"),
            Item::new(12, "复制为密码（不记录）").enabled(e.kind.is_text()),
            Item::new(13, if e.tags.is_empty() { "按标签筛选（无标签）" } else { "按第一个标签筛选" }).enabled(!e.tags.is_empty()),
            Item::sep(),
            Item::new(11, "删除"),
        ];
        let cmd = menu::popup(self.hwnd, x, y, &items);
        match cmd {
            1 => self.paste(e.clone(), false),
            2 => self.paste(e.clone(), true),
            3 => {
                crate::paste::write_entry(&e, false);
                store::store().lock().unwrap_or_else(|p| p.into_inner()).touch(e.id);
                self.toast("已复制");
            }
            4 => self.toggle_pin(idx),
            14 => {
                if e.secret {
                    store::store().lock().unwrap_or_else(|p| p.into_inner()).set_secret(e.id, false, "");
                    self.toast("已移出密钥");
                    self.reload();
                } else {
                    self.edit_secret();
                }
            }
            15 => self.edit_secret(),
            5 => self.edit_tags(),
            6 => self.edit_note(),
            7 => self.open_entry(&e),
            8 => {
                let first = e.file_paths().into_iter().next().unwrap_or_else(|| e.content.clone());
                crate::source::reveal(&first);
            }
            9 => self.edit_external(),
            10 => self.edit_hotkey(),
            12 => {
                crate::win_clip::write_private_text(&e.content);
                self.toast("已复制（不会进入历史）");
            }
            13 => {
                if let Some(tag) = e.tags.first().cloned() {
                    self.tab = Tab::All;
                    self.text = tag.clone();
                    unsafe {
                        let t = wide(&tag);
                        let _ = SetWindowTextW(self.edit, PCWSTR(t.as_ptr()));
                    }
                    self.placeholder = false;
                    self.sel = 0;
                    self.scroll = 0;
                    self.reload();
                    self.toast(&format!("筛选 #{tag}"));
                }
            }
            11 => {
                store::store().lock().unwrap_or_else(|p| p.into_inner()).delete(e.id);
                self.reload();
            }
            _ => {}
        }
    }

    fn open_entry(&self, e: &Entry) {
        match e.kind {
            Kind::Image => {
                // 外部图片直接打开；本模块的附件是密文，先解密到临时目录。
                if !store::Store::attachment_is_protected(&e.content) {
                    win_clip::shell_open(&e.content);
                    return;
                }
                let Some(bytes) = store::Store::read_attachment(&e.content) else {
                    return;
                };
                let dir = zx_core::cfg::clipboard_dir().join("open");
                let _ = std::fs::create_dir_all(&dir);
                let ext = e.ext.as_deref().unwrap_or("png");
                let path = dir.join(format!("{}.{ext}", e.id));
                if std::fs::write(&path, bytes).is_ok() {
                    win_clip::shell_open(&path.to_string_lossy());
                }
            }
            Kind::File | Kind::Video => {
                for p in e.file_paths() {
                    win_clip::shell_open(&p);
                }
            }
            Kind::Url => win_clip::shell_open(&e.content),
            _ => self.open_temp_copy(e),
        }
    }

    fn open_temp_copy(&self, e: &Entry) {
        let dir = zx_core::cfg::clipboard_dir().join("edit");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.txt", e.id));
        if std::fs::write(&path, &e.content).is_ok() {
            win_clip::shell_open(&path.to_string_lossy());
        }
    }

    fn edit_tags(&mut self) {
        let Some(e) = self.selected() else { return };
        let initial = e.tags.join(", ");
        let Some(text) = prompt("编辑标签", "多个标签用逗号分隔（回车确认，Esc 取消）", &initial, false) else { return };
        let tags: Vec<String> = text
            .split([',', '，', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        {
            let mut s = store::store().lock().unwrap_or_else(|p| p.into_inner());
            s.set_tags(e.id, &tags);
        }
        self.reload();
    }

    /// 收进密钥 / 编辑密钥信息：标题 + 描述，两项都只是元数据。
    fn edit_secret(&mut self) {
        let Some(e) = self.selected() else { return };
        let default_title = if e.title.trim().is_empty() {
            let t = preview_line(&e);
            let t = t.trim();
            if t.is_empty() {
                e.secret_title()
            } else if t.chars().count() > 24 {
                t.chars().take(24).collect()
            } else {
                t.to_string()
            }
        } else {
            e.title.clone()
        };
        let Some(v) = form(
            "密钥信息",
            "标题和描述只用来分辨用途；粘贴时只会写入密钥本身的内容。",
            &[("标题", &default_title, false), ("描述（可留空）", &e.note, false)],
        ) else {
            return;
        };
        let title = v.first().cloned().unwrap_or_default();
        let desc = v.get(1).cloned().unwrap_or_default();
        {
            let mut st = store::store().lock().unwrap_or_else(|p| p.into_inner());
            st.set_secret(e.id, true, &title);
            st.set_note(e.id, &desc);
        }
        self.reload();
        if let Some(pos) = self.entries.iter().position(|x| x.id == e.id) {
            self.sel = pos;
            self.ensure_visible();
        }
        self.toast("已收进密钥");
    }

    fn edit_note(&mut self) {
        let Some(e) = self.selected() else { return };
        let Some(text) = prompt("备注", "只给自己看的说明（回车换行，Ctrl+Enter 保存）", &e.note, true) else { return };
        store::store().lock().unwrap_or_else(|p| p.into_inner()).set_note(e.id, &text);
        self.reload();
    }

    fn edit_hotkey(&mut self) {
        let Some(e) = self.selected() else { return };
        let Some(text) = prompt("条目快捷键", "例如 Ctrl+Alt+1；留空清除（全局生效）", &e.hotkey, false) else { return };
        let mut s = store::store().lock().unwrap_or_else(|p| p.into_inner());
        s.set_hotkey(e.id, text.trim());
        drop(s);
        crate::hotkeys::reload_item_hotkeys();
        self.reload();
    }

    fn edit_external(&mut self) {
        let Some(e) = self.selected() else { return };
        if !e.kind.is_text() {
            self.toast("只有文本可以外部编辑");
            return;
        }
        let dir = zx_core::cfg::clipboard_dir().join("edit");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.txt", e.id));
        if std::fs::write(&path, &e.content).is_err() {
            return;
        }
        win_clip::shell_open(&path.to_string_lossy());
        let id = e.id;
        let original = e.content.clone();
        let hwnd_raw = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let mut last = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            for _ in 0..900 {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let now = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if now != last {
                    last = now;
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if text != original {
                            store::store().lock().unwrap_or_else(|p| p.into_inner()).update_content(id, &text);
                            unsafe {
                                let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
                                let _ = PostMessageW(hwnd, WM_REFRESH, WPARAM(0), LPARAM(0));
                            }
                            break;
                        }
                    }
                }
            }
        });
        self.toast("已用默认编辑器打开，存盘后自动写回");
    }

    fn toast(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), crate::model::now_ms()));
        unsafe {
            let _ = SetTimer(self.hwnd, 1, 300, None);
        }
        self.invalidate();
    }

    fn row_screen_rect(&self, idx: usize) -> (f32, i32, f32, f32) {
        let y = self.dip(BODY_Y) + (idx - self.scroll) as f32 * self.row_h();
        let mut pt = POINT { x: self.px(PAD), y: y as i32 };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut pt);
        }
        (pt.x as f32, pt.y, self.dip(CARD_W), self.row_h())
    }
}

/// `fg` 按 `alpha` 叠在 `bg` 上，返回不透明 ARGB。
fn blend_on(bg: u32, fg: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let ch = |shift: u32| {
        let b = (bg >> shift) & 0xFF;
        let f = (fg >> shift) & 0xFF;
        (b * (255 - a) + f * a) / 255
    };
    (0xFF << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// GDI 字体的行高（TM 高度，含上下留白）。原生 EDIT 的垂直居中要靠它算。
fn font_text_height(font: HFONT) -> i32 {
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return 16;
        }
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let mut tm = TEXTMETRICW::default();
        let ok = GetTextMetricsW(dc, &mut tm).as_bool();
        SelectObject(dc, old);
        let _ = DeleteDC(dc);
        if ok && tm.tmHeight > 0 {
            tm.tmHeight
        } else {
            16
        }
    }
}

fn badge_colors(k: Kind, light: bool) -> (u32, u32) {
    let base = match k {
        Kind::Text => 0xFF0F6CBD,
        Kind::Code => 0xFF7A3E9D,
        Kind::Url => 0xFF0B7A6B,
        Kind::RichText => 0xFFB25E00,
        Kind::Image => 0xFF2E7D32,
        Kind::File => 0xFF5A5A5A,
        Kind::Video => 0xFFB03A2E,
    };
    if light {
        (with_alpha(base, 0x20), base)
    } else {
        (with_alpha(base, 0x40), 0xFFEDEDED)
    }
}

fn preview_line(e: &Entry) -> String {
    // 密钥在列表里只露标题，正文绝不直接显示。
    if e.secret {
        return e.secret_title();
    }
    match e.kind {
        Kind::Image => e.display_line(),
        Kind::File | Kind::Video => e.display_line(),
        _ => {
            let s = if e.preview.trim().is_empty() { e.content.clone() } else { e.preview.clone() };
            s.replace('\n', " ")
        }
    }
}

/// 锁形图标（密钥分类用）。`x,y` 是左上角，`w,h` 是尺寸；
/// `bg` 用来在锁体上「挖」出钥匙孔，透明色表示不挖。
fn lock_glyph(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, color: u32, bg: u32) {
    let bw = w * 0.60;
    let bx = x + (w - bw) / 2.0;
    let body_y = y + h * 0.45;
    let body_h = h * 0.38;
    let stroke = (h * 0.08).max(1.2);
    // 锁梁
    c.stroke_round(bx + bw * 0.16, y + h * 0.17, bw * 0.68, h * 0.46, bw * 0.34, stroke, color);
    // 锁体（盖住锁梁下半截）
    c.fill_round(bx, body_y, bw, body_h, w * 0.12, color);
    if (bg >> 24) != 0 {
        let cx = x + w / 2.0;
        c.fill_round(cx - w * 0.045, body_y + body_h * 0.26, w * 0.09, body_h * 0.42, w * 0.045, bg);
    }
}

/// 书签形状：收藏（置顶）标记。`x,y` 是左上角，`w,h` 是尺寸。
fn bookmark(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, color: u32) {
    let pts = [
        (x, y),
        (x + w, y),
        (x + w, y + h),
        (x + w / 2.0, y + h * 0.72),
        (x, y + h),
    ];
    c.fill_polygon(&pts, color);
}

// ---------------------------------------------------------------- 窗口

/// 在 UI 线程上建一个隐藏的调度窗口：热键线程/采集线程都通过它进出面板。
pub fn init() {
    register();
    register_hover_class();
    register_dispatcher();
}

pub fn is_visible() -> bool {
    let h = PANEL_HWND.load(std::sync::atomic::Ordering::SeqCst);
    h != 0 && unsafe { IsWindowVisible(HWND(h as *mut core::ffi::c_void)).as_bool() }
}

pub fn target_window() -> Option<HWND> {
    let t = TARGET_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if t == 0 {
        None
    } else {
        Some(HWND(t as *mut core::ffi::c_void))
    }
}

pub fn toast(msg: &str) {
    let h = PANEL_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let boxed = Box::into_raw(Box::new(msg.to_string()));
    unsafe {
        let _ = PostMessageW(HWND(h as *mut core::ffi::c_void), WM_TOAST, WPARAM(boxed as usize), LPARAM(0));
    }
}

/// 采集线程调用：通知面板刷新（PostMessage 可跨线程）。
pub fn refresh() {
    let h = PANEL_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut core::ffi::c_void), WM_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn hide() {
    dispatch(WM_CLIP_HIDE, 0);
}

pub fn hide_for_paste() {
    hide();
}

pub fn toggle() {
    dispatch(WM_CLIP_TOGGLE, 0);
}

/// 低级键盘钩子里用：只投消息，绝不能等 UI（钩子回调有 300ms 超时）。
pub fn toggle_async() {
    let d = DISPATCHER.load(std::sync::atomic::Ordering::SeqCst);
    if d == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(HWND(d as *mut core::ffi::c_void), WM_CLIP_TOGGLE, WPARAM(0), LPARAM(0));
    }
}

pub fn show(focus_search: bool) {
    dispatch(WM_CLIP_SHOW, if focus_search { 1 } else { 0 });
}

pub fn show_search() {
    dispatch(WM_CLIP_SEARCH, 0);
}

fn dispatch(msg: u32, arg: usize) {
    let d = DISPATCHER.load(std::sync::atomic::Ordering::SeqCst);
    if d == 0 {
        return;
    }
    unsafe {
        SendMessageW(HWND(d as *mut core::ffi::c_void), msg, WPARAM(arg), LPARAM(0));
    }
}

/// 真正创建/显示面板（只在 UI 线程执行）。
fn show_now(focus_search: bool) {
    let existing = PANEL.with(|p| *p.borrow());
    if !existing.is_null() {
        unsafe {
            let panel = &mut *existing;
            let target = current_target();
            panel.target = target;
            TARGET_HWND.store(panel.target.0 as isize, std::sync::atomic::Ordering::SeqCst);
            panel.sort = Sort::from_name(&crate::cfg().sort);
            panel.pinned = crate::cfg().window_pinned;
            panel.hide_card();
            panel.reload();
            // 每次唤起都重新跟随插入符：上次的位置可能已经离输入框很远了。
            let (px, py) = match caret_anchor(target) {
                Some(a) => position_near_caret(a, panel.w, panel.h),
                None => position_at_cursor(panel.w, panel.h),
            };
            let _ = SetWindowPos(panel.hwnd, HWND_TOPMOST, px, py, panel.w, panel.h, SWP_SHOWWINDOW);
            let _ = ShowWindow(panel.hwnd, SW_SHOW);
            panel.shown_at = crate::model::now_ms();
            // 搜索内容为空时补回占位提示（用户可能清空过）。
            if panel.text.trim().is_empty() && !panel.placeholder {
                panel.set_placeholder(true);
            }
            focus_panel(panel.hwnd, panel.edit);
            if focus_search {
                let _ = SetFocus(panel.edit);
                // 占位提示不是真实内容：别全选（那会变成一大块蓝色）。
                let end = if panel.placeholder { 0 } else { -1 };
                SendMessageW(panel.edit, 0x00B1, WPARAM(0), LPARAM(end));
            }
            panel.draw();
        }
        return;
    }
    create_panel(focus_search);
}

/// 首次创建面板窗口（只在 UI 线程执行）。
fn create_panel(focus_search: bool) {
    let dpi = unsafe {
        let d = GetDpiForSystem();
        if d == 0 { 96 } else { d }
    };
    let cfg = crate::cfg();
    let w = (cfg.window_width as f32 * dpi as f32 / 96.0).round() as i32;
    let h = (cfg.window_height as f32 * dpi as f32 / 96.0).round() as i32;
    let Some(hwnd) = window::create_window(CLASS, "驻匣 · 剪贴板", WS_POPUP, WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w, h) else {
        zx_core::log::warn("剪贴板面板创建失败");
        return;
    };
    window::set_window_icon(hwnd);
    let rounded = apply_round_corners(hwnd, w, h, dpi);
    let light = match zx_core::cfg::cfg().theme() {
        Some(zx_core::Theme::Light) => true,
        Some(zx_core::Theme::Dark) => false,
        None => zx_core::win::apps_light(),
    };
    let font_edit = unsafe {
        let name = wide("Microsoft YaHei UI");
        CreateFontW(
            -((10 * dpi as i32) / 72), 0, 0, 0, 400, 0, 0, 0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            PCWSTR(name.as_ptr()),
        )
    };
    let card = create_hover_card(w, h, dpi, light);
    let target = current_target();
    let mut panel = Box::new(Panel {
        hwnd,
        edit: HWND::default(),
        dpi,
        w,
        h,
        canvas: Canvas::new(w, h, dpi),
        light,
        text: String::new(),
        tab: Tab::All,
        sort: Sort::from_name(&crate::cfg().sort),
        entries: Vec::new(),
        sel: 0,
        scroll: 0,
        hover: -1,
        hover_pin: -1,
        target,
        pinned: cfg.window_pinned,
        toast: None,
        in_dialog: false,
        font_edit,
        thumbs: HashMap::new(),
        placeholder: false,
        shown_at: 0,
        card,
        card_canvas: Canvas::new(1, 1, dpi),
        card_img: None,
        pending_card: -1,
        fallback_region: !rounded,
    });

    let (ex, ey, ew, eh) = panel.edit_rect();
    let edit = unsafe {
        let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP
            | WINDOW_STYLE(ES_AUTOHSCROLL as u32)
            | WINDOW_STYLE(ES_LEFT as u32);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("EDIT"),
            PCWSTR::null(),
            style,
            ex, ey, ew, eh,
            hwnd,
            HMENU(ID_EDIT as isize as *mut core::ffi::c_void),
            HINSTANCE::default(),
            None,
        )
    };
    let edit = match edit {
        Ok(h) => h,
        Err(e) => {
            zx_core::log::warn(format!("搜索框创建失败：{e}"));
            unsafe {
                let _ = DestroyWindow(hwnd);
                if !card.is_invalid() {
                    let _ = DestroyWindow(card);
                }
                let _ = DeleteObject(HGDIOBJ(panel.font_edit.0));
            }
            return;
        }
    };
    panel.edit = edit;
    subclass_edit(edit);
    unsafe {
        SendMessageW(edit, WM_SETFONT, WPARAM(panel.font_edit.0 as usize), LPARAM(1));
        // 默认左边距贴着控件边缘，字形会撞到放大镜：左右各留 2px。
        SendMessageW(
            edit,
            0x00D3, // EM_SETMARGINS
            WPARAM(0x0001 | 0x0002),
            LPARAM((2 | (2 << 16)) as isize),
        );
    }

    let ptr = Box::into_raw(panel);
    PANEL.with(|p| *p.borrow_mut() = ptr);
    PANEL_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);

    unsafe {
        let (x, y) = match caret_anchor(target) {
            Some(a) => position_near_caret(a, w, h),
            None => position_at_cursor(w, h),
        };
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW);
        focus_panel(hwnd, edit);
        (*ptr).shown_at = crate::model::now_ms();
        (*ptr).target = current_target();
        TARGET_HWND.store((*ptr).target.0 as isize, std::sync::atomic::Ordering::SeqCst);
        (*ptr).set_placeholder(true);
        (*ptr).reload();
        if focus_search {
            let _ = SetFocus(edit);
            SendMessageW(edit, 0x00B1, WPARAM(0), LPARAM(0));
        }
        (*ptr).draw();
    }
    zx_core::log::info(format!("剪贴板面板已创建 {w}x{h} dpi={dpi}"));
}

fn create_hover_card(w: i32, h: i32, dpi: u32, _light: bool) -> HWND {
    let _ = (w, h);
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        if let Ok(card) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            PCWSTR(wide(CLASS_HOVER).as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 1, 1,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(hinst.0),
            None,
        ) {
            let _ = dpi;
            return card;
        }
    }
    HWND::default()
}

fn hide_now() {
    PANEL.with(|p| {
        let ptr = *p.borrow();
        if !ptr.is_null() {
            unsafe {
                (*ptr).hide_card();
                let _ = ShowWindow((*ptr).hwnd, SW_HIDE);
            }
        }
    });
}

fn register_hover_class() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        static NAME: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
        let name = NAME.get_or_init(|| wide(CLASS_HOVER));
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(hover_proc),
            hInstance: HINSTANCE(hinst.0),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }
}

unsafe extern "system" fn hover_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, w, l)
}

fn register_dispatcher() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        static NAME: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
        let name = NAME.get_or_init(|| wide(CLASS_DISPATCH));
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(dispatch_proc),
            hInstance: HINSTANCE(hinst.0),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
        if DISPATCHER.load(std::sync::atomic::Ordering::SeqCst) != 0 {
            return;
        }
        if let Ok(h) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(name.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE::default(),
            0, 0, 0, 0,
            HWND_MESSAGE,
            HMENU::default(),
            HINSTANCE(hinst.0),
            None,
        ) {
            DISPATCHER.store(h.0 as isize, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

unsafe extern "system" fn dispatch_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CLIP_SHOW => {
            show_now(w.0 != 0);
            return LRESULT(0);
        }
        WM_CLIP_SEARCH => {
            show_now(true);
            return LRESULT(0);
        }
        WM_CLIP_TOGGLE => {
            if is_visible() {
                hide_now();
            } else {
                show_now(true);
            }
            return LRESULT(0);
        }
        WM_CLIP_HIDE => {
            hide_now();
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

/// 把面板抢到前台。`SetForegroundWindow` 在别的进程正前台时经常被系统拒绝，
/// 临时 attach 到前台线程的输入队列可以稳定拿到焦点。
fn focus_panel(hwnd: HWND, edit: HWND) {
    unsafe {
        // 先尝试直接抢；被拒时 attach 到前台线程再试。
        // 面板是 TOPMOST 工具窗，连续两次失败基本就是前台锁没放开。
        for attempt in 0..3 {
            let fg = GetForegroundWindow();
            if fg == hwnd {
                let _ = SetFocus(edit);
                return;
            }
            let mut pid = 0u32;
            let fg_thread = GetWindowThreadProcessId(fg, Some(&mut pid));
            let self_thread = windows::Win32::System::Threading::GetCurrentThreadId();
            let attached = fg_thread != 0
                && fg_thread != self_thread
                && windows::Win32::System::Threading::AttachThreadInput(self_thread, fg_thread, true).as_bool();
            let _ = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow(hwnd);
            let _ = SetFocus(edit);
            if attached {
                let _ = windows::Win32::System::Threading::AttachThreadInput(self_thread, fg_thread, false);
            }
            if GetForegroundWindow() == hwnd {
                return;
            }
            // 有些前台程序（浏览器、游戏）会一直拒绝；注入一次 Alt 松开前台锁再试。
            if attempt == 1 {
                tap_alt();
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        zx_core::log::warn("剪贴板面板未能取得前台焦点（面板仍可见，可用鼠标操作）");
    }
}

/// 注入一次 Alt 按下/抬起：这是解锁 `SetForegroundWindow` 的老办法。
fn tap_alt() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VK_MENU,
    };
    let ev = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let events = [ev(false), ev(true)];
    unsafe {
        SendInput(&events, std::mem::size_of::<INPUT>() as i32);
    }
}

fn current_target() -> HWND {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || crate::source::is_own_window(fg) {
            let t = crate::source::foreground_raw();
            if crate::source::is_own_window(t) { HWND::default() } else { t }
        } else {
            fg
        }
    }
}

/// 面板四角改圆角。Win11 用 DWM 的圆角（抗锯齿、带阴影），
/// 老系统 / DWM 拒绝时退回 `SetWindowRgn` 的硬裁切。
fn apply_round_corners(hwnd: HWND, w: i32, h: i32, dpi: u32) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
    let pref: i32 = 2; // DWMWCP_ROUND
    let ok = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
        .is_ok()
    };
    if !ok {
        zx_ui::painter::set_round_region(hwnd, w, h, zx_core::win::dpi_scale_i(12.0, dpi));
    }
    ok
}

/// 插入符位置（屏幕坐标）。比鼠标位置更贴近「我正在哪儿输入」。
#[derive(Clone, Copy)]
struct Anchor {
    x: i32,
    top: i32,
    bottom: i32,
}

/// 取目标窗口所属线程的插入符。取不到（自绘输入框、部分 UWP）就返回 None，
/// 调用方退回鼠标位置。
fn caret_anchor(target: HWND) -> Option<Anchor> {
    if target.is_invalid() {
        return None;
    }
    unsafe {
        let mut pid = 0u32;
        let tid = GetWindowThreadProcessId(target, Some(&mut pid));
        if tid == 0 {
            return None;
        }
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(tid, &mut gti).is_err() || gti.hwndCaret.is_invalid() {
            return None;
        }
        let mut top = POINT { x: gti.rcCaret.left, y: gti.rcCaret.top };
        let mut bottom = POINT { x: gti.rcCaret.left, y: gti.rcCaret.bottom };
        if !ClientToScreen(gti.hwndCaret, &mut top).as_bool()
            || !ClientToScreen(gti.hwndCaret, &mut bottom).as_bool()
        {
            return None;
        }
        if bottom.y <= top.y {
            return None;
        }
        Some(Anchor { x: top.x, top: top.y, bottom: bottom.y })
    }
}

/// 贴着插入符摆放：左边缘对齐光标，优先向下弹，放不下就翻到上方。
fn position_near_caret(a: Anchor, w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mon = MonitorFromPoint(
            POINT { x: a.x, y: a.bottom },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let wa = if GetMonitorInfoW(mon, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
        };
        let gap = 6;
        let x = a.x.clamp(wa.left + 8, (wa.right - w - 8).max(wa.left + 8));
        let below = a.bottom + gap;
        let y = if below + h <= wa.bottom - 8 {
            below
        } else {
            (a.top - h - gap).max(wa.top + 8)
        };
        (x, y)
    }
}

fn position_at_cursor(w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(mon, &mut info).as_bool() {
            return (100, 100);
        }
        let wa = info.rcWork;
        let x = (pt.x - w / 2).clamp(wa.left + 8, (wa.right - w - 8).max(wa.left + 8));
        let y = if pt.y + 20 + h < wa.bottom {
            pt.y + 20
        } else {
            (pt.y - h - 20).max(wa.top + 8)
        };
        (x, y)
    }
}

fn register() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        static NAME: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
        let name = NAME.get_or_init(|| wide(CLASS));
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW | CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(panel_proc),
            hInstance: HINSTANCE(hinst.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }
}

/// 搜索框内容 → 重新过滤（粘贴、退格、输入法上屏都走这里）。
fn sync_search() {
    PANEL.with(|p| {
        let ptr = *p.borrow();
        if ptr.is_null() {
            return;
        }
        unsafe {
            let p = &mut *ptr;
            let len = GetWindowTextLengthW(p.edit);
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(p.edit, &mut buf);
            let text = String::from_utf16_lossy(&buf[..n as usize]);
            if p.placeholder {
                if text == PLACEHOLDER {
                    return;
                }
                // 粘贴 / SetWindowText 不会走 WM_CHAR：文本变了就退出占位态，
                // 否则列表不刷新、下一次按键还会把粘贴的内容清掉。
                p.placeholder = false;
            }
            if text != p.text {
                p.text = text;
                p.sel = 0;
                p.scroll = 0;
                p.reload();
                p.draw();
            }
        }
    });
}

fn panel_of(hwnd: HWND) -> Option<&'static mut Panel> {
    PANEL.with(|p| {
        let ptr = *p.borrow();
        if !ptr.is_null() && unsafe { (*ptr).hwnd } == hwnd {
            unsafe { Some(&mut *ptr) }
        } else {
            None
        }
    })
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            // 必须 BeginPaint/EndPaint：不校验更新区域会无限重绘。
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(p) = panel_of(hwnd) {
                p.draw_to(hdc);
            }
            let _ = EndPaint(hwnd, &ps);
            // 子窗口在父窗口 WM_PAINT 期间会被父的更新区域裁掉，所以放到 EndPaint 之后重绘。
            if let Some(p) = panel_of(hwnd) {
                let _ = InvalidateRect(p.edit, None, true);
                let _ = UpdateWindow(p.edit);
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            if let Some(p) = panel_of(hwnd) {
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                p.w = (r.right - r.left).max(1);
                p.h = (r.bottom - r.top).max(1);
                p.canvas.resize(p.w, p.h);
                if p.fallback_region {
                    zx_ui::painter::set_round_region(hwnd, p.w, p.h, zx_core::win::dpi_scale_i(12.0, p.dpi));
                }
                let (ex, ey, ew, eh) = p.edit_rect();
                let _ = SetWindowPos(p.edit, None, ex, ey, ew, eh, SWP_NOZORDER);
                p.hide_card();
                p.draw();
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            let id = (w.0 & 0xFFFF) as i32;
            let code = ((w.0 >> 16) & 0xFFFF) as u32;
            if id == ID_EDIT && code == 0x0300 {
                sync_search();
            }
            return LRESULT(0);
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let hdc = HDC(w.0 as *mut core::ffi::c_void);
            if let Some(p) = panel_of(hwnd) {
                let pal = theme::for_window(p.light);
                let rgb = |argb: u32| -> COLORREF {
                    let r = ((argb >> 16) & 0xFF) as u8;
                    let g = ((argb >> 8) & 0xFF) as u8;
                    let b = (argb & 0xFF) as u8;
                    COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
                };
                let color = if p.placeholder { pal.subtle } else { pal.text };
                let bg = p.search_bg(&pal);
                SetTextColor(hdc, rgb(color));
                // 字形背景和输入框底色必须一致，否则文字后面会露出一条浅色。
                SetBkColor(hdc, rgb(bg));
                SetBkMode(hdc, TRANSPARENT);
                static BRUSH: std::sync::OnceLock<std::sync::Mutex<(u32, isize)>> =
                    std::sync::OnceLock::new();
                let cell = BRUSH.get_or_init(|| std::sync::Mutex::new((0, 0)));
                let mut guard = cell.lock().unwrap_or_else(|p| p.into_inner());
                let want = bg;
                if guard.0 != want || guard.1 == 0 {
                    if guard.1 != 0 {
                        let _ = DeleteObject(HGDIOBJ(guard.1 as *mut core::ffi::c_void));
                    }
                    guard.1 = CreateSolidBrush(rgb(want)).0 as isize;
                    guard.0 = want;
                }
                return LRESULT(guard.1);
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            if let Some(p) = panel_of(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as f32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
                let idx = p.row_at(x, y);
                let chrome = p.hit_chrome(x, y).unwrap_or(-1);
                let tab = p.tab_at(x, y).map(|t| 800 + t as i32).unwrap_or(-1);
                let pin_hover = idx
                    .filter(|&i| p.hit_pin(x, y, i))
                    .map(|i| i as i32)
                    .unwrap_or(-1);
                let row_i = idx.map(|i| i as i32).unwrap_or(-1);
                let new_hover = if chrome != -1 { chrome } else if tab != -1 { tab } else { row_i };

                if row_i != p.hover && pin_hover == -1 {
                    p.hover = new_hover;
                    p.hover_pin = pin_hover;
                    p.hide_card();
                    if let Some(i) = idx {
                        p.schedule_card(i);
                    }
                    p.invalidate();
                } else if new_hover != p.hover || pin_hover != p.hover_pin {
                    p.hover = new_hover;
                    p.hover_pin = pin_hover;
                    p.invalidate();
                }
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            return LRESULT(0);
        }
        0x02A3 => { // WM_MOUSELEAVE
            if let Some(p) = panel_of(hwnd) {
                p.hover = -1;
                p.hover_pin = -1;
                p.hide_card();
                p.invalidate();
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            if let Some(p) = panel_of(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as f32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
                p.hide_card();
                // EDIT 被收紧到字高之后，搜索带上下还有一点空隙；
                // 点在这个带里也应该直接开始输入，而不是落空。
                if y >= p.dip(SEARCH_Y)
                    && y <= p.dip(SEARCH_Y + SEARCH_H)
                    && x >= p.dip(PAD)
                    && x <= p.w as f32 - p.dip(PAD)
                {
                    let _ = SetFocus(p.edit);
                    SendMessageW(p.edit, 0x00B1 /* EM_SETSEL */, WPARAM(0), LPARAM(-1));
                    return LRESULT(0);
                }
                p.click(x, y);
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            if let Some(p) = panel_of(hwnd) {
                let x = (l.0 & 0xFFFF) as i16 as f32;
                let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
                p.hide_card();
                if let Some(idx) = p.row_at(x, y) {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    p.context_menu(pt.x, pt.y, idx);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            if let Some(p) = panel_of(hwnd) {
                let delta = ((w.0 >> 16) & 0xFFFF) as i16 as i32;
                let step = if delta > 0 { -1 } else { 1 };
                let max_scroll = p.entries.len().saturating_sub(p.rows_visible());
                let next = (p.scroll as i32 + step).clamp(0, max_scroll as i32) as usize;
                if next != p.scroll {
                    p.scroll = next;
                    p.hide_card();
                    p.invalidate();
                }
            }
            return LRESULT(0);
        }
        0x0100 => {
            if let Some(p) = panel_of(hwnd) {
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                let alt = GetKeyState(VK_MENU.0 as i32) < 0;
                let vk = w.0 as u16;
                if p.on_key(vk, ctrl, shift, alt) {
                    return LRESULT(0);
                }
            }
        }
        WM_TIMER => {
            if let Some(p) = panel_of(hwnd) {
                if w.0 == 2 {
                    let _ = KillTimer(hwnd, 2);
                    let pending = p.pending_card;
                    if pending >= 0 && pending as usize == p.sel || pending >= 0 && p.hover == pending {
                        p.show_card(pending as usize);
                    }
                    return LRESULT(0);
                }
                p.toast = p.toast.clone().filter(|(_, at)| crate::model::now_ms() - at < 1500);
                p.invalidate();
                if p.toast.is_some() {
                    // 还没过期：继续滴答，到点后再把定时器停掉。
                    let _ = SetTimer(hwnd, 1, 300, None);
                } else {
                    let _ = KillTimer(hwnd, 1);
                }
            }
            return LRESULT(0);
        }
        WM_ACTIVATE => {
            let active = (w.0 & 0xFFFF) != 0;
            if let Some(p) = panel_of(hwnd) {
                if !active && !p.pinned && !p.in_dialog && crate::model::now_ms() - p.shown_at > 250 {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    p.hide_card();
                }
            }
        }
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some(p) = panel_of(hwnd) {
                p.hide_card();
            }
            return LRESULT(0);
        }
        zx_ui::window::WM_APP_RELOAD => {
            if let Some(p) = panel_of(hwnd) {
                p.sort = Sort::from_name(&crate::cfg().sort);
                p.pinned = crate::cfg().window_pinned;
                p.reload();
                p.draw();
            }
            crate::hotkeys::reload();
            return LRESULT(0);
        }
        WM_REFRESH => {
            if let Some(p) = panel_of(hwnd) {
                let keep = p.sel;
                p.reload();
                p.sel = keep.min(p.entries.len().saturating_sub(1));
                p.draw();
            }
            return LRESULT(0);
        }
        WM_TOAST => {
            let ptr = w.0 as *mut String;
            if !ptr.is_null() {
                let msg = unsafe { *Box::from_raw(ptr) };
                if let Some(p) = panel_of(hwnd) {
                    p.toast(&msg);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

// ---------------------------------------------------------------- 搜索框子类化

/// 搜索框原来的窗口过程（子类化前）。
static EDIT_OLD_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

unsafe fn edit_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let old = EDIT_OLD_PROC.load(std::sync::atomic::Ordering::SeqCst);
    let old_proc: WNDPROC = std::mem::transmute(old);
    match msg {
        WM_KEYDOWN => {
            let vk = w.0 as u16;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            let alt = GetKeyState(VK_MENU.0 as i32) < 0;
            let passthrough = ctrl && matches!(vk, 0x41 | 0x43 | 0x56 | 0x58 | 0x5A);
            if !passthrough {
                let handled = PANEL.with(|p| {
                    let ptr = *p.borrow();
                    if ptr.is_null() {
                        return false;
                    }
                    (*ptr).on_key(vk, ctrl, shift, alt)
                });
                if handled {
                    return LRESULT(0);
                }
            }
        }
        WM_CHAR => {
            // Ctrl+K / Ctrl+N 这类快捷键会经 TranslateMessage 再生成一个 WM_CHAR
            // （Ctrl+K → 0x0B），它既不是正文，也会把占位提示误清掉，必须丢掉。
            // 退格 / Tab / 回车 / Esc 这些控制键要放行。
            let ch = w.0 as u32;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let ctrl_letter = ctrl
                && ((0x01..=0x1A).contains(&ch)
                    || (0x41..=0x5A).contains(&ch)
                    || (0x61..=0x7A).contains(&ch))
                && !matches!(ch, 0x08 | 0x09 | 0x0A | 0x0D);
            if ctrl_letter {
                return LRESULT(0);
            }
            PANEL.with(|p| {
                let ptr = *p.borrow();
                if !ptr.is_null() && (*ptr).placeholder {
                    (*ptr).set_placeholder(false);
                }
            });
            let r = CallWindowProcW(old_proc, hwnd, msg, w, l);
            PANEL.with(|p| {
                let ptr = *p.borrow();
                if !ptr.is_null() {
                    let p = &mut *ptr;
                    let len = GetWindowTextLengthW(p.edit);
                    let mut buf = vec![0u16; len as usize + 1];
                    let n = GetWindowTextW(p.edit, &mut buf);
                    let text = String::from_utf16_lossy(&buf[..n as usize]);
                    if text != p.text {
                        p.text = text;
                        p.sel = 0;
                        p.scroll = 0;
                        p.reload();
                        p.draw();
                    }
                }
            });
            return r;
        }
        WM_SETFOCUS => {
            let r = CallWindowProcW(old_proc, hwnd, msg, w, l);
            // 默认会全选；占位提示不该以选中态出现。
            let ph = PANEL.with(|p| {
                let ptr = *p.borrow();
                !ptr.is_null() && (*ptr).placeholder
            });
            if ph {
                SendMessageW(hwnd, 0x00B1, WPARAM(0), LPARAM(0));
            }
            return r;
        }
        WM_KILLFOCUS => {
            let r = CallWindowProcW(old_proc, hwnd, msg, w, l);
            // 弹对话框、点别处之后如果框是空的，把提示放回来。
            PANEL.with(|p| {
                let ptr = *p.borrow();
                if !ptr.is_null() {
                    let p = &mut *ptr;
                    let len = GetWindowTextLengthW(hwnd);
                    if !p.placeholder && len == 0 {
                        p.set_placeholder(true);
                    }
                }
            });
            return r;
        }
        WM_LBUTTONDOWN => {
            // 点占位提示 = 开始输入：先把提示清掉，再让默认处理放插入符。
            let ph = PANEL.with(|p| {
                let ptr = *p.borrow();
                !ptr.is_null() && (*ptr).placeholder
            });
            if ph {
                PANEL.with(|p| {
                    let ptr = *p.borrow();
                    if !ptr.is_null() {
                        (*ptr).set_placeholder(false);
                    }
                });
            }
        }
        WM_MOUSEWHEEL => {
            let parent = GetParent(hwnd).unwrap_or_default();
            let _ = SendMessageW(parent, msg, w, l);
            return LRESULT(0);
        }
        _ => {}
    }
    CallWindowProcW(old_proc, hwnd, msg, w, l)
}

/// 子类化（在 show 里创建 EDIT 之后调用）。
pub fn subclass_edit(edit: HWND) {
    unsafe {
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as usize as isize);
        EDIT_OLD_PROC.store(old, std::sync::atomic::Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------- 表单对话框
//
// 原先的 prompt() 只能收一个字段；密钥要「标题 + 描述」两个字段，所以统一成
// form()：一行一个标签 + 输入框，回车确认（多行框用 Ctrl+Enter），Esc 取消。
// prompt() 只是 form() 的单字段包装，行为保持不变。

struct Form {
    edits: Vec<HWND>,
    value: Option<Vec<String>>,
    done: bool,
}

thread_local! {
    static FORM: RefCell<Option<Box<Form>>> = const { RefCell::new(None) };
}

fn prompt(title: &str, hint: &str, initial: &str, multiline: bool) -> Option<String> {
    let fields = [("", initial, multiline)];
    form(title, hint, &fields).and_then(|mut v| if v.is_empty() { None } else { Some(v.remove(0)) })
}

fn form(title: &str, hint: &str, fields: &[(&str, &str, bool)]) -> Option<Vec<String>> {
    let panel_ptr = PANEL.with(|p| *p.borrow());
    if !panel_ptr.is_null() {
        unsafe { (*panel_ptr).in_dialog = true; }
    }
    let out = form_inner(title, hint, fields);
    if !panel_ptr.is_null() {
        unsafe { (*panel_ptr).in_dialog = false; }
    }
    out
}

fn form_inner(title: &str, hint: &str, fields: &[(&str, &str, bool)]) -> Option<Vec<String>> {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        static NAME: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
        let name = NAME.get_or_init(|| wide(PROMPT_CLASS));
        static CLASS_DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        CLASS_DONE.get_or_init(|| {
            let brush = CreateSolidBrush(COLORREF(0x00F3F3F3));
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(form_proc),
                hInstance: HINSTANCE(hinst.0),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: brush,
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            if RegisterClassExW(&wc) != 0 {
                // 类画刷由系统持有，进程内用一次，不释放。
            } else {
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
        });

        let dpi = GetDpiForSystem().max(96);
        let k = dpi as f32 / 96.0;
        let px = |v: f32| (v * k).round() as i32;
        let margin = px(18.0);
        let client_w = px(460.0);

        let name_font = wide("Microsoft YaHei UI");
        let font = CreateFontW(
            -((10 * dpi as i32) / 72), 0, 0, 0, 400, 0, 0, 0,
            DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32, PCWSTR(name_font.as_ptr()),
        );
        let font_small = CreateFontW(
            -((9 * dpi as i32) / 72), 0, 0, 0, 400, 0, 0, 0,
            DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32, PCWSTR(name_font.as_ptr()),
        );
        // 输入框高度跟着字高走，文字才不会贴在框顶（和搜索框同一个道理）。
        let edit_h = font_text_height(font) + px(10.0);

        // 客户区布局：提示行 → 每个字段（标签 + 输入框）→ 按钮行
        let mut y = px(16.0);
        let hint_y = if hint.is_empty() { 0 } else { y };
        if !hint.is_empty() {
            y += px(20.0);
        }
        let mut layout: Vec<(&str, i32, i32, i32, bool)> = Vec::new();
        for (label, _, multiline) in fields {
            let label_y = y;
            if !label.is_empty() {
                y += px(18.0);
            }
            let eh = if *multiline { px(120.0) } else { edit_h };
            layout.push((*label, label_y, y, eh, *multiline));
            y += eh + px(12.0);
        }
        let btn_w = px(88.0);
        let btn_h = px(30.0);
        let client_h = y + px(14.0) + btn_h + px(16.0);

        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex_style = WS_EX_TOPMOST | WS_EX_DLGMODALFRAME;
        let Ok(hwnd) = CreateWindowExW(
            ex_style,
            PCWSTR(name.as_ptr()),
            PCWSTR(wide(title).as_ptr()),
            style,
            CW_USEDEFAULT, CW_USEDEFAULT, client_w, client_h,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(hinst.0),
            None,
        ) else {
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteObject(HGDIOBJ(font_small.0));
            return None;
        };
        // 建立后按客户区反推外框：不同 DPI / 主题的标题栏高度都不一样，
        // 硬编码会让按钮压到客户区外。
        let mut cr = RECT::default();
        let _ = GetClientRect(hwnd, &mut cr);
        let ww = client_w + (client_w - (cr.right - cr.left)).max(0);
        let wh = client_h + (client_h - (cr.bottom - cr.top)).max(0);

        if !hint.is_empty() {
            let hint_w = wide(hint);
            let hw = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("STATIC"),
                PCWSTR(hint_w.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0080), // SS_NOPREFIX
                margin, hint_y, client_w - margin * 2, px(18.0),
                hwnd,
                HMENU::default(),
                HINSTANCE::default(),
                None,
            ).unwrap_or_default();
            SendMessageW(hw, WM_SETFONT, WPARAM(font_small.0 as usize), LPARAM(1));
        }

        let mut edits = Vec::new();
        for (label, label_y, ey, eh, multiline) in &layout {
            if !label.is_empty() {
                let lw = wide(label);
                let h = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    PCWSTR(lw.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0080),
                    margin, *label_y, client_w - margin * 2, px(16.0),
                    hwnd,
                    HMENU::default(),
                    HINSTANCE::default(),
                    None,
                ).unwrap_or_default();
                SendMessageW(h, WM_SETFONT, WPARAM(font_small.0 as usize), LPARAM(1));
            }
            let index = edits.len();
            let field = fields.get(index).map(|f| f.1).unwrap_or("");
            let value = wide(field);
            let mut st = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
            if *multiline {
                st = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_MULTILINE as u32)
                    | WINDOW_STYLE(ES_AUTOVSCROLL as u32) | WS_VSCROLL;
            }
            let edit = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                windows::core::w!("EDIT"),
                PCWSTR(value.as_ptr()),
                st,
                margin, *ey, client_w - margin * 2, *eh,
                hwnd,
                // 注意：不能复用 101/102/103，否则第 2 个输入框的
                // EN_SETFOCUS（WM_COMMAND id=102）会被当成「点了确定」。
                HMENU((1000 + index as i32) as isize as *mut core::ffi::c_void),
                HINSTANCE::default(),
                None,
            ).unwrap_or_default();
            SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            subclass_form_edit(edit);
            edits.push(edit);
        }

        let by = client_h - px(16.0) - btn_h;
        let cancel = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("BUTTON"),
            PCWSTR(wide("取消").as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            client_w - margin - btn_w, by, btn_w, btn_h,
            hwnd,
            HMENU(ID_CANCEL as isize as *mut core::ffi::c_void),
            HINSTANCE::default(),
            None,
        ).unwrap_or_default();
        let ok = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("BUTTON"),
            PCWSTR(wide("确定").as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            client_w - margin - btn_w * 2 - px(10.0), by, btn_w, btn_h,
            hwnd,
            HMENU(ID_OK as isize as *mut core::ffi::c_void),
            HINSTANCE::default(),
            None,
        ).unwrap_or_default();
        for b in [ok, cancel] {
            SendMessageW(b, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }

        let mut f = Box::new(Form {
            edits,
            value: None,
            done: false,
        });
        let ptr = &mut *f as *mut Form;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        FORM.with(|s| *s.borrow_mut() = Some(f));

        let (x, y) = {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            (pt.x - ww / 2, pt.y - wh / 2)
        };
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, ww, wh, SWP_SHOWWINDOW);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let first = FORM.with(|s| s.borrow().as_ref().and_then(|f| f.edits.first().copied()));
        if let Some(first) = first {
            let _ = SetFocus(first);
            SendMessageW(first, 0x00B1, WPARAM(0), LPARAM(-1));
        }

        let mut msg = MSG::default();
        while !FORM.with(|s| s.borrow().as_ref().map(|p| p.done).unwrap_or(true)) {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // 先销毁窗口（form_proc 里还挂着 GWLP_USERDATA 指针），再释放 Form 与字体。
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(font_small.0));
        FORM.with(|s| s.borrow_mut().take().and_then(|p| p.value))
    }
}

unsafe extern "system" fn form_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Form;
    match msg {
        WM_COMMAND => {
            let id = (w.0 & 0xFFFF) as i32;
            let code = ((w.0 >> 16) & 0xFFFF) as u32;
            // 只处理按钮的 BN_CLICKED；输入框的 EN_SETFOCUS / EN_CHANGE 也会
            // 走 WM_COMMAND，不能当成「确定/取消」。
            if code == 0 && (id == ID_OK || id == ID_CANCEL) && !ptr.is_null() {
                let f = &mut *ptr;
                if id == ID_OK {
                    let mut vals = Vec::new();
                    for &e in &f.edits {
                        let len = GetWindowTextLengthW(e);
                        let mut buf = vec![0u16; len as usize + 1];
                        let n = GetWindowTextW(e, &mut buf);
                        vals.push(String::from_utf16_lossy(&buf[..n as usize]));
                    }
                    f.value = Some(vals);
                }
                f.done = true;
            }
            return LRESULT(0);
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(w.0 as *mut core::ffi::c_void);
            SetTextColor(hdc, COLORREF(0x006B6B6B));
            SetBkMode(hdc, TRANSPARENT);
            static BRUSH: std::sync::OnceLock<isize> = std::sync::OnceLock::new();
            let b = *BRUSH.get_or_init(|| CreateSolidBrush(COLORREF(0x00F3F3F3)).0 as isize);
            return LRESULT(b);
        }
        WM_CLOSE => {
            if !ptr.is_null() {
                (*ptr).done = true;
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if w.0 as u16 == 0x1B => {
            if !ptr.is_null() {
                (*ptr).done = true;
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

/// 表单里的输入框：回车 = 确定（多行框要 Ctrl+Enter），Esc = 取消。
static FORM_EDIT_OLD: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

unsafe extern "system" fn form_edit_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let old = FORM_EDIT_OLD.load(std::sync::atomic::Ordering::SeqCst);
    if old == 0 {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let old_proc: WNDPROC = std::mem::transmute(old);
    if msg == WM_CHAR {
        let ch = w.0 as u32;
        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
        let ctrl_letter = ctrl
            && ((0x01..=0x1A).contains(&ch)
                || (0x41..=0x5A).contains(&ch)
                || (0x61..=0x7A).contains(&ch))
            && !matches!(ch, 0x08 | 0x09 | 0x0A | 0x0D);
        if ctrl_letter {
            // 快捷键（Ctrl+K/N…）抬手时补发的 WM_CHAR：别写进标题/描述。
            return LRESULT(0);
        }
    }
    if msg == WM_KEYDOWN {
        let vk = w.0 as u16;
        if vk == 0x0D {
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let multiline = (style & (ES_MULTILINE as u32)) != 0;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            if !multiline || ctrl {
                let parent = GetParent(hwnd).unwrap_or_default();
                let _ = SendMessageW(parent, WM_COMMAND, WPARAM(ID_OK as usize), LPARAM(0));
                return LRESULT(0);
            }
        } else if vk == 0x1B {
            let parent = GetParent(hwnd).unwrap_or_default();
            let _ = SendMessageW(parent, WM_COMMAND, WPARAM(ID_CANCEL as usize), LPARAM(0));
            return LRESULT(0);
        }
    }
    CallWindowProcW(old_proc, hwnd, msg, w, l)
}

fn subclass_form_edit(edit: HWND) {
    unsafe {
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, form_edit_proc as *const () as usize as isize);
        if FORM_EDIT_OLD.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            FORM_EDIT_OLD.store(old, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
