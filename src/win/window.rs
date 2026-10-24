//! 窗口创建与样式。
//!
//! 设计要点：
//! - `WS_POPUP`：无边框，自己处理 hit-test。
//! - `WS_EX_TOOLWINDOW`：不进任务栏、不进 Alt+Tab。
//! - `WS_EX_LAYERED`：透明度必需，创建时就带上（后续只改 alpha，不再动样式）。
//! - 不使用 winit：`SetParent` 与 winit 的窗口管理会互相打架（计划书 §4.1）。

use anyhow::{anyhow, Context, Result};
use std::ffi::c_void;
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowRect, LoadCursorW,
    RegisterClassExW, ShowWindow, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW,
    SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

use crate::diag;
use crate::win::dpi::Rect;

/// 窗口类名。全局唯一，M5 的单实例唤醒按它查找自己的窗口。
#[allow(dead_code)]
pub const CLASS_NAME: &str = "TLShellWindow";

/// 创建窗口所需的几何参数（物理像素）。
#[derive(Debug, Clone, Copy)]
pub struct WindowSpec {
    pub rect: Rect,
}

impl Default for WindowSpec {
    fn default() -> Self {
        Self {
            rect: Rect::new(CW_USEDEFAULT, CW_USEDEFAULT, 900, 420),
        }
    }
}

/// 由窗口过程回调进入应用逻辑的签名。
pub type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// 注册窗口类。重复注册视为成功（多实例场景）。
pub fn register_class(hinstance: HINSTANCE, proc: WndProc) -> Result<()> {
    // 类光标给箭头，而不是 I-beam：真正的样式由 `WM_SETCURSOR` 按「指针下面是
    // 按钮还是网格」逐个决定（应用层 `refresh_cursor`）。这里只是兜底——万一那条
    // 消息没到，整块客户区也不该长得像一个可以框选文字的文本框。
    // SAFETY: 只是加载系统预定义光标资源名，不涉及任何调用方指针。
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(proc),
        hInstance: hinstance,
        hCursor: cursor,
        lpszClassName: w!("TLShellWindow"),
        ..Default::default()
    };

    // SAFETY: wc 在调用期间存活，字符串字面量是静态宽字符。
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        let err = windows::core::Error::from_thread();
        // 类已存在不算失败：第二个实例注册同一个类会命中这里。
        if err.code().0 as u32 & 0xFFFF != 1410 {
            return Err(anyhow!("RegisterClassExW 失败: {err}"));
        }
    }
    Ok(())
}

/// 当前进程的模块句柄。
pub fn module_handle() -> Result<HINSTANCE> {
    // SAFETY: 传 None 取本进程可执行文件的模块句柄。
    let module = unsafe { GetModuleHandleW(None).context("GetModuleHandleW 失败") }?;
    Ok(HINSTANCE(module.0))
}

/// 创建主窗口。
pub fn create(hinstance: HINSTANCE, spec: &WindowSpec, app_ptr: *mut c_void) -> Result<HWND> {
    let ex_style = WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0);
    let style = WINDOW_STYLE(WS_POPUP.0);

    // SAFETY: 类已注册；app_ptr 由调用方保证在窗口存活期内有效。
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            w!("TLShellWindow"),
            w!("TLShell"),
            style,
            spec.rect.x,
            spec.rect.y,
            spec.rect.w,
            spec.rect.h,
            None,
            None,
            Some(hinstance),
            Some(app_ptr.cast_const().cast()),
        )
        .context("CreateWindowExW 失败")?
    };
    Ok(hwnd)
}

/// 显示窗口但不抢焦点（首次出现时不该打断用户当前工作）。
pub fn show(hwnd: HWND) {
    // SAFETY: hwnd 有效。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        // SAFETY: hwnd 有效，UpdateWindow 只投递一次 WM_PAINT。
        let _ = UpdateWindow(hwnd);
    }
}

pub fn destroy(hwnd: HWND) {
    // SAFETY: hwnd 有效。
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// 窗口矩形（屏幕坐标）。
pub fn window_rect(hwnd: HWND) -> Result<Rect> {
    let mut rect = RECT::default();
    // SAFETY: hwnd 有效，rect 是栈上可写结构。
    unsafe { GetWindowRect(hwnd, &mut rect).context("GetWindowRect 失败")? };
    Ok(Rect::new(
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}

/// 客户区矩形（相对窗口自身，原点恒为 0,0）。
pub fn client_rect(hwnd: HWND) -> Result<Rect> {
    let mut rect = RECT::default();
    // SAFETY: 同上。
    unsafe { GetClientRect(hwnd, &mut rect).context("GetClientRect 失败")? };
    Ok(Rect::new(
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}

/// 窗口当前 DPI。
pub fn dpi(hwnd: HWND) -> u32 {
    // SAFETY: hwnd 有效。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        crate::win::dpi::BASE_DPI
    } else {
        dpi
    }
}

/// 窗口过程的公共前置：取回与窗口绑定的应用指针。
///
/// 返回 `None` 表示消息发生在应用指针可用之前（例如 `WM_NCCREATE` 之前），
/// 调用方必须转交 `DefWindowProcW`。
pub unsafe fn app_from_window(hwnd: HWND) -> Option<*mut c_void> {
    // SAFETY: 由调用方保证在窗口过程上下文中调用。
    unsafe {
        let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
        );
        if ptr == 0 {
            None
        } else {
            Some(ptr as *mut c_void)
        }
    }
}

/// 在 `WM_NCCREATE` 时把应用指针写进窗口。
pub unsafe fn bind_app(hwnd: HWND, lparam: LPARAM) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA,
    };
    // SAFETY: WM_NCCREATE 的 lparam 指向 CREATESTRUCTW，生命周期覆盖本次调用。
    unsafe {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
}

/// 默认消息处理。
pub unsafe fn default_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: 透传给系统默认窗口过程。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 把 `COLORREF` 从 `#rrggbb` 解析出来。解析失败返回 `None`，由调用方回退默认色。
#[allow(dead_code)] // 目前没有 GDI 侧的 COLORREF 参数要传；留着给将来画原生控件用。
pub fn parse_color(text: &str) -> Option<u32> {
    let rgb = parse_rgb(text)?;
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    // COLORREF 是 0x00BBGGRR。
    Some((b << 16) | (g << 8) | r)
}

/// 解析 `#RRGGBB`，按 **`0x00RRGGBB`** 返回——这是 DIB 像素与主题色的约定。
///
/// 与 [`parse_color`] 的区别只有一个字节序，但用错整窗颜色就会通道颠倒：
/// 蓝灰主题会渲染成棕灰。绘制路径（painter → DIB）必须用这个，
/// 只有真的要交给 GDI 的 `COLORREF` 参数才用 `parse_color`。
pub fn parse_rgb(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// `WM_ERASEBKGND` 返回 1：所有像素都由我们绘制，不需要系统擦背景。
pub const ERASE_HANDLED: LRESULT = LRESULT(1);

/// 启动期自检用：把窗口样式打出来。
pub fn describe(hwnd: HWND) -> String {
    let rect = window_rect(hwnd).unwrap_or_default();
    let client = client_rect(hwnd).unwrap_or_default();
    let dpi = dpi(hwnd);
    diag::info(&format!(
        "window hwnd={:?} rect={rect:?} client={client:?} dpi={dpi}",
        hwnd.0
    ));
    format!(
        "rect={}x{}@({},{}); client={}x{}; dpi={dpi}",
        rect.w, rect.h, rect.x, rect.y, client.w, client.h
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_reads_rrggbb_into_colorref() {
        // #1e1e2e → COLORREF 0x002E1E1E
        assert_eq!(parse_color("#1e1e2e"), Some(0x002E_1E1E));
        assert_eq!(parse_color("1e1e2e"), Some(0x002E_1E1E));
    }

    #[test]
    fn parse_rgb_keeps_rrggbb_order() {
        // 绘制路径的约定（DIB 里 0x00RRGGBB）：#585b70 → 0x00585B70。
        assert_eq!(parse_rgb("#585b70"), Some(0x0058_5B70));
        assert_eq!(parse_rgb("89b4fa"), Some(0x0089_B4FA));
        assert_eq!(parse_rgb("#12345"), None);
    }

    #[test]
    fn parse_color_rejects_garbage() {
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gggggg"), None);
        assert_eq!(parse_color(""), None);
    }
}
