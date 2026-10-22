//! 提权状态自检。
//!
//! 没有标题栏、没有任务栏按钮，窗口里也看不到「管理员」三个字——
//! 这正是安全评审 S-01 指出的问题：从管理员 shell 启动 TLShell 时，
//! 它会静默继承高完整性级别，而界面上没有任何线索。
//!
//! 这里只做一件事：查令牌的 `TokenElevation`，让调用方决定怎么提示。
//! 不尝试降权（降权需要重新创建受限令牌并重启进程，超出计划书范围）。

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 当前进程是否以提升后的权限运行。
///
/// 查询失败一律返回 `false` 并交给调用方记录，绝不把「查不到」当成「已提权」。
pub fn is_elevated() -> Result<bool> {
    // SAFETY: GetCurrentProcess 返回伪句柄，不需要也不能关闭。
    let process = unsafe { GetCurrentProcess() };
    let mut token = HANDLE::default();
    // SAFETY: token 是输出参数，句柄在 CloseHandle 前有效。
    unsafe {
        OpenProcessToken(process, TOKEN_QUERY, &mut token).context("打开进程令牌失败")?;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    // SAFETY: 缓冲区大小与 TokenElevation 的实际大小一致，返回长度写回 returned。
    let result = unsafe {
        GetTokenInformation(
            token,
            TOKEN_INFORMATION_CLASS(TokenElevation.0),
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            size,
            &mut returned,
        )
    };
    // SAFETY: 与 OpenProcessToken 配对。
    unsafe {
        let _ = CloseHandle(token);
    }
    result.context("查询令牌提权状态失败")?;
    Ok(elevation.TokenIsElevated != 0)
}
