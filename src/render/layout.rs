//! 网格 ↔ 像素的唯一换算点。
//!
//! 硬边界（UI 规范）：**painter 不做布局**。所有“第几行第几列对应哪个像素”的算术
//! 都在这里，painter 只按算好的坐标填色。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub cell_w: i32,
    pub cell_h: i32,
    pub padding: i32,
    pub cols: usize,
    pub rows: usize,
    /// 单元格顶边到基线的距离，绘制字形时用。
    pub ascent: i32,
    /// 顶部留给标签栏的高度（客户区坐标）。标签栏不属于网格，
    /// 但「第 0 行在哪」必须只由这里回答，painter 才能继续不做布局。
    pub top_inset: i32,
}

/// 标签栏上一格的可点区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabSlot {
    /// 标签体的 x 范围 `[start, end)`。
    pub start: i32,
    pub end: i32,
    /// 关闭叉号的 x 中心。
    pub close_center: i32,
}

/// 标签栏命中结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    /// 切到第 n 个标签。
    Select(usize),
    /// 关掉第 n 个标签。
    Close(usize),
    /// 新建标签。
    New,
    /// 清空当前标签的屏幕与滚动缓冲。
    Clear,
    /// 落在标签栏上但没有可点的东西。
    Blank,
}

/// 标签栏高度：一行字高加上下各 3px 的呼吸空间。
pub fn tab_bar_height(cell_h: i32) -> i32 {
    cell_h.max(1) + 6
}

/// 按标签标题算出每个标签的可点范围。
///
/// 终端字体是等宽的，所以标题宽度就是「字符数 × 单元格宽」，不需要测量文本。
pub fn tab_slots(cell_w: i32, titles: &[&str]) -> Vec<TabSlot> {
    let cell_w = cell_w.max(1);
    let mut slots = Vec::with_capacity(titles.len());
    let mut cursor = 4;
    for title in titles {
        let width = title.chars().count() as i32 * cell_w + 16;
        slots.push(TabSlot {
            start: cursor,
            end: cursor + width,
            close_center: cursor + width - 8,
        });
        cursor += width + 2;
    }
    slots
}

/// 新建按钮的 x 中心：紧跟最后一个标签之后。
pub fn new_button_center(cell_w: i32, titles: &[&str]) -> i32 {
    let slots = tab_slots(cell_w, titles);
    slots.last().map_or(12, |slot| slot.end + 10)
}

/// 清屏按钮的 x 中心：在「+」之后再空两个单元格。
///
/// 用文字「清屏」而不是图标：字形缓存已经覆盖 CJK，画字符比自造图标稳。
pub fn clear_button_center(cell_w: i32, titles: &[&str]) -> i32 {
    new_button_center(cell_w, titles) + cell_w.max(1) * 2 + 10
}

/// 标签栏上的 x 坐标落在哪一格。
///
/// 叉号判定优先于标签体：叉号在标签体内部，先判叉号用户才不会「想关却切过去」。
pub fn hit_tab_bar(cell_w: i32, titles: &[&str], x: i32) -> TabHit {
    let cell_w = cell_w.max(1);
    for (index, slot) in tab_slots(cell_w, titles).iter().enumerate() {
        if (x - slot.close_center).abs() <= 6 {
            return TabHit::Close(index);
        }
        if x >= slot.start && x < slot.end {
            return TabHit::Select(index);
        }
    }
    // 判据必须是"离谁更近"，不能是两张互相重叠的区间：
    // 清屏的 ±(2 格+4) 半径很大，直接按顺序判会把「+」整个吞掉。
    let new_center = new_button_center(cell_w, titles);
    let clear_center = clear_button_center(cell_w, titles);
    let to_new = (x - new_center).abs();
    let to_clear = (x - clear_center).abs();
    if to_new <= 8 && to_new <= to_clear {
        return TabHit::New;
    }
    if to_clear <= cell_w * 2 + 4 {
        return TabHit::Clear;
    }
    TabHit::Blank
}

impl Layout {
    /// 按客户区尺寸算出能放下多少行列。
    ///
    /// 余数不摊进单元格——宁可右边和下边留一点空隙，也不让字符被拉伸变形。
    pub fn compute(
        cell_w: i32,
        cell_h: i32,
        padding: i32,
        ascent: i32,
        client_w: i32,
        client_h: i32,
    ) -> Self {
        Self::compute_with_inset(cell_w, cell_h, padding, ascent, client_w, client_h, 0)
    }

    /// 同上，但顶部留出 `top_inset` 给标签栏。
    pub fn compute_with_inset(
        cell_w: i32,
        cell_h: i32,
        padding: i32,
        ascent: i32,
        client_w: i32,
        client_h: i32,
        top_inset: i32,
    ) -> Self {
        let cell_w = cell_w.max(1);
        let cell_h = cell_h.max(1);
        let padding = padding.max(0);
        let top_inset = top_inset.max(0);
        let usable_w = (client_w - padding * 2).max(0);
        let usable_h = (client_h - padding * 2 - top_inset).max(0);

        let cols = (usable_w / cell_w).max(1) as usize;
        let rows = (usable_h / cell_h).max(1) as usize;

        Self {
            cell_w,
            cell_h,
            padding,
            cols,
            rows,
            ascent: ascent.clamp(1, cell_h),
            top_inset,
        }
    }

    /// 单元格左上角的像素坐标。
    pub fn cell_origin(&self, col: usize, row: usize) -> (i32, i32) {
        (
            self.padding + col as i32 * self.cell_w,
            self.padding + self.top_inset + row as i32 * self.cell_h,
        )
    }

    /// 整块文本区域的尺寸（不含 padding）。
    #[allow(dead_code)] // M4 圆角区域计算用
    pub fn content_size(&self) -> (i32, i32) {
        (
            self.cols as i32 * self.cell_w,
            self.rows as i32 * self.cell_h,
        )
    }

    /// 需要重绘的窗口尺寸：文本区域 + 两侧 padding + 标签栏。
    #[allow(dead_code)] // M2 尺寸校准用
    pub fn required_client(&self) -> (i32, i32) {
        let (w, h) = self.content_size();
        (w + self.padding * 2, h + self.padding * 2 + self.top_inset)
    }

    /// 像素 → 网格坐标。落在 padding、标签栏或越界时返回 `None`。
    #[allow(dead_code)] // M2 命中测试用
    pub fn hit(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        let rel_x = x - self.padding;
        let rel_y = y - self.padding - self.top_inset;
        if rel_x < 0 || rel_y < 0 {
            return None;
        }
        let col = (rel_x / self.cell_w) as usize;
        let row = (rel_y / self.cell_h) as usize;
        if col >= self.cols || row >= self.rows {
            return None;
        }
        Some((col, row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_geometry_matches_spec() {
        // 900×420 / padding 10 / 8×18 单元格 → 110 列 × 22 行。
        let layout = Layout::compute(8, 18, 10, 14, 900, 420);
        assert_eq!(layout.cols, 110);
        assert_eq!(layout.rows, 22);
    }

    #[test]
    fn higher_dpi_keeps_column_count() {
        // 200% 缩放下单元格与窗口同比例放大，列数应当不变。
        let layout = Layout::compute(16, 36, 20, 28, 1800, 840);
        assert_eq!(layout.cols, 110);
        assert_eq!(layout.rows, 22);
    }

    #[test]
    fn tiny_window_still_has_one_cell() {
        let layout = Layout::compute(8, 18, 10, 14, 5, 5);
        assert_eq!(layout.cols, 1);
        assert_eq!(layout.rows, 1);
    }

    #[test]
    fn cell_origin_accounts_for_padding() {
        let layout = Layout::compute(8, 18, 10, 14, 900, 420);
        assert_eq!(layout.cell_origin(0, 0), (10, 10));
        assert_eq!(layout.cell_origin(2, 3), (26, 64));
    }

    #[test]
    fn hit_maps_back_to_the_same_cell() {
        let layout = Layout::compute(8, 18, 10, 14, 900, 420);
        assert_eq!(layout.hit(10, 10), Some((0, 0)));
        assert_eq!(layout.hit(17, 27), Some((0, 0)));
        assert_eq!(layout.hit(18, 28), Some((1, 1)));
    }

    #[test]
    fn hit_rejects_padding_and_out_of_range() {
        let layout = Layout::compute(8, 18, 10, 14, 900, 420);
        assert_eq!(layout.hit(5, 12), None);
        assert_eq!(layout.hit(12, 5), None);
        assert_eq!(layout.hit(10_000, 10), None);
    }

    #[test]
    fn required_client_covers_text_area_plus_padding() {
        let layout = Layout::compute(8, 18, 10, 14, 900, 420);
        let (w, h) = layout.required_client();
        assert_eq!((w, h), (110 * 8 + 20, 22 * 18 + 20));
    }

    #[test]
    fn ascent_is_clamped_into_cell() {
        let layout = Layout::compute(8, 18, 10, 999, 900, 420);
        assert_eq!(layout.ascent, 18);
    }

    #[test]
    fn top_inset_shifts_the_grid_down_by_exactly_the_bar() {
        let plain = Layout::compute(8, 18, 10, 14, 900, 420);
        let inset = Layout::compute_with_inset(8, 18, 10, 14, 900, 420, 24);
        // 标签栏吃掉的高度只从行数里扣，列数不受影响。
        assert_eq!(inset.cols, plain.cols);
        // 标签栏 24px 吃掉一行半：400/18=22 行，376/18=20 行。
        assert_eq!(inset.rows, plain.rows - 2);
        assert_eq!(inset.cell_origin(0, 0), (10, 34));
        assert_eq!(inset.hit(10, 34), Some((0, 0)));
        // 标签栏那一条不能被当成第 0 行（否则点击标签会顺带选中文本）。
        assert_eq!(inset.hit(10, 20), None);
    }

    #[test]
    fn tab_slots_grow_left_to_right_without_overlap() {
        let slots = super::tab_slots(8, &["shell", "pwsh-long"]);
        assert_eq!(slots.len(), 2);
        assert!(slots[0].start < slots[0].end);
        assert!(slots[0].end < slots[1].start);
        // 更长的标题更宽。
        assert!(slots[1].end - slots[1].start > slots[0].end - slots[0].start);
    }

    #[test]
    fn close_wins_over_select_inside_the_same_tab() {
        let titles = ["shell"];
        let slots = super::tab_slots(8, &titles);
        assert_eq!(
            super::hit_tab_bar(8, &titles, slots[0].close_center),
            super::TabHit::Close(0)
        );
        // 标签体左侧离叉号够远，应当切换而不是关闭。
        assert_eq!(
            super::hit_tab_bar(8, &titles, slots[0].start + 1),
            super::TabHit::Select(0)
        );
    }

    #[test]
    fn new_button_sits_after_the_last_tab() {
        let titles = ["a", "b"];
        let center = super::new_button_center(8, &titles);
        assert_eq!(super::hit_tab_bar(8, &titles, center), super::TabHit::New);
        // 空标签栏时也留一个可点的「+」。
        assert_eq!(super::hit_tab_bar(8, &[], 12), super::TabHit::New);
    }

    #[test]
    fn blank_area_of_the_bar_is_neither_tab_nor_new() {
        let titles = ["shell"];
        // 越过「+」和「清屏」之后才是真正的空白。
        let far = super::clear_button_center(8, &titles) + 60;
        assert_eq!(super::hit_tab_bar(8, &titles, far), super::TabHit::Blank);
    }

    #[test]
    fn clear_button_sits_after_the_new_button() {
        let titles = ["shell"];
        let plus = super::new_button_center(8, &titles);
        let clear = super::clear_button_center(8, &titles);
        assert!(clear > plus + 8);
        assert_eq!(super::hit_tab_bar(8, &titles, clear), super::TabHit::Clear);
        // 两个按钮挨着的边界不能互相吞掉。
        assert_eq!(super::hit_tab_bar(8, &titles, plus), super::TabHit::New);
    }
}
