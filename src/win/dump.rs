//! 把绘制面原样落盘成 BMP，供无人值守的视觉验收取证。
//!
//! 为什么需要它：窗口常驻桌面最底层，屏幕区域通常被其它窗口盖住，
//! 截屏只能截到盖在上面的窗口。这里直接把**真正推送给
//! `UpdateLayeredWindow` 的像素**写成文件，验的是同一份字节，
//! 比截屏更接近事实。
//!
//! 输出是 24 位 BMP，按给定背景色合成 alpha（默认白底——白壁纸是最差条件）。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use windows::core::Result;

/// 把预乘 alpha 的 `0xAARRGGBB` 像素按 `background`（`0x00RRGGBB`）合成后写成 24 位 BMP。
pub fn write_bmp(
    path: &Path,
    pixels: &[u32],
    width: usize,
    height: usize,
    background: u32,
) -> Result<()> {
    let row_bytes = width * 3;
    let padding = (4 - row_bytes % 4) % 4;
    let image_size = (row_bytes + padding) * height;
    let file_size = 54 + image_size;

    let file = File::create(path)
        .map_err(|err| windows::core::Error::new(windows::core::HRESULT(-1), format!("{err}")))?;
    let mut out = BufWriter::new(file);
    let write_io = |out: &mut BufWriter<File>, bytes: &[u8]| -> Result<()> {
        out.write_all(bytes)
            .map_err(|err| windows::core::Error::new(windows::core::HRESULT(-1), format!("{err}")))
    };

    // BITMAPFILEHEADER
    write_io(&mut out, b"BM")?;
    write_io(&mut out, &(file_size as u32).to_le_bytes())?;
    write_io(&mut out, &0u16.to_le_bytes())?;
    write_io(&mut out, &0u16.to_le_bytes())?;
    write_io(&mut out, &54u32.to_le_bytes())?;
    // BITMAPINFOHEADER
    write_io(&mut out, &40u32.to_le_bytes())?;
    write_io(&mut out, &(width as i32).to_le_bytes())?;
    write_io(&mut out, &(height as i32).to_le_bytes())?;
    write_io(&mut out, &1u16.to_le_bytes())?;
    write_io(&mut out, &24u16.to_le_bytes())?;
    write_io(&mut out, &0u32.to_le_bytes())?;
    write_io(&mut out, &(image_size as u32).to_le_bytes())?;
    write_io(&mut out, &2835i32.to_le_bytes())?;
    write_io(&mut out, &2835i32.to_le_bytes())?;
    write_io(&mut out, &0u32.to_le_bytes())?;
    write_io(&mut out, &0u32.to_le_bytes())?;

    let pad = [0u8; 3];
    // BMP 行序自下而上。
    for y in (0..height).rev() {
        let mut row = Vec::with_capacity(row_bytes + padding);
        for x in 0..width {
            let pixel = pixels.get(y * width + x).copied().unwrap_or(0);
            let (r, g, b) = blend_over(pixel, background);
            row.push(b);
            row.push(g);
            row.push(r);
        }
        row.extend_from_slice(&pad[..padding]);
        write_io(&mut out, &row)?;
    }
    out.flush().map_err(|err| {
        windows::core::Error::new(windows::core::HRESULT(-1), format!("flush 失败: {err}"))
    })?;
    Ok(())
}

/// 预乘像素在背景色上做 `src + dst * (1 - a)`。
fn blend_over(pixel: u32, background: u32) -> (u8, u8, u8) {
    let alpha = (pixel >> 24) & 0xFF;
    let inverse = 255 - alpha;
    let channel = |shift: u32| -> u8 {
        let src = (pixel >> shift) & 0xFF;
        let dst = (background >> shift) & 0xFF;
        (src + (dst * inverse + 127) / 255).min(255) as u8
    };
    (channel(16), channel(8), channel(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_pixel_survives_blending() {
        assert_eq!(blend_over(0xFF20_4060, 0x00FF_FFFF), (0x20, 0x40, 0x60));
    }

    #[test]
    fn transparent_pixel_shows_background() {
        assert_eq!(blend_over(0x0000_0000, 0x00FF_FFFF), (255, 255, 255));
    }

    #[test]
    fn half_alpha_lands_midway() {
        // 0xAARRGGBB 预乘：alpha=128、白色文字压在白底上应接近白。
        let (r, _, _) = blend_over((128 << 24) | (128 << 16) | (128 << 8) | 128, 0x00FF_FFFF);
        assert!(r >= 250, "r={r}");
    }

    #[test]
    fn writes_a_readable_header() {
        let dir = std::env::temp_dir();
        let path = dir.join("tlshell-dump-test.bmp");
        let pixels = vec![0xFF00_0000u32; 4];
        write_bmp(&path, &pixels, 2, 2, 0x00FF_FFFF).expect("写 BMP");
        let bytes = std::fs::read(&path).expect("读回");
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(bytes.len(), 54 + 2 * 3 * 2 + 2 * 2);
        let _ = std::fs::remove_file(&path);
    }
}
