//! 绘制：把 `Terminal` 的网格刷成一块**预乘 ARGB** 像素缓冲。
//!
//! 为什么要预乘：背景透明度和文字透明度是两个独立的量（用户要求「字体和背景
//! 分开」）。缓冲里每个像素自带 alpha，背景只影响 `clear_with` 的那一层，
//! 字形按覆盖度叠上去之后 alpha 趋近 255，于是「背景全透明、文字照旧清晰」成立。
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
use crate::render::metrics::{is_wide, FontSet};
use crate::render::theme::{blend, over, Theme};
use crate::term::Terminal;

/// 可写的像素缓冲视图。
pub struct Frame<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

impl Frame<'_> {
    /// 把 `0x00RRGGBB` 按 `alpha` 预乘成缓冲里的像素值。
    fn premult(color: u32, alpha: u8) -> u32 {
        match alpha {
            0 => 0,
            255 => 0xFF00_0000 | (color & 0x00FF_FFFF),
            a => {
                let a = u32::from(a);
                let scale = |shift: u32| -> u32 { (((color >> shift) & 0xFF) * a + 127) / 255 };
                (a << 24) | (scale(16) << 16) | (scale(8) << 8) | scale(0)
            }
        }
    }

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
            *slot = Self::premult(color, 255);
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
            *slot = over(*slot, color, coverage);
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

    /// 按背景透明度铺底。
    fn clear_with(&mut self, color: u32, alpha: u8) {
        self.pixels.fill(Self::premult(color, alpha));
    }

    /// 带 alpha 的矩形填充（选区、单元格底色这类「半透明装饰」）。
    fn fill_rect_a(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32, alpha: u8) {
        let pixel = Self::premult(color, alpha);
        for py in y..y.saturating_add(height) {
            for px in x..x.saturating_add(width) {
                if px < 0 || py < 0 {
                    continue;
                }
                let (cx, cy) = (px as usize, py as usize);
                if cx >= self.width || cy >= self.height {
                    continue;
                }
                let index = cy * self.width + cx;
                if let Some(slot) = self.pixels.get_mut(index) {
                    *slot = pixel;
                }
            }
        }
    }

    #[allow(dead_code)] // 不透明铺底仍在用，保留与之成对的入口
    fn clear(&mut self, color: u32) {
        self.pixels.fill(color);
    }
}

/// 提权常驻标记的几何：在边框内侧画一圈 1px 警示色。
///
/// 抽成自由函数是为了能脱离字体直接测像素——提权线索不能只在真机上靠眼睛确认。
fn draw_elevation_ring(frame: &mut Frame<'_>, color: u32) {
    let width = frame.width as i32;
    let height = frame.height as i32;
    // 窗口太小时内圈会盖住正文，宁可不画。
    if width < 10 || height < 10 {
        return;
    }
    // 两像素而不是一像素：整窗 alpha 0.88 与浅壁纸会把它压淡，
    // 一像素的警示线在 130 DPI 下看起来像渲染瑕疵而不是警告。
    frame.stroke_rect(2, 2, width - 4, height - 4, color);
    frame.stroke_rect(3, 3, width - 6, height - 6, color);
}

/// 一次绘制需要知道的会话状态。
#[derive(Debug, Clone, Copy)]
pub struct DrawFlags {
    /// 光标是否处于「亮」的那一半闪烁周期。
    pub cursor_visible: bool,
    /// 窗口是否有键盘焦点：决定边框颜色与光标形态（空心框 / 实心块）。
    pub focused: bool,
    /// 进程是否提权：为真时画常驻警示圈（S-01）。
    pub elevated: bool,
}

pub struct Painter {
    pub fonts: FontSet,
    pub theme: Theme,
    /// 背景透明度（0 = 背景完全不可见，只剩文字与装饰）。
    pub bg_alpha: u8,
    /// 文字透明度：只乘在字形覆盖度上，与背景互不影响。
    pub text_alpha: u8,
    cache: GlyphCache,
}

/// 设置面板的绘制输入：只带文本与选中行，不带任何状态机。
pub struct Panel<'a> {
    pub title: &'a str,
    /// 每行 (名称, 值文本)。
    pub rows: Vec<(&'a str, String)>,
    pub selected: usize,
    pub footer: &'a str,
    /// 有未保存改动时把提示条整条标出来。
    pub dirty: bool,
}

/// 标签栏的一个标签：只带标题和是否当前，不带会话。
/// 标签栏的即时状态：鼠标停在哪、当前 shell 类型怎么写。
///
/// 打包成一个结构是因为 `draw_tab_bar` 的参数已经到 7 个，
/// 再拆开两个裸参数就过不了 clippy 的 `too_many_arguments`。
pub struct TabBarState<'a> {
    pub hover: crate::render::layout::TabHit,
    pub shell_label: &'a str,
}

pub struct TabView<'a> {
    pub title: &'a str,
    pub active: bool,
}

/// 标签栏上的一段文字：不在网格里，坐标就是像素。
struct BarRun<'a> {
    x: i32,
    y: i32,
    text: &'a str,
    color: u32,
    bold: bool,
}

/// 一次文本绘制的输入：参数打包成结构体，避免函数签名长到需要关闭 lint。
struct TextRun<'a> {
    row: usize,
    x_start: i32,
    text: &'a str,
    color: u32,
    bold: bool,
}

/// 把透明度乘进字形覆盖度。
fn scale_coverage(coverage: u8, alpha: u8) -> u8 {
    match alpha {
        255 => coverage,
        0 => 0,
        a => ((u32::from(coverage) * u32::from(a) + 127) / 255) as u8,
    }
}

/// 文本在网格上占的像素宽度。
fn text_width(text: &str, layout: &Layout) -> i32 {
    text.chars()
        .map(|ch| {
            if is_wide(ch) {
                layout.cell_w * 2
            } else {
                layout.cell_w
            }
        })
        .sum()
}

/// 铺底时至少保留的 alpha。
///
/// 分层窗口（`UpdateLayeredWindow`）的命中测试是**按像素 alpha** 做的：
/// alpha 为 0 的像素既不接收鼠标、也不产生 `WM_NCHITTEST`，消息直接穿透到
/// 下面的窗口。于是「背景透明度 0」不能真的画 alpha 0，否则窗口变成一块
/// 只有字形能点中的玻璃——用户点空白处就等于点在桌面/后面的窗口上。
///
/// 1/255 的不透明度人眼看不出来（叠在白底上偏不到 1 个色阶），但足以让
/// 整块客户区保持可点击。
const MIN_PAINTED_ALPHA: u8 = 1;

/// 把用户设的透明度抬到「能点中」的下限。抽成自由函数是为了能直接单测。
fn floor_painted_alpha(alpha: u8) -> u8 {
    alpha.max(MIN_PAINTED_ALPHA)
}

impl Painter {
    /// 实际铺底用的 alpha：用户设的 0 抬到 1，其余原样。
    pub fn painted_bg_alpha(&self) -> u8 {
        floor_painted_alpha(self.bg_alpha)
    }

    pub fn new(fonts: FontSet, theme: Theme) -> Self {
        Self {
            fonts,
            theme,
            bg_alpha: 255,
            text_alpha: 255,
            cache: GlyphCache::new(),
        }
    }

    /// 主绘制入口。
    ///
    /// 会话状态打包成 [`DrawFlags`]：参数再多一个就会让调用点变成一串
    /// 无法自解释的 bool，加状态时也容易喂错位置。
    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        term: &Terminal,
        flags: DrawFlags,
        overlay: Option<&str>,
    ) {
        let DrawFlags {
            cursor_visible,
            focused,
            elevated,
        } = flags;
        frame.clear_with(self.theme.background, self.painted_bg_alpha());

        let grid = term.grid();
        let display_offset = grid.display_offset() as i32;
        let mode = term.mode();

        // 选区换成屏幕坐标（行号加回滚偏移），边界用闭区间。
        let selection = term.selection_range().map(|(start, end)| {
            (
                start.line.0 + display_offset,
                start.column.0,
                end.line.0 + display_offset,
                end.column.0,
            )
        });

        for row in 0..layout.rows {
            let line = Line(row as i32 - display_offset);
            for col in 0..layout.cols {
                let cell = &grid[line][Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                    || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let selected = selection.is_some_and(|(sl, sc, el, ec)| {
                    let row = row as i32;
                    if sl == el {
                        // 单行选区：只覆盖首尾列之间。
                        row == sl && col >= sc && col <= ec
                    } else if row == sl {
                        col >= sc
                    } else if row == el {
                        col <= ec
                    } else {
                        row > sl && row < el
                    }
                });
                self.draw_cell(frame, layout, cell, row, col, selected);
            }
        }

        if mode.contains(TermMode::SHOW_CURSOR) {
            self.draw_cursor(frame, layout, term, display_offset, cursor_visible, focused);
        }

        if let Some(text) = overlay {
            self.draw_overlay(frame, layout, text);
        }
        self.draw_border(frame, focused);
        if elevated {
            self.draw_elevation_mark(frame);
        }
    }

    fn draw_cell(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        cell: &alacritty_terminal::term::cell::Cell,
        row: usize,
        col: usize,
        selected: bool,
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

        // 选区底色优先于单元格自身的背景，否则彩色输出会把选区淹没。
        if selected {
            bg = self.theme.selection;
        }

        // 背景全透明时纯背景色差看不出来，装饰层给一个下限保证选区/底纹可辨。
        let decor_alpha = self.painted_bg_alpha().max(150);
        if bg != self.theme.background {
            frame.fill_rect_a(x0, y0, cell_width, layout.cell_h, bg, decor_alpha);
        }

        // V-38：只靠底色无法同时满足深/浅两种壁纸的对比度（0.88 透明度下，选区色
        // 与背景的亮度区间会互相挤压）。所以再压一条强调色下沿——它是「形状」线索，
        // 不依赖亮度差，白壁纸下依然可见。
        if selected {
            let edge = (layout.cell_h / 12).max(1);
            frame.fill_rect_a(
                x0,
                y0 + layout.cell_h - edge,
                cell_width,
                edge,
                self.theme.selection_edge,
                decor_alpha,
            );
        }

        if cell.c != ' ' && cell.c != '\0' {
            self.draw_glyph(
                frame,
                layout,
                cell.c,
                cell.flags,
                fg,
                x0,
                y0,
                self.text_alpha,
            );
        }

        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            let thickness = self.fonts.underline_thickness;
            let y = y0 + self.fonts.underline_y;
            frame.fill_rect_a(x0, y, cell_width, thickness, fg, self.text_alpha);
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            let y = y0 + layout.cell_h / 2;
            frame.fill_rect_a(
                x0,
                y,
                cell_width,
                self.fonts.underline_thickness,
                fg,
                self.text_alpha,
            );
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
        alpha: u8,
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
                // 文字透明度只乘在覆盖度上：背景再透，字心的 alpha 仍是 255。
                let ink = scale_coverage(coverage, alpha);
                frame.blend_pixel(left + gx as i32, top + gy as i32, color, ink);
                // 没有独立粗体文件时用双描合成：右移一像素再画一次。
                if bold && self.fonts.bold.is_none() {
                    frame.blend_pixel(
                        left + gx as i32 + 1,
                        top + gy as i32,
                        color,
                        scale_coverage(coverage / 2, alpha),
                    );
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
        // 缺字方块也要服从文字透明度：之前用 stroke_rect（满 alpha），
        // 于是「文字透明度 0.4」的帧里方块仍是全亮 —— 像素取证一眼就看出来了。
        let inset = 1;
        let alpha = self.text_alpha;
        let left = x0 + inset;
        let top = y0 + inset;
        let right = x0 + layout.cell_w - inset - 1;
        let bottom = y0 + layout.cell_h - inset - 1;
        for x in left..=right {
            frame.blend_pixel(x, top, color, alpha);
            frame.blend_pixel(x, bottom, color, alpha);
        }
        for y in top..=bottom {
            frame.blend_pixel(left, y, color, alpha);
            frame.blend_pixel(right, y, color, alpha);
        }
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
                self.text_alpha,
            );
        }
    }

    /// 画设置面板：整窗覆盖，标题 + 若干「名称 值」行 + 底部操作提示。
    ///
    /// 面板只接收**已经算好的文本**（`Panel`），不读配置、不改状态——绘制与
    /// 状态机分离是这条渲染管线的硬边界（UI 设计师的 3 条边界之一）。
    pub fn draw_panel(&mut self, frame: &mut Frame<'_>, layout: &Layout, panel: &Panel<'_>) {
        frame.clear_with(self.theme.background, self.painted_bg_alpha());

        // 标题行：加粗、前景色。
        let title_row = 0;
        self.draw_text(
            frame,
            layout,
            TextRun {
                row: title_row,
                x_start: layout.padding,
                text: panel.title,
                color: self.theme.foreground,
                bold: true,
            },
        );

        // 行从第 2 行开始，留一行空隙。
        for (index, (label, value)) in panel.rows.iter().enumerate() {
            let row = index + 2;
            if row >= layout.rows.saturating_sub(2) {
                break;
            }
            let (_, y0) = layout.cell_origin(0, row);
            if index == panel.selected {
                // 选中行：整行压一层选区色，再写文字，明暗两种壁纸下都看得见。
                frame.fill_rect(
                    layout.padding / 2,
                    y0,
                    frame.width as i32 - layout.padding,
                    layout.cell_h,
                    self.theme.selection,
                );
            }
            self.draw_text(
                frame,
                layout,
                TextRun {
                    row,
                    x_start: layout.padding,
                    text: label,
                    color: self.theme.foreground,
                    bold: index == panel.selected,
                },
            );
            // 值右对齐，避免长短不一的数字把行读成两列错位。
            let value_width = text_width(value, layout);
            let x = (frame.width as i32 - layout.padding - value_width).max(layout.padding);
            self.draw_text(
                frame,
                layout,
                TextRun {
                    row,
                    x_start: x,
                    text: value,
                    color: self.theme.foreground,
                    bold: index == panel.selected,
                },
            );
        }

        // 底部提示行。
        let footer_row = layout.rows.saturating_sub(1);
        if panel.dirty {
            frame.fill_rect(
                0,
                (frame.height as i32 - layout.cell_h).max(0),
                frame.width as i32,
                layout.cell_h,
                self.theme.selection,
            );
        }
        self.draw_text(
            frame,
            layout,
            TextRun {
                row: footer_row,
                x_start: layout.padding,
                text: panel.footer,
                color: self.theme.foreground,
                bold: false,
            },
        );
        self.draw_border(frame, true);
    }

    /// 画顶部标签栏（浏览器式导航条）。
    ///
    /// 几何全部来自 [`crate::render::layout::tab_bar`]：painter 只按算好的 x 落笔，
    /// 点击判定用的是同一份 `TabBar`，所以「看起来在哪」和「点得到哪」不可能漂移。
    /// `hover` 让鼠标悬停的格子亮一下——没有反馈的按钮等于没有按钮。
    pub fn draw_tab_bar(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        bar_h: i32,
        tabs: &[TabView<'_>],
        focused: bool,
        state: TabBarState<'_>,
    ) {
        let hover = state.hover;
        let shell_label = state.shell_label;
        use crate::render::layout::{TabHit, TAB_CLOSE_W, TAB_PAD_LEFT};

        let bar_alpha = self.painted_bg_alpha().max(160);
        frame.fill_rect_a(
            0,
            0,
            frame.width as i32,
            bar_h,
            self.theme.selection,
            bar_alpha,
        );
        // 与网格之间的分隔线：壁纸很亮时这一条让「窗口边界」还看得见。
        frame.fill_rect_a(
            0,
            bar_h - 1,
            frame.width as i32,
            1,
            self.theme.focus_ring,
            120,
        );

        let titles: Vec<&str> = tabs.iter().map(|view| view.title).collect();
        let bar = crate::render::layout::tab_bar(layout.cell_w, &titles, frame.width as i32);
        let y0 = ((bar_h - layout.cell_h) / 2).max(0);
        let muted = blend(self.theme.background, self.theme.foreground, 150);
        let cell_w = layout.cell_w.max(1);

        for (index, (view, slot)) in tabs.iter().zip(bar.slots.iter()).enumerate() {
            let color = if view.active {
                self.theme.foreground
            } else {
                muted
            };
            let hovered = matches!(hover, TabHit::Select(i) | TabHit::Close(i) if i == index);
            if hovered {
                frame.fill_rect_a(
                    slot.start,
                    1,
                    slot.end - slot.start,
                    bar_h - 2,
                    self.theme.foreground,
                    30,
                );
            }
            if view.active {
                // 当前标签：整块提到正文背景色，下沿一条强调线。
                frame.fill_rect_a(
                    slot.start,
                    1,
                    slot.end - slot.start,
                    bar_h - 2,
                    self.theme.background,
                    self.painted_bg_alpha().max(200),
                );
                frame.fill_rect_a(
                    slot.start,
                    bar_h - 3,
                    slot.end - slot.start,
                    2,
                    self.theme.focus_ring,
                    255,
                );
            }
            // 标题按可用宽度截断：叉号占位是硬约束，宁可出省略号也不能压到 × 上。
            let text_room = (slot.end - slot.start) - TAB_PAD_LEFT - TAB_CLOSE_W - 6;
            let max_chars = (text_room / cell_w).max(1) as usize;
            let title = if view.title.chars().count() > max_chars {
                let mut short: String = view
                    .title
                    .chars()
                    .take(max_chars.saturating_sub(1))
                    .collect();
                short.push('…');
                short
            } else {
                view.title.to_owned()
            };
            self.draw_bar_text(
                frame,
                layout,
                BarRun {
                    x: slot.start + TAB_PAD_LEFT,
                    y: y0,
                    text: &title,
                    color,
                    bold: view.active,
                },
            );
            // 叉号用「×」字形：比画两条斜线稳定，且免费获得抗锯齿。
            if hover == TabHit::Close(index) {
                frame.fill_rect_a(
                    slot.close_center - cell_w / 2 - 2,
                    2,
                    cell_w + 4,
                    bar_h - 4,
                    self.theme.foreground,
                    40,
                );
            }
            self.draw_bar_text(
                frame,
                layout,
                BarRun {
                    x: slot.close_center - cell_w / 2,
                    y: y0,
                    text: "×",
                    color,
                    bold: false,
                },
            );
        }

        // 按钮：+ / shell 类型 / 清屏。悬停时整块亮一下。
        let plus_w = cell_w + 8;
        if hover == TabHit::New {
            frame.fill_rect_a(
                bar.new_center - plus_w / 2,
                2,
                plus_w,
                bar_h - 4,
                self.theme.foreground,
                40,
            );
        }
        self.draw_bar_text(
            frame,
            layout,
            BarRun {
                x: bar.new_center - cell_w / 2,
                y: y0,
                text: "+",
                color: self.theme.foreground,
                bold: true,
            },
        );

        let shell_w = cell_w * 4;
        if hover == TabHit::Shell {
            frame.fill_rect_a(
                bar.shell_center - shell_w / 2,
                2,
                shell_w,
                bar_h - 4,
                self.theme.foreground,
                40,
            );
        }
        let shell_text_w = text_width(shell_label, layout);
        self.draw_bar_text(
            frame,
            layout,
            BarRun {
                x: bar.shell_center - shell_text_w / 2,
                y: y0,
                text: shell_label,
                color: muted,
                bold: false,
            },
        );

        // 「清屏」按真实字宽居中：两个汉字 = 四格，按格数估宽会把它们挤成一团。
        let clear_text = "清屏";
        let clear_w = cell_w * crate::render::layout::CLEAR_W_CELLS;
        if hover == TabHit::Clear {
            frame.fill_rect_a(
                bar.clear_center - clear_w / 2,
                2,
                clear_w,
                bar_h - 4,
                self.theme.foreground,
                40,
            );
        }
        self.draw_bar_text(
            frame,
            layout,
            BarRun {
                x: bar.clear_center - text_width(clear_text, layout) / 2,
                y: y0,
                text: clear_text,
                color: muted,
                bold: false,
            },
        );

        // 标签栏盖住了窗口上沿，边框要重描一次，否则圆角在顶部像是缺了一块。
        self.draw_border(frame, focused);
    }

    /// 画右侧回滚指示条：正在看历史时才出现。
    ///
    /// 为什么需要它：滚回历史之后终端内容不再跟随输出，屏幕上没有任何线索说明
    /// 「你正在看过去」，用户会以为程序卡住了。一条竖条 + 底部提示行把这件事说清楚。
    pub fn draw_scrollback(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        offset: usize,
        history: usize,
    ) {
        if offset == 0 || history == 0 {
            return;
        }
        let track_top = layout.top_inset + layout.padding;
        let track_h = (frame.height as i32 - track_top - layout.padding).max(1);
        let ratio = (offset.min(history) as f32 / history as f32).clamp(0.0, 1.0);
        let thumb_h = ((track_h as f32) * 0.15).max(18.0) as i32;
        let thumb_h = thumb_h.min(track_h);
        let y = track_top + (((track_h - thumb_h) as f32) * ratio) as i32;
        let x = (frame.width as i32 - layout.padding - 3).max(0);
        frame.fill_rect_a(x, y, 3, thumb_h, self.theme.focus_ring, 200);
    }

    /// 没有终端可用时（shell 退出后）画一屏：铺底 + 一行提示 + 边框。
    pub fn draw_idle(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &Layout,
        flags: DrawFlags,
        message: Option<&str>,
    ) {
        frame.clear_with(self.theme.background, self.painted_bg_alpha());
        if let Some(message) = message {
            let (x, y) = layout.cell_origin(0, 0);
            let width = frame.width as i32;
            let mut cursor = x;
            for ch in message.chars() {
                if cursor + layout.cell_w > width {
                    break;
                }
                if ch != ' ' {
                    self.draw_glyph(
                        frame,
                        layout,
                        ch,
                        Flags::empty(),
                        self.theme.foreground,
                        cursor,
                        y,
                        self.text_alpha,
                    );
                }
                cursor += layout.cell_w;
            }
        }
        if flags.elevated {
            draw_elevation_ring(frame, self.theme.elevated);
        }
        self.draw_border(frame, flags.focused);
    }

    /// 在任意像素坐标写一段等宽文本（标签栏用，不在网格里）。
    ///
    /// 参数打包成 [`BarRun`]：它们一起描述「一行字画在哪、什么颜色」，
    /// 拆成 5 个位置参数既难读，也会撞上 clippy 的参数上限。
    fn draw_bar_text(&mut self, frame: &mut Frame<'_>, layout: &Layout, run: BarRun<'_>) {
        let BarRun {
            x,
            y,
            text,
            color,
            bold,
        } = run;
        let flags = if bold { Flags::BOLD } else { Flags::empty() };
        let alpha = self.text_alpha;
        let limit = frame.width as i32;
        let mut cursor = x;
        for ch in text.chars() {
            // 宽字符（汉字）占两格：按一格步进会把「清」「屏」画在同一处，
            // 用户看到的就是两个字叠成一团。与 `draw_text` 用同一条推进规则。
            let advance = if is_wide(ch) {
                layout.cell_w * 2
            } else {
                layout.cell_w
            };
            if cursor + advance > limit {
                break;
            }
            if ch != ' ' {
                self.draw_glyph(frame, layout, ch, flags, color, cursor, y, alpha);
            }
            cursor += advance;
        }
    }

    /// 在网格坐标上写一串文本。
    ///
    /// 终端网格是等宽的，所以这里按 `cell_w` 逐格推进；宽字符占两格，
    /// 与 `draw_cell` 的推进规则保持一致。
    fn draw_text(&mut self, frame: &mut Frame<'_>, layout: &Layout, run: TextRun<'_>) {
        let TextRun {
            row,
            x_start,
            text,
            color,
            bold,
        } = run;
        if row >= layout.rows {
            return;
        }
        let (_, y0) = layout.cell_origin(0, row);
        let mut x = x_start;
        let limit = frame.width as i32;
        for ch in text.chars() {
            if x + layout.cell_w > limit {
                break;
            }
            if ch != ' ' {
                self.draw_glyph(
                    frame,
                    layout,
                    ch,
                    Flags::empty(),
                    color,
                    x,
                    y0,
                    self.text_alpha,
                );
                if bold {
                    // 没有独立粗体文件时用右移一像素双描加粗，与单元格同一套做法。
                    self.draw_glyph(
                        frame,
                        layout,
                        ch,
                        Flags::empty(),
                        color,
                        x + 1,
                        y0,
                        self.text_alpha,
                    );
                }
            }
            x += if is_wide(ch) {
                layout.cell_w * 2
            } else {
                layout.cell_w
            };
        }
    }

    /// 底部一行状态条：粘贴确认、配置告警这类提示都走它。
    ///
    /// 用一行高度换可见性，不引入任何额外的窗口或控件。
    fn draw_overlay(&mut self, frame: &mut Frame<'_>, layout: &Layout, text: &str) {
        let width = frame.width as i32;
        // 上移 2px，避免字形底缘与外框叠在一起。
        let top = (frame.height as i32 - layout.cell_h - 2).max(0);
        frame.fill_rect(0, top, width, layout.cell_h, self.theme.selection);
        let mut x = layout.padding;
        for ch in text.chars() {
            if x + layout.cell_w > width {
                break;
            }
            self.draw_glyph(
                frame,
                layout,
                ch,
                Flags::empty(),
                self.theme.foreground,
                x,
                top,
                self.text_alpha,
            );
            // 宽字符占两格，否则中文提示会挤成一团。
            x += if is_wide(ch) {
                layout.cell_w * 2
            } else {
                layout.cell_w
            };
        }
    }

    /// 提权常驻标记：边框内侧再画一圈警示色。
    ///
    /// 提权态必须有**常驻**视觉线索：无边框窗口没有任何别的东西提醒用户
    /// 「这里的操作会改动系统」，30 秒提示条过去之后就什么都不剩了（S-01）。
    /// 画在内侧而不是替换外框，是为了让「有焦点/失焦」仍然可读。
    fn draw_elevation_mark(&self, frame: &mut Frame<'_>) {
        draw_elevation_ring(frame, self.theme.elevated);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(pixels: &mut [u32], width: usize, height: usize) -> Frame<'_> {
        Frame {
            pixels,
            width,
            height,
        }
    }

    /// 背景透明度设成 0 也必须留 1/255：分层窗口按像素 alpha 做命中测试，
    /// alpha 0 的像素会把鼠标让给下面的窗口，整块客户区就点不动了。
    #[test]
    fn zero_background_alpha_still_keeps_the_surface_clickable() {
        assert_eq!(floor_painted_alpha(0), 1);
        assert_eq!(floor_painted_alpha(1), 1);
        assert_eq!(floor_painted_alpha(128), 128);
        assert_eq!(floor_painted_alpha(255), 255);
    }

    #[test]
    fn elevation_ring_sits_inside_the_border() {
        let (w, h) = (40usize, 20usize);
        let mut pixels = vec![0x0011_2233u32; w * h];
        let mark = 0x00F9_E2AFu32;
        draw_elevation_ring(&mut frame_of(&mut pixels, w, h), mark);
        // 缓冲是预乘 ARGB：颜色相同，但 alpha 是不透的 0xFF。
        let rgb = |pixel: u32| pixel & 0x00FF_FFFF;
        let at = |x: usize, y: usize| pixels[y * w + x];
        assert_eq!(rgb(at(2, 2)), mark, "内圈左上角应被染色");
        assert_eq!(rgb(at(2, h - 3)), mark, "内圈左下角应被染色");
        assert_eq!(rgb(at(3, 3)), mark, "警示圈是两像素宽");
        assert_eq!(rgb(at(w - 3, 2)), mark, "内圈右上角应被染色");
        assert_eq!(at(0, 0), 0x0011_2233, "最外圈留给焦点边框，不能被覆盖");
        assert_eq!(at(0, 2), 0x0011_2233, "外框列不能被覆盖");
        assert_eq!(at(5, 5), 0x0011_2233, "内圈以里是正文区，不能被污染");
    }

    #[test]
    fn elevation_ring_skips_tiny_windows() {
        let (w, h) = (9usize, 9usize);
        let mut pixels = vec![0x0011_2233u32; w * h];
        draw_elevation_ring(&mut frame_of(&mut pixels, w, h), 0x00FF_0000);
        assert!(pixels.iter().all(|&p| p == 0x0011_2233), "太小就不该画");
    }
}
