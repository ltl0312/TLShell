//! 分层窗口透明度。
//!
//! 方案说明（对应计划书 §9 M4 与 UI 规范 G4）：
//! 这里走 `SetLayeredWindowAttributes(LWA_ALPHA)`，即**整窗统一 alpha**。
//! 它的已知代价是文字与背景一起变透，且不支持逐像素 alpha——于是圆角用
//! `SetWindowRgn(CreateRoundRectRgn)` 做**区域裁剪**：窗口被真的切成圆角矩形，
//! 四角之外直接不可见，露出壁纸。
//!
//! 区域裁剪的边缘是硬边（无抗锯齿）。要拿到抗锯齿圆角只能改走
//! `UpdateLayeredWindow` 逐像素 alpha——那会推翻 `softbuffer` 这条技术选型，
//! 超出计划书范围，因此这里接受硬边并在 README 里写明。

use anyhow::{bail, Context, Result};
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, DeleteObject, SetWindowRgn, HGDIOBJ};
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

/// 把窗口裁成圆角矩形。
///
/// `radius <= 0` 时取消裁剪。成功后区域句柄归系统所有，不能再 `DeleteObject`。
pub fn apply_round_corners(hwnd: HWND, width: i32, height: i32, radius: i32) -> Result<()> {
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    if radius <= 0 {
        // SAFETY: 传 None 表示撤销区域限制，只影响显示裁剪。
        unsafe {
            let _ = SetWindowRgn(hwnd, None, true);
        }
        return Ok(());
    }

    // 圆角半径不能超过短边的一半，否则弧线会自交。
    let max_radius = width.min(height) / 2;
    let radius = radius.min(max_radius.max(0));
    if radius <= 0 {
        // SAFETY: 同上。
        unsafe {
            let _ = SetWindowRgn(hwnd, None, true);
        }
        return Ok(());
    }

    // `CreateRoundRectRgn` 的右/下边界是开区间，所以各 +1 才能覆盖到最后一个像素。
    // SAFETY: 纯 GDI 资源创建，入参都是本函数的整数。
    let region = unsafe { CreateRoundRectRgn(0, 0, width + 1, height + 1, radius * 2, radius * 2) };
    if region.0.is_null() {
        bail!("CreateRoundRectRgn 失败");
    }

    // SAFETY: region 是刚创建的有效区域；成功后由系统接管其生命周期。
    let applied = unsafe { SetWindowRgn(hwnd, Some(region), true) };
    if applied == 0 {
        // SAFETY: 系统没有接管，必须自己释放，否则泄漏一个 GDI 区域。
        unsafe {
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
        bail!("SetWindowRgn 失败");
    }
    Ok(())
}
