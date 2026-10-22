//! `Term` 的薄封装：负责尺寸适配、字节喂入与事件回收。
//!
//! 所有权约定（代码评审阻塞项①）：**`Term` 只被 UI 线程持有**，读线程只发送
//! `Vec<u8>`。这里没有任何 `Arc<Mutex<Term>>`，也没有跨线程共享。

use std::sync::mpsc::{channel, Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Cell;
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
            },
            rx,
        )
    }

    /// 把 PTY 字节喂给 VT 状态机。
    pub fn feed(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
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
    #[allow(dead_code)] // M5 滚动回看用
    pub fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    #[allow(dead_code)] // M5 滚动回看用
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    #[allow(dead_code)] // M5 选中复制用
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
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
