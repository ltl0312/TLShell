//! 配置模型、默认值与文件加载。
//!
//! 任何字段缺失都靠 `#[serde(default)]` 退化为默认值，不允许因为一个字段写错就启动失败。
//! 加载失败一律退回默认值并记告警——终端打不开比配置写错严重得多。
//!
//! 配置文件放在 `%APPDATA%\TLShell\config.toml`（计划书 §6.2）。不用 Roaming 之外
//! 的位置是为了让用户能跟着漫游配置走；日志则相反，落在 `%LOCALAPPDATA%`（见 `diag`）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub mod state;

/// 配置目录 `%APPDATA%\TLShell`。取不到 `APPDATA` 时退回当前目录，至少不会崩。
pub fn config_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(appdata) if !appdata.is_empty() => PathBuf::from(appdata).join("TLShell"),
        _ => PathBuf::from("."),
    }
}

/// 配置文件路径 `%APPDATA%\TLShell\config.toml`。
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// 一次加载的结果：配置本身、实际使用的路径、以及所有告警。
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub warnings: Vec<String>,
    /// 文件本来不存在、这次顺手写了一份默认配置。
    pub created: bool,
}

impl Loaded {
    fn from_defaults(path: PathBuf) -> Self {
        Self {
            config: Config::default(),
            path,
            warnings: Vec::new(),
            created: false,
        }
    }
}

/// 加载配置。
///
/// - `explicit` 为 `Some` 时用它，否则用 `config_path()`。
/// - 文件不存在 → 写一份带注释的默认配置，返回默认值。
/// - 文件损坏 → 返回默认值 + 告警，**不覆盖**用户的文件（他可能只是想手改）。
pub fn load(explicit: Option<&Path>) -> Loaded {
    let path = explicit.map(Path::to_path_buf).unwrap_or_else(config_path);
    let mut loaded = Loaded::from_defaults(path.clone());

    if !path.exists() {
        if let Err(err) = write_default(&path) {
            loaded.warnings.push(format!("默认配置写入失败: {err:#}"));
        } else {
            loaded.created = true;
        }
        return loaded;
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            loaded
                .warnings
                .push(format!("读取 {} 失败: {err}", path.display()));
            return loaded;
        }
    };

    match toml::from_str::<Config>(&text) {
        Ok(mut config) => {
            config.sanitize();
            loaded.config = config;
        }
        Err(err) => {
            loaded.warnings.push(format!(
                "{} 解析失败，本次使用默认配置: {err}",
                path.display()
            ));
        }
    }
    loaded
}

/// 写一份默认配置，字段顺序与注释对齐计划书 §6.1。
pub fn write_default(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
    }
    let defaults = Config::default();
    let body = format!(
        r#"# TLShell 配置
# 修改后重启生效。删掉本文件会重新生成一份默认值。

[shell]
# 裸文件名只会在白名单目录里查找，不走 PATH，也不看当前目录。
# 白名单：%SystemRoot%\System32、System32\WindowsPowerShell\v1.0、
#         %ProgramFiles%\PowerShell\7（含 7-preview）
program = "{program}"
args = []

[window]
width = {width}
height = {height}
x = {x}
y = {y}
# 0.10 到 1.00。低于 0.70 时正文对比度可能不足。
opacity = {opacity}
padding = {padding}
border_radius = {border_radius}

[font]
family = "{family}"
size = {size}
line_height = {line_height}
# 留空表示自动挑选 CJK 回退字体。
fallback_family = ""

[colors]
background = "{background}"
foreground = "{foreground}"
cursor = "{cursor}"
selection = "{selection}"
focus_ring = "{focus_ring}"

[keys]
quit = "{quit}"
hide = "{hide}"

[security]
# OSC 52：允许程序写系统剪贴板。默认关闭。
osc52 = {osc52}
# 允许 shell.program 指向白名单之外的路径。默认关闭。
allow_custom_shell = {allow_custom_shell}
# 粘贴时剥离控制字符并对多行内容二次确认。默认开启。
paste_guard = {paste_guard}
"#,
        program = defaults.shell.program,
        width = defaults.window.width,
        height = defaults.window.height,
        x = defaults.window.x,
        y = defaults.window.y,
        opacity = defaults.window.opacity,
        padding = defaults.window.padding,
        border_radius = defaults.window.border_radius,
        family = defaults.font.family,
        size = defaults.font.size,
        line_height = defaults.font.line_height,
        background = defaults.colors.background,
        foreground = defaults.colors.foreground,
        cursor = defaults.colors.cursor,
        selection = defaults.colors.selection,
        focus_ring = defaults.colors.focus_ring,
        quit = defaults.keys.quit,
        hide = defaults.keys.hide,
        osc52 = defaults.security.osc52,
        allow_custom_shell = defaults.security.allow_custom_shell,
        paste_guard = defaults.security.paste_guard,
    );
    std::fs::write(path, body).with_context(|| format!("写入默认配置失败: {}", path.display()))
}

/// 透明度低于该值时正文对比度会掉到不可读区间，仅告警不拒绝。
#[allow(dead_code)] // M4 接入玻璃外观时用于启动告警
pub const OPACITY_WARN_BELOW: f64 = 0.70;
/// 透明度硬下界。计划书 §9 M4-② 要求 0.5 仍能明显变透，因此下界不能抬到 0.75。
pub const OPACITY_MIN: f64 = 0.10;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shell: ShellConfig,
    pub window: WindowConfig,
    pub font: FontConfig,
    pub colors: ColorsConfig,
    pub keys: KeysConfig,
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// 可执行文件。裸名会按白名单解析为绝对路径（见 `resolve_program`）。
    pub program: String,
    pub args: Vec<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            program: "pwsh.exe".to_owned(),
            args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub opacity: f64,
    pub padding: i32,
    pub border_radius: i32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 900,
            height: 420,
            x: 40,
            y: 40,
            opacity: 0.88,
            padding: 10,
            border_radius: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    /// 逻辑字号（磅值，按 96 DPI 解释）。
    pub size: f64,
    pub line_height: f64,
    /// 用于 CJK 与 emoji 回退的字体族。空串表示使用内置候选链。
    pub fallback_family: String,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Cascadia Mono".to_owned(),
            size: 14.0,
            line_height: 1.25,
            fallback_family: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection: String,
    pub focus_ring: String,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            background: "#1e1e2e".to_owned(),
            foreground: "#cdd6f4".to_owned(),
            cursor: "#f5e0dc".to_owned(),
            selection: "#585b70".to_owned(),
            focus_ring: "#89b4fa".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    pub quit: String,
    pub hide: String,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            quit: "Ctrl+Alt+Q".to_owned(),
            hide: "Ctrl+Alt+H".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// OSC 52 剪贴板写入。默认关闭：远端进程不应能改写本机剪贴板。
    pub osc52: bool,
    /// 允许 `program` 指向白名单之外的路径。默认关闭。
    pub allow_custom_shell: bool,
    /// 粘贴时剥离控制字符并对多行内容做确认。
    pub paste_guard: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            osc52: false,
            allow_custom_shell: false,
            paste_guard: true,
        }
    }
}

/// 数值钳制。越界值被夹到合法区间而不是让启动失败。
impl Config {
    pub fn sanitize(&mut self) {
        self.window.width = self.window.width.clamp(200, 16384);
        self.window.height = self.window.height.clamp(120, 16384);
        self.window.padding = self.window.padding.clamp(0, 64);
        self.window.border_radius = self.window.border_radius.clamp(0, 64);

        if !self.window.opacity.is_finite() {
            self.window.opacity = WindowConfig::default().opacity;
        }
        self.window.opacity = self.window.opacity.clamp(OPACITY_MIN, 1.0);

        // 圆角不能超过短边的一半，否则自绘时弧线会自交。
        let max_radius = self.window.width.min(self.window.height) / 2;
        self.window.border_radius = self.window.border_radius.min(max_radius.max(0));

        if !self.font.size.is_finite() {
            self.font.size = FontConfig::default().size;
        }
        self.font.size = self.font.size.clamp(6.0, 72.0);

        if !self.font.line_height.is_finite() {
            self.font.line_height = FontConfig::default().line_height;
        }
        self.font.line_height = self.font.line_height.clamp(0.8, 3.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_plan_section_6_1() {
        let cfg = Config::default();
        assert_eq!(cfg.shell.program, "pwsh.exe");
        assert_eq!(cfg.window.width, 900);
        assert_eq!(cfg.window.height, 420);
        assert_eq!(cfg.window.opacity, 0.88);
        assert_eq!(cfg.window.padding, 10);
        assert_eq!(cfg.window.border_radius, 8);
        assert_eq!(cfg.font.family, "Cascadia Mono");
        assert_eq!(cfg.font.size, 14.0);
        assert_eq!(cfg.font.line_height, 1.25);
        assert_eq!(cfg.colors.background, "#1e1e2e");
        assert_eq!(cfg.colors.foreground, "#cdd6f4");
        assert_eq!(cfg.colors.cursor, "#f5e0dc");
        assert_eq!(cfg.keys.quit, "Ctrl+Alt+Q");
    }

    #[test]
    fn sanitize_clamps_out_of_range_values() {
        let mut cfg = Config::default();
        cfg.window.width = 1;
        cfg.window.height = 1;
        cfg.window.opacity = 5.0;
        cfg.font.size = 0.0;
        cfg.sanitize();
        assert_eq!(cfg.window.width, 200);
        assert_eq!(cfg.window.height, 120);
        assert_eq!(cfg.window.opacity, 1.0);
        assert_eq!(cfg.font.size, 6.0);
    }

    #[test]
    fn sanitize_rejects_non_finite_opacity() {
        let mut cfg = Config::default();
        cfg.window.opacity = f64::NAN;
        cfg.sanitize();
        assert_eq!(cfg.window.opacity, 0.88);
    }

    #[test]
    fn sanitize_keeps_half_opacity_usable() {
        // 计划书 M4-② 要求 0.5 明显更透，因此不能被抬到 0.75。
        let mut cfg = Config::default();
        cfg.window.opacity = 0.5;
        cfg.sanitize();
        assert_eq!(cfg.window.opacity, 0.5);
    }

    #[test]
    fn sanitize_bounds_border_radius_by_half_short_side() {
        let mut cfg = Config::default();
        cfg.window.width = 200;
        cfg.window.height = 120;
        cfg.window.border_radius = 64;
        cfg.sanitize();
        assert_eq!(cfg.window.border_radius, 60);
    }
}
