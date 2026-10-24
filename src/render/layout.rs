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
    /// 切换当前标签的 shell 类型（pwsh / powershell / cmd）。
    Shell,
    /// 清空当前标签的屏幕与滚动缓冲。
    Clear,
    /// 落在标签栏上但没有可点的东西。
    Blank,
}

/// 标签之间的空隙（像素）。
pub const TAB_GAP: i32 = 4;
/// 标签文字的左边距。
pub const TAB_PAD_LEFT: i32 = 8;
/// 叉号占位宽度：标题区必须给它让出这么多，否则最后一个字会和 × 叠在一起。
pub const TAB_CLOSE_W: i32 = 18;
/// 「清屏」按钮占几个单元格宽。
///
/// 它是两个**汉字**，每个汉字在网格里占两格，所以文字本身就占四格：
/// 按两格给宽会让文字溢出到窗口右边缘之外，绘制时还会因为按一格步进
/// 而把「清」「屏」压在同一处（用户看到的字叠字）。
/// 给五格而不是四格，是为了文字两侧各留半格——四格宽时文字正好顶满按钮，
/// 右边缘顶到窗口边框上，看上去像被裁掉了。
pub const CLEAR_W_CELLS: i32 = 5;

/// 标签栏一次算好的全部几何。
///
/// 为什么「一次算完」：标签位置、按钮位置、标签可绘制右界三者互相约束
/// （标签不许画到按钮上）。拆成几个各算一遍的函数，就会出现三处彼此不一致——
/// 用户实测的「导航栏排布错乱」正是这么来的：标题宽度没给叉号留位，
/// 标题最后一个字和 × 直接压在一起。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabBar {
    pub slots: Vec<TabSlot>,
    /// 「+」新建按钮的中心。
    pub new_center: i32,
    /// 「清屏」按钮的中心。
    pub clear_center: i32,
    /// shell 类型按钮的中心（显示 pwsh / ps5 / cmd，点了切换）。
    pub shell_center: i32,
    /// 标签可绘制、可命中的右界：再往右就是按钮区。
    pub clip_end: i32,
}

/// 标签栏高度：一行字高加上下各 3px 的呼吸空间。
pub fn tab_bar_height(cell_h: i32) -> i32 {
    cell_h.max(1) + 6
}

/// 单个标签的宽度：左边距 + 标题 + 叉号占位 + 右边距。
pub fn tab_width(cell_w: i32, title: &str) -> i32 {
    let cell_w = cell_w.max(1);
    TAB_PAD_LEFT + title.chars().count() as i32 * cell_w + TAB_CLOSE_W + 6
}

/// 算出整条标签栏的几何。
///
/// 按钮从右往左排：清屏 → shell 类型 → 新建。右对齐的好处是标签再多也不会把按钮挤走，
/// 也不会出现「按钮跟着最后一个标签跑，标签一多就被挤出窗口」。
pub fn tab_bar(cell_w: i32, titles: &[&str], client_w: i32) -> TabBar {
    let cell_w = cell_w.max(1);
    let client_w = client_w.max(1);

    let margin = 10;
    let clear_w = cell_w * CLEAR_W_CELLS;
    let clear_center = (client_w - margin - clear_w / 2).max(0);
    let clear_left = client_w - margin - clear_w;
    let shell_w = cell_w * 4;
    let shell_center = (clear_left - 12 - shell_w / 2).max(0);
    let shell_left = clear_left - 12 - shell_w;
    let new_center = (shell_left - 14).max(0);
    let clip_end = (new_center - 12).max(0);

    let mut slots = Vec::with_capacity(titles.len());
    let mut cursor = 4;
    for title in titles {
        let width = tab_width(cell_w, title);
        if cursor + width > clip_end {
            // 放不下就不再往后排：宁可不显示，也不让标签压到按钮上。
            break;
        }
        slots.push(TabSlot {
            start: cursor,
            end: cursor + width,
            close_center: cursor + width - 6 - TAB_CLOSE_W / 2,
        });
        cursor += width + TAB_GAP;
    }

    TabBar {
        slots,
        new_center,
        clear_center,
        shell_center,
        clip_end,
    }
}

/// 标签栏上的 x 坐标落在哪一格。
///
/// 判据是「离谁更近」而不是按顺序比区间：叉号在标签体内部，
/// 两个按钮的半径也会重叠，按顺序判必然有一方被吞掉。
pub fn hit_tab_bar(bar: &TabBar, cell_w: i32, x: i32) -> TabHit {
    let cell_w = cell_w.max(1);
    for (index, slot) in bar.slots.iter().enumerate() {
        if (x - slot.close_center).abs() <= TAB_CLOSE_W / 2 {
            return TabHit::Close(index);
        }
        if x >= slot.start && x < slot.end {
            return TabHit::Select(index);
        }
    }
    let to_new = (x - bar.new_center).abs();
    let to_shell = (x - bar.shell_center).abs();
    let to_clear = (x - bar.clear_center).abs();
    if to_new <= 10 && to_new <= to_shell && to_new <= to_clear {
        return TabHit::New;
    }
    if to_shell <= cell_w * 2 && to_shell <= to_clear {
        return TabHit::Shell;
    }
    if to_clear <= cell_w * (CLEAR_W_CELLS / 2) + 4 {
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
    fn tabs_never_overlap_and_leave_room_for_the_close_button() {
        let bar = super::tab_bar(8, &["shell", "pwsh-long"], 900);
        assert_eq!(bar.slots.len(), 2);
        assert!(bar.slots[0].start < bar.slots[0].end);
        assert!(bar.slots[0].end + super::TAB_GAP <= bar.slots[1].start);
        // 更长的标题更宽。
        let first = bar.slots[0].end - bar.slots[0].start;
        let second = bar.slots[1].end - bar.slots[1].start;
        assert!(second > first);
        // 关键回归：标题区必须给叉号留出整块位置，否则最后一个字会压到 × 上
        // （用户看到的「导航栏排布错乱」就是这个）。
        for (slot, title) in bar.slots.iter().zip(["shell", "pwsh-long"]) {
            let room = (slot.end - slot.start) - super::TAB_PAD_LEFT - super::TAB_CLOSE_W - 6;
            assert!(room >= title.chars().count() as i32 * 8);
            assert!(slot.close_center + super::TAB_CLOSE_W / 2 <= slot.end);
        }
    }

    #[test]
    fn close_wins_over_select_inside_the_same_tab() {
        let bar = super::tab_bar(8, &["shell"], 900);
        assert_eq!(
            super::hit_tab_bar(&bar, 8, bar.slots[0].close_center),
            super::TabHit::Close(0)
        );
        // 标签体左侧离叉号够远，应当切换而不是关闭。
        assert_eq!(
            super::hit_tab_bar(&bar, 8, bar.slots[0].start + 1),
            super::TabHit::Select(0)
        );
    }

    #[test]
    fn buttons_are_right_aligned_and_out_of_the_tab_strip() {
        let titles = ["shell 1", "shell 2", "shell 3"];
        let bar = super::tab_bar(8, &titles, 900);
        // 三个按钮从右往左排：清屏 > shell > 新建。
        assert!(bar.clear_center > bar.shell_center);
        assert!(bar.shell_center > bar.new_center);
        assert!(bar.slots.iter().all(|slot| slot.end <= bar.clip_end));
        assert_eq!(
            super::hit_tab_bar(&bar, 8, bar.new_center),
            super::TabHit::New
        );
        assert_eq!(
            super::hit_tab_bar(&bar, 8, bar.shell_center),
            super::TabHit::Shell
        );
        assert_eq!(
            super::hit_tab_bar(&bar, 8, bar.clear_center),
            super::TabHit::Clear
        );
    }

    #[test]
    fn tabs_are_dropped_instead_of_covering_the_buttons() {
        // 窗口很窄时宁可不画标签，也不能让标签压到按钮上。
        let titles = ["shell 1", "shell 2", "shell 3", "shell 4"];
        let bar = super::tab_bar(8, &titles, 240);
        assert!(bar.slots.len() < titles.len());
        assert!(bar.slots.iter().all(|slot| slot.end <= bar.clip_end));
    }

    #[test]
    fn blank_area_of_the_bar_is_neither_tab_nor_button() {
        let bar = super::tab_bar(8, &["shell"], 900);
        assert_eq!(super::hit_tab_bar(&bar, 8, 600), super::TabHit::Blank);
        // 空标签栏也留一个可点的「+」。
        let empty = super::tab_bar(8, &[], 900);
        assert_eq!(
            super::hit_tab_bar(&empty, 8, empty.new_center),
            super::TabHit::New
        );
    }

    #[test]
    fn shell_button_sits_between_new_and_clear() {
        let bar = super::tab_bar(8, &["shell"], 900);
        assert!(bar.shell_center > bar.new_center + 8);
        assert!(bar.shell_center < bar.clear_center - 8);
    }

    #[test]
    fn clear_button_is_wide_enough_for_two_cjk_glyphs() {
        // 回归：「清屏」两个字曾被画在同一处（宽字符按一格步进），按钮又只有两格宽，
        // 文字溢出到窗口右边缘之外。现在按钮给足四格，命中区覆盖整块。
        let cell_w = 8;
        let client_w = 900;
        let bar = super::tab_bar(cell_w, &["shell"], client_w);
        let half = cell_w * super::CLEAR_W_CELLS / 2;
        assert!(bar.clear_center + half <= client_w);
        assert!(bar.clear_center - half > bar.shell_center + cell_w);
        for dx in [-half + 1, -cell_w, cell_w, half - 1] {
            assert_eq!(
                super::hit_tab_bar(&bar, cell_w, bar.clear_center + dx),
                super::TabHit::Clear,
                "离清屏中心 {dx} 像素处应当是清屏按钮"
            );
        }
    }
}
