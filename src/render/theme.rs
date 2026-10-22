//! 配色：单一真源。
//!
//! 所有颜色在这里一次性从配置解析成 `0x00RRGGBB`，painter 只做算术，不再解析字符串。
//! ANSI 16 色的取值见 `docs/design/2026-10-22-tlshell-ui-spec.md`：
//! 官方 Catppuccin 的 `black`（`#45475a`）在 `#1e1e2e` 上只有 1.8:1，正文不可读，
//! 因此这里把它提到 `#6c7086`，`bright black` 用 `#9399b2`。

use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::config::ColorsConfig;
use crate::win::window::parse_color;

/// 打包成 `0x00RRGGBB`，与 softbuffer 的缓冲格式一致。
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    pub focus_ring: u32,
    /// 顺序与 `NamedColor` 的 0..=15 一致。
    pub ansi: [u32; 16],
}

impl Theme {
    pub fn from_config(colors: &ColorsConfig) -> Self {
        let fallback = Self::default();
        Self {
            background: parse_color(&colors.background).unwrap_or(fallback.background),
            foreground: parse_color(&colors.foreground).unwrap_or(fallback.foreground),
            cursor: parse_color(&colors.cursor).unwrap_or(fallback.cursor),
            selection: parse_color(&colors.selection).unwrap_or(fallback.selection),
            focus_ring: parse_color(&colors.focus_ring).unwrap_or(fallback.focus_ring),
            ansi: fallback.ansi,
        }
    }

    /// 把 VT 颜色解析成实际像素值。
    pub fn resolve(&self, color: Color) -> u32 {
        match color {
            Color::Spec(spec) => rgb(spec.r, spec.g, spec.b),
            Color::Indexed(index) => self.indexed(index),
            Color::Named(named) => self.named(named),
        }
    }

    /// 256 色调色板：0–15 用主题色，16–231 是 6×6×6 立方，232–255 是灰阶。
    pub fn indexed(&self, index: u8) -> u32 {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let steps = [0u8, 95, 135, 175, 215, 255];
                let r = steps[(i / 36) as usize];
                let g = steps[((i % 36) / 6) as usize];
                let b = steps[(i % 6) as usize];
                rgb(r, g, b)
            }
            232..=255 => {
                let level = 8 + (index - 232) * 10;
                rgb(level, level, level)
            }
        }
    }

    pub fn named(&self, named: NamedColor) -> u32 {
        match named {
            NamedColor::Black => self.ansi[0],
            NamedColor::Red => self.ansi[1],
            NamedColor::Green => self.ansi[2],
            NamedColor::Yellow => self.ansi[3],
            NamedColor::Blue => self.ansi[4],
            NamedColor::Magenta => self.ansi[5],
            NamedColor::Cyan => self.ansi[6],
            NamedColor::White => self.ansi[7],
            NamedColor::BrightBlack => self.ansi[8],
            NamedColor::BrightRed => self.ansi[9],
            NamedColor::BrightGreen => self.ansi[10],
            NamedColor::BrightYellow => self.ansi[11],
            NamedColor::BrightBlue => self.ansi[12],
            NamedColor::BrightMagenta => self.ansi[13],
            NamedColor::BrightCyan => self.ansi[14],
            NamedColor::BrightWhite => self.ansi[15],
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            // Dim 系列没有单独的颜色，统一落到前景色，由 painter 做暗化。
            _ => self.foreground,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: 0x001e1e2e,
            foreground: 0x00cdd6f4,
            cursor: 0x00f5e0dc,
            selection: 0x00585b70,
            focus_ring: 0x0089b4fa,
            ansi: [
                0x006c7086, // 0 black（由 #45475a 提亮，保证可读）
                0x00f38ba8, // 1 red
                0x00a6e3a1, // 2 green
                0x00f9e2af, // 3 yellow
                0x0089b4fa, // 4 blue
                0x00cba6f7, // 5 magenta
                0x0094e2d5, // 6 cyan
                0x00bac2de, // 7 white
                0x009399b2, // 8 bright black
                0x00f37799, // 9 bright red
                0x00b4f0a8, // 10 bright green
                0x00f9e2af, // 11 bright yellow
                0x0089b4fa, // 12 bright blue
                0x00cba6f7, // 13 bright magenta
                0x0094e2d5, // 14 bright cyan
                0x00cdd6f4, // 15 bright white
            ],
        }
    }
}

/// 线性插值，`t` 是 0..=255 的覆盖度。
pub fn blend(dst: u32, src: u32, coverage: u8) -> u32 {
    if coverage == 0 {
        return dst;
    }
    if coverage == 255 {
        return src;
    }
    let t = coverage as u32;
    let inv = 255 - t;
    let mix = |shift: u32| -> u32 {
        let d = (dst >> shift) & 0xff;
        let s = (src >> shift) & 0xff;
        (d * inv + s * t) / 255
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_packs_in_expected_order() {
        assert_eq!(rgb(0x1e, 0x1e, 0x2e), 0x001e1e2e);
        assert_eq!(rgb(0xff, 0x00, 0x00), 0x00ff0000);
    }

    #[test]
    fn blend_returns_endpoints_unchanged() {
        assert_eq!(blend(0x00000000, 0x00ffffff, 0), 0x00000000);
        assert_eq!(blend(0x00000000, 0x00ffffff, 255), 0x00ffffff);
    }

    #[test]
    fn blend_midpoint_is_between() {
        let mixed = blend(0x00000000, 0x00ffffff, 128);
        let r = (mixed >> 16) & 0xff;
        assert!((127..=129).contains(&r), "期望接近 128，实际 {r}");
    }

    #[test]
    fn indexed_palette_uses_theme_for_low_indices() {
        let theme = Theme::default();
        assert_eq!(theme.indexed(1), theme.ansi[1]);
        assert_eq!(theme.indexed(0), theme.ansi[0]);
    }

    #[test]
    fn indexed_cube_endpoints() {
        let theme = Theme::default();
        assert_eq!(theme.indexed(16), 0x00000000);
        assert_eq!(theme.indexed(231), 0x00ffffff);
    }

    #[test]
    fn indexed_grayscale_is_monotonic() {
        let theme = Theme::default();
        let dark = theme.indexed(232);
        let light = theme.indexed(255);
        assert!(light > dark);
        let channel = light & 0xff;
        assert_eq!(channel, 8 + 23 * 10);
    }

    #[test]
    fn named_foreground_follows_theme() {
        let theme = Theme::default();
        assert_eq!(theme.named(NamedColor::Foreground), theme.foreground);
        assert_eq!(theme.named(NamedColor::Background), theme.background);
    }

    #[test]
    fn config_overrides_are_parsed() {
        let colors = ColorsConfig {
            background: "#000000".to_owned(),
            foreground: "#ffffff".to_owned(),
            ..ColorsConfig::default()
        };
        let theme = Theme::from_config(&colors);
        assert_eq!(theme.background, 0x00000000);
        assert_eq!(theme.foreground, 0x00ffffff);
    }

    #[test]
    fn invalid_color_falls_back_to_default() {
        let colors = ColorsConfig {
            background: "not-a-color".to_owned(),
            ..ColorsConfig::default()
        };
        let theme = Theme::from_config(&colors);
        assert_eq!(theme.background, Theme::default().background);
    }
}
