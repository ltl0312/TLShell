//! 应用装配与消息循环。
//!
//! 所有权约定（解决计划书 §5.2 与 §5.3-4 的冲突）：
//! - `Term` 由 UI 线程独占，PTY 读线程只把字节塞进 channel，绝不共享可变状态。
//! - 窗口过程里的一切 panic 都被 `catch_unwind` 拦住，转交 `DefWindowProcW`，
//!   因为 unwind 穿过 `extern "system"` 边界是未定义行为。

use anyhow::Result;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowLongPtrW, IsWindow, PostQuitMessage, ShowWindow,
    TranslateMessage, GWLP_USERDATA, GWL_EXSTYLE, MSG, SW_HIDE, WM_CLOSE, WM_DESTROY,
    WM_ERASEBKGND, WM_HOTKEY, WM_NCCREATE, WM_PAINT,
};

use crate::config::Config;
use crate::diag;
use crate::win::hotkey::{self, Hotkey};
use crate::win::window::{self, WindowSpec};
use crate::win::{dpi, layered};

pub mod selftest;

/// 退出热键的注册 id。
pub const HOTKEY_ID_QUIT: i32 = 1;
/// 隐藏/清屏热键的注册 id。
pub const HOTKEY_ID_HIDE: i32 = 2;

pub struct App {
    hwnd: HWND,
    config: Config,
    quit_hotkey: Option<Hotkey>,
    hide_hotkey: Option<Hotkey>,
    background: u32,
    /// 记录热键注册失败的原因，供自检与日志使用。
    pub hotkey_warnings: Vec<String>,
}

impl App {
    pub fn new(config: Config) -> Self {
        let background = window::parse_color(&config.colors.background).unwrap_or(0x002E_1E1E);
        Self {
            hwnd: HWND::default(),
            config,
            quit_hotkey: None,
            hide_hotkey: None,
            background,
            hotkey_warnings: Vec::new(),
        }
    }

    #[allow(dead_code)] // M5 单实例与重挂使用
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 自检路径里窗口先于 `App` 的字段就绪，需要补写一次。
    pub fn set_hwnd(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    #[allow(dead_code)] // M1 起渲染层读取
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 注册全局热键。失败只告警：`--quit` 永远是可用的退路。
    fn register_hotkeys(&mut self, hwnd: HWND) {
        match hotkey::parse(&self.config.keys.quit) {
            Ok(spec) => match Hotkey::register(hwnd, HOTKEY_ID_QUIT, &spec) {
                Ok(handle) => self.quit_hotkey = Some(handle),
                Err(err) => {
                    let message = format!("退出热键注册失败，请使用 `TLShell.exe --quit`: {err:#}");
                    diag::warn(&message);
                    self.hotkey_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("退出热键配置非法: {err:#}");
                diag::warn(&message);
                self.hotkey_warnings.push(message);
            }
        }

        match hotkey::parse(&self.config.keys.hide) {
            Ok(spec) => match Hotkey::register(hwnd, HOTKEY_ID_HIDE, &spec) {
                Ok(handle) => self.hide_hotkey = Some(handle),
                Err(err) => {
                    let message = format!("隐藏热键注册失败: {err:#}");
                    diag::warn(&message);
                    self.hotkey_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("隐藏热键配置非法: {err:#}");
                diag::warn(&message);
                self.hotkey_warnings.push(message);
            }
        }
    }

    /// 消息分发。返回 `LRESULT`；未处理的消息由调用方转交默认过程。
    unsafe fn handle(
        &mut self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                window::paint_solid(hwnd, self.background);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(window::ERASE_HANDLED),
            WM_HOTKEY => {
                match wparam.0 as i32 {
                    HOTKEY_ID_QUIT => {
                        diag::info("收到退出热键");
                        // SAFETY: PostQuitMessage 只投递线程消息。
                        unsafe { PostQuitMessage(0) };
                    }
                    HOTKEY_ID_HIDE => {
                        diag::info("收到隐藏热键");
                        // SAFETY: hwnd 有效。
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                // 无边框窗口没有关闭按钮；外部 WM_CLOSE 不结束会话（安全评审 SYS-6）。
                diag::info("忽略外部 WM_CLOSE");
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: PostQuitMessage 只投递线程消息。
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// 窗口过程。任何 panic 都退化为默认处理，绝不跨 FFI 边界 unwind。
pub unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 由调用方（窗口过程）保证在正确的线程与窗口上下文中。
        unsafe { dispatch(hwnd, msg, wparam, lparam) }
    }));

    match result {
        Ok(value) => value,
        Err(_) => {
            diag::error("窗口过程 panic，已降级为默认处理");
            // SAFETY: 透传系统默认过程。
            unsafe { window::default_proc(hwnd, msg, wparam, lparam) }
        }
    }
}

unsafe fn dispatch(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: WM_NCCREATE 的 lparam 是 CREATESTRUCTW。
        unsafe { window::bind_app(hwnd, lparam) };
        // SAFETY: 透传默认过程以完成非客户区创建。
        return unsafe { window::default_proc(hwnd, msg, wparam, lparam) };
    }

    // SAFETY: 只读窗口绑定的应用指针。
    let ptr = unsafe { window::app_from_window(hwnd) };
    let Some(ptr) = ptr else {
        // SAFETY: 应用指针尚未绑定，交给默认过程。
        return unsafe { window::default_proc(hwnd, msg, wparam, lparam) };
    };

    // SAFETY: 指针由 `run` 通过 `Box::leak` 提供，存活到进程结束；
    // 且窗口过程只在该 UI 线程上被调用，不存在别名竞争。
    let app = unsafe { &mut *(ptr as *mut App) };

    // SAFETY: 同上，处于窗口过程上下文。
    if let Some(result) = unsafe { app.handle(hwnd, msg, wparam, lparam) } {
        return result;
    }

    // SAFETY: 未处理的消息交给默认过程。
    unsafe { window::default_proc(hwnd, msg, wparam, lparam) }
}

/// 启动消息循环，返回进程退出码。
pub fn run(config: Config) -> Result<i32> {
    let hinstance = window::module_handle()?;
    window::register_class(hinstance, wndproc)?;

    let spec = WindowSpec {
        rect: dpi::Rect::new(
            config.window.x,
            config.window.y,
            config.window.width,
            config.window.height,
        ),
    };

    // 泄漏而不是回收：`App` 必须比窗口活得更久，且窗口过程可能在任意时刻回访它。
    // 进程退出时由操作系统回收，不存在累积泄漏。
    let app: &'static mut App = Box::leak(Box::new(App::new(config.clone())));
    let app_ptr: *mut c_void = (app as *mut App).cast();

    let hwnd = window::create(hinstance, &spec, app_ptr)?;
    app.hwnd = hwnd;

    layered::set_opacity(hwnd, app.config.window.opacity)?;
    app.register_hotkeys(hwnd);
    window::show(hwnd);

    diag::info(&format!("启动完成: {}", window::describe(hwnd)));
    Ok(message_loop())
}

fn message_loop() -> i32 {
    let mut msg = MSG::default();
    loop {
        // SAFETY: msg 是栈上可写结构，GetMessageW 会在本线程阻塞。
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match ret.0 {
            -1 => {
                diag::error("GetMessageW 失败");
                return 1;
            }
            0 => return msg.wParam.0 as i32,
            _ => {
                // SAFETY: msg 由上一步填充。
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// 给已存在的窗口发关闭请求。单实例场景下由第二个进程调用。
#[allow(dead_code)] // M5 单实例握手使用
pub fn request_close(hwnd: HWND) -> bool {
    // SAFETY: 只读查询 + 投递消息。
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(hwnd),
            WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        )
        .is_ok()
    }
}

/// 读取窗口扩展样式，供自检断言 `WS_EX_TOOLWINDOW`。
pub fn ex_style(hwnd: HWND) -> u32 {
    // SAFETY: hwnd 有效。
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
}

/// 供自检使用：把用户数据指针读回来，确认绑定成功。
pub fn bound_user_data(hwnd: HWND) -> isize {
    // SAFETY: hwnd 有效。
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) }
}
