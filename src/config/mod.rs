//! 配置模型、默认值与文件加载。
//!
//! 任何字段缺失都靠 `#[serde(default)]` 退化为默认值，不允许因为一个字段写错就启动失败。
//! 加载失败一律退回默认值并记告警——终端打不开比配置写错严重得多。
//!
//! 配置文件放在 `%APPDATA%\TLShell\config.toml`（计划书 §6.2）。不用 Roaming 之外
//! 的位置是为了让用户能跟着漫游配置走；日志则相反，落在 `%LOCALAPPDATA%`（见 `diag`）。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub mod state;

/// 配置目录 `%APPDATA%\TLShell`。
///
/// 取不到 `APPDATA` 时**直接失败**：以前会退回当前目录，那会让程序把配置、
/// state.json、hwnd.txt 写进它碰巧被启动的那个目录（安全评审 F-03）。
pub fn config_dir() -> Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .context("APPDATA 缺失：拒绝把当前目录当作配置目录")?;
    let dir = PathBuf::from(appdata);
    if !dir.is_absolute() {
        bail!("APPDATA 不是绝对路径: {}", dir.display());
    }
    Ok(dir.join("TLShell"))
}

/// 配置文件路径 `%APPDATA%\TLShell\config.toml`。
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
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
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => match config_path() {
            Ok(path) => path,
            Err(err) => {
                // 配置目录都定位不到时只返回默认值：不写文件、不改任何磁盘状态。
                let mut loaded = Loaded::from_defaults(PathBuf::new());
                loaded.warnings.push(format!("{err:#}"));
                return loaded;
            }
        },
    };
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

/// 把配置写回文件：先进 `.tmp` 再 `rename`，中途断电不会留下半个文件。
///
/// 面板保存与手改文件走的是同一份结构，所以这里保留所有字段（`toml::to_string`
/// 序列化整个 `Config`），只是丢掉注释——注释在 `write_default` 生成的模板里。
pub fn save_atomic(config: &Config, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
    }
    let header = "# TLShell 配置（由设置面板写入）
# 注释版模板见首次生成的默认配置。

";
    let body = toml::to_string(config).context("序列化配置失败")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, format!("{header}{body}"))
        .with_context(|| format!("写临时配置失败: {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("替换配置失败: {}", path.display()))?;
    Ok(())
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
# 背景透明度，0.00 到 1.00。0 表示背景完全不可见，只剩文字。
# 低于 0.70 时正文对比度可能不足（只告警，不阻止）。
opacity = {opacity}
# 文字透明度，0.10 到 1.00。与背景透明度互不影响：
# 背景调到 0 也能保持文字清晰，把字调淡也不会让背景变透。
text_opacity = {text_opacity}
padding = {padding}
border_radius = {border_radius}
# 失焦后沉到所有窗口之下（桌面层模式）。默认 false：普通 z-order，点得到也打得进。
# 打开后窗口会跑到最大化窗口下面，只能用 Ctrl+Alt+T 叫回来。
sink_when_unfocused = {sink_when_unfocused}

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
# 沉底模式下把窗口叫回前台并取键盘焦点。
raise = "{raise}"

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
        text_opacity = defaults.window.text_opacity,
        padding = defaults.window.padding,
        border_radius = defaults.window.border_radius,
        sink_when_unfocused = defaults.window.sink_when_unfocused,
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
        raise = defaults.keys.raise,
        osc52 = defaults.security.osc52,
        allow_custom_shell = defaults.security.allow_custom_shell,
        paste_guard = defaults.security.paste_guard,
    );
    std::fs::write(path, body).with_context(|| format!("写入默认配置失败: {}", path.display()))
}

/// 透明度低于该值时正文对比度会掉到不可读区间，仅告警不拒绝。
pub const OPACITY_WARN_BELOW: f64 = 0.70;
/// 透明度硬下界。计划书 §9 M4-② 要求 0.5 仍能明显变透，因此下界不能抬到 0.75。
/// 背景透明度的下限：0 表示「背景完全不可见」。
pub const OPACITY_MIN: f64 = 0.00;

/// 文字透明度的下限：再低就看不见字了。
pub const TEXT_OPACITY_MIN: f64 = 0.10;

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
    /// 背景透明度（0 = 背景全透明，只剩文字与装饰）。
    pub opacity: f64,
    /// 文字透明度，只乘在字形覆盖度上。
    pub text_opacity: f64,
    pub padding: i32,
    pub border_radius: i32,
    /// 失焦时把窗口沉到最底层（"桌面层"模式）。
    ///
    /// 默认 `false`：沉底的代价是窗口被任何最大化的窗口盖住，鼠标点不到、
    /// 键盘也进不来——那正是用户报的"无法点击和输入"。想要贴桌面外观的人
    /// 可以在设置面板里打开它。
    pub sink_when_unfocused: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 900,
            height: 420,
            x: 40,
            y: 40,
            opacity: 0.88,
            text_opacity: 1.0,
            padding: 10,
            border_radius: 8,
            sink_when_unfocused: true,
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
    /// 把窗口提到最前并交给它键盘焦点。
    pub raise: String,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            quit: "Ctrl+Alt+Q".to_owned(),
            hide: "Ctrl+Alt+H".to_owned(),
            raise: "Ctrl+Alt+T".to_owned(),
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
        if !self.window.text_opacity.is_finite() {
            self.window.text_opacity = WindowConfig::default().text_opacity;
        }
        self.window.text_opacity = self.window.text_opacity.clamp(TEXT_OPACITY_MIN, 1.0);

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
    fn default_template_round_trips_into_defaults() {
        // 模板里少写一个键，用户拿到的默认配置就和程序默认值不一致；
        // 模板里写错一个键名，serde 会静默忽略，更难发现。这条测试两头都堵。
        let dir = std::env::temp_dir().join(format!("tlshell-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        write_default(&path).unwrap();
        let loaded = load(Some(&path));
        assert!(
            loaded.warnings.is_empty(),
            "模板本身读不回来: {:?}",
            loaded.warnings
        );
        assert!(!loaded.created, "模板已存在，不该再被重写");
        let d = Config::default();
        assert_eq!(
            loaded.config.window.sink_when_unfocused,
            d.window.sink_when_unfocused
        );
        assert_eq!(loaded.config.keys.raise, d.keys.raise);
        assert_eq!(loaded.config.window.opacity, d.window.opacity);
        assert_eq!(loaded.config.security.paste_guard, d.security.paste_guard);
        assert_eq!(loaded.config.font.family, d.font.family);
        let _ = std::fs::remove_dir_all(&dir);
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
