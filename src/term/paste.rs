//! 粘贴文本的净化与 bracketed paste 包装。
//!
//! 剪贴板里的内容是不可信输入：直接写进 PTY 等于把控制字符交给 shell 解释，
//! 一个被复制的 `\x1b]52;c;...` 或 `\x04` 就足以改写命令行。因此：
//!
//! 1. 只保留可打印字符与 `\t`、`\n`，其余 C0/DEL/C1 全部丢弃；
//! 2. `\r\n` 与裸 `\r` 统一成 `\n`（粘贴不是敲回车，不该触发执行）；
//! 3. 终端处于 bracketed paste 模式时用 `ESC[200~ ... ESC[201~` 包起来，
//!    让 shell 知道这是粘贴而不是键入，避免自动补全或立即执行。

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// 去掉控制字符并把换行归一化为 `\n`。
///
/// 保留：可打印字符、`\t`(0x09)、`\n`(0x0a)。
/// 丢弃：其余 C0（含 `\r`、`\x1b`、`\x04` 等）、DEL(0x7f)、C1(0x80..=0x9f)。
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                // CRLF 与裸 CR 都归一化成单个 LF。
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(ch),
            ch if ch.is_control() => {}
            ch if ('\u{80}'..='\u{9f}').contains(&ch) => {}
            ch => out.push(ch),
        }
    }
    out
}

/// 粘贴内容的行数（末尾换行不计入）。
pub fn line_count(text: &str) -> usize {
    let trimmed = text.trim_end_matches('\n');
    if trimmed.is_empty() {
        0
    } else {
        trimmed.lines().count()
    }
}

/// 是否需要在写入前要求二次确认（多行粘贴）。
pub fn needs_confirm(text: &str) -> bool {
    line_count(text) > 1
}

/// 生成最终写入 PTY 的字节。`bracketed` 为真时用 bracketed paste 包裹。
pub fn bytes_for_pty(sanitized: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(sanitized.len() + PASTE_START.len() + PASTE_END.len());
    if bracketed {
        out.extend_from_slice(PASTE_START);
    }
    out.extend_from_slice(sanitized.as_bytes());
    if bracketed {
        out.extend_from_slice(PASTE_END);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escape_and_control_bytes() {
        assert_eq!(sanitize("a\x1b[31mb\x04c\x7fd"), "a[31mbcd");
    }

    #[test]
    fn normalizes_crlf_and_cr() {
        assert_eq!(sanitize("one\r\ntwo\rthree"), "one\ntwo\nthree");
    }

    #[test]
    fn keeps_tab_and_newline() {
        assert_eq!(sanitize("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn drops_c1_controls() {
        assert_eq!(sanitize("a\u{9b}b"), "ab");
    }

    #[test]
    fn counts_lines_ignoring_trailing_newline() {
        assert_eq!(line_count("one\ntwo\n"), 2);
        assert_eq!(line_count("one"), 1);
        assert_eq!(line_count("\n\n"), 0);
    }

    #[test]
    fn multiline_needs_confirm() {
        assert!(needs_confirm("one\ntwo"));
        assert!(!needs_confirm("one"));
        assert!(!needs_confirm("one\n"));
    }

    #[test]
    fn wraps_with_bracketed_paste() {
        assert_eq!(bytes_for_pty("x", true), b"\x1b[200~x\x1b[201~");
        assert_eq!(bytes_for_pty("x", false), b"x");
    }

    #[test]
    fn paste_of_osc52_payload_is_neutralized() {
        // 攻击者把 OSC 52 序列放进剪贴板：净化后只剩无害文本。
        let hostile = "\x1b]52;c;ZWNobyBwd25lZA==\x07";
        let clean = sanitize(hostile);
        assert!(!clean.contains('\x1b'));
        assert!(!clean.contains('\x07'));
    }
}
