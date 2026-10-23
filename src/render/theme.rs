//! 配色：单一真源。
//!
//! 所有颜色在这里一次性从配置解析成 `0x00RRGGBB`，painter 只做算术，不再解析字符串。
//! ANSI 16 色的取值见 `docs/design/2026-10-22-tlshell-ui-spec.md`：
//! 官方 Catppuccin 的 `black`（`#45475a`）在 `#1e1e2e` 上只有 1.8:1，正文不可读，
//! 因此这里把它提到 `#6c7086`，`bright black` 用 `#9399b2`。

use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::config::ColorsConfig;
use crate::win::window::parse_rgb;

/// 打包成 `0x00RRGGBB`，与 DIB 的 `0xAARRGGBB` 存储格式共用同一条装配路径
/// （alpha 由 `surface::apply_round_mask` 在推送前补上）。
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    /// 选区下沿的强调色：白壁纸下选区底色本身分不出层次，靠这条线兜底（V-38）。
    pub selection_edge: u32,
    pub focus_ring: u32,
    /// 提权态常驻标记色。
    ///
    /// 不开放配置：管理员态是安全线索，不该被用户调成接近背景的颜色而消失（S-01）。
    pub elevated: u32,
    /// 顺序与 `NamedColor` 的 0..=15 一致。
    pub ansi: [u32; 16],
}

impl Theme {
    pub fn from_config(colors: &ColorsConfig) -> Self {
        let fallback = Self::default();
        Self {
            background: parse_rgb(&colors.background).unwrap_or(fallback.background),
            foreground: parse_rgb(&colors.foreground).unwrap_or(fallback.foreground),
            cursor: parse_rgb(&colors.cursor).unwrap_or(fallback.cursor),
            selection: parse_rgb(&colors.selection).unwrap_or(fallback.selection),
            // 强调色复用 `focus_ring`（配置里的 accent），不新增配置项：
            // 它已经要为「有焦点」保证对比度，正好也满足「选区在任何壁纸上都看得见」。
            selection_edge: parse_rgb(&colors.focus_ring).unwrap_or(fallback.selection_edge),
            focus_ring: parse_rgb(&colors.focus_ring).unwrap_or(fallback.focus_ring),
            elevated: fallback.elevated,
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
            selection_edge: 0x0089b4fa,
            focus_ring: 0x0089b4fa,
            // Catppuccin yellow：与背景 #1e1e2e 的对比度 11.9:1，任何壁纸上都在。
            elevated: 0x00f9e2af,
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
/// 把「直通色」按覆盖度合成到「预乘 ARGB」像素上。
///
/// 和 [`blend`] 的区别是这里处理的是**像素**而不是颜色：目标像素的 A 通道
/// 参与运算，源像素按覆盖度先预乘再叠加。文字画在透明背景上时，
/// `dst` 的 alpha 是 0，合成结果的 alpha 就等于覆盖度——文字自己不会变透明。
pub fn over(dst: u32, src: u32, coverage: u8) -> u32 {
    if coverage == 0 {
        return dst;
    }
    let sa = u32::from(coverage);
    if sa == 255 {
        return 0xFF00_0000 | (src & 0x00FF_FFFF);
    }
    let inv = 255 - sa;
    let dst_a = (dst >> 24) & 0xFF;
    let out_a = sa + (dst_a * inv) / 255;
    let mix = |shift: u32| -> u32 {
        let d = (dst >> shift) & 0xFF;
        let s = (src >> shift) & 0xFF;
        (s * sa + d * inv) / 255
    };
    (out_a << 24) | (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

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
    fn theme_colors_are_in_rrggbb_order() {
        // 这里错了整窗颜色会通道颠倒（蓝灰变棕灰）：绘制路径必须是 0x00RRGGBB。
        let colors = ColorsConfig {
            background: "#1e1e2e".to_owned(),
            selection: "#585b70".to_owned(),
            ..ColorsConfig::default()
        };
        let theme = Theme::from_config(&colors);
        assert_eq!(theme.background, 0x001E_1E2E);
        assert_eq!(theme.selection, 0x0058_5B70);
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

    /// 把半透明窗口色合成到给定壁纸上（整窗 alpha 走 `UpdateLayeredWindow`）。
    fn composite(color: u32, opacity: f64, wallpaper: u32) -> u32 {
        let channel = |shift: u32| -> u32 {
            let src = (color >> shift) & 0xFF;
            let dst = (wallpaper >> shift) & 0xFF;
            let value = f64::from(src) * opacity + f64::from(dst) * (1.0 - opacity);
            (value.round() as u32).min(255) << shift
        };
        channel(16) | channel(8) | channel(0)
    }

    /// WCAG 相对亮度。
    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| -> f64 {
            let value = ((color >> shift) & 0xFF) as f64 / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn selection_edge_stays_visible_on_any_wallpaper() {
        // V-38：0.88 透明度下，选区**底色**在白壁纸与深壁纸上会被挤进同一个亮度区间，
        // 单靠它是无解的（这正是当初记录下来的结论）。强调色下沿是形状线索，
        // 必须对两种极端壁纸都达到 3:1。
        let theme = Theme::default();
        // 0.70 是 UI 规范里的可读性下限；更低的透明度下连正文都读不了，
        // 那不是选区的问题，所以只在下限及以上做保证。
        for opacity in [0.88, 0.70] {
            for wallpaper in [0x00FF_FFFFu32, 0x0000_0000] {
                let background = composite(theme.background, opacity, wallpaper);
                let edge = composite(theme.selection_edge, opacity, wallpaper);
                let ratio = contrast(edge, background);
                assert!(
                    ratio >= 3.0,
                    "opacity={opacity} wallpaper=0x{wallpaper:06X}: 选区下沿对比度只有 {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn default_selection_fill_alone_splits_the_extremes_badly() {
        // 反证：同一组条件下，纯底色填充不可能两端都达标——这是选「加下沿」的理由。
        let theme = Theme::default();
        let white = composite(theme.background, 0.88, 0x00FF_FFFF);
        let fill = composite(theme.selection, 0.88, 0x00FF_FFFF);
        assert!(
            contrast(fill, white) < 3.0,
            "如果纯底色已经达标，这条兜底就不需要了"
        );
    }
}
