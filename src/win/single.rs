//! 单实例与「唤醒已有窗口」。
//!
//! 用命名互斥体判重。启动时把 HWND 写进一个小文件，第二个实例读文件、
//! `IsWindow` 校验、**再确认该窗口属于同一个可执行文件**，最后用自定义消息唤醒。
//! 句柄文件是可被任意本地进程改写的数据，不能只凭它就去 `ShowWindow`/`PostMessage`。

use std::path::PathBuf;

use anyhow::Result;
use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow, PostMessageW};

/// 唤醒已有实例时投递的消息号。
pub const WAKE_MESSAGE: u32 = 0x8001;

/// 请求已有实例退出。用它而不是 `WM_CLOSE`：后者在无边框常驻窗口里会被
/// 有意忽略（外部程序不该能顺手关掉终端），退出必须是一个明确的请求。
pub const QUIT_MESSAGE: u32 = 0x8002;

/// 命名互斥体的持有者。析构即释放，进程崩溃时由系统回收。
pub struct InstanceLock(HANDLE);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // SAFETY: 句柄由 CreateMutexW 创建，只关一次。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 抢单实例。返回 `None` 表示已经有一个实例在跑。
pub fn acquire() -> Option<InstanceLock> {
    // SAFETY: 名字是静态宽字符串，不涉及调用方指针。
    let handle = unsafe { CreateMutexW(None, true, w!(r"Local\TLShell.SingleInstance")) }.ok()?;
    // SAFETY: 只读线程局部错误码。
    let already = unsafe { GetLastError() } == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
    if already {
        // SAFETY: 抢锁失败，句柄归我们，需要关掉。
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    Some(InstanceLock(handle))
}

fn hwnd_file() -> Option<PathBuf> {
    crate::config::config_dir()
        .ok()
        .map(|dir| dir.join("hwnd.txt"))
}

/// 记录当前窗口句柄，供后续实例唤醒。
pub fn remember(hwnd: isize) {
    let Some(path) = hwnd_file() else {
        crate::diag::warn("hwnd 文件不可写：配置目录不可定位");
        return;
    };
    if let Err(err) = std::fs::write(&path, format!("{hwnd}")) {
        crate::diag::warn(&format!("写入 hwnd 文件失败: {err}"));
    }
}

/// 这个句柄是否属于「和我们同一个可执行文件」的进程。
///
/// `hwnd.txt` 是一个可被任意本地进程改写的文件；只做 `IsWindow` 就
/// `ShowWindow` + `PostMessage` 等于把句柄交给别人指定的窗口（安全评审 F-04）。
fn owned_by_self(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    // SAFETY: 只读取该窗口的线程/进程 id，不解引用任何指针。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    // SAFETY: 只申请查询信息的最小权限。
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    let mut buffer = vec![0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer 与 length 都在本帧内有效，length 以 u16 元素计。
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .is_ok();
    // SAFETY: process 是本函数打开的句柄，只关闭一次。
    let _ = unsafe { CloseHandle(process) };
    if !ok {
        return false;
    }
    let image = String::from_utf16_lossy(&buffer[..length as usize]);
    let Ok(own) = std::env::current_exe() else {
        return false;
    };
    image.eq_ignore_ascii_case(&own.to_string_lossy())
}

/// 从 `hwnd.txt` 取已有的窗口，并确认它仍然存在、确实属于本程序。
///
/// `hwnd.txt` 是纯文本，任何进程都能改；少一次归属校验就等于把
/// 「PostMessage 到任意窗口」暴露给本地任意程序（安全评审 F-04）。
fn existing_window() -> Option<HWND> {
    let path = hwnd_file()?;
    let text = std::fs::read_to_string(path).ok()?;
    let value = text.trim().parse::<isize>().ok()?;
    if value == 0 {
        return None;
    }
    let hwnd = HWND(value as *mut std::ffi::c_void);
    // SAFETY: 只查询句柄有效性。
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    if !owned_by_self(hwnd) {
        crate::diag::warn("hwnd.txt 指向的窗口不属于本程序，忽略");
        return None;
    }
    Some(hwnd)
}

/// 让已有实例退出。返回 `true` 表示请求已经投递到那个窗口。
pub fn request_quit() -> bool {
    let Some(hwnd) = existing_window() else {
        return false;
    };
    // SAFETY: 目标窗口已确认属于本程序；PostMessageW 不阻塞也不解引用指针。
    unsafe {
        PostMessageW(
            Some(hwnd),
            QUIT_MESSAGE,
            Default::default(),
            Default::default(),
        )
    }
    .is_ok()
}

/// 尝试唤醒已有实例。返回是否真的唤醒成功。
pub fn wake_existing() -> bool {
    let Some(hwnd) = existing_window() else {
        return false;
    };
    // SAFETY: 目标窗口已确认属于本程序；PostMessageW 不阻塞也不解引用指针。
    // 这里不抢前台：拿到键盘焦点由窗口自己的 WAKE_MESSAGE 处理（见 dock）。
    unsafe {
        PostMessageW(
            Some(hwnd),
            WAKE_MESSAGE,
            Default::default(),
            Default::default(),
        )
    }
    .is_ok()
}

/// 供 `main` 用：抢锁失败时尽量把旧窗口叫到前面，然后安静退出。
pub fn ensure_single() -> Result<Option<InstanceLock>> {
    match acquire() {
        Some(lock) => Ok(Some(lock)),
        None => {
            if wake_existing() {
                crate::diag::info("已有实例在运行，已尝试唤醒");
            } else {
                crate::diag::warn("已有实例在运行，但没能唤醒它的窗口");
            }
            Ok(None)
        }
    }
}
