//! 字体度量：把“字号 + 行高 + DPI”换算成“单元格像素尺寸 + 基线”。
//!
//! 关键约束（UI 规范 G2）：`Cascadia Mono` 不含 CJK 字形，必须配一个回退字体；
//! 两个字体的 advance 不同，所以 CJK 字号要单独算，让一个汉字正好占两格：
//!
//! ```text
//! cjk_px = ascii_px × 2 × ascii_advance / cjk_advance
//! ```
//!
//! 所有像素取整只发生在这里，painter 拿到的都是整数。

use anyhow::{bail, Context, Result};
use fontdue::{Font, FontSettings};

use crate::config::FontConfig;
use crate::diag;
use crate::win::dpi::BASE_DPI;
use crate::win::font as gdi;

/// 主字体候选：按优先级尝试，第一个装了的胜出。
const PRIMARY_CANDIDATES: &[&str] = &["Cascadia Mono", "Cascadia Code", "Consolas", "Courier New"];

/// CJK 回退候选。`Sarasa Mono SC` 是等宽 + 中文全覆盖，最优；其余只保证有字形。
const FALLBACK_CANDIDATES: &[&str] = &[
    "Sarasa Mono SC",
    "Sarasa Term SC",
    "Microsoft YaHei",
    "Microsoft YaHei UI",
    "SimSun",
    "MS Gothic",
];

/// 判断一个字符在终端里是否占两格。
///
/// 只覆盖实际会用到的宽字符区段（CJK、假名、谚文、全角标点、CJK 扩展 B+）。
/// 这是给绘制提示条这类“非网格文本”用的；网格里的宽度由 VT 层的 `WIDE_CHAR` 标志给出。
pub fn is_wide(ch: char) -> bool {
    let code = ch as u32;
    matches!(
        code,
        0x1100..=0x115F
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
    )
}

/// 单元格尺寸：宽高都取整到物理像素，最小 1。
pub fn cell_box(advance: f32, size_px: f32, line_height: f32) -> (i32, i32) {
    let width = advance.round() as i32;
    let height = (size_px * line_height).round() as i32;
    (width.max(1), height.max(1))
}

/// 让一个汉字占两格的 CJK 像素字号。
pub fn cjk_pixel_size(ascii_px: f32, ascii_advance: f32, cjk_advance: f32) -> f32 {
    if ascii_advance <= 0.0 || cjk_advance <= 0.0 {
        return ascii_px;
    }
    let scaled = ascii_px * (2.0 * ascii_advance) / cjk_advance;
    scaled.clamp(1.0, 512.0)
}

pub struct FontSet {
    pub primary: Font,
    pub bold: Option<Font>,
    pub fallback: Option<Font>,
    pub primary_family: String,
    pub fallback_family: Option<String>,
    pub ascii_px: f32,
    pub cjk_px: f32,
    pub cell_w: i32,
    pub cell_h: i32,
    /// 单元格顶边到基线的距离。
    pub ascent: i32,
    pub underline_y: i32,
    pub underline_thickness: i32,
}

impl FontSet {
    pub fn load(config: &FontConfig, dpi: u32) -> Result<Self> {
        let scale = dpi.max(1) as f32 / BASE_DPI as f32;
        let requested = config.size as f32;
        let line_height = config.line_height as f32;
        let ascii_px = (requested * scale).clamp(6.0, 256.0);

        let (primary, primary_family) = load_first(config.family.as_str(), PRIMARY_CANDIDATES)
            .context("找不到任何可用主字体")?;
        if !primary_family.eq_ignore_ascii_case(&config.family) {
            diag::warn(&format!(
                "字体 `{}` 未安装，改用 `{primary_family}`",
                config.family
            ));
        }

        let bold = load_bold(&primary_family);

        let (fallback, fallback_family) = if config.fallback_family.trim().is_empty() {
            load_first("", FALLBACK_CANDIDATES)
                .map_or((None, None), |(font, name)| (Some(font), Some(name)))
        } else {
            match gdi::load_family(&config.fallback_family, false) {
                Ok(data) => match parse_font(&data) {
                    Ok(font) => (Some(font), Some(config.fallback_family.clone())),
                    Err(err) => {
                        diag::warn(&format!("回退字体解析失败: {err:#}"));
                        (None, None)
                    }
                },
                Err(err) => {
                    diag::warn(&format!("回退字体不可用: {err:#}"));
                    (None, None)
                }
            }
        };

        if fallback.is_none() {
            diag::warn("没有可用的 CJK 回退字体，中文将显示为缺字方框");
        }

        let probe = if primary.lookup_glyph_index('M') != 0 {
            'M'
        } else {
            'm'
        };
        let advance = primary.metrics(probe, ascii_px).advance_width;
        let advance = if advance > 0.0 {
            advance
        } else {
            ascii_px * 0.6
        };

        let cjk_px = match &fallback {
            Some(font) => {
                let cjk_advance = font.metrics('中', ascii_px).advance_width;
                cjk_pixel_size(ascii_px, advance, cjk_advance)
            }
            None => ascii_px,
        };

        let (cell_w, cell_h) = cell_box(advance, ascii_px, line_height);

        let ascent = primary
            .horizontal_line_metrics(ascii_px)
            .map(|m| m.ascent.round() as i32)
            .unwrap_or_else(|| (ascii_px * 0.8).round() as i32)
            .clamp(1, cell_h);

        let underline_thickness = ((ascii_px / 12.0).round() as i32).max(1);
        let underline_y = (cell_h - underline_thickness - 1).max(1);

        Ok(Self {
            primary,
            bold,
            fallback,
            primary_family,
            fallback_family,
            ascii_px,
            cjk_px,
            cell_w,
            cell_h,
            ascent,
            underline_y,
            underline_thickness,
        })
    }
}

fn load_first(preferred: &str, candidates: &[&str]) -> Option<(Font, String)> {
    let mut order: Vec<&str> = Vec::new();
    if !preferred.trim().is_empty() {
        order.push(preferred);
    }
    for candidate in candidates {
        if !order
            .iter()
            .any(|name| name.eq_ignore_ascii_case(candidate))
        {
            order.push(candidate);
        }
    }

    for family in order {
        match gdi::load_family(family, false) {
            Ok(data) => match parse_font(&data) {
                Ok(font) => return Some((font, family.to_owned())),
                Err(err) => diag::warn(&format!("字体 `{family}` 解析失败: {err:#}")),
            },
            Err(err) => diag::debug(&format!("字体 `{family}` 不可用: {err:#}")),
        }
    }
    None
}

fn load_bold(family: &str) -> Option<Font> {
    let data = gdi::load_family(family, true).ok()?;
    parse_font(&data).ok()
}

fn parse_font(data: &[u8]) -> Result<Font> {
    if data.is_empty() {
        bail!("字体数据为空");
    }
    Font::from_bytes(
        data,
        FontSettings {
            collection_index: 0,
            ..FontSettings::default()
        },
    )
    .map_err(|err| anyhow::anyhow!("fontdue 解析失败: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_box_rounds_and_clamps() {
        assert_eq!(cell_box(8.0, 14.0, 1.25), (8, 18));
        assert_eq!(cell_box(7.6, 14.0, 1.25), (8, 18));
        assert_eq!(cell_box(0.0, 14.0, 1.25), (1, 18));
        assert_eq!(cell_box(8.0, 0.0, 1.25), (8, 1));
    }

    #[test]
    fn cjk_size_makes_glyph_two_cells_wide() {
        // ASCII 8px/格，CJK 在同一字号下 advance 是 14 → 想要 16px 宽。
        let px = cjk_pixel_size(14.0, 8.0, 14.0);
        assert!((px - 16.0).abs() < 0.01, "实际 {px}");
    }

    #[test]
    fn cjk_size_degrades_gracefully() {
        assert_eq!(cjk_pixel_size(14.0, 0.0, 14.0), 14.0);
        assert_eq!(cjk_pixel_size(14.0, 8.0, 0.0), 14.0);
    }

    #[test]
    fn cjk_size_is_bounded() {
        let px = cjk_pixel_size(14.0, 8.0, 0.001);
        assert!(px <= 512.0);
    }

    #[test]
    fn cell_box_handles_large_dpi() {
        // 200% 缩放下 14pt 字体约 28px。
        let (w, h) = cell_box(16.0, 28.0, 1.25);
        assert_eq!((w, h), (16, 35));
    }
}
