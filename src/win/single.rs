//! 单实例与「唤醒已有窗口」。
//!
//! 用命名互斥体判重，但**唤醒不能靠 `FindWindowW`**：窗口挂到 WorkerW 之后是子窗口，
//! `FindWindowW` 只搜顶层窗口，永远找不到。所以启动时把 HWND 写进一个小文件，
//! 第二个实例读文件 + `IsWindow` 校验 + `PostMessage` 自定义消息唤醒。

use std::path::PathBuf;

use anyhow::Result;
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindow, PostMessageW, SetForegroundWindow, ShowWindow, SW_SHOW,
};

/// 唤醒已有实例时投递的消息号。
pub const WAKE_MESSAGE: u32 = 0x8001;

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

fn hwnd_file() -> PathBuf {
    crate::config::config_dir().join("hwnd.txt")
}

/// 记录当前窗口句柄，供后续实例唤醒。
pub fn remember(hwnd: isize) {
    let path = hwnd_file();
    if let Err(err) = std::fs::write(&path, format!("{hwnd}")) {
        crate::diag::warn(&format!("写入 hwnd 文件失败: {err}"));
    }
}

/// 尝试唤醒已有实例。返回是否真的唤醒成功。
pub fn wake_existing() -> bool {
    let Ok(text) = std::fs::read_to_string(hwnd_file()) else {
        return false;
    };
    let Ok(value) = text.trim().parse::<isize>() else {
        return false;
    };
    if value == 0 {
        return false;
    }
    let hwnd = windows::Win32::Foundation::HWND(value as *mut std::ffi::c_void);
    // SAFETY: 只查询句柄有效性。
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    // SAFETY: 目标窗口属于同用户会话；PostMessageW 不阻塞也不解引用指针。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = PostMessageW(
            Some(hwnd),
            WAKE_MESSAGE,
            Default::default(),
            Default::default(),
        );
    }
    true
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
