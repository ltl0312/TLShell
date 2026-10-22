//! 键盘 → 字节流编码。
//!
//! 逻辑移植自 Alacritty 的 `src/input.rs`（Apache-2.0，见 `NOTICE`），按 TLShell 的
//! 输入路径裁剪：我们只从 `WM_KEYDOWN` 与 `WM_CHAR` 两条消息里取信息，没有 key-up
//! 事件，也不支持 kitty 键盘协议。
//!
//! 这里是**纯函数**，不接触 Win32，便于单测覆盖全部键位与修饰键组合。

use alacritty_terminal::term::TermMode;

/// 修饰键状态。Win 键不参与终端编码（只用于全局热键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Modifiers {
    #[allow(dead_code)] // 测试与调用方构造“无修饰键”基线用
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
    };

    /// xterm 的修饰键参数：1 + Shift(1) + Alt(2) + Ctrl(4)。
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + (u8::from(self.alt) << 1) + (u8::from(self.ctrl) << 2)
    }
}

/// 需要编码的按键。可打印字符走 [`Key::Char`]，其余是功能性按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1–F12。
    Function(u8),
}

/// 把一次按键编码成要写入 PTY 的字节。
///
/// 返回 `None` 表示这个组合在终端语义下没有对应输入（例如裸 `Shift`）。
pub fn encode(key: Key, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let bytes = match key {
        Key::Char(c) => encode_char(c, mods)?,
        Key::Enter => {
            let base = if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
                b"\n".to_vec()
            } else {
                b"\r".to_vec()
            };
            with_alt(base, mods)
        }
        Key::Tab => {
            if mods.shift {
                // Shift+Tab 就是 CBT。
                b"\x1b[Z".to_vec()
            } else {
                with_alt(b"\t".to_vec(), mods)
            }
        }
        Key::Backspace => {
            if mods.ctrl {
                // Ctrl+Backspace：删词。
                b"\x08".to_vec()
            } else {
                with_alt(b"\x7f".to_vec(), mods)
            }
        }
        Key::Escape => with_alt(b"\x1b".to_vec(), mods),
        Key::Up => cursor_key(b'A', mods, mode),
        Key::Down => cursor_key(b'B', mods, mode),
        Key::Right => cursor_key(b'C', mods, mode),
        Key::Left => cursor_key(b'D', mods, mode),
        Key::Home => cursor_key(b'H', mods, mode),
        Key::End => cursor_key(b'F', mods, mode),
        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),
        Key::Function(n) => function_key(n, mods, mode)?,
    };
    Some(bytes)
}

fn with_alt(mut bytes: Vec<u8>, mods: Modifiers) -> Vec<u8> {
    if mods.alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn encode_char(c: char, mods: Modifiers) -> Option<Vec<u8>> {
    if mods.ctrl {
        // Ctrl 组合优先按控制码解释；没有对应控制码的（如 Ctrl+中文）退回字面字符。
        if let Some(control) = control_code(c) {
            return Some(with_alt(vec![control], mods));
        }
    }
    let mut buf = [0u8; 4];
    let encoded = c.encode_utf8(&mut buf).as_bytes().to_vec();
    Some(with_alt(encoded, mods))
}

/// `Ctrl+<key>` 对应的 C0 控制码。
fn control_code(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    let code = match lower {
        ' ' | '@' => 0x00,
        'a'..='z' => (lower as u8) & 0x1f,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' | '/' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(code)
}

/// 方向键 / Home / End：应用光标模式下用 SS3，否则 CSI。
fn cursor_key(final_byte: u8, mods: Modifiers, mode: TermMode) -> Vec<u8> {
    let param = mods.param();
    if mode.contains(TermMode::APP_CURSOR) {
        if param == 1 {
            vec![0x1b, b'O', final_byte]
        } else {
            format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
        }
    } else if param == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
    }
}

/// Insert/Delete/PageUp/PageDown：CSI `n ~`。
fn tilde_key(number: u8, mods: Modifiers) -> Vec<u8> {
    let param = mods.param();
    if param == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{param}~").into_bytes()
    }
}

fn function_key(n: u8, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let param = mods.param();
    // F1–F4 在应用光标模式下走 SS3。
    let ss3 = match n {
        1 => Some(b'P'),
        2 => Some(b'Q'),
        3 => Some(b'R'),
        4 => Some(b'S'),
        _ => None,
    };
    if let Some(final_byte) = ss3 {
        return Some(if mode.contains(TermMode::APP_CURSOR) {
            if param == 1 {
                vec![0x1b, b'O', final_byte]
            } else {
                format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
            }
        } else if param == 1 {
            vec![0x1b, b'[', final_byte]
        } else {
            format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
        });
    }

    // F5–F12 的编号不是连续的。
    let number = match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(tilde_key(number, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> TermMode {
        TermMode::empty()
    }

    fn app() -> TermMode {
        TermMode::APP_CURSOR
    }

    fn text(key: Key, mods: Modifiers, mode: TermMode) -> String {
        String::from_utf8(encode(key, mods, mode).unwrap()).unwrap()
    }

    #[test]
    fn plain_letter_is_literal() {
        assert_eq!(text(Key::Char('a'), Modifiers::NONE, plain()), "a");
    }

    #[test]
    fn cjk_letter_is_utf8() {
        assert_eq!(text(Key::Char('中'), Modifiers::NONE, plain()), "中");
    }

    #[test]
    fn ctrl_letter_becomes_control_code() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(text(Key::Char('c'), ctrl, plain()), "\x03");
        assert_eq!(text(Key::Char('A'), ctrl, plain()), "\x01");
        assert_eq!(text(Key::Char(' '), ctrl, plain()), "\x00");
    }

    #[test]
    fn ctrl_bracket_family() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(text(Key::Char('['), ctrl, plain()), "\x1b");
        assert_eq!(text(Key::Char('\\'), ctrl, plain()), "\x1c");
        assert_eq!(text(Key::Char(']'), ctrl, plain()), "\x1d");
        assert_eq!(text(Key::Char('_'), ctrl, plain()), "\x1f");
    }

    #[test]
    fn alt_prefixes_escape() {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(text(Key::Char('b'), alt, plain()), "\x1bb");
    }

    #[test]
    fn enter_respects_line_feed_mode() {
        assert_eq!(text(Key::Enter, Modifiers::NONE, plain()), "\r");
        assert_eq!(
            text(Key::Enter, Modifiers::NONE, TermMode::LINE_FEED_NEW_LINE),
            "\n"
        );
    }

    #[test]
    fn backspace_is_del_by_default() {
        assert_eq!(text(Key::Backspace, Modifiers::NONE, plain()), "\x7f");
    }

    #[test]
    fn shift_tab_is_cbt() {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(text(Key::Tab, shift, plain()), "\x1b[Z");
    }

    #[test]
    fn arrows_use_csi_or_ss3() {
        assert_eq!(text(Key::Up, Modifiers::NONE, plain()), "\x1b[A");
        assert_eq!(text(Key::Up, Modifiers::NONE, app()), "\x1bOA");
    }

    #[test]
    fn arrow_with_ctrl_adds_param() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(text(Key::Right, ctrl, plain()), "\x1b[1;5C");
        assert_eq!(text(Key::Right, ctrl, app()), "\x1b[1;5C");
    }

    #[test]
    fn navigation_keys_use_tilde() {
        assert_eq!(text(Key::Delete, Modifiers::NONE, plain()), "\x1b[3~");
        assert_eq!(text(Key::PageUp, Modifiers::NONE, plain()), "\x1b[5~");
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(text(Key::PageDown, shift, plain()), "\x1b[6;2~");
    }

    #[test]
    fn function_keys_cover_the_gaps() {
        assert_eq!(text(Key::Function(1), Modifiers::NONE, plain()), "\x1b[P");
        assert_eq!(text(Key::Function(1), Modifiers::NONE, app()), "\x1bOP");
        assert_eq!(text(Key::Function(5), Modifiers::NONE, plain()), "\x1b[15~");
        assert_eq!(
            text(Key::Function(11), Modifiers::NONE, plain()),
            "\x1b[23~"
        );
        assert_eq!(
            text(Key::Function(12), Modifiers::NONE, plain()),
            "\x1b[24~"
        );
        assert!(encode(Key::Function(13), Modifiers::NONE, plain()).is_none());
    }

    #[test]
    fn home_end_follow_cursor_mode() {
        assert_eq!(text(Key::Home, Modifiers::NONE, plain()), "\x1b[H");
        assert_eq!(text(Key::End, Modifiers::NONE, app()), "\x1bOF");
    }

    #[test]
    fn modifier_param_matches_xterm() {
        assert_eq!(Modifiers::NONE.param(), 1);
        assert_eq!(
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            }
            .param(),
            2
        );
        assert_eq!(
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
            .param(),
            3
        );
        assert_eq!(
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
            .param(),
            5
        );
        assert_eq!(
            Modifiers {
                shift: true,
                ctrl: true,
                alt: true
            }
            .param(),
            8
        );
    }
}
