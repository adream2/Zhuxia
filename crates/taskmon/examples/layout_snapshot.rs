//! 布局引擎的行为快照。
//!
//! 重写 `taskmon/layout.rs` 前后各跑一次，逐字节对比：布局是纯几何，
//! 差一个像素就会让任务栏文字错位，所以必须用全矩阵输入而不是抽查几个。
//!
//! ```text
//! cargo run -q -p taskmon --example layout_snapshot > target/tmp/layout-old.txt
//! ```

use taskmon::layout::{self, Layout, PACKED};

fn dump(tag: &str, l: &Layout) {
    let mut line = format!(
        "{tag}|v={} two={} w={:.3} h={:.3} lefts={:?} mask={:#07b} gpu_both={} compact={} cols={} row_h={:.3} col_rows={:?} temp={} net=({},{})",
        l.vertical as u8,
        l.two_line as u8,
        l.width,
        l.height,
        l.lefts,
        l.mask,
        l.gpu_both as u8,
        l.compact as u8,
        l.columns,
        l.row_h,
        l.col_rows,
        l.temp_pos,
        l.net_pos,
        l.net_col
    );
    line.push_str(&format!(" net_row={} pos_slot={:?}", l.net_row, l.pos_slot));
    line.push_str(&format!(" pos_col={:?} pos_row={:?}", l.pos_col, l.pos_row));
    println!("{line}");

    // 派生几何：命中矩形与分组宽度
    let rects: Vec<String> = (0..5)
        .map(|slot| match l.slot_rect(slot, 0.0, 12.0, 24.0) {
            Some((a, b, c, d)) => format!("{a:.2},{b:.2},{c:.2},{d:.2}"),
            None => "-".to_string(),
        })
        .collect();
    println!("{tag}|rects={rects:?}");

    let mut derived = String::new();
    for g in 0..5 {
        derived.push_str(&format!(
            " g{g}[vis={} left={:.3} w={:.3}]",
            l.group_visible(g) as u8,
            l.group_left(g),
            l.group_w(g)
        ));
    }
    for c in -1..5 {
        derived.push_str(&format!(" c{c}[{:.3}/{:.3}]", l.column_left(c), l.column_width(c)));
    }
    for p in -1..(PACKED as i32 + 1) {
        let pos = match l.pos_rect(p) {
            Some((a, b, c, d)) => format!("{a:.2},{b:.2},{c:.2},{d:.2}"),
            None => "-".to_string(),
        };
        let at = match l.pos_rect_at(p, 0.0, 12.0, 24.0) {
            Some((a, b, c, d)) => format!("{a:.2},{b:.2},{c:.2},{d:.2}"),
            None => "-".to_string(),
        };
        derived.push_str(&format!(
            " p{p}[slot={} h={:.2} top={:.2} r={pos} at={at}]",
            l.slot_at(p),
            l.strip_pos_h(p),
            l.strip_pos_top(p)
        ));
    }
    for s in 0..5 {
        derived.push_str(&format!(" pos_of{s}={}", l.position_of(s)));
    }
    println!("{tag}|{derived}");
}

fn main() {
    // 顺序：默认、反序、含重复与越界
    let orders: [[usize; 5]; 3] = [[0, 1, 2, 3, 4], [4, 3, 2, 1, 0], [2, 2, 9, 0, 1]];

    for mask in 0u32..32 {
        for gpu in 0u8..3 {
            for vertical in [false, true] {
                for compact in [false, true] {
                    for avail in [0.0f32, 44.0, 71.0, 72.0, 140.0, 300.0] {
                        for (oi, order) in orders.iter().enumerate() {
                            let l = layout::compute(mask, gpu, vertical, avail, compact, order);
                            dump(
                                &format!(
                                    "m{mask:02} g{gpu} v{} c{} a{avail:.0} o{oi}",
                                    vertical as u8, compact as u8
                                ),
                                &l,
                            );
                        }
                    }
                }
            }
        }
    }

    for band in [0, 1, 2, 30, 48, 80] {
        for dpi in [96u32, 120, 144, 192] {
            println!(
                "avail/{band}/{dpi}\t{:.4}",
                layout::vertical_avail_width(band, dpi)
            );
        }
    }

    for order in orders {
        println!("norm/{order:?}\t{:?}", layout::normalize_order(&order));
    }

    // 常量本身也是契约
    println!(
        "consts\t{:?} {:?} {} {} {} {} {} {} {}",
        layout::GROUP_W,
        layout::COMPACT_W,
        layout::STRIP_H,
        layout::GRID_ROW_H,
        layout::STRIP_TWO_LINE_MIN_W,
        layout::STRIP_BLOCK_H,
        layout::STRIP_BLOCK_PAD,
        layout::STRIP_LINE_GAP,
        layout::PACKED
    );
    for g in 0..5 {
        for compact in [false, true] {
            println!(
                "consts/g{g}/{compact}\t{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                layout::group_w(g, compact),
                layout::grid_row_h(compact),
                layout::strip_block_h(compact),
                layout::strip_block_pad(compact),
                layout::row_inset(compact),
                layout::gpu_both_strip_block_h(compact)
            );
            println!("consts/net/{compact}\t{:.2}", layout::net_strip_block_h(compact));
        }
    }
}
