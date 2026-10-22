//! 分层窗口样式位。
//!
//! 从 M6 起透明度与圆角都改走**逐像素 alpha**（`src/win/surface.rs` 的
//! `UpdateLayeredWindow(ULW_ALPHA)`），因此这里只剩一件事：确保窗口带
//! `WS_EX_LAYERED`。这是 `UpdateLayeredWindow` 的前置条件。
//!
//! 两条**不要**回退的路（对应 UI 规范 V-19 / V-02 / V-38）：
//! - `SetLayeredWindowAttributes(LWA_ALPHA)`：整窗统一 alpha，文字跟背景一起变透，
//!   且与 `UpdateLayeredWindow` 互斥——一旦调用会把逐像素 alpha 顶掉。
//! - `SetWindowRgn(CreateRoundRectRgn)`：区域裁剪是硬边，会把抗锯齿边缘整块切掉。
//!
//! 圆角现在由 `surface::round_rect_coverage` 的 4×4 超采样覆盖率乘进 alpha，
//! 边缘是半透明的过渡像素，白壁纸下不会有锯齿。

use anyhow::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED,
};

/// 给窗口打开 `WS_EX_LAYERED`。必须在第一次 `UpdateLayeredWindow` 之前调用。
pub fn ensure_layered(hwnd: HWND) -> Result<()> {
    // SAFETY: hwnd 是调用方持有的有效窗口；GWL_EXSTYLE 只读写窗口样式位。
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_LAYERED.0 == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | WS_EX_LAYERED.0) as isize);
        }
    }
    Ok(())
}
