//! 输入法上屏结果的读取。
//!
//! 有些输入法（实测：微软拼音的部分版本）上屏时不发 `WM_IME_CHAR`，只在
//! `WM_IME_COMPOSITION` 里带 `GCS_RESULTSTR`。只处理前者会让「打完候选词
//! 按空格、字却没进终端」看起来像输入法坏了，所以两条路都要接。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_RESULTSTR,
};

/// 取本次 `WM_IME_COMPOSITION` 的上屏字符串。
///
/// 返回 `None` 表示没有结果串（还在组字），空串表示结果串为空。
pub fn result_string(hwnd: HWND) -> Option<String> {
    // SAFETY: 只读查询输入上下文，且 `ImmReleaseContext` 与 `ImmGetContext` 成对出现。
    unsafe {
        let context = ImmGetContext(hwnd);
        if context.is_invalid() {
            return None;
        }
        // 先问长度（字节数，UTF-16 码元数 = 字节数 / 2）。
        let bytes = ImmGetCompositionStringW(context, GCS_RESULTSTR, None, 0);
        let text = if bytes <= 0 {
            None
        } else {
            let units = bytes as usize / 2;
            let mut buf = vec![0u16; units];
            let copied = ImmGetCompositionStringW(
                context,
                GCS_RESULTSTR,
                Some(buf.as_mut_ptr().cast()),
                bytes as u32,
            );
            if copied <= 0 {
                None
            } else {
                let len = (copied as usize / 2).min(units);
                Some(String::from_utf16_lossy(&buf[..len]))
            }
        };
        let _ = ImmReleaseContext(hwnd, context);
        text
    }
}
