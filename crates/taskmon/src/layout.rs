//! 任务栏覆盖层的布局引擎。
//!
//! 三种形态：
//! - **横向两行网格**（默认任务栏）：每两格一组，组宽随「是否含网速」变化；
//! - **竖直列网格**（侧边任务栏）：按可用厚度决定 1–4 列；
//! - **窄条带双行块**：宽度不足时改成「标签在上、数值在下」。
//!
//! 绘制、命中测试、详情弹窗的锚点全部由这一份几何推出，所以任何改动都必须
//! 让 `examples/layout_snapshot` 的输出保持不变（见 `REWRITE.md`）。

/// 普通分组宽度：四组里最后一组（网速）更宽。
pub const GROUP_W: [f32; 4] = [70.0, 70.0, 70.0, 84.0];
/// 紧凑模式的同一组宽度。
pub const COMPACT_W: [f32; 4] = [65.0, 65.0, 65.0, 78.0];
/// 单行条带的高度。
pub const STRIP_H: f32 = 16.0;
/// 竖直多列时的行高。
pub const GRID_ROW_H: f32 = 24.0;
/// 低于这个宽度就改成双行块。
pub const STRIP_TWO_LINE_MIN_W: f32 = 72.0;
pub const STRIP_BLOCK_H: f32 = 44.0;
pub const STRIP_BLOCK_PAD: f32 = 4.0;
pub const STRIP_LINE_GAP: f32 = 1.0;
/// 一个格子里最多放几个「位置」（8 = 4 组 × 2 行）。
pub const PACKED: usize = 8;

pub fn group_w(g: usize, compact: bool) -> f32 {
    let table = if compact { COMPACT_W } else { GROUP_W };
    table[g.min(3)]
}

pub fn grid_row_h(compact: bool) -> f32 {
    if compact { 18.0 } else { GRID_ROW_H }
}

pub fn strip_block_h(compact: bool) -> f32 {
    if compact { 40.0 } else { STRIP_BLOCK_H }
}

pub fn strip_block_pad(compact: bool) -> f32 {
    if compact { 2.0 } else { STRIP_BLOCK_PAD }
}

/// 双行块内部左右留白。
pub fn row_inset(compact: bool) -> f32 {
    if compact { 3.0 } else { 5.0 }
}

/// 显存占用两行（读写分开显示）时块更高。
pub fn gpu_both_strip_block_h(compact: bool) -> f32 {
    if compact { 56.0 } else { 62.5 }
}

/// 网速块更高：上下行各一行。
pub fn net_strip_block_h(compact: bool) -> f32 {
    if compact { 72.0 } else { 81.0 }
}

/// 一次布局的完整结果。字段是给绘制层直接读的，不要当成内部缓存改来改去。
#[derive(Clone, Copy, Default)]
pub struct Layout {
    pub vertical: bool,
    /// 是否采用「标签在上、数值在下」的双行块。
    pub two_line: bool,
    pub width: f32,
    pub height: f32,
    /// 每个分组的左边界；-1 表示这一组没出现。
    pub lefts: [f32; 4],
    /// 位置 → 槽位（槽位是 0..=4 的固定编号）。-1 表示空位。
    pub pos_slot: [i32; PACKED],
    /// 显存两行里的「第二行」位置（没有则 -1）。
    pub temp_pos: i32,
    /// 网速占的两个位置里的第一个（没有则 -1）。
    pub net_pos: i32,
    /// 位掩码：第 n 位表示槽位 n 是否可见。
    pub mask: u32,
    /// 显存内容是否是「读写都要显示」。
    pub gpu_both: bool,
    pub compact: bool,
    pub columns: i32,
    pub row_h: f32,
    /// 每一列实际放了几行。
    pub col_rows: [i32; 4],
    pub pos_col: [i32; PACKED],
    pub pos_row: [i32; PACKED],
    pub net_col: i32,
    pub net_row: i32,
}

impl Layout {
    pub fn slot_at(&self, pos: i32) -> i32 {
        if pos < 0 || pos as usize >= PACKED {
            -1
        } else {
            self.pos_slot[pos as usize]
        }
    }

    /// 槽位落在第几个位置；没出现返回 -1。
    pub fn position_of(&self, slot: i32) -> i32 {
        self.pos_slot
            .iter()
            .position(|&s| s == slot)
            .map(|i| i as i32)
            .unwrap_or(-1)
    }

    pub fn group_visible(&self, g: usize) -> bool {
        self.group_left(g) >= 0.0
    }

    pub fn group_left(&self, g: usize) -> f32 {
        self.lefts[g.min(3)]
    }

    /// 这一组当前的宽度。含网速的那一组用更宽的表项。
    pub fn group_w(&self, g: usize) -> f32 {
        let has_net = self.net_pos >= 0 && (self.net_pos / 2) as usize == g;
        group_w(if has_net { 3 } else { 0 }, self.compact)
    }

    pub fn slot_visible(&self, slot: u32) -> bool {
        self.mask & (1 << slot) != 0
    }

    pub fn column_left(&self, c: i32) -> f32 {
        let mut x = 0.0;
        for i in 0..c {
            x += self.column_width(i);
        }
        x
    }

    pub fn column_width(&self, c: i32) -> f32 {
        if self.columns > 1 {
            // 多列时每列宽度由「这列是不是网速所在列」决定。
            group_w(if self.net_col == c { 3 } else { 0 }, self.compact)
        } else {
            self.width
        }
    }

    fn set_pos(&mut self, pos: usize, slot: i32) {
        if pos < PACKED {
            self.pos_slot[pos] = slot;
        }
    }

    fn set_cell(&mut self, pos: usize, col: i32, row: i32) {
        if pos < PACKED {
            self.pos_col[pos] = col;
            self.pos_row[pos] = row;
        }
    }

    fn add_col_rows(&mut self, col: i32, rows: i32) {
        if (0..4).contains(&col) {
            self.col_rows[col as usize] += rows;
        }
    }

    /// 相邻两个位置（pos 与 pos+1）里，这个位置算哪一行：
    /// 网速的第二行、显存读写两行的第二行都折到 0 高度，由前一行跨占。
    pub fn strip_pos_h(&self, pos: i32) -> f32 {
        let slot = self.slot_at(pos);
        if slot == 4 {
            return if pos == self.net_pos + 1 {
                0.0
            } else {
                net_strip_block_h(self.compact)
            };
        }
        if self.gpu_both && slot == 3 {
            return if pos == self.temp_pos {
                0.0
            } else {
                gpu_both_strip_block_h(self.compact)
            };
        }
        strip_block_h(self.compact)
    }

    pub fn strip_pos_top(&self, pos: i32) -> f32 {
        let mut top = 0.0;
        for p in 0..pos {
            top += self.strip_pos_h(p);
        }
        top
    }

    /// 槽位的命中矩形（逻辑 DIP）。`top/mid/bottom` 是横向两行网格的三条边。
    pub fn slot_rect(
        &self,
        slot: i32,
        top: f32,
        mid: f32,
        bottom: f32,
    ) -> Option<(f32, f32, f32, f32)> {
        if self.vertical {
            let pos = self.position_of(slot);
            let mut r = self.pos_rect(pos)?;
            // 跨两行的槽位（网速、显存双行）要把高度补成两倍行高。
            if !self.two_line && (slot == 4 || (slot == 3 && self.gpu_both)) {
                r.3 += self.row_h;
            }
            return Some(r);
        }
        let pos = self.position_of(slot);
        let mut r = self.pos_rect_at(pos, top, mid, bottom)?;
        if slot == 4 || (slot == 3 && self.gpu_both) {
            r.3 = bottom;
        }
        Some(r)
    }

    pub fn pos_rect(&self, pos: i32) -> Option<(f32, f32, f32, f32)> {
        if pos < 0 || pos as usize >= PACKED || self.slot_at(pos) < 0 {
            return None;
        }
        let col = self.pos_col[pos as usize];
        let left = self.column_left(col);
        let right = left + self.column_width(col);
        if self.two_line {
            let top = self.strip_pos_top(pos);
            Some((left, top, right, top + self.strip_pos_h(pos)))
        } else {
            let top = self.pos_row[pos as usize] as f32 * self.row_h;
            Some((left, top, right, top + self.row_h))
        }
    }

    /// 横向布局里按「组」切出来的矩形：偶数位占上半、奇数位占下半。
    pub fn pos_rect_at(
        &self,
        pos: i32,
        top: f32,
        mid: f32,
        bottom: f32,
    ) -> Option<(f32, f32, f32, f32)> {
        if self.vertical {
            return self.pos_rect(pos);
        }
        if pos < 0 || pos as usize >= PACKED || self.slot_at(pos) < 0 {
            return None;
        }
        let g = (pos / 2) as usize;
        let gx0 = self.group_left(g);
        if gx0 < 0.0 {
            return None;
        }
        let gx1 = gx0 + self.group_w(g);
        if pos % 2 == 0 {
            Some((gx0, top, gx1, mid))
        } else {
            Some((gx0, mid, gx1, bottom))
        }
    }
}

/// 归一化分组顺序：丢掉越界与重复，缺的按编号补在后面。
pub fn normalize_order(order: &[usize; 5]) -> [usize; 5] {
    let mut seen = 0u8;
    let mut out = [0usize; 5];
    let mut n = 0;
    for &s in order {
        if s < 5 && seen & (1 << s) == 0 {
            seen |= 1 << s;
            out[n] = s;
            n += 1;
        }
    }
    for s in 0..5 {
        if seen & (1 << s) == 0 {
            out[n] = s;
            n += 1;
        }
    }
    out
}

/// 这个槽位在当前显存内容下要占几个「位置」。
fn slot_span(slot: i32, gpu_both: bool) -> usize {
    if slot == 4 || (slot == 3 && gpu_both) {
        2
    } else {
        1
    }
}

/// 把可见槽位按顺序填进位置表。
fn pack_slots(l: &mut Layout, gpu_content: u8, vertical: bool, order: &[usize; 5]) -> usize {
    let mut pos = 0usize;
    for &slot in order {
        if l.mask & (1 << slot) == 0 {
            continue;
        }
        let slot = slot as i32;
        let span = slot_span(slot, l.gpu_both);
        // 横向布局里跨两行的项目必须从「格子顶行」开始，奇数位先跳一格。
        if span == 2 && !vertical && pos % 2 == 1 {
            pos += 1;
        }
        if pos + span > PACKED {
            break;
        }
        match slot {
            4 => {
                // 网速上下行：两个位置都指向同一个槽位。
                l.net_pos = pos as i32;
                l.set_pos(pos, slot);
                pos += 1;
                l.set_pos(pos, slot);
                pos += 1;
            }
            3 => {
                // 显存：可能只有总量、只有分项、或两者都有。
                if gpu_content != 1 {
                    l.set_pos(pos, slot);
                    pos += 1;
                }
                if gpu_content != 0 {
                    l.set_pos(pos, slot);
                    l.temp_pos = pos as i32;
                    pos += 1;
                }
            }
            _ => {
                l.set_pos(pos, slot);
                pos += 1;
            }
        }
    }
    pos
}

/// 横向：每两格一组，格子之间没有间隙。
fn place_horizontal(mut l: Layout, packed: usize) -> Layout {
    let mut x = 0.0;
    let groups = packed.div_ceil(2);
    for g in 0..groups {
        let left = x;
        if g < 4 {
            l.lefts[g] = left;
        }
        x += l.group_w(g);
    }
    l.width = x;
    l.height = 0.0;
    l
}

pub fn compute(
    mask: u32,
    gpu_content: u8,
    vertical: bool,
    avail_width_dip: f32,
    compact: bool,
    order: &[usize; 5],
) -> Layout {
    let mut l = Layout {
        lefts: [-1.0; 4],
        pos_slot: [-1; PACKED],
        pos_col: [-1; PACKED],
        pos_row: [-1; PACKED],
        temp_pos: -1,
        net_pos: -1,
        net_col: -1,
        mask,
        gpu_both: gpu_content == 2,
        compact,
        ..Default::default()
    };
    let order = normalize_order(order);
    let packed = pack_slots(&mut l, gpu_content, vertical, &order);
    if vertical {
        compute_vertical(l, packed as i32, avail_width_dip)
    } else {
        place_horizontal(l, packed)
    }
}

/// 竖直任务栏：先算自然宽度，再按可用厚度决定列数。
fn compute_vertical(mut l: Layout, packed: i32, avail_width_dip: f32) -> Layout {
    l.vertical = true;
    let has_net = l.net_pos >= 0;
    let w0 = group_w(0, l.compact);
    let w3 = group_w(3, l.compact);
    let net_extra = if has_net { w3 - w0 } else { 0.0 };
    // 自然宽度：普通列宽度 + 网速列的额外宽度。
    let natural = if packed > 0 { w0 + net_extra } else { 0.0 };
    if natural <= 0.0 {
        return l;
    }

    let avail = if avail_width_dip > 0.0 { avail_width_dip } else { natural };
    let items = packed - if has_net { 1 } else { 0 };
    // +1e-4 是浮点容差：正好放得下 N 列时不要因为误差少算一列。
    let mut max_cols = ((avail - net_extra) / w0 + 1e-4) as i32;
    max_cols = max_cols.clamp(1, items.max(1)).min(4);

    if max_cols == 1 {
        return single_column(l, packed, natural, avail, has_net);
    }
    grid_columns(l, packed, max_cols, has_net, w0, net_extra)
}

/// 只有一列：窄到放不下两行时改成双行块。
fn single_column(mut l: Layout, packed: i32, natural: f32, avail: f32, has_net: bool) -> Layout {
    l.columns = 1;
    l.width = natural.min(avail);
    l.two_line = l.width < STRIP_TWO_LINE_MIN_W.min(natural);
    l.row_h = if l.two_line {
        strip_block_h(l.compact)
    } else {
        STRIP_H
    };
    l.col_rows[0] = packed;
    for p in 0..packed as usize {
        l.set_cell(p, 0, p as i32);
    }
    if has_net {
        l.net_col = 0;
        l.net_row = l.net_pos;
    }
    l.height = if l.two_line {
        l.strip_pos_top(packed)
    } else {
        packed as f32 * l.row_h
    };
    l
}

/// 多列网格：按目标行数均匀分列，放不下就提前换列。
fn grid_columns(
    mut l: Layout,
    packed: i32,
    max_cols: i32,
    has_net: bool,
    w0: f32,
    net_extra: f32,
) -> Layout {
    l.row_h = grid_row_h(l.compact);
    let target = (packed + max_cols - 1) / max_cols;
    let items = packed - if has_net { 1 } else { 0 };
    let mut col = 0;
    let mut row = 0;
    for p in 0..packed {
        let second_net = has_net && p == l.net_pos + 1;
        let second_gpu = l.gpu_both && p == l.temp_pos;
        let row_span = if has_net && p == l.net_pos { 2 } else { 1 };
        // 跨两行的项目放不下当前列，或剩下的项已经填不满剩余列，就换列。
        let would_overflow = row + row_span > target;
        let not_enough_left = items - p - 1 < max_cols - 1 - col;
        if !second_net && !second_gpu && row > 0 && col + 1 < max_cols
            && (would_overflow || not_enough_left)
        {
            col += 1;
            row = 0;
        }
        l.set_cell(p as usize, col, row);
        l.add_col_rows(col, 1);
        row += 1;
    }
    if has_net {
        l.net_col = l.pos_col[l.net_pos as usize];
        l.net_row = l.pos_row[l.net_pos as usize];
    }
    l.columns = col + 1;
    l.width = l.columns as f32 * w0 + net_extra;
    let max_rows = l.col_rows.iter().copied().max().unwrap_or(0);
    l.height = max_rows as f32 * l.row_h;
    l
}

/// 竖直任务栏的可用宽度（DIP）：厚度减 2 DIP；未测量时返回 0，表示用自然宽度。
pub fn vertical_avail_width(band_w: i32, dpi: u32) -> f32 {
    let max_w = band_w - zx_core::win::dpi_scale_i(2.0, dpi);
    if max_w > 0 {
        max_w as f32 * 96.0 / dpi as f32
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_is_deduped_and_completed() {
        assert_eq!(normalize_order(&[2, 2, 9, 0, 1]), [2, 0, 1, 3, 4]);
        assert_eq!(normalize_order(&[4, 3, 2, 1, 0]), [4, 3, 2, 1, 0]);
    }

    #[test]
    fn horizontal_groups_are_packed_left_to_right() {
        // 5 个槽位 + 网速占两格 = 6 个位置 = 3 组；第四组不存在，左边界保持 -1
        let l = compute(0b11111, 0, false, 0.0, false, &[0, 1, 2, 3, 4]);
        assert_eq!(l.lefts, [0.0, 70.0, 140.0, -1.0]);
        assert_eq!(l.group_w(0), 70.0);
        // 网速落在第三组，那一组用宽表项 84
        assert_eq!(l.group_w(2), 84.0);
        assert_eq!(l.width, 70.0 + 70.0 + 84.0);
    }

    #[test]
    fn net_slot_gets_two_positions_with_zero_second_height() {
        let l = compute(0b10000, 0, false, 0.0, false, &[0, 1, 2, 3, 4]);
        assert_eq!(l.net_pos, 0);
        assert_eq!(l.strip_pos_h(0), net_strip_block_h(false));
        assert_eq!(l.strip_pos_h(1), 0.0);
    }

    #[test]
    fn vertical_single_column_turns_two_line_when_narrow() {
        let l = compute(0b11111, 0, true, 60.0, false, &[0, 1, 2, 3, 4]);
        assert!(l.vertical);
        assert_eq!(l.columns, 1);
        assert!(l.two_line);
    }

    #[test]
    fn vertical_grid_splits_into_columns() {
        let l = compute(0b11111, 0, true, 300.0, false, &[0, 1, 2, 3, 4]);
        assert!(l.columns > 1);
        assert!(l.width > 70.0);
        assert!(l.height > 0.0);
    }

    #[test]
    fn empty_mask_has_no_groups() {
        let l = compute(0, 0, false, 0.0, false, &[0, 1, 2, 3, 4]);
        assert_eq!(l.width, 0.0);
        assert_eq!(l.group_left(0), -1.0);
        assert!(!l.group_visible(0));
    }
}
