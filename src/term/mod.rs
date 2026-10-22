//! 终端内核：VT 状态机、PTY 会话、键盘编码、shell 解析。

pub mod grid;
pub mod input;
pub mod session;
pub mod shell;

pub use grid::{TermSize, Terminal};
