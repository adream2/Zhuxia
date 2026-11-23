//! GIF 编码：脏矩形 + 中位切分 + 边录边写。
//!
//! 三点决策，理由都在 `docs/RECORD.md` §六：
//!
//! 1. **边录边写**，不缓存全部帧再统一编码。容器（`gif` crate）是流式的，脏矩形只需要
//!    "上一帧"，帧延迟只需要"上一节拍"。所以内存恒定在两帧，收尾几乎零等待。
//!    （带编辑器的录制器需要缓存帧；本项目没有编辑器，不需要为它付内存。）
//! 2. **脏矩形**是体积的第一杠杆：静止画面几乎不占体积，打字演示 90% 的帧只有光标在动。
//! 3. **中位切分**而不是 NeuQuant：屏幕内容是"少数主色 + 大量抗锯齿近似色"，
//!    按颜色分布切箱比自组织网络保真得多。
//!
//! 帧延迟用"厘秒整数 + 余数进位"，10/20/25fps 下零漂移（100 能被整除）。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::source::{FrameBuf, Rect};

/// 已写字节数（给控制条显示体积用）。
#[derive(Clone, Default)]
pub struct ByteCounter(Arc<AtomicU64>);

impl ByteCounter {
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

struct Counting<W: Write> {
    inner: W,
    count: Arc<AtomicU64>,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// 一帧"待写"的内容：帧 + 它应该显示多少节拍。
struct Pending {
    frame: FrameBuf,
    ticks: u32,
}

pub struct GifWriter {
    /// 先写临时文件，成功后 rename，避免留半截 GIF。
    tmp: PathBuf,
    dst: PathBuf,
    w: i32,
    h: i32,
    enc: Option<gif::Encoder<Counting<BufWriter<File>>>>,
    /// 最后一次真正写进 GIF 的帧（脏矩形与它比）。
    written: Option<FrameBuf>,
    /// 上一节拍看到的帧（还没写出去）。
    seen: Option<Pending>,
    fps: i32,
    /// 延迟的厘秒余数（`ticks * 100 / fps` 的余数带进位，零漂移）。
    carry: u64,
    /// 一共 push 了多少节拍（帧数统计）。
    pub ticks: u32,
    /// 真正写进 GIF 的帧数。
    pub frames: u32,
    pub bytes: ByteCounter,
    /// 复用的大块 scratch，避免每帧分配。
    scratch: Scratch,
}

/// 直方图与映射缓存（每帧复用）。
struct Scratch {
    /// 5:5:5 直方图：像素计数
    cnt: Vec<u32>,
    /// 5:5:5 直方图：各通道累加（用于算 bin 的平均色）
    sum: Vec<[u32; 3]>,
    /// 颜色 → 索引，0xFF = 未算
    map: Vec<u8>,
}

const BINS: usize = 32 * 32 * 32;

impl Scratch {
    fn new() -> Self {
        Scratch { cnt: vec![0; BINS], sum: vec![[0; 3]; BINS], map: vec![0xFF; BINS] }
    }

    fn reset(&mut self) {
        self.cnt.fill(0);
        self.sum.fill([0, 0, 0]);
        self.map.fill(0xFF);
    }
}

impl GifWriter {
    /// 建一个 GIF 输出。`w × h` 是输出尺寸，`fps` 决定帧延迟。
    pub fn create(path: &Path, w: i32, h: i32, fps: i32) -> std::io::Result<Self> {
        let w = w.clamp(2, 65535);
        let h = h.clamp(2, 65535);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("gif.part");
        let file = File::create(&tmp)?;
        let counter = ByteCounter::default();
        let sink = Counting { inner: BufWriter::with_capacity(64 * 1024, file), count: counter.0.clone() };
        // 不写全局表：每帧带自己的局部表（不同内容各用各的 256 色，互不挤占）。
        let mut enc = gif::Encoder::new(sink, w as u16, h as u16, &[])
            .map_err(std::io::Error::other)?;
        enc.set_repeat(gif::Repeat::Infinite)
            .map_err(std::io::Error::other)?;
        Ok(GifWriter {
            tmp,
            dst: path.to_path_buf(),
            w,
            h,
            enc: Some(enc),
            written: None,
            seen: None,
            fps: fps.clamp(5, 25),
            carry: 0,
            ticks: 0,
            frames: 0,
            bytes: counter,
            scratch: Scratch::new(),
        })
    }

    /// 推进一个节拍。内容没变就不写帧，只把时长累加到上一帧。
    pub fn push(&mut self, frame: &FrameBuf) -> std::io::Result<()> {
        self.ticks = self.ticks.saturating_add(1);
        if let Some(seen) = self.seen.as_mut() {
            if same_frame(&seen.frame, frame) {
                seen.ticks = seen.ticks.saturating_add(1);
                return Ok(());
            }
            // 内容变了：`seen` 的显示到此结束，把它写出去（带它实际保持的时长）
            let pending = self.seen.take().expect("seen");
            self.write_pending(pending)?;
        }
        self.seen = Some(Pending { frame: frame.clone(), ticks: 1 });
        Ok(())
    }

    /// 收尾：把最后一帧写出去，关闭文件，rename 到目标路径。返回最终路径。
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        if let Some(pending) = self.seen.take() {
            self.write_pending(pending)?;
        }
        // 空录制（一帧都没有）：补一帧底色，至少产出可打开的文件
        if self.written.is_none() {
            let blank = {
                let mut b = FrameBuf::new(self.w, self.h);
                b.clear(0xFF1A1A1A);
                b
            };
            let _ = self.write_frame(&blank, Rect::new(0, 0, self.w, self.h), 10);
        }
        if let Some(enc) = self.enc.take() {
            // into_inner 写 trailer（含循环标记）并把 sink 还回来
            let mut sink = enc
                .into_inner()
                .map_err(std::io::Error::other)?;
            sink.flush()?;
        }
        std::fs::rename(&self.tmp, &self.dst)?;
        Ok(self.dst)
    }

    /// 丢弃：删掉临时文件，不留残渣。
    pub fn abort(mut self) {
        self.enc.take();
        let _ = std::fs::remove_file(&self.tmp);
    }

    fn write_pending(&mut self, pending: Pending) -> std::io::Result<()> {
        let dirty = match self.written.as_ref() {
            None => Rect::new(0, 0, self.w, self.h),
            Some(prev) => match dirty_rect(prev, &pending.frame) {
                Some(r) => r,
                None => return Ok(()), // 完全一样（不该发生，双保险）
            },
        };
        self.write_frame(&pending.frame, dirty, pending.ticks)?;
        self.written = Some(pending.frame);
        Ok(())
    }

    fn write_frame(&mut self, frame: &FrameBuf, dirty: Rect, ticks: u32) -> std::io::Result<()> {
        // 厘秒：ticks 个节拍 * (100/fps)，余数进下一帧
        let num = ticks as u64 * 100 + self.carry;
        let mut cs = num / self.fps as u64;
        self.carry = num % self.fps as u64;
        if cs == 0 {
            cs = 1;
        }
        cs = cs.min(6553); // u16 上限 65535，1/100s 的单位换算后不会溢出，这里只是保险

        let (palette, indices) = quantize(frame, dirty, &mut self.scratch);
        let f = gif::Frame {
            left: dirty.x.max(0) as u16,
            top: dirty.y.max(0) as u16,
            width: dirty.w as u16,
            height: dirty.h as u16,
            delay: cs as u16,
            dispose: gif::DisposalMethod::Keep,
            palette: Some(palette),
            buffer: std::borrow::Cow::Owned(indices),
            ..Default::default()
        };
        if let Some(enc) = self.enc.as_mut() {
            enc.write_frame(&f).map_err(std::io::Error::other)?;
        }
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }
}

/// 两帧是否完全一致（行比较走 memcmp，很快）。
pub fn same_frame(a: &FrameBuf, b: &FrameBuf) -> bool {
    if a.w != b.w || a.h != b.h {
        return false;
    }
    a.buf == b.buf
}

/// 变化像素的包围盒。先按行比（memcmp），再只扫"变了的行"。
pub fn dirty_rect(prev: &FrameBuf, cur: &FrameBuf) -> Option<Rect> {
    if prev.w != cur.w || prev.h != cur.h {
        return Some(Rect::new(0, 0, cur.w, cur.h));
    }
    let h = cur.h;
    let mut top = -1i32;
    let mut bottom = 0i32;
    for y in 0..h {
        if cur.row(y as usize) != prev.row(y as usize) {
            if top < 0 {
                top = y;
            }
            bottom = y;
        }
    }
    if top < 0 {
        return None;
    }
    let (mut left, mut right) = (cur.w, -1i32);
    for y in top..=bottom {
        let a = prev.row(y as usize);
        let b = cur.row(y as usize);
        // 从左侧找第一个不同的像素
        let mut x = 0i32;
        while x < cur.w {
            let o = x as usize * 4;
            if a[o..o + 4] != b[o..o + 4] {
                left = left.min(x);
                break;
            }
            x += 1;
        }
        // 从右侧找最后一个不同的像素
        let mut x = cur.w - 1;
        while x >= 0 {
            let o = x as usize * 4;
            if a[o..o + 4] != b[o..o + 4] {
                right = right.max(x);
                break;
            }
            x -= 1;
        }
    }
    if right < left {
        return None;
    }
    Some(Rect::new(left, top, right - left + 1, bottom - top + 1))
}

// ---------------------------------------------------------------- 量化

/// 中位切分 + 索引映射。返回 (palette: Vec<u8> RGB 三字节组, indices: 逐像素索引)。
fn quantize(frame: &FrameBuf, r: Rect, scratch: &mut Scratch) -> (Vec<u8>, Vec<u8>) {
    scratch.reset();
    let r = Rect::new(
        r.x.max(0),
        r.y.max(0),
        r.w.min(frame.w - r.x.max(0)),
        r.h.min(frame.h - r.y.max(0)),
    );
    let npix = (r.w * r.h).max(1) as u32;
    // 采样步长：≥4 万像素时每隔几个取一个（调色板统计不需要每个像素）
    let step = ((npix / 40_000).max(1)) as i32;

    let mut y = 0;
    while y < r.h {
        let row = frame.row((r.y + y) as usize);
        let mut x = 0;
        while x < r.w {
            let o = (r.x + x) as usize * 4;
            let (b, g, rd) = (row[o] as u32, row[o + 1] as u32, row[o + 2] as u32);
            let idx = (((rd >> 3) << 10) | ((g >> 3) << 5) | (b >> 3)) as usize;
            scratch.cnt[idx] = scratch.cnt[idx].saturating_add(1);
            let s = &mut scratch.sum[idx];
            s[0] = s[0].saturating_add(rd);
            s[1] = s[1].saturating_add(g);
            s[2] = s[2].saturating_add(b);
            x += step;
        }
        y += step;
    }

    // 非空 bin → (r,g,b,count)
    let mut items: Vec<Item> = Vec::new();
    for i in 0..BINS {
        let c = scratch.cnt[i];
        if c == 0 {
            continue;
        }
        let s = scratch.sum[i];
        items.push(Item {
            r: (s[0] / c) as u8,
            g: (s[1] / c) as u8,
            b: (s[2] / c) as u8,
            n: c,
        });
    }
    if items.is_empty() {
        items.push(Item { r: 0, g: 0, b: 0, n: 1 });
    }

    let palette = median_cut(items, 256);
    // 调色板长度必须是 3 * 2^n
    let n = palette.len().max(2).next_power_of_two().min(256);
    let mut pal_bytes = Vec::with_capacity(n * 3);
    for c in &palette {
        pal_bytes.extend_from_slice(c);
    }
    while pal_bytes.len() < n * 3 {
        pal_bytes.extend_from_slice(&[0, 0, 0]);
    }

    // 每个出现的 bin 预先把索引算好（比逐像素找最近色快一个量级）
    for i in 0..BINS {
        if scratch.cnt[i] == 0 {
            continue;
        }
        let s = scratch.sum[i];
        let c = scratch.cnt[i];
        let (r8, g8, b8) = ((s[0] / c) as u8, (s[1] / c) as u8, (s[2] / c) as u8);
        scratch.map[i] = nearest(&palette, r8, g8, b8);
    }

    let mut indices = vec![0u8; (r.w * r.h) as usize];
    for y in 0..r.h {
        let row = frame.row((r.y + y) as usize);
        for x in 0..r.w {
            let o = (r.x + x) as usize * 4;
            let (b, g, rd) = (row[o], row[o + 1], row[o + 2]);
            let idx = ((rd as usize >> 3) << 10) | ((g as usize >> 3) << 5) | (b as usize >> 3);
            let mut m = scratch.map[idx];
            if m == 0xFF {
                m = nearest(&palette, rd, g, b);
                scratch.map[idx] = m;
            }
            indices[(y * r.w + x) as usize] = m;
        }
    }
    (pal_bytes, indices)
}

#[derive(Clone, Copy)]
struct Item {
    r: u8,
    g: u8,
    b: u8,
    n: u32,
}

impl Item {
    fn ch(&self, c: usize) -> u8 {
        match c {
            0 => self.r,
            1 => self.g,
            _ => self.b,
        }
    }
}

/// 中位切分：反复把"最该切"的箱沿最宽的通道切成两半。
fn median_cut(items: Vec<Item>, max: usize) -> Vec<[u8; 3]> {
    let mut work = vec![items];
    if work[0].is_empty() {
        return vec![[0, 0, 0]];
    }
    while work.len() < max {
        // 选箱：优先切"像素多 × 跨度大"的那个
        let mut best: Option<(usize, usize, i32)> = None; // (idx, channel, score)
        for (i, b) in work.iter().enumerate() {
            if b.len() < 2 {
                continue;
            }
            let mut range = [0i32; 3];
            for (c, slot) in range.iter_mut().enumerate() {
                let mut lo = 255i32;
                let mut hi = 0i32;
                for it in b {
                    let v = it.ch(c) as i32;
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                *slot = hi - lo;
            }
            let ch = (0..3).max_by_key(|&c| range[c]).unwrap_or(0);
            if range[ch] == 0 {
                continue;
            }
            let weight: u32 = b.iter().map(|it| it.n).sum::<u32>().max(1);
            let score = (range[ch] as f32 * (weight as f32).ln().max(1.0)) as i32;
            if best.map(|(_, _, s)| score > s).unwrap_or(true) {
                best = Some((i, ch, score));
            }
        }
        let Some((i, ch, _)) = best else { break };
        let mut b = work.remove(i);
        b.sort_by_key(|it| it.ch(ch));
        // 按像素数对半分，别把 1 个像素的孤立色单独切出来
        let total: u32 = b.iter().map(|it| it.n).sum();
        let mut acc = 0u32;
        let mut cut = 1usize;
        for (k, it) in b.iter().enumerate() {
            acc = acc.saturating_add(it.n);
            if acc * 2 >= total {
                cut = (k + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let right = b.split_off(cut);
        work.push(b);
        work.push(right);
    }
    work.iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let mut acc = [0u64; 3];
            let mut n = 0u64;
            for it in b {
                let w = it.n as u64;
                acc[0] += it.r as u64 * w;
                acc[1] += it.g as u64 * w;
                acc[2] += it.b as u64 * w;
                n += w;
            }
            let n = n.max(1);
            [(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]
        })
        .collect()
}

/// 调色板里离 `(r,g,b)` 最近的颜色索引。用加权距离（人眼对绿更敏感）。
fn nearest(pal: &[[u8; 3]], r: u8, g: u8, b: u8) -> u8 {
    let mut best = 0usize;
    let mut bestd = i32::MAX;
    for (i, c) in pal.iter().enumerate() {
        let dr = c[0] as i32 - r as i32;
        let dg = c[1] as i32 - g as i32;
        let db = c[2] as i32 - b as i32;
        let d = dr * dr * 3 + dg * dg * 6 + db * db;
        if d < bestd {
            bestd = d;
            best = i;
            if d == 0 {
                break;
            }
        }
    }
    best.min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(w: i32, h: i32, argb: u32) -> FrameBuf {
        let mut b = FrameBuf::new(w, h);
        b.clear(argb);
        b
    }

    #[test]
    fn dirty_rect_finds_changed_block() {
        let a = buf(64, 32, 0xFF202020);
        let mut b = a.clone();
        // 改 (10,5) 一个像素
        let o = 5 * b.stride() + 10 * 4;
        b.buf[o] = 0xFF;
        let r = dirty_rect(&a, &b).expect("dirty");
        assert_eq!((r.x, r.y, r.w, r.h), (10, 5, 1, 1));
    }

    #[test]
    fn dirty_rect_none_when_identical() {
        let a = buf(16, 16, 0xFF334455);
        assert!(dirty_rect(&a, &a.clone()).is_none());
    }

    #[test]
    fn dirty_rect_spans_two_corners() {
        let a = buf(40, 40, 0xFF000000);
        let mut b = a.clone();
        let p = |x: i32, y: i32, b: &mut FrameBuf| {
            let o = y as usize * b.stride() + x as usize * 4;
            b.buf[o] = 0xFF;
        };
        p(3, 2, &mut b);
        p(30, 20, &mut b);
        let r = dirty_rect(&a, &b).expect("dirty");
        assert_eq!((r.x, r.y, r.w, r.h), (3, 2, 28, 19));
    }

    #[test]
    fn quantize_produces_valid_palette() {
        let mut f = FrameBuf::new(64, 64);
        // 四个色块
        for y in 0..64 {
            for x in 0..64 {
                let o = y as usize * f.stride() + x as usize * 4;
                let c: u32 = match (x / 32, y / 32) {
                    (0, 0) => 0xFFFF0000,
                    (1, 0) => 0xFF00FF00,
                    (0, 1) => 0xFF0000FF,
                    _ => 0xFFFFFFFF,
                };
                f.buf[o] = (c & 0xFF) as u8;
                f.buf[o + 1] = ((c >> 8) & 0xFF) as u8;
                f.buf[o + 2] = ((c >> 16) & 0xFF) as u8;
                f.buf[o + 3] = 255;
            }
        }
        let mut s = Scratch::new();
        let (pal, idx) = quantize(&f, Rect::new(0, 0, 64, 64), &mut s);
        assert_eq!(pal.len() % 3, 0);
        assert!(pal.len() >= 6 && pal.len() <= 768);
        assert_eq!(idx.len(), 64 * 64);
        let n = pal.len() / 3;
        assert!(idx.iter().all(|&i| (i as usize) < n));
    }

    #[test]
    fn median_cut_keeps_distinct_colors() {
        let items = vec![
            Item { r: 255, g: 0, b: 0, n: 100 },
            Item { r: 0, g: 255, b: 0, n: 100 },
            Item { r: 0, g: 0, b: 255, n: 100 },
        ];
        let pal = median_cut(items, 16);
        assert_eq!(pal.len(), 3, "{pal:?}");
        assert!(pal.contains(&[255, 0, 0]));
        assert!(pal.contains(&[0, 255, 0]));
        assert!(pal.contains(&[0, 0, 255]));
    }

    #[test]
    fn nearest_exact_match() {
        let pal = vec![[0, 0, 0], [255, 255, 255], [128, 0, 0]];
        assert_eq!(nearest(&pal, 255, 255, 255), 1);
        assert_eq!(nearest(&pal, 2, 2, 2), 0);
        assert_eq!(nearest(&pal, 130, 0, 0), 2);
    }
}
