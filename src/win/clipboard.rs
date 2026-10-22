//! 剪贴板写入。
//!
//! 只实现「写纯文本」一条路径，而且**只能由用户的显式动作触发**（选中后按 Ctrl+C）：
//! OSC 52 那条程序写剪贴板的通路默认是关闭的（见 `config::SecurityConfig::osc52`），
//! 因为终端里的程序不该有能力悄悄替换你的剪贴板内容。

use anyhow::{bail, Context, Result};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Foundation::HLOCAL;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// `CF_UNICODETEXT` 在 `Win32::System::Ole` 里，为它单开一个 feature 不值当。
/// 这个常量从 Windows 1.0 起就没变过，直接写数值。
const CF_UNICODETEXT: u32 = 13;

/// 把 `text` 写进系统剪贴板（覆盖原有内容）。
pub fn set_text(text: &str) -> Result<()> {
    // CF_UNICODETEXT 要求以 NUL 结尾的 UTF-16 序列。
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * std::mem::size_of::<u16>();

    // SAFETY: 空 owner 表示归当前任务所有；OpenClipboard 成功后必须配对 CloseClipboard。
    unsafe {
        OpenClipboard(None).context("打开剪贴板失败")?;

        // 从这一行开始，任何提前返回都必须先关掉剪贴板，所以用闭包收口。
        let result = write_inner(&wide, bytes);

        let _ = CloseClipboard();
        result
    }
}

/// 剪贴板已经打开的前提下写数据。调用方负责 `CloseClipboard`。
///
/// # Safety
///
/// 调用前必须已经成功 `OpenClipboard`。
unsafe fn write_inner(wide: &[u16], bytes: usize) -> Result<()> {
    // SAFETY: 剪贴板已打开，EmptyClipboard 只清空内容。
    if unsafe { EmptyClipboard() }.is_err() {
        bail!("清空剪贴板失败");
    }

    // SAFETY: GMEM_MOVEABLE 让系统接管这块内存的移动，SetClipboardData 之后不能自己释放。
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }.context("分配剪贴板内存失败")?;
    if handle.is_invalid() {
        bail!("分配剪贴板内存失败");
    }

    // SAFETY: handle 是刚分配的可写内存块。
    let locked = unsafe { GlobalLock(handle) };
    if locked.is_null() {
        // SAFETY: 分配成功但锁定失败，自己释放避免泄漏。
        unsafe {
            let _ = windows::Win32::Foundation::LocalFree(Some(HLOCAL(handle.0)));
        }
        bail!("锁定剪贴板内存失败");
    }

    // SAFETY: locked 指向 bytes 大小的可写区域，wide 正好占 bytes 字节，两者不重叠。
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), locked.cast::<u8>(), bytes);
    }

    // SAFETY: 与 GlobalLock 配对。
    unsafe {
        let _ = GlobalUnlock(handle);
    }

    // SAFETY: 所有权在这里转交给系统，成功之后不能再碰 handle。
    if unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0))) }.is_err() {
        // SAFETY: 系统没有接管，句柄仍归我们，释放是安全的。
        unsafe {
            let _ = windows::Win32::Foundation::LocalFree(Some(HLOCAL(handle.0)));
        }
        bail!("写入剪贴板失败");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// 只验证裁剪逻辑，不碰真实剪贴板——单测不该污染用户的剪贴板。
    #[test]
    fn utf16_payload_is_nul_terminated() {
        let mut wide: Vec<u16> = "中文 abc".encode_utf16().collect();
        wide.push(0);
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(wide.len(), "中文 abc".encode_utf16().count() + 1);
        assert_eq!(
            "中文 abc".encode_utf16().count() * 2,
            (wide.len() - 1) * std::mem::size_of::<u16>()
        );
    }
}
