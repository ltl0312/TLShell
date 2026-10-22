//! 全局热键的解析与注册。
//!
//! 解析是纯逻辑（可单测）；注册失败**不是**致命错误——热键可能被其他程序占用，
//! 调用方必须能降级到 `TLShell.exe --quit`。

use anyhow::{bail, Context, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};

/// 一条已解析的热键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: HOT_KEY_MODIFIERS,
    /// 虚拟键码（`VK_*` 或 ASCII 大写字母）。
    pub vk: u32,
}

impl HotkeySpec {
    /// 转成 `MOD_*` 掩码的整数形式，便于在测试里断言。
    #[cfg(test)]
    pub fn modifier_bits(&self) -> u32 {
        self.modifiers.0
    }
}

/// 解析形如 `Ctrl+Alt+Q` 的字符串。
pub fn parse(spec: &str) -> Result<HotkeySpec> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        bail!("热键字符串为空");
    }

    let mut modifiers = MOD_NOREPEAT;
    let mut key: Option<u32> = None;
    let mut modifier_count = 0usize;

    for raw in trimmed.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            bail!("热键 `{spec}` 含空片段");
        }
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => {
                modifiers |= MOD_CONTROL;
                modifier_count += 1;
            }
            "alt" => {
                modifiers |= MOD_ALT;
                modifier_count += 1;
            }
            "shift" => {
                modifiers |= MOD_SHIFT;
                modifier_count += 1;
            }
            "win" | "super" | "meta" => {
                modifiers |= MOD_WIN;
                modifier_count += 1;
            }
            _ => {
                if key.is_some() {
                    bail!("热键 `{spec}` 含多个主键");
                }
                key =
                    Some(parse_key(part).with_context(|| format!("热键 `{spec}` 的主键无法识别"))?);
            }
        }
    }

    if modifier_count == 0 {
        bail!("热键 `{spec}` 至少需要一个修饰键（全局热键不能用裸键）");
    }

    let vk = key.ok_or_else(|| anyhow::anyhow!("热键 `{spec}` 缺少主键"))?;
    Ok(HotkeySpec { modifiers, vk })
}

fn parse_key(part: &str) -> Result<u32> {
    let upper = part.to_ascii_uppercase();

    // 单字符：字母与数字直接用 ASCII 码作为虚拟键码。
    let mut chars = upper.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Ok(c as u32);
        }
    }

    // 功能键 F1..F24。
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Ok(0x70 + n - 1);
            }
        }
    }

    let vk = match upper.as_str() {
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "DELETE" | "DEL" => 0x2E,
        "INSERT" | "INS" => 0x2D,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "`" | "BACKQUOTE" => 0xC0,
        "/" | "SLASH" => 0xBF,
        _ => bail!("未知主键 `{part}`"),
    };
    Ok(vk)
}

/// 已注册的热键。Drop 时注销。
pub struct Hotkey {
    id: i32,
    hwnd: HWND,
}

impl Hotkey {
    /// 注册热键。失败时返回错误，由调用方决定是否降级。
    pub fn register(hwnd: HWND, id: i32, spec: &HotkeySpec) -> Result<Self> {
        // SAFETY: hwnd 由调用方保证是本线程创建的有效窗口；注册表项由系统持有。
        unsafe {
            RegisterHotKey(Some(hwnd), id, spec.modifiers, spec.vk)
                .context("RegisterHotKey 失败（该组合可能已被其他程序占用）")?;
        }
        Ok(Self { id, hwnd })
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        // SAFETY: 只注销本结构注册过的 (hwnd, id) 组合，重复注销是无害的空操作。
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_quit_hotkey() {
        let spec = parse("Ctrl+Alt+Q").expect("默认热键必须可解析");
        assert_eq!(spec.vk, b'Q' as u32);
        assert_ne!(spec.modifier_bits() & MOD_CONTROL.0, 0);
        assert_ne!(spec.modifier_bits() & MOD_ALT.0, 0);
        assert_eq!(spec.modifier_bits() & MOD_SHIFT.0, 0);
    }

    #[test]
    fn parses_hide_hotkey() {
        let spec = parse("Ctrl+Alt+H").expect("隐藏热键必须可解析");
        assert_eq!(spec.vk, b'H' as u32);
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(parse("ctrl+alt+q").unwrap(), parse("CTRL+ALT+Q").unwrap());
    }

    #[test]
    fn parses_function_keys() {
        assert_eq!(parse("Ctrl+F1").unwrap().vk, 0x70);
        assert_eq!(parse("Ctrl+F12").unwrap().vk, 0x7B);
    }

    #[test]
    fn rejects_missing_modifier() {
        // 裸键做全局热键会把整个键盘吞掉，必须拒绝。
        assert!(parse("Q").is_err());
    }

    #[test]
    fn rejects_empty_and_unknown() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
        assert!(parse("Foo+Q").is_err());
        assert!(parse("Ctrl+").is_err());
    }

    #[test]
    fn rejects_two_main_keys() {
        assert!(parse("Ctrl+Q+W").is_err());
    }
}
