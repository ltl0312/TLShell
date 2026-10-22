//! 分层窗口透明度。
//!
//! 方案说明（对应计划书 §9 M4 与 UI 规范 G4）：
//! 这里走 `SetLayeredWindowAttributes(LWA_ALPHA)`，即**整窗统一 alpha**。
//! 它的已知代价是文字与背景一起变透，且无法做逐像素圆角——
//! 因此圆角由渲染层自己画（`render::painter` 负责圆角遮罩），不依赖窗口区域裁剪。

use anyhow::{Context, Result};
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    GetLayeredWindowAttributes, GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW,
    GWL_EXSTYLE, LWA_ALPHA, WS_EX_LAYERED,
};

/// 给窗口打开 `WS_EX_LAYERED`。必须在第一次设置 alpha 之前调用。
pub fn ensure_layered(hwnd: HWND) -> Result<()> {
    // SAFETY: hwnd 是调用方持有的有效窗口；GWL_EXSTYLE 读写的是窗口样式位。
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_LAYERED.0 == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | WS_EX_LAYERED.0) as isize);
        }
    }
    Ok(())
}

/// 设置整窗不透明度。`opacity` 会被夹到 `[0.0, 1.0]`。
pub fn set_opacity(hwnd: HWND, opacity: f64) -> Result<()> {
    ensure_layered(hwnd)?;
    let clamped = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let alpha = (clamped * 255.0).round().clamp(0.0, 255.0) as u8;

    // SAFETY: 窗口已带 WS_EX_LAYERED；LWA_ALPHA 只影响合成阶段，不改变窗口像素内容。
    unsafe {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA)
            .context("SetLayeredWindowAttributes 失败")?;
    }
    Ok(())
}

/// 读回当前 alpha（0..=255）。未启用分层或读取失败时返回 `None`。
pub fn read_opacity(hwnd: HWND) -> Option<u8> {
    let mut alpha = 0u8;
    let mut flags = windows::Win32::UI::WindowsAndMessaging::LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
    // SAFETY: 三个出参都指向本栈帧上的可写变量。
    let ok = unsafe {
        GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
    };
    if ok {
        Some(alpha)
    } else {
        None
    }
}
