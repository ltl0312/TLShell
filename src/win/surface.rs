//! 逐像素 alpha 的绘制面：`CreateDIBSection` + `UpdateLayeredWindow`。
//!
//! 为什么要自己管一块 DIB（视觉验收 V-19 / V-02 / V-38）：
//! - `SetWindowRgn` 只能把窗口裁成硬边矩形，圆角处必然是锯齿；
//! - `SetLayeredWindowAttributes(LWA_ALPHA)` 是**整窗**透明度，文字和背景一起被稀释；
//! - 只有 `UpdateLayeredWindow(ULW_ALPHA)` + 32 位 DIB 才能做到「圆角抗锯齿、
//!   背景半透明、文字保持对比度」三者同时成立——这正是壁纸透出来的唯一正解。
//!
//! 本文件只管三件事：开/关 DIB、把绘制好的 `0x00RRGGBB` 转成预乘 BGRA、
//! 以及把结果推给窗口。它不做布局、不认识 PTY、不管理时间（与 painter 同样纪律）。

use std::ffi::c_void;

use windows::core::Result;
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, UpdateLayeredWindow, ULW_ALPHA};

/// 圆角抗锯齿的采样密度：每个像素 4×4。
const SUPERSAMPLE: u32 = 4;

/// 一块 32 位 DIB 及其设备上下文。
pub struct Surface {
    hdc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    /// DIB 的像素首地址，生命周期与 `bitmap` 绑定。
    bits: *mut u32,
    width: usize,
    height: usize,
}

// SAFETY: DIB 的像素内存由 GDI 持有，`Surface` 只在 UI 线程使用；
// 裸指针只是 GDI 交给我们写像素的入口，不跨线程共享。
unsafe impl Send for Surface {}

impl Surface {
    /// 建一块 `width × height` 的 32 位顶朝下 DIB。
    pub fn new(width: usize, height: usize) -> Result<Self> {
        let (w, h) = (width.max(1), height.max(1));
        // SAFETY: 传 None 取屏幕 DC，只用于建兼容 DC，用完立刻释放。
        let screen = unsafe { GetDC(None) };
        if screen.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }
        // SAFETY: screen 刚取到且有效。
        let hdc = unsafe { CreateCompatibleDC(Some(screen)) };
        // SAFETY: 释放上面取得的屏幕 DC。
        unsafe { ReleaseDC(None, screen) };
        if hdc.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // 负数高度 = 顶朝下，与绘制用的行序一致，省掉一次翻转。
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: info 是栈上有效的 BITMAPINFO，bits 接收像素首地址。
        let bitmap = unsafe {
            CreateDIBSection(
                Some(hdc),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                Some(HANDLE::default()),
                0,
            )
        }?;
        if bits.is_null() {
            // SAFETY: bitmap 刚创建成功，失败路径必须释放。
            unsafe {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(hdc);
            }
            return Err(windows::core::Error::from_thread());
        }
        // SAFETY: bitmap 有效，hdc 有效。
        let previous = unsafe { SelectObject(hdc, bitmap.into()) };

        Ok(Self {
            hdc,
            bitmap,
            previous,
            bits: bits.cast::<u32>(),
            width: w,
            height: h,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// 以 `0x00RRGGBB` 视角借出像素缓冲，供 `painter` 直接画。
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        // SAFETY: bits 指向 width*height 个 u32，由 GDI 分配且在本结构存活期间有效。
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.width * self.height) }
    }

    /// 把当前像素推给分层窗口：圆角按 `radius` 抗锯齿，整体再乘 `opacity`。
    pub fn present(&mut self, hwnd: HWND, radius: f32, opacity: f64) -> Result<()> {
        let (width, height) = (self.width, self.height);
        let alpha = opacity.clamp(0.0, 1.0);
        {
            let pixels = self.pixels_mut();
            apply_round_mask(pixels, width, height, radius, alpha);
        }

        let mut rect = Default::default();
        // SAFETY: rect 是栈上可写结构。
        unsafe { GetWindowRect(hwnd, &mut rect)? };
        let destination = POINT {
            x: rect.left,
            y: rect.top,
        };
        let size = SIZE {
            cx: width as i32,
            cy: height as i32,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255, // 逐像素 alpha 已经算进 A 通道
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: 四个指针都指向栈上有效结构；hdc 里选中的正是这块 DIB。
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&destination),
                Some(&size),
                Some(self.hdc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: 三个句柄都由本结构创建，且只释放一次。
        unsafe {
            let _ = SelectObject(self.hdc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.hdc);
        }
    }
}

/// 就地完成两件事：圆角覆盖率乘进 alpha、颜色按 alpha 预乘。
///
/// `UpdateLayeredWindow` 要的是**预乘** BGRA：`AC_SRC_ALPHA` 下每个通道
/// 都必须先乘 alpha，否则半透明区域会发白发灰（V-02 文字稀释就是这么来的）。
pub fn apply_round_mask(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    radius: f32,
    opacity: f64,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let Some(slot) = pixels.get_mut(index) else {
                continue;
            };
            let coverage = round_rect_coverage(x, y, width, height, radius);
            let alpha = (f64::from(coverage) * opacity * 255.0)
                .round()
                .clamp(0.0, 255.0) as u32;
            let color = *slot & 0x00FF_FFFF;
            if alpha == 0 {
                *slot = 0;
                continue;
            }
            let scale = |shift: u32| -> u32 {
                let channel = (color >> shift) & 0xFF;
                (channel * alpha + 127) / 255
            };
            *slot = (alpha << 24) | (scale(16) << 16) | (scale(8) << 8) | scale(0);
        }
    }
}

/// 像素中心点落在圆角矩形内的比例（0..=1），4×4 超采样。
pub fn round_rect_coverage(x: usize, y: usize, width: usize, height: usize, radius: f32) -> f32 {
    if radius <= 0.0 || width == 0 || height == 0 {
        return 1.0;
    }
    let radius = radius
        .min(width as f32 / 2.0)
        .min(height as f32 / 2.0)
        .max(0.0);
    if radius <= 0.0 {
        return 1.0;
    }
    let (fx, fy) = (x as f32, y as f32);
    // 只有四个角的包围盒里才需要超采样，其余像素直接算满覆盖。
    let left = fx < radius;
    let right = fx >= width as f32 - radius;
    let top = fy < radius;
    let bottom = fy >= height as f32 - radius;
    if !(left || right) || !(top || bottom) {
        return 1.0;
    }
    let center_x = if left { radius } else { width as f32 - radius };
    let center_y = if top { radius } else { height as f32 - radius };

    let step = 1.0 / SUPERSAMPLE as f32;
    let mut inside = 0u32;
    for sy in 0..SUPERSAMPLE {
        for sx in 0..SUPERSAMPLE {
            let px = fx + (sx as f32 + 0.5) * step;
            let py = fy + (sy as f32 + 0.5) * step;
            let (dx, dy) = (px - center_x, py - center_y);
            if dx * dx + dy * dy <= radius * radius {
                inside += 1;
            }
        }
    }
    inside as f32 / (SUPERSAMPLE * SUPERSAMPLE) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::GetPixel;

    /// 用 GDI 自己当裁判：我们按 `0x00RRGGBB` 写进 DIB，Windows 读出来必须还是同一个颜色。
    /// 这条错了就是整窗颜色通道颠倒（蓝灰主题会变成棕灰），比任何截图都可靠。
    #[test]
    fn dib_channel_order_matches_windows_expectation() {
        let mut surface = Surface::new(2, 2).expect("创建 DIB");
        surface.pixels_mut()[0] = 0x00FF_0000; // 约定：纯红
                                               // SAFETY: dc 是 Surface 持有的有效内存 DC，坐标在 DIB 范围内。
        let colorref = unsafe { GetPixel(surface.hdc, 0, 0) };
        assert_ne!(colorref.0, u32::MAX, "GetPixel 失败（CLR_INVALID）");
        // COLORREF = 0x00BBGGRR：红色应当在最低字节。
        assert_eq!(
            colorref.0 & 0xFF,
            0xFF,
            "0x00RRGGBB 写进 DIB 后 GDI 读到的不是红色：COLORREF=0x{:08X}",
            colorref.0
        );
    }

    #[test]
    fn interior_pixels_are_fully_opaque() {
        assert_eq!(round_rect_coverage(50, 50, 100, 60, 10.0), 1.0);
    }

    #[test]
    fn corner_is_antialiased_not_hard() {
        // 最角上那一格完全在圆外 → 必须被切掉（这是圆角本身）。
        assert_eq!(round_rect_coverage(0, 0, 100, 60, 10.0), 0.0);
        // 弧线穿过的格子必须有 0<coverage<1 —— 这一格就是抗锯齿的证据（V-19）。
        let partial: Vec<(usize, usize, f32)> = (0..10)
            .flat_map(|y| (0..10).map(move |x| (x, y)))
            .filter_map(|(x, y)| {
                let coverage = round_rect_coverage(x, y, 100, 60, 10.0);
                (coverage > 0.0 && coverage < 1.0).then_some((x, y, coverage))
            })
            .collect();
        assert!(
            !partial.is_empty(),
            "圆角没有任何过渡像素，说明是硬边（V-19）"
        );
        // 边角格子的覆盖率必须随离圆心距离单调变化，不能是 0/1 跳变。
        for (x, y, coverage) in &partial {
            assert!(
                *coverage > 0.02 && *coverage < 0.98,
                "({x},{y}) coverage={coverage} 太接近 0/1，等效于硬边"
            );
        }
    }

    #[test]
    fn zero_radius_keeps_everything_opaque() {
        assert_eq!(round_rect_coverage(0, 0, 100, 60, 0.0), 1.0);
    }

    #[test]
    fn mask_premultiplies_and_folds_opacity() {
        let mut pixels = vec![0x00FF_FFFFu32; 4];
        apply_round_mask(&mut pixels, 2, 2, 0.0, 0.5);
        // alpha = round(0.5 * 255) = 128；白色预乘后三通道都等于 128。
        assert_eq!(pixels[0], (128 << 24) | (128 << 16) | (128 << 8) | 128);
    }

    #[test]
    fn fully_transparent_pixels_become_zero() {
        let mut pixels = vec![0x00FF_FFFF_u32; 4];
        apply_round_mask(&mut pixels, 2, 2, 0.0, 0.0);
        assert!(pixels.iter().all(|pixel| *pixel == 0));
    }
}
