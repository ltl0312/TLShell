//! 虚拟键 → 文本：自带一份 `ToUnicodeEx` 翻译。
//!
//! 为什么不直接等 `WM_CHAR`：`WM_CHAR` 是 `TranslateMessage` 的产物，它依赖
//! 消息队列里的扫描码与键状态。注入的按键若不带扫描码，`TranslateMessage`
//! 就什么也不产生，窗口永远收不到字符——真实探针里表现成「按键收到了 5 次、
//! 字符 0 个」，用户侧表现就是**打字没反应**。自己翻译一次，输入就不再取决于
//! 消息泵有没有替我们补上那一步；`WM_CHAR` 仍在，只是不再是唯一入口。
//!
//! 这里只做纯翻译，不做编码：按下 Ctrl/Alt 的组合仍交给 `term::input::encode`
//! （终端要发的是 `\x03` 这类控制序列，不是字符）。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, ToUnicodeEx, HKL, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

/// 单次翻译最多取出的 UTF-16 单元数。
///
/// 一个按键最多产出一个字符（含代理对的两个单元），留 8 个单元足够，
/// 也挡住 `ToUnicodeEx` 在异常键表下写出超长串的可能。
const MAX_UNITS: usize = 8;

/// 本线程当前的键盘布局句柄。
fn current_layout() -> Option<HKL> {
    // SAFETY: 只读查询；空句柄表示当前线程，不接触任何其它线程的输入状态。
    let thread = unsafe { GetWindowThreadProcessId(HWND(std::ptr::null_mut()), None) };
    // SAFETY: 同上，thread 是本线程 id。
    Some(unsafe { GetKeyboardLayout(thread) })
}

/// 把一次 `WM_KEYDOWN` 翻译成文本。
///
/// 返回空串表示「这个键在当前布局与修饰键下不产生字符」（功能键、纯修饰键、
/// 死键的第一次按下都属此类），调用方应保持原样继续走后续分支。
pub fn text_for(vk: u16, scan: u16) -> String {
    let mut state = [0u8; 256];
    // SAFETY: state 是 256 字节的栈上数组，正好是 GetKeyboardState 要求的长度。
    if unsafe { GetKeyboardState(&mut state) }.is_err() {
        return String::new();
    }
    // 注入路径下 Shift 的键状态可能没写进线程状态；按物理键状态再确认一次，
    // 否则会把 Shift+A 打成小写 a。
    // SAFETY: 只读查询。
    let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    state[VK_SHIFT.0 as usize] = if shift_down { 0x80 } else { 0 };

    let mut buf = [0u16; MAX_UNITS];
    // SAFETY: 缓冲区长度与传入长度一致；布局句柄来自当前线程。
    let written = unsafe {
        ToUnicodeEx(
            u32::from(vk),
            u32::from(scan),
            &state,
            &mut buf,
            0,
            current_layout(),
        )
    };
    // 0 = 无字符；负数 = 死键（等待下一个键），两者都不产出文本。
    if written <= 0 {
        return String::new();
    }
    let len = (written as usize).min(MAX_UNITS);
    let text = String::from_utf16_lossy(&buf[..len]);
    // Esc 在多数布局下会被翻译成 U+001B，方向键同理。它们已经由键盘分支
    // 处理成转义序列，这里必须丢掉，否则同一个键会写两遍（还写错）。
    text.chars().filter(|ch| !ch.is_control()).collect()
}

/// `VK_PACKET` 的扫描码 → 文本。
///
/// 有些注入路径（真实探针实测）把所有按键都送成 `VK_PACKET`，字符并不在线，
/// 只有扫描码是真的。这时按扫描码反查虚拟键再翻译一次，物理键盘的
/// `Shift+A`、`;`、`/` 才拿得到正确字符。
pub fn text_for_packet(scan: u16) -> String {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VSC_TO_VK_EX};
    // 扫描码是 7 位量（扩展键用 E0 前缀，不在这个字段里）。超出范围的
    // 值不可能是扫描码，交给「UTF-16 码元」那条解释去处理。
    if scan == 0 || scan >= 0x80 {
        return String::new();
    }
    // SAFETY: 纯换算，无副作用。
    let vk = unsafe { MapVirtualKeyW(u32::from(scan), MAPVK_VSC_TO_VK_EX) } as u16;
    if vk == 0 || vk == 0xe5 {
        return String::new();
    }
    text_for(vk, scan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmapped_key_yields_nothing() {
        // 0xFF 不是真实虚拟键，翻译结果必须是空串而不是乱码。
        assert!(text_for(0xff, 0).is_empty());
    }

    #[test]
    fn packet_scan_codes_are_never_read_as_raw_code_points() {
        // 真缺陷回归：把扫描码当成 UTF-16 码元解释时，敲 `ipconfig` 会写出
        // `ip.o1!i"ipconfig`（C=0x2E→'.'、N=0x31→'1'、F=0x21→'!'、G=0x22→'"'）。
        // 判据：任何扫描码都不得原样变成「数值相同的字符」。
        // 不比对具体字母——测试线程没有真实键盘布局时 `ToUnicodeEx` 可能返回空，
        // 那是环境影响，不是缺陷；字母正确性由 `--probe-real` 在真窗口上验完整词。
        for scan in [0x17_u16, 0x19, 0x2e, 0x18, 0x31, 0x21, 0x22, 0x20] {
            let got = text_for_packet(scan);
            let raw = char::from_u32(u32::from(scan)).map(String::from);
            assert_ne!(Some(got.clone()), raw, "扫描码 {scan:#04x} 被当成码元");
            assert!(got.chars().count() <= 1, "扫描码 {scan:#04x} 产出多个字符");
        }
    }

    #[test]
    fn non_scan_values_fall_through_to_the_unicode_reading() {
        // ≥0x80 不可能是扫描码，必须返回空串，让调用方按 UTF-16 码元处理（CJK）。
        for scan in [0x80_u16, 0x4e2d, 0xfffd] {
            assert!(text_for_packet(scan).is_empty(), "扫描码 {scan:#04x}");
        }
    }

    #[test]
    fn control_keys_never_produce_text() {
        // 功能键、Esc、方向键都不该被当成字符写进 PTY。
        for vk in [0x1b_u16, 0x70, 0x25, 0x26, 0x27, 0x28] {
            assert!(text_for(vk, 0).is_empty(), "vk={vk:#x} 不应产生文本");
        }
    }
}
