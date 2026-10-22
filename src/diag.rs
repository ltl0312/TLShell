//! 极简诊断日志。
//!
//! 设计约束（来自安全评审）：
//! 1. 落盘位置是 `%LOCALAPPDATA%\TLShell\logs\`，**不是** Roaming——日志不该跟着漫游同步。
//! 2. 默认级别 `warn`；`info` 需要 `TLSHELL_LOG=info` 才输出。
//! 3. **绝不**记录 PTY 字节流、键盘输入、剪贴板内容。本模块只接受调用方构造的短文本。
//! 4. 单文件上限 1 MiB，保留 3 个，滚动覆盖。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_FILES: usize = 3;

static SINK: Mutex<Option<File>> = Mutex::new(None);
static VERBOSE: Mutex<bool> = Mutex::new(false);

/// 日志目录：`%LOCALAPPDATA%\TLShell\logs`。
pub fn log_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TEMP").map(PathBuf::from))?;
    Some(base.join("TLShell").join("logs"))
}

/// 初始化日志。失败是静默降级：没有日志也要能跑。
pub fn init() {
    let verbose = std::env::var("TLSHELL_LOG")
        .map(|v| v.eq_ignore_ascii_case("info") || v.eq_ignore_ascii_case("debug"))
        .unwrap_or(false);
    if let Ok(mut slot) = VERBOSE.lock() {
        *slot = verbose;
    }

    let Some(dir) = log_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("tlshell.log");
    rotate_if_needed(&dir, &path);

    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        if let Ok(mut slot) = SINK.lock() {
            *slot = Some(file);
        }
    }
}

fn rotate_if_needed(dir: &std::path::Path, path: &PathBuf) {
    let too_big = fs::metadata(path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false);
    if !too_big {
        return;
    }
    // tlshell.log → tlshell.1.log → tlshell.2.log
    for index in (1..MAX_FILES).rev() {
        let from = if index == 1 {
            path.clone()
        } else {
            dir.join(format!("tlshell.{}.log", index - 1))
        };
        let to = dir.join(format!("tlshell.{index}.log"));
        if from.exists() {
            let _ = fs::rename(&from, &to);
        }
    }
}

/// 转义外部可控字符串，避免换行伪造日志行。
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().take(512) {
        match ch {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\x1b' => out.push_str("\\e"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    if text.chars().count() > 512 {
        out.push('…');
    }
    out
}

fn write_line(level: &str, message: &str) {
    let line = format!("[{level}] {message}\n");
    if let Ok(mut slot) = SINK.lock() {
        if let Some(file) = slot.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

pub fn error(message: &str) {
    write_line("error", &escape(message));
}

pub fn warn(message: &str) {
    write_line("warn", &escape(message));
}

pub fn info(message: &str) {
    let verbose = VERBOSE.lock().map(|v| *v).unwrap_or(false);
    if verbose {
        write_line("info", &escape(message));
    }
}
