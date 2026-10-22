//! 字形光栅缓存。
//!
//! `fontdue` 每次 `rasterize` 都要重新算轮廓，一帧上千个单元格会明显掉帧，所以按
//! `(字符, 字面)` 缓存覆盖率位图。缓存只增不减，达到上限时整体清空——比实现 LRU
//! 简单，而实际工作集（一屏不同字符数）远小于上限。

use std::collections::HashMap;
use std::sync::Arc;

use crate::render::metrics::FontSet;

/// 缓存条目上限。一屏最多约 110×40 = 4400 格，其中不同字符通常不超过 1000。
const CACHE_LIMIT: usize = 4096;

/// 一张已光栅化的字形。
#[derive(Debug, Clone)]
pub struct Glyph {
    pub width: usize,
    pub height: usize,
    /// 位图左边相对笔位置的偏移。
    pub left: i32,
    /// 位图顶边相对基线的偏移（向上为正）。
    pub top: i32,
    /// 每像素一个字节的覆盖率。
    pub coverage: Vec<u8>,
}

/// 用哪套字面渲染。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Face {
    Regular,
    Bold,
    Fallback,
    BoldFallback,
}

impl Face {
    fn slot(self) -> u8 {
        match self {
            Face::Regular => 0,
            Face::Bold => 1,
            Face::Fallback => 2,
            Face::BoldFallback => 3,
        }
    }
}

#[derive(Default)]
pub struct GlyphCache {
    entries: HashMap<(u32, u8), Option<Arc<Glyph>>>,
}

impl GlyphCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取字形；缺字返回 `None`（调用方负责画缺字框）。
    pub fn get(&mut self, fonts: &FontSet, c: char, bold: bool) -> Option<Arc<Glyph>> {
        let face = resolve_face(fonts, c, bold);
        let key = (c as u32, face.slot());
        if let Some(cached) = self.entries.get(&key) {
            return cached.clone();
        }

        if self.entries.len() >= CACHE_LIMIT {
            self.entries.clear();
        }

        let glyph = rasterize(fonts, c, face).map(Arc::new);
        self.entries.insert(key, glyph.clone());
        glyph
    }
}

fn resolve_face(fonts: &FontSet, c: char, bold: bool) -> Face {
    let primary_has = fonts.primary.lookup_glyph_index(c) != 0;
    if primary_has {
        return if bold && fonts.bold.is_some() {
            Face::Bold
        } else {
            Face::Regular
        };
    }
    if let Some(fallback) = &fonts.fallback {
        if fallback.lookup_glyph_index(c) != 0 {
            return if bold {
                Face::BoldFallback
            } else {
                Face::Fallback
            };
        }
    }
    Face::Regular
}

fn rasterize(fonts: &FontSet, c: char, face: Face) -> Option<Glyph> {
    let (font, px) = match face {
        Face::Regular => (&fonts.primary, fonts.ascii_px),
        Face::Bold => (fonts.bold.as_ref()?, fonts.ascii_px),
        Face::Fallback => (fonts.fallback.as_ref()?, fonts.cjk_px),
        // 回退字体没有粗体文件时退回常规字重，由 painter 双描加粗。
        Face::BoldFallback => (fonts.fallback.as_ref()?, fonts.cjk_px),
    };

    let (metrics, coverage) = font.rasterize(c, px);
    if metrics.width == 0 || metrics.height == 0 {
        // 空格这类没有轮廓的字符：合法，返回空位图。
        return Some(Glyph {
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            coverage: Vec::new(),
        });
    }

    Some(Glyph {
        width: metrics.width,
        height: metrics.height,
        left: metrics.xmin,
        top: metrics.ymin + metrics.height as i32,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_slot_is_stable() {
        assert_eq!(Face::Regular.slot(), 0);
        assert_eq!(Face::Bold.slot(), 1);
        assert_eq!(Face::Fallback.slot(), 2);
        assert_eq!(Face::BoldFallback.slot(), 3);
    }

    #[test]
    fn distinct_faces_use_distinct_keys() {
        // 同一个字符在不同字面下必须是不同的缓存条目。
        assert_ne!(Face::Regular.slot(), Face::Bold.slot());
        assert_ne!(Face::Fallback.slot(), Face::Regular.slot());
    }
}
