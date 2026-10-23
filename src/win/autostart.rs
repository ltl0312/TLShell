//! 随 Windows 启动：写 `HKCU\...\Run`，不写启动文件夹快捷方式。
//!
//! 为什么用注册表而不是 `Startup` 目录的 `.lnk`：创建快捷方式要 COM
//! （`IShellLink` + `IPersistFile`），失败模式多、还依赖资源管理器是否响应；
//! `Run` 键是一次 `reg.exe` 调用，返回值就说明成败，而且用户能在
//! 「任务管理器的启动」页里看到它、随时禁用。
//!
//! 三个约束：
//! - 只写 `HKCU`，不碰 `HKLM`：不需要管理员，也不给整机装东西；
//! - 值里写**当前可执行文件的绝对路径**（带引号），不是 `%APPDATA%` 相对路径；
//! - 关掉时只删自己那一个值，不动 Run 键里的其它条目。

use std::path::Path;

use crate::diag;

/// 注册表位置：当前用户的 Run 键。
pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// 值名。删/查都只针对它。
pub const VALUE_NAME: &str = "TLShell";

/// 隐藏 `reg.exe` 的控制台窗口：GUI 子系统进程里弹一个黑框很突兀。
#[cfg(windows)]
fn hidden(command: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW)
}

fn reg(args: &[&str]) -> Option<std::process::Output> {
    let mut command = std::process::Command::new("reg.exe");
    command.args(args);
    hidden(&mut command);
    match command.output() {
        Ok(output) => Some(output),
        Err(err) => {
            diag::warn(&format!("调用 reg.exe 失败: {err}"));
            None
        }
    }
}

/// 读取已登记的命令行（未登记返回 `None`）。
pub fn registered_command() -> Option<String> {
    let output = reg(&["query", RUN_KEY, "/v", VALUE_NAME])?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // 输出形如：`    TLShell    REG_SZ    "C:\path\tlshell.exe"`
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with(VALUE_NAME) {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let _name = parts.next();
        let _kind = parts.next();
        let rest = trimmed
            .split_once("REG_SZ")
            .or_else(|| trimmed.split_once("REG_EXPAND_SZ"))
            .map(|(_, value)| value.trim())?;
        return Some(rest.to_owned());
    }
    None
}

/// 当前是否已登记为自启动。
pub fn is_enabled() -> bool {
    registered_command().is_some()
}

/// 登记自启动。`exe` 是要写的可执行文件路径。
pub fn enable(exe: &Path) -> bool {
    let quoted = format!("\"{}\"", exe.display());
    let output = reg(&[
        "add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d", &quoted, "/f",
    ]);
    match output {
        Some(output) if output.status.success() => true,
        Some(output) => {
            diag::warn(&format!(
                "登记自启动失败: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            false
        }
        None => false,
    }
}

/// 取消自启动。值本来就不存在时也算成功（幂等）。
pub fn disable() -> bool {
    let output = reg(&["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]);
    match output {
        Some(output) if output.status.success() => true,
        // 找不到就是「已经不在」，对调用方是成功。
        Some(_) => !is_enabled(),
        None => false,
    }
}

/// 把当前状态写成一行，供 CLI 与日志使用。
pub fn describe() -> String {
    match registered_command() {
        Some(command) => format!("随 Windows 启动: 已开启（{command}）"),
        None => "随 Windows 启动: 未开启".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_paths_are_user_scope_only() {
        // 只允许 HKCU：写 HKLM 需要管理员，也会影响别的用户。
        assert!(RUN_KEY.starts_with("HKCU\\"));
        assert!(!RUN_KEY.contains("HKLM"));
    }

    #[test]
    fn value_name_is_stable() {
        // 值名是删除/查询的唯一锚点，改名等于把老条目变成孤儿。
        assert_eq!(VALUE_NAME, "TLShell");
    }

    #[test]
    fn describe_never_panics() {
        let text = describe();
        assert!(text.contains("随 Windows 启动"));
    }
}
