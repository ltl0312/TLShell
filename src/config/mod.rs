//! 配置模型与默认值。
//!
//! 本模块只负责**数据形状**与默认值；文件加载、校验与钳制在后续里程碑接入。
//! 任何字段缺失都靠 `#[serde(default)]` 退化为默认值，不允许因为一个字段写错就启动失败。

use serde::{Deserialize, Serialize};

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
