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

use std::ffi::OsString;
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
    /// 配置的 shell 不存在、改用退路时记下原值，供界面提示（F-13：回退不能是静默的）。
    pub fallback_from: Option<String>,
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
/// 已存在 `-Command` / `/K` / `/C` 时不再注入命令本身，避免吞掉用户参数；
/// 但 `-NoProfile` 与 `cmd /D` 属于加固项，无论用户怎么写都会补上。
pub fn inject_args(kind: ShellKind, user_args: &[String]) -> Vec<String> {
    // PowerShell 允许参数前缀缩写（`-c`、`-com`、`-nop` 都合法），
    // 所以判定必须是前缀匹配，否则 `-Com` 会被漏掉、我们再注入一次 `-Command`。
    let has_pwsh = |long: &str, min_prefix: usize| {
        let long = long.to_ascii_lowercase();
        user_args.iter().any(|arg| {
            arg.strip_prefix('-').is_some_and(|raw| {
                let raw = raw.to_ascii_lowercase();
                raw.len() >= min_prefix && raw.len() <= long.len() && long.starts_with(&raw)
            })
        })
    };
    let has_slash = |needle: &str| user_args.iter().any(|arg| arg.eq_ignore_ascii_case(needle));

    let mut args = user_args.to_vec();
    match kind {
        ShellKind::PowerShell5 => {
            if !has_pwsh("command", 1) {
                if !has_pwsh("nologo", 3) {
                    args.push("-NoLogo".to_owned());
                }
                args.push("-NoExit".to_owned());
                args.push("-Command".to_owned());
                args.push(PS5_UTF8_SETUP.to_owned());
            }
            // `-NoProfile` 必须在 -Command 之前，否则 PowerShell 会把它当成命令文本。
            if !has_pwsh("noprofile", 3) {
                args.insert(0, "-NoProfile".to_owned());
            }
        }
        ShellKind::Cmd => {
            // `/D` 关掉 AutoRun：注册表里的 AutoRun 命令会在每个 cmd 实例里跑一遍。
            if !has_slash("/d") {
                args.insert(0, "/D".to_owned());
            }
            if !has_slash("/k") && !has_slash("/c") {
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
    let mut fallback_from = None;
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
                    fallback_from = Some(config.program.clone());
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
        fallback_from,
    })
}

/// 传给子进程的环境变量白名单（安全评审 S-03 的 24 项）。
///
/// 只继承这些名字；其余一律不传，特别是云凭据、令牌、`KUBECONFIG`、
/// `NODE_OPTIONS`、`PYTHONPATH`、`BASH_ENV`、`JAVA_TOOL_OPTIONS`、
/// `RUSTC_WRAPPER`、`CL`、`GIT_SSH*`、`__COMPAT_LAYER`、各类 `*_proxy`。
pub const ENV_ALLOWLIST: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "USERDOMAIN",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_ARCHITEW6432",
    "NUMBER_OF_PROCESSORS",
];

/// 构造子进程环境：白名单继承 + 六项自算（ComSpec / PATH / PATHEXT /
/// PSModulePath / TERM / COLORTERM）。
pub fn child_env() -> Result<Vec<(OsString, OsString)>> {
    let system_root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .context("SystemRoot 缺失：拒绝在不可信环境里启动 shell")?;
    let program_files = std::env::var_os("ProgramFiles")
        .filter(|value| !value.is_empty())
        .context("ProgramFiles 缺失：拒绝在不可信环境里启动 shell")?;

    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for key in ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            env.push((OsString::from(key), value));
        }
    }

    let root = PathBuf::from(&system_root);
    let system32 = root.join("System32");
    // ComSpec 自算：不信任继承来的值，它决定 `cmd` 从哪来。
    env.push((
        OsString::from("ComSpec"),
        system32.join("cmd.exe").into_os_string(),
    ));
    // PATH 自算：只给系统目录。继承 PATH 等于把用户随手加的目录、
    // 网络共享、旧版运行时都塞进子进程的 DLL 与命令搜索路径。
    let path_dirs = [
        system32.clone(),
        root.clone(),
        system32.join("Wbem"),
        system32.join("WindowsPowerShell").join("v1.0"),
        PathBuf::from(&program_files).join("PowerShell").join("7"),
    ];
    env.push((
        OsString::from("PATH"),
        OsString::from(join_dirs(&path_dirs)),
    ));
    // PATHEXT 去掉 .VBS / .JS / .WSF：否则一次「打错名字」会变成脚本执行。
    env.push((
        OsString::from("PATHEXT"),
        OsString::from(".COM;.EXE;.BAT;.CMD"),
    ));

    let mut module_dirs = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        module_dirs.push(
            PathBuf::from(profile)
                .join("Documents")
                .join("WindowsPowerShell")
                .join("Modules"),
        );
    }
    module_dirs.push(
        system32
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("Modules"),
    );
    module_dirs.push(
        PathBuf::from(&program_files)
            .join("PowerShell")
            .join("7")
            .join("Modules"),
    );
    env.push((
        OsString::from("PSModulePath"),
        OsString::from(join_dirs(&module_dirs)),
    ));

    env.push((OsString::from("TERM"), OsString::from("xterm-256color")));
    env.push((OsString::from("COLORTERM"), OsString::from("truecolor")));
    env.push((OsString::from("TLSHELL_SESSION"), OsString::from("1")));
    Ok(env)
}

fn join_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(";")
}

/// 解析启动目录：必须是存在的绝对路径、不是 UNC、不是重解析点。
///
/// `USERPROFILE` 缺失时直接失败——退回进程自身 cwd 等于让子进程在
/// 一个我们没审过的目录里启动（F-05）。
pub fn resolve_cwd(user_profile: Option<OsString>) -> Result<PathBuf> {
    let profile = user_profile
        .filter(|value| !value.is_empty())
        .context("USERPROFILE 缺失：拒绝在没有用户目录的情况下启动 shell")?;
    let path = PathBuf::from(profile);
    if !path.is_absolute() {
        bail!("USERPROFILE 不是绝对路径: {}", path.display());
    }
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") || text.starts_with("//") {
        bail!("拒绝把 UNC 路径当作启动目录: {text}");
    }
    let canonical = strip_verbatim(
        path.canonicalize()
            .with_context(|| format!("启动目录不可用: {}", path.display()))?,
    );
    let meta = std::fs::metadata(&canonical)
        .with_context(|| format!("启动目录不可读: {}", canonical.display()))?;
    if !meta.is_dir() {
        bail!("启动目录不是目录: {}", canonical.display());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        /// `FILE_ATTRIBUTE_REPARSE_POINT`
        const REPARSE: u32 = 0x0400;
        if meta.file_attributes() & REPARSE != 0 {
            bail!("拒绝把重解析点当作启动目录: {}", canonical.display());
        }
    }
    Ok(canonical)
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
            owned(&[
                "-NoProfile",
                "-NoLogo",
                "-NoExit",
                "-Command",
                PS5_UTF8_SETUP
            ])
        );
    }

    #[test]
    fn powershell5_adds_nologo_when_missing() {
        let args = inject_args(ShellKind::PowerShell5, &[]);
        assert!(args.iter().any(|arg| arg == "-NoLogo"));
    }

    /// `-NoProfile` 必须在命令文本之前，否则 PowerShell 会把它当参数交给命令。
    #[test]
    fn powershell5_noprofile_goes_first() {
        let args = inject_args(ShellKind::PowerShell5, &[]);
        assert_eq!(args.first().map(String::as_str), Some("-NoProfile"));
        let command_at = args.iter().position(|arg| arg == "-Command").unwrap();
        let noprofile_at = args.iter().position(|arg| arg == "-NoProfile").unwrap();
        assert!(noprofile_at < command_at);
    }

    /// PowerShell 支持参数前缀缩写，`-Com` 已经是 `-Command`，不能重复注入。
    #[test]
    fn powershell5_recognizes_abbreviated_switches() {
        let user = owned(&["-NoP", "-Com", "Get-Date"]);
        assert_eq!(inject_args(ShellKind::PowerShell5, &user), user);
    }

    #[test]
    fn powershell5_skips_injection_when_user_passes_command() {
        let user = owned(&["-NoProfile", "-Command", "Get-Date"]);
        assert_eq!(inject_args(ShellKind::PowerShell5, &user), user);
    }

    #[test]
    fn cmd_gets_chcp() {
        let args = inject_args(ShellKind::Cmd, &[]);
        assert_eq!(args, owned(&["/D", "/K", "chcp 65001 >nul"]));
    }

    /// `/D` 是加固项：即使用户自己给了 `/K`，也要关掉注册表里的 AutoRun。
    #[test]
    fn cmd_always_disables_autorun() {
        let args = inject_args(ShellKind::Cmd, &owned(&["/K", "echo hi"]));
        assert_eq!(args, owned(&["/D", "/K", "echo hi"]));
        let already = owned(&["/d", "/K", "echo hi"]);
        assert_eq!(inject_args(ShellKind::Cmd, &already), already);
    }

    #[test]
    fn child_env_path_is_system_only() {
        let env = child_env().expect("Windows 上 SystemRoot/ProgramFiles 必然存在");
        let path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .expect("PATH 必须自算");
        // 只给系统目录：不出现用户目录、临时目录、网络共享。
        assert!(path.to_ascii_lowercase().contains(r"\system32"));
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        if !profile.is_empty() {
            assert!(!path.contains(&profile), "PATH 不应带上用户目录: {path}");
        }
        assert!(!path.contains(r"\\"), "PATH 不应带上网络共享: {path}");
    }

    #[test]
    fn child_env_drops_script_hosts_from_pathext() {
        let env = child_env().expect("环境构造应成功");
        let pathext = env
            .iter()
            .find(|(key, _)| key == "PATHEXT")
            .map(|(_, value)| value.to_string_lossy().to_ascii_uppercase())
            .expect("PATHEXT 必须自算");
        assert!(!pathext.contains(".VBS"));
        assert!(!pathext.contains(".JS"));
        assert!(!pathext.contains(".WSF"));
        assert!(pathext.contains(".EXE"));
    }

    #[test]
    fn child_env_never_leaks_known_injection_variables() {
        // 这些名字即使存在于父进程环境里，也不允许出现在子进程环境中。
        let env = child_env().expect("环境构造应成功");
        for blocked in [
            "NODE_OPTIONS",
            "PYTHONPATH",
            "BASH_ENV",
            "JAVA_TOOL_OPTIONS",
            "RUSTC_WRAPPER",
            "__COMPAT_LAYER",
            "KUBECONFIG",
        ] {
            assert!(
                !env.iter().any(|(key, _)| key.eq_ignore_ascii_case(blocked)),
                "{blocked} 不应出现在子进程环境里"
            );
        }
    }

    #[test]
    fn resolve_cwd_rejects_missing_profile() {
        assert!(resolve_cwd(None).is_err());
        assert!(resolve_cwd(Some(OsString::new())).is_err());
    }

    #[test]
    fn resolve_cwd_rejects_unc_and_relative() {
        assert!(resolve_cwd(Some(OsString::from(r"\\server\share"))).is_err());
        assert!(resolve_cwd(Some(OsString::from(r"relative\dir"))).is_err());
    }

    #[test]
    fn resolve_cwd_accepts_user_profile() {
        let Some(profile) = std::env::var_os("USERPROFILE") else {
            return;
        };
        let cwd = resolve_cwd(Some(profile.clone())).expect("用户目录应当可用");
        assert!(cwd.is_absolute());
        assert!(!cwd.to_string_lossy().starts_with(r"\\?\"));
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
