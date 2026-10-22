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
        let cell_w = cell_w.max(1);
        let cell_h = cell_h.max(1);
        let padding = padding.max(0);
        let usable_w = (client_w - padding * 2).max(0);
        let usable_h = (client_h - padding * 2).max(0);

        let cols = (usable_w / cell_w).max(1) as usize;
        let rows = (usable_h / cell_h).max(1) as usize;

        Self {
            cell_w,
            cell_h,
            padding,
            cols,
            rows,
            ascent: ascent.clamp(1, cell_h),
        }
    }

    /// 单元格左上角的像素坐标。
    pub fn cell_origin(&self, col: usize, row: usize) -> (i32, i32) {
        (
            self.padding + col as i32 * self.cell_w,
            self.padding + row as i32 * self.cell_h,
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

    /// 需要重绘的窗口尺寸：文本区域 + 两侧 padding。
    #[allow(dead_code)] // M2 尺寸校准用
    pub fn required_client(&self) -> (i32, i32) {
        let (w, h) = self.content_size();
        (w + self.padding * 2, h + self.padding * 2)
    }

    /// 像素 → 网格坐标。落在 padding 或越界时返回 `None`。
    #[allow(dead_code)] // M2 命中测试用
    pub fn hit(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        let rel_x = x - self.padding;
        let rel_y = y - self.padding;
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
}
