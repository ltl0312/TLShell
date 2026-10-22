//! 渲染层：字体度量、字形缓存、布局换算、配色、绘制。

pub mod cursor;
pub mod glyph;
pub mod layout;
pub mod metrics;
pub mod painter;
pub mod theme;

pub use cursor::Blink;
pub use layout::Layout;
pub use metrics::FontSet;
pub use painter::{Frame, Painter};
pub use theme::Theme;
