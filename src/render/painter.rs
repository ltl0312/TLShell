//! 绘制：把 `Terminal` 的网格刷成一块 `0x00RRGGBB` 像素缓冲。
//!
//! 职责边界（UI 规范硬边界）：
//! - painter **不做布局**，行列换算全在 [`Layout`]；
//! - painter **不认识 PTY**，只读网格与光标状态；
//! - painter **不管理时间**，闪烁开关由调用方传进来。

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;

use crate::render::glyph::GlyphCache;
use crate::render::layout::Layout;
use crate::render::metrics::FontSet;
use crate::render::theme::{blend, Theme};
use crate::term::Terminal;

/// 可写的像素缓冲视图。
pub struct Frame<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

impl Frame<'_> {
    fn put(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        let index = y * self.width + x;
        if let Some(slot) = self.pixels.get_mut(index) {
            *slot = color;
        }
    }

    fn blend_pixel(&mut self, x: i32, y: i32, color: u32, coverage: u8) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        let index = y * self.width + x;
        if let Some(slot) = self.pixels.get_mut(index) {
            *slot = blend(*slot, color, coverage);
        }
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        for row in y..y.saturating_add(h) {
            for col in x..x.saturating_add(w) {
                self.put(col, row, color);
            }
        }
    }

    fn stroke_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        if w <= 0 || h <= 0 {
            return;
        }
        for col in x..x + w {
            self.put(col, y, color);
            self.put(col, y + h - 1, color);
        }
        for row in y..y + h {
            self.put(x, row, color);
            self.put(x + w - 1, row, color);
        }
    }

    fn clear(&mut self, color: u32) {
        self.pixels.fill(color);
    }
}

pub struct Painter {
    pub fonts: FontSet,
    pub theme: Theme,
    cache: GlyphCache,
}

impl Painter {
    pub fn new(fonts: FontSet, theme: Theme) -> Self {
        Self {
            fonts,
            theme,
            cache: GlyphCache::new(),
        }
    }

    /// 主绘制入口。
    ///
    /// `cursor_visible` 是闪烁状态；`focused` 决定边框与光标形态。
    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        term: &Terminal,
        cursor_visible: bool,
        focused: bool,
    ) {
        frame.clear(self.theme.background);

        let grid = term.grid();
        let display_offset = grid.display_offset() as i32;
        let mode = term.mode();

        for row in 0..layout.rows {
            let line = Line(row as i32 - display_offset);
            for col in 0..layout.cols {
                let cell = &grid[line][Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                    || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                self.draw_cell(frame, layout, cell, row, col);
            }
        }

        if mode.contains(TermMode::SHOW_CURSOR) {
            self.draw_cursor(frame, layout, term, display_offset, cursor_visible, focused);
        }

        self.draw_border(frame, focused);
    }

    fn draw_cell(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        cell: &alacritty_terminal::term::cell::Cell,
        row: usize,
        col: usize,
    ) {
        let (x0, y0) = layout.cell_origin(col, row);
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let cell_width = if wide {
            layout.cell_w * 2
        } else {
            layout.cell_w
        };

        let mut fg = self.theme.resolve(cell.fg);
        let mut bg = self.theme.resolve(cell.bg);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        } else if cell.flags.contains(Flags::DIM) {
            // 用 60% 覆盖度把前景色往背景上拉，模拟暗色。
            fg = blend(bg, fg, 153);
        }

        if bg != self.theme.background {
            frame.fill_rect(x0, y0, cell_width, layout.cell_h, bg);
        }

        if cell.c != ' ' && cell.c != '\0' {
            self.draw_glyph(frame, layout, cell.c, cell.flags, fg, x0, y0);
        }

        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            let thickness = self.fonts.underline_thickness;
            let y = y0 + self.fonts.underline_y;
            frame.fill_rect(x0, y, cell_width, thickness, fg);
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            let y = y0 + layout.cell_h / 2;
            frame.fill_rect(x0, y, cell_width, self.fonts.underline_thickness, fg);
        }
    }

    #[allow(clippy::too_many_arguments)] // 绘制原语，参数摊平比引入结构体更直白
    fn draw_glyph(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        c: char,
        flags: Flags,
        color: u32,
        x0: i32,
        y0: i32,
    ) {
        let bold = flags.intersects(Flags::BOLD | Flags::BOLD_ITALIC | Flags::DIM_BOLD);
        let Some(glyph) = self.cache.get(&self.fonts, c, bold) else {
            self.draw_missing_glyph(frame, layout, x0, y0, color);
            return;
        };
        if glyph.width == 0 || glyph.height == 0 {
            return;
        }

        let baseline = y0 + layout.ascent;
        let top = baseline - glyph.top;
        let left = x0 + glyph.left;

        for gy in 0..glyph.height {
            for gx in 0..glyph.width {
                let coverage = glyph.coverage[gy * glyph.width + gx];
                if coverage == 0 {
                    continue;
                }
                frame.blend_pixel(left + gx as i32, top + gy as i32, color, coverage);
                // 没有独立粗体文件时用双描合成：右移一像素再画一次。
                if bold && self.fonts.bold.is_none() {
                    frame.blend_pixel(left + gx as i32 + 1, top + gy as i32, color, coverage / 2);
                }
            }
        }
    }

    /// 缺字框：一个空心矩形。比什么都不画更容易被发现。
    fn draw_missing_glyph(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        x0: i32,
        y0: i32,
        color: u32,
    ) {
        let inset = 1;
        frame.stroke_rect(
            x0 + inset,
            y0 + inset,
            layout.cell_w - inset * 2,
            layout.cell_h - inset * 2,
            color,
        );
    }

    fn draw_cursor(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        term: &Terminal,
        display_offset: i32,
        cursor_visible: bool,
        focused: bool,
    ) {
        let point = term.cursor_point();
        let row = point.line.0 + display_offset;
        if row < 0 || row as usize >= layout.rows || point.column.0 >= layout.cols {
            return;
        }
        let (x0, y0) = layout.cell_origin(point.column.0, row as usize);
        let cursor_color = self.theme.cursor;

        if !focused {
            // 失焦：空心框，不闪，不用实心块——不会盖住字符。
            frame.stroke_rect(x0, y0, layout.cell_w, layout.cell_h, cursor_color);
            return;
        }
        if !cursor_visible {
            return;
        }

        frame.fill_rect(x0, y0, layout.cell_w, layout.cell_h, cursor_color);

        // 块状光标必须反色，否则浅色光标上的浅色字符等于消失（UI 规范 G3）。
        let cell = term.cell(point.line, point.column);
        if cell.c != ' ' && cell.c != '\0' {
            self.draw_glyph(
                frame,
                layout,
                cell.c,
                cell.flags,
                self.theme.background,
                x0,
                y0,
            );
        }
    }

    fn draw_border(&self, frame: &mut Frame<'_>, focused: bool) {
        let color = if focused {
            self.theme.focus_ring
        } else {
            // 失焦用背景与前景之间的一档灰，避免整窗轮廓刺眼。
            blend(self.theme.background, self.theme.foreground, 60)
        };
        let width = frame.width as i32;
        let height = frame.height as i32;
        frame.stroke_rect(0, 0, width, height, color);
    }
}
