//! 输入法上屏结果的读取。
//!
//! 有些输入法（实测：微软拼音的部分版本）上屏时不发 `WM_IME_CHAR`，只在
//! `WM_IME_COMPOSITION` 里带 `GCS_RESULTSTR`。只处理前者会让「打完候选词
//! 按空格、字却没进终端」看起来像输入法坏了，所以两条路都要接。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmGetConversionStatus, ImmReleaseContext,
    GCS_COMPSTR, GCS_RESULTSTR, IME_CONVERSION_MODE, IME_SENTENCE_MODE,
};

/// IME 处于「本地语言」转换模式（中文/日文假名）。搜狗、微软拼音在中文模式下
/// 都会把字母当拼音吃掉，但这个位只有问输入上下文才拿得到。
const IME_CMODE_NATIVE: u32 = 0x0001;
/// 片假名模式，同样属于「字母会被吃掉」。
const IME_CMODE_KATAKANA: u32 = 0x0002;

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

/// 当前输入法的转换模式（`IME_CMODE_*` 位组合），拿不到上下文时返回 `None`。
pub fn conversion_mode(hwnd: HWND) -> Option<u32> {
    // SAFETY: 只读查询输入上下文，释放与获取成对。
    unsafe {
        let context = ImmGetContext(hwnd);
        if context.is_invalid() {
            return None;
        }
        let mut conversion = IME_CONVERSION_MODE(0);
        let mut sentence = IME_SENTENCE_MODE(0);
        let ok = ImmGetConversionStatus(context, Some(&mut conversion), Some(&mut sentence));
        let _ = ImmReleaseContext(hwnd, context);
        ok.as_bool().then_some(conversion.0)
    }
}

/// 输入法是否处于「会把字母吃成拼音」的模式。
///
/// 为什么不能只看 [`is_composing`]：搜狗输入法组字时不往本窗口写 `GCS_COMPSTR`
/// （组字在它自己的候选窗里），`is_composing` 恒为 false，于是字母被当成普通
/// 字符写进 PTY——用户看到「打一遍拼音，回车又出现一遍中文」。
/// 转换模式不受这个差异影响，所以用它当判据。
pub fn wants_native_input(hwnd: HWND) -> bool {
    conversion_mode(hwnd).is_some_and(|mode| mode & (IME_CMODE_NATIVE | IME_CMODE_KATAKANA) != 0)
}

/// 是否正在组字（有未上屏的拼音串）。
pub fn is_composing(hwnd: HWND) -> bool {
    // SAFETY: 同上，只读查询。
    unsafe {
        let context = ImmGetContext(hwnd);
        if context.is_invalid() {
            return false;
        }
        let bytes = ImmGetCompositionStringW(context, GCS_COMPSTR, None, 0);
        let _ = ImmReleaseContext(hwnd, context);
        bytes > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有任何输入上下文时（自检窗口没有 IME 绑定），两条查询都必须安全地返回 false，
    /// 而不是崩或 panic。
    #[test]
    fn queries_are_safe_without_a_context() {
        use windows::Win32::Foundation::HWND;
        let null = HWND(std::ptr::null_mut());
        assert!(!is_composing(null));
        assert!(result_string(null).is_none());
        // 没有上下文就没有转换模式，也就不能声称「输入法要吃字母」。
        assert!(conversion_mode(null).is_none());
        assert!(!wants_native_input(null));
    }
}
