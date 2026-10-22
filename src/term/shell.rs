//! shell 解析与启动参数注入。
//!
//! 安全约束（安全评审 EXEC-1/2/3）：
//! 1. **绝不**用裸文件名交给 `CreateProcess` 搜索——那会先命中当前目录，等于把
//!    “在哪个目录启动”变成代码执行路径。
//! 2. 只允许白名单目录下的可执行文件；绝对路径也必须落在白名单内，除非用户显式
//!    在配置里打开 `security.allow_custom_shell`。
//! 3. 按**解析后的绝对路径**判断 shell 类型，而不是按用户写的名字（名字可伪造）。
//!
//! 参数注入（计划书 §4.2）：
//! - `powershell.exe`（5.1）：控制台默认不是 UTF-8，必须显式把 Input/Output 编码设为 UTF-8。
//! - `pwsh.exe`（7+）：默认 UTF-8，不注入。
//! - `cmd.exe`：注入 `chcp 65001`。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::ShellConfig;

/// 解析后的 shell 种类。判断依据是绝对路径，不是用户输入的名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell5,
    PowerShell7,
    Cmd,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShell {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub kind: ShellKind,
}

/// 允许执行的目录（绝对路径，均已规范化）。
pub fn allowed_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let root = PathBuf::from(system_root);
        dirs.push(root.join("System32"));
        dirs.push(root.join("System32").join("WindowsPowerShell").join("v1.0"));
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let root = PathBuf::from(program_files);
        dirs.push(root.join("PowerShell").join("7"));
        dirs.push(root.join("PowerShell").join("7-preview"));
    }
    dirs
}

/// 把用户写的 `program` 解析成绝对路径。
///
/// - 带路径分隔符 → 视为路径，规范化后必须存在。
/// - 裸文件名 → **只**在白名单目录里找，不做 PATH 搜索，更不做当前目录搜索。
pub fn resolve_program(program: &str) -> Result<PathBuf> {
    let trimmed = program.trim();
    if trimmed.is_empty() {
        bail!("shell.program 为空");
    }

    let has_separator = trimmed.contains('\\') || trimmed.contains('/');
    let with_ext = if Path::new(trimmed).extension().is_some() {
        trimmed.to_owned()
    } else {
        format!("{trimmed}.exe")
    };

    if has_separator {
        let candidate = PathBuf::from(&with_ext);
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("shell.program 路径不存在: {}", candidate.display()))?;
        return Ok(strip_verbatim(canonical));
    }

    let mut searched = Vec::new();
    for dir in allowed_dirs() {
        let candidate = dir.join(&with_ext);
        searched.push(candidate.display().to_string());
        if candidate.is_file() {
            // 规范化后仍要复核父目录，防止 `System32\..\..\evil.exe` 这类写法。
            let canonical = candidate
                .canonicalize()
                .with_context(|| format!("无法规范化 {}", candidate.display()))?;
            let canonical = strip_verbatim(canonical);
            if is_allowed_path(&canonical) {
                return Ok(canonical);
            }
        }
    }

    bail!(
        "在允许的目录中找不到 `{trimmed}`。已搜索: {}",
        searched.join(", ")
    )
}

/// 规范化后的路径是否落在白名单目录内。
pub fn is_allowed_path(path: &Path) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    let canonical = strip_verbatim(canonical);
    allowed_dirs().iter().any(|dir| {
        dir.canonicalize()
            .map(|d| canonical.starts_with(strip_verbatim(d)))
            .unwrap_or(false)
    })
}

/// `canonicalize` 在 Windows 上会返回 `\\?\C:\...`，去掉前缀才能正常传给 `CommandBuilder`。
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// 按绝对路径判定 shell 种类。
pub fn classify(program: &Path) -> ShellKind {
    let lower = program.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with("\\windows\\system32\\windowspowershell\\v1.0\\powershell.exe") {
        ShellKind::PowerShell5
    } else if lower.ends_with("\\powershell\\7\\pwsh.exe")
        || lower.ends_with("\\powershell\\7-preview\\pwsh.exe")
        || lower.ends_with("\\system32\\pwsh.exe")
    {
        ShellKind::PowerShell7
    } else if lower.ends_with("\\system32\\cmd.exe") {
        ShellKind::Cmd
    } else {
        ShellKind::Other
    }
}

/// PowerShell 5.1 的 UTF-8 前置脚本。
const PS5_UTF8_SETUP: &str =
    "[Console]::OutputEncoding=[Console]::InputEncoding=[Text.UTF8Encoding]::new()";

/// 按种类补启动参数。纯函数，便于单测。
///
/// 已存在 `-Command` / `-c` / `/K` / `/C` 时不注入，避免吞掉用户参数。
pub fn inject_args(kind: ShellKind, user_args: &[String]) -> Vec<String> {
    let has = |needle: &str| user_args.iter().any(|arg| arg.eq_ignore_ascii_case(needle));

    let mut args = user_args.to_vec();
    match kind {
        ShellKind::PowerShell5 => {
            if !has("-command") && !has("-c") {
                if !has("-nologo") {
                    args.push("-NoLogo".to_owned());
                }
                args.push("-NoExit".to_owned());
                args.push("-Command".to_owned());
                args.push(PS5_UTF8_SETUP.to_owned());
            }
        }
        ShellKind::Cmd => {
            if !has("/k") && !has("/c") {
                args.push("/K".to_owned());
                args.push("chcp 65001 >nul".to_owned());
            }
        }
        ShellKind::PowerShell7 | ShellKind::Other => {}
    }
    args
}

/// 完整解析：路径 + 白名单校验 + 参数注入。
pub fn resolve(config: &ShellConfig, allow_custom: bool) -> Result<ResolvedShell> {
    let program = match resolve_program(&config.program) {
        Ok(program) => program,
        Err(err) => {
            // 配置的 shell 不在机器上时，退到白名单里的另一版 PowerShell。
            // 退路同样走白名单查找，绝不交给 PATH 或当前目录。
            match fallback_program(&config.program).and_then(|name| resolve_program(name).ok()) {
                Some(program) => {
                    crate::diag::warn(&format!(
                        "`{}` 不可用，改用 {}",
                        config.program,
                        program.display()
                    ));
                    program
                }
                None => return Err(err),
            }
        }
    };
    if !allow_custom && !is_allowed_path(&program) {
        bail!(
            "shell.program 不在允许目录内: {}（如需使用请设置 security.allow_custom_shell = true）",
            program.display()
        );
    }
    let kind = classify(&program);
    let args = inject_args(kind, &config.args);
    Ok(ResolvedShell {
        program,
        args,
        kind,
    })
}

/// 双版本 PowerShell 互为退路：`pwsh.exe` 不存在时用 `powershell.exe`，反之亦然。
fn fallback_program(program: &str) -> Option<&'static str> {
    let stem = Path::new(program.trim())
        .file_stem()?
        .to_string_lossy()
        .to_ascii_lowercase();
    match stem.as_str() {
        "pwsh" => Some("powershell.exe"),
        "powershell" => Some("pwsh.exe"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn fallback_pairs_the_two_powershells() {
        assert_eq!(fallback_program("pwsh.exe"), Some("powershell.exe"));
        assert_eq!(fallback_program("pwsh"), Some("powershell.exe"));
        assert_eq!(fallback_program("powershell.exe"), Some("pwsh.exe"));
        assert_eq!(fallback_program("cmd.exe"), None);
    }

    #[test]
    fn pwsh7_gets_no_injection() {
        assert_eq!(
            inject_args(ShellKind::PowerShell7, &owned(&["-NoLogo"])),
            owned(&["-NoLogo"])
        );
    }

    #[test]
    fn powershell5_gets_utf8_setup() {
        let args = inject_args(ShellKind::PowerShell5, &owned(&["-NoLogo"]));
        assert_eq!(
            args,
            owned(&["-NoLogo", "-NoExit", "-Command", PS5_UTF8_SETUP])
        );
    }

    #[test]
    fn powershell5_adds_nologo_when_missing() {
        let args = inject_args(ShellKind::PowerShell5, &[]);
        assert_eq!(args.first().map(String::as_str), Some("-NoLogo"));
    }

    #[test]
    fn powershell5_skips_injection_when_user_passes_command() {
        let user = owned(&["-NoProfile", "-Command", "Get-Date"]);
        assert_eq!(inject_args(ShellKind::PowerShell5, &user), user);
    }

    #[test]
    fn cmd_gets_chcp() {
        let args = inject_args(ShellKind::Cmd, &[]);
        assert_eq!(args, owned(&["/K", "chcp 65001 >nul"]));
    }

    #[test]
    fn cmd_skips_injection_when_user_passes_k() {
        let user = owned(&["/K", "echo hi"]);
        assert_eq!(inject_args(ShellKind::Cmd, &user), user);
    }

    #[test]
    fn classify_uses_absolute_path_not_name() {
        assert_eq!(
            classify(Path::new(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            )),
            ShellKind::PowerShell5
        );
        assert_eq!(
            classify(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            ShellKind::PowerShell7
        );
        assert_eq!(
            classify(Path::new(r"C:\Windows\System32\cmd.exe")),
            ShellKind::Cmd
        );
        // 名字叫 powershell.exe 但不在系统目录 → 不能当成 PowerShell 5.1。
        assert_eq!(
            classify(Path::new(r"C:\tmp\powershell.exe")),
            ShellKind::Other
        );
    }

    #[test]
    fn resolve_rejects_empty_program() {
        assert!(resolve_program("   ").is_err());
    }

    #[test]
    fn resolve_rejects_relative_path_that_does_not_exist() {
        assert!(resolve_program(r".\not-a-real-shell.exe").is_err());
    }

    #[test]
    fn allowed_dirs_include_system32() {
        let dirs = allowed_dirs();
        assert!(dirs.iter().any(|d| d.ends_with("System32")));
    }
}
