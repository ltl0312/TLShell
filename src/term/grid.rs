//! `Term` 的薄封装：负责尺寸适配、字节喂入与事件回收。
//!
//! 所有权约定（代码评审阻塞项①）：**`Term` 只被 UI 线程持有**，读线程只发送
//! `Vec<u8>`。这里没有任何 `Arc<Mutex<Term>>`，也没有跨线程共享。

use std::sync::mpsc::{channel, Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config as TermConfig, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

/// 网格尺寸（列 × 行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl TermSize {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

// `Dimensions` 对 `(usize, usize)` 的实现只存在于 `#[cfg(test)]` 里，必须自己实现。
impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// 把 `Term` 内部事件转交给 UI 线程。
///
/// 关键点：`Event::PtyWrite` 承载着 **DSR 应答**（`ESC[6n` → `ESC[行;列R`）。
/// PowerShell 在 ConPTY 下启动时会先发 `ESC[6n` 并阻塞等待，收不到应答就永久卡死，
/// 所以这条事件必须原样写回 PTY，不能丢。
struct EventProxy(Sender<Event>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        // 接收端随 App 一起存活；发送失败只说明正在退出，忽略即可。
        let _ = self.0.send(event);
    }
}

pub struct Terminal {
    term: Term<EventProxy>,
    processor: Processor,
    size: TermSize,
    /// 选区的两个角，都是**网格坐标**（行号不含回滚偏移）。
    ///
    /// 不用上游 `Selection`：它的 `range_simple` 在「起点落在格子右半边」时会把
    /// 首字符砍掉，反向拖选（从右往左）必然丢第一个字。自己存两个格子坐标，
    /// 语义就是「首尾两格都算选中」，与画面上看到的一致。
    sel_start: Option<Point>,
    sel_end: Option<Point>,
    /// ConPTY 的 Win32 输入模式（`CSI ? 9001 h`）。
    ///
    /// 上游 `TermMode` 没有这个标志位（开发者工具实测项 ②），只能自己记。
    /// 记录它不是为了改键盘编码——我们始终发 VT 序列，实测 PowerShell、
    /// cmd 与 vim 都工作正常——而是为了让日志能解释“程序为什么要的是另一种输入”。
    win32_input: bool,
}

impl Terminal {
    /// 建一个终端。返回 `(Terminal, 事件接收端)`。
    pub fn new(size: TermSize, scrollback: usize, osc52: Osc52) -> (Self, Receiver<Event>) {
        let size = TermSize::new(size.cols, size.rows);
        let config = TermConfig {
            scrolling_history: scrollback,
            osc52,
            ..TermConfig::default()
        };

        let (tx, rx) = channel();
        let term = Term::new(config, &size, EventProxy(tx));
        let processor = Processor::default();
        (
            Self {
                term,
                processor,
                size,
                sel_start: None,
                sel_end: None,
                win32_input: false,
            },
            rx,
        )
    }

    /// 把 PTY 字节喂给 VT 状态机。
    pub fn feed(&mut self, bytes: &[u8]) {
        self.track_win32_input(bytes);
        self.processor.advance(&mut self.term, bytes);
    }

    /// 扫一遍 `CSI ? 9001 h/l`，状态变化时记一条日志。
    ///
    /// 只在跨块边界时可能漏检（序列被两个读缓冲切开）；漏检的后果只是日志少一条，
    /// 输入路径不依赖这个标志，所以不做跨块拼接。
    fn track_win32_input(&mut self, bytes: &[u8]) {
        const ON: &[u8] = b"[?9001h";
        const OFF: &[u8] = b"[?9001l";
        if bytes.windows(ON.len()).any(|window| window == ON) && !self.win32_input {
            self.win32_input = true;
            crate::diag::info("ConPTY 切到 Win32 输入模式（?9001h）；本终端继续发送 VT 序列");
        }
        if bytes.windows(OFF.len()).any(|window| window == OFF) && self.win32_input {
            self.win32_input = false;
            crate::diag::info("ConPTY 退出 Win32 输入模式（?9001l）");
        }
    }

    /// 当前是否处于 Win32 输入模式（只用于诊断）。
    #[allow(dead_code)] // 诊断用，暂未接入 UI
    pub fn win32_input_mode(&self) -> bool {
        self.win32_input
    }

    pub fn resize(&mut self, size: TermSize) {
        let size = TermSize::new(size.cols, size.rows);
        if size == self.size {
            return;
        }
        self.term.resize(size);
        self.size = size;
    }

    #[allow(dead_code)] // M3 持久化记录列数用
    pub fn size(&self) -> TermSize {
        self.size
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn grid(&self) -> &Grid<Cell> {
        self.term.grid()
    }

    pub fn cursor_point(&self) -> Point {
        self.term.grid().cursor.point
    }

    /// 正数向上滚（看历史），负数向下滚。
    pub fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// 滚到历史缓冲的最顶端（最早那一行）。
    pub fn scroll_to_top(&mut self) {
        self.term.scroll_display(Scroll::Top);
    }

    /// 历史缓冲总行数：滚动条指示器用它算比例。
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// 选中区域的文字：逐行取格，行尾空格去掉，行间用 `\n` 连接。
    ///
    /// 宽字符的右半格（`WIDE_CHAR_SPACER`）必须跳过，否则一个汉字会变成两个字符。
    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let grid = self.term.grid();
        let columns = grid.columns();
        let mut out = String::new();
        let mut line = start.line;
        while line <= end.line {
            let first = if line == start.line {
                start.column.0
            } else {
                0
            };
            let last = if line == end.line {
                end.column.0.min(columns.saturating_sub(1))
            } else {
                columns.saturating_sub(1)
            };
            let mut text = String::new();
            if first <= last {
                for col in first..=last {
                    let cell = &grid[line][Column(col)];
                    if cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    text.push(if cell.c == '\0' { ' ' } else { cell.c });
                }
            }
            out.push_str(text.trim_end_matches(' '));
            if line != end.line {
                out.push('\n');
            }
            line += 1;
        }
        Some(out)
    }

    /// 视口顶部相对历史缓冲的偏移行数（滚回历史后为正）。
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// 当前选中的文字范围（起点、终点）。没选区或选区为空时返回 `None`。
    pub fn selection_range(&self) -> Option<(Point, Point)> {
        let (mut start, mut end) = (self.sel_start?, self.sel_end?);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if start == end {
            return None;
        }
        Some((start, end))
    }

    /// 起一个简单选区。坐标是**网格坐标**（行号已扣掉 `display_offset`）。
    pub fn start_selection(&mut self, point: Point) {
        self.sel_start = Some(point);
        self.sel_end = Some(point);
    }

    /// 拖拽时更新选区终点。
    pub fn update_selection(&mut self, point: Point) {
        if self.sel_start.is_some() {
            self.sel_end = Some(point);
        }
    }

    /// 清掉选区。
    pub fn clear_selection(&mut self) {
        self.sel_start = None;
        self.sel_end = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// 供渲染层直接按行列取单元格。
    pub fn cell(&self, line: Line, column: Column) -> &Cell {
        &self.term.grid()[line][column]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(cols: usize, rows: usize) -> Terminal {
        let (term, _rx) = Terminal::new(TermSize::new(cols, rows), 100, Osc52::Disabled);
        term
    }

    #[test]
    fn term_size_rejects_zero() {
        let size = TermSize::new(0, 0);
        assert_eq!(size.cols, 1);
        assert_eq!(size.rows, 1);
    }

    #[test]
    fn dimensions_matches_term_size() {
        let size = TermSize::new(80, 24);
        assert_eq!(size.columns(), 80);
        assert_eq!(size.screen_lines(), 24);
        assert_eq!(size.total_lines(), 24);
        assert_eq!(size.last_column(), Column(79));
        assert_eq!(size.history_size(), 0);
    }

    #[test]
    fn plain_text_lands_in_grid() {
        let mut term = terminal(20, 4);
        term.feed(b"hi");
        assert_eq!(term.cell(Line(0), Column(0)).c, 'h');
        assert_eq!(term.cell(Line(0), Column(1)).c, 'i');
        assert_eq!(term.cursor_point(), Point::new(Line(0), Column(2)));
    }

    #[test]
    fn sgr_sets_foreground_color() {
        let mut term = terminal(20, 4);
        term.feed(b"\x1b[31mR");
        let cell = term.cell(Line(0), Column(0));
        assert_eq!(cell.c, 'R');
        assert!(matches!(
            cell.fg,
            alacritty_terminal::vte::ansi::Color::Named(
                alacritty_terminal::vte::ansi::NamedColor::Red
            )
        ));
    }

    #[test]
    fn cjk_wide_char_marks_flags() {
        let mut term = terminal(20, 4);
        term.feed("中".as_bytes());
        let cell = term.cell(Line(0), Column(0));
        assert_eq!(cell.c, '中');
        assert!(cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR));
    }

    #[test]
    fn tracks_win32_input_mode_sequences() {
        let (mut term, _rx) = Terminal::new(TermSize::new(80, 24), 100, Osc52::Disabled);
        assert!(!term.win32_input_mode());
        term.feed(b"[?9001h");
        assert!(term.win32_input_mode());
        term.feed(b"[?9001l");
        assert!(!term.win32_input_mode());
    }

    #[test]
    fn dsr_query_emits_pty_write_event() {
        let (mut term, rx) = Terminal::new(TermSize::new(20, 4), 100, Osc52::Disabled);
        term.feed(b"\x1b[6n");
        let event = rx.try_recv().expect("DSR 必须产生应答事件");
        match event {
            Event::PtyWrite(text) => assert_eq!(text, "\x1b[1;1R"),
            other => panic!("期望 PtyWrite，实际 {other:?}"),
        }
    }

    #[test]
    fn selection_keeps_the_first_cell_when_dragging_backwards() {
        // 用户实测缺陷：从右往左拖，复制结果少了第一个字符（"S C:\..." 而不是 "PS C:\..."）。
        let mut term = terminal(20, 4);
        term.feed(b"PS C:\\Users");
        term.start_selection(Point::new(Line(0), Column(10)));
        term.update_selection(Point::new(Line(0), Column(0)));
        assert_eq!(term.selection_text().as_deref(), Some("PS C:\\Users"));
    }

    #[test]
    fn selection_trims_trailing_spaces_but_not_leading_ones() {
        let mut term = terminal(20, 4);
        term.feed(b"  hi  ");
        term.start_selection(Point::new(Line(0), Column(0)));
        term.update_selection(Point::new(Line(0), Column(5)));
        assert_eq!(term.selection_text().as_deref(), Some("  hi"));
    }

    #[test]
    fn selection_spans_lines_with_newlines() {
        let mut term = terminal(20, 4);
        term.feed(b"one\r\ntwo");
        term.start_selection(Point::new(Line(0), Column(1)));
        term.update_selection(Point::new(Line(1), Column(2)));
        assert_eq!(term.selection_text().as_deref(), Some("ne\ntwo"));
    }

    #[test]
    fn selection_skips_wide_char_spacers() {
        let mut term = terminal(20, 4);
        term.feed("中文".as_bytes());
        term.start_selection(Point::new(Line(0), Column(0)));
        term.update_selection(Point::new(Line(0), Column(3)));
        assert_eq!(term.selection_text().as_deref(), Some("中文"));
    }

    #[test]
    fn selection_extracts_text_from_grid() {
        let mut term = terminal(20, 4);
        term.feed(b"hello world");
        // 第 0 行第 0 列到第 4 列，也就是 "hello"。
        term.start_selection(Point::new(Line(0), Column(0)));
        term.update_selection(Point::new(Line(0), Column(4)));
        assert!(term.has_selection());
        assert_eq!(term.selection_text().as_deref(), Some("hello"));

        term.clear_selection();
        assert!(!term.has_selection());
        assert_eq!(term.selection_text(), None);
    }

    #[test]
    fn selection_survives_scrolling_into_history() {
        let (mut term, _rx) = Terminal::new(TermSize::new(20, 2), 100, Osc52::Disabled);
        for i in 0..10 {
            {
                term.feed(format!("line{i}").as_bytes());
                term.feed(b"\r\n");
            }
        }
        // 视口默认停在底部，要显式往回滚才看得到历史。
        term.scroll(5);
        assert!(term.display_offset() > 0);
        // 滚回历史后行号是负的；选区照样能取出文字。
        term.start_selection(Point::new(Line(-1), Column(0)));
        term.update_selection(Point::new(Line(-1), Column(3)));
        assert!(term.has_selection());
    }

    #[test]
    fn resize_changes_reported_size() {
        let mut term = terminal(20, 4);
        term.resize(TermSize::new(40, 10));
        assert_eq!(term.size(), TermSize::new(40, 10));
        assert_eq!(term.grid().columns(), 40);
    }

    #[test]
    fn line_feed_advances_line() {
        let mut term = terminal(20, 4);
        term.feed(b"a\r\nb");
        assert_eq!(term.cell(Line(0), Column(0)).c, 'a');
        assert_eq!(term.cell(Line(1), Column(0)).c, 'b');
    }
}
