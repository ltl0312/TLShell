//! 窗口内设置面板的**模型**（不涉及绘制，选型与取值都在这里，便于单测）。
//!
//! 为什么不做独立的设置窗口：这个程序是常驻桌面的单窗口工具，多开一个窗口
//! 就要多处理一套 z-order、焦点、DPI、生命周期问题，而真正的需求只是
//! 「不改文件也能调参数」。面板画在同一个窗口里，关掉就回到终端。
//!
//! 写盘不在这里做：`Settings::apply` 只把值搬回 `Config`，落盘由 `config::save_atomic`
//! 负责（原子替换 + 失败可见），这样「能不能写」和「写什么」两件事互不牵连。

use crate::config::Config;

/// 一行可调项。
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// 显示名（中文，面向使用者）。
    pub label: &'static str,
    /// 当前值。
    pub value: f64,
    pub min: f64,
    pub max: f64,
    /// 每次按方向键的步长。
    pub step: f64,
    /// 显示小数位。
    pub decimals: usize,
    /// 单位的后缀，例如 `px`、`x`（可空）。
    pub suffix: &'static str,
    /// 是否为开关（0/1），开关用「关/开」显示。
    pub toggle: bool,
}

impl Item {
    fn slider(
        label: &'static str,
        value: f64,
        min: f64,
        max: f64,
        step: f64,
        decimals: usize,
        suffix: &'static str,
    ) -> Self {
        Self {
            label,
            value,
            min,
            max,
            step,
            decimals,
            suffix,
            toggle: false,
        }
    }

    fn switch(label: &'static str, on: bool) -> Self {
        Self {
            label,
            value: if on { 1.0 } else { 0.0 },
            min: 0.0,
            max: 1.0,
            step: 1.0,
            decimals: 0,
            suffix: "",
            toggle: true,
        }
    }

    /// 调整一档，返回是否真的变了（到头了就不算改动）。
    pub fn adjust(&mut self, up: bool) -> bool {
        let before = self.value;
        if self.toggle {
            self.value = if self.value > 0.5 { 0.0 } else { 1.0 };
        } else if up {
            self.value = (self.value + self.step).min(self.max);
        } else {
            self.value = (self.value - self.step).max(self.min);
        }
        // 浮点累加会漂：每一步都按步长对齐，避免 0.8800000001 这种值落盘。
        if !self.toggle && self.step > 0.0 {
            let steps = ((self.value - self.min) / self.step).round();
            self.value = (self.min + steps * self.step).clamp(self.min, self.max);
        }
        (self.value - before).abs() > f64::EPSILON
    }

    /// 显示用的值文本。
    pub fn value_text(&self) -> String {
        if self.toggle {
            return if self.value > 0.5 { "开" } else { "关" }.to_owned();
        }
        format!("{:.*}{}", self.decimals, self.value, self.suffix)
    }
}

/// 设置面板的全部状态。
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub items: Vec<Item>,
    pub selected: usize,
    /// 打开面板之后有没有改过值（用来提示「未保存」）。
    pub dirty: bool,
}

impl Settings {
    /// 从当前配置取初值。
    pub fn from_config(config: &Config) -> Self {
        Self {
            items: vec![
                // 背景透明度下限是 0：调到 0 就是「只有文字贴在壁纸上」。
                Item::slider(
                    "背景不透明度",
                    config.window.opacity,
                    0.00,
                    1.00,
                    0.02,
                    2,
                    "",
                ),
                Item::slider(
                    "文字不透明度",
                    config.window.text_opacity,
                    0.10,
                    1.00,
                    0.02,
                    2,
                    "",
                ),
                Item::slider("字号", config.font.size, 6.0, 72.0, 1.0, 0, "px"),
                Item::slider("行高", config.font.line_height, 0.8, 3.0, 0.05, 2, "x"),
                Item::slider(
                    "内边距",
                    config.window.padding as f64,
                    0.0,
                    64.0,
                    2.0,
                    0,
                    "px",
                ),
                Item::slider(
                    "圆角",
                    config.window.border_radius as f64,
                    0.0,
                    64.0,
                    2.0,
                    0,
                    "px",
                ),
                Item::switch("粘贴保护", config.security.paste_guard),
                Item::switch("OSC 52 写入剪贴板", config.security.osc52),
                Item::switch("失焦沉到桌面层", config.window.sink_when_unfocused),
                // 这一项不写进 config.toml：它是注册表里的状态，打开面板时现读，
                // 保存时按当前值去登记/取消，避免「配置文件说开、注册表里没有」。
                Item::switch("随 Windows 启动", crate::win::autostart::is_enabled()),
            ],
            selected: 0,
            dirty: false,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 是否有可调项（clippy 要求 `len` 配套 `is_empty`）。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 上下移动选中行（循环）。
    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    /// 左右调整当前行的值。
    pub fn adjust(&mut self, up: bool) -> bool {
        let Some(item) = self.items.get_mut(self.selected) else {
            return false;
        };
        let changed = item.adjust(up);
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// 把面板上的值搬回配置。
    ///
    /// 只动面板里有的字段，其余字段原样保留——面板不是配置的完整表示，
    /// 覆盖式写盘会把用户手写的其它字段抹掉。
    pub fn apply(&self, config: &mut Config) {
        let get = |index: usize| self.items.get(index).map(|item| item.value);
        if let Some(value) = get(0) {
            config.window.opacity = value;
        }
        if let Some(value) = get(1) {
            config.window.text_opacity = value;
        }
        if let Some(value) = get(2) {
            config.font.size = value;
        }
        if let Some(value) = get(3) {
            config.font.line_height = value;
        }
        if let Some(value) = get(4) {
            config.window.padding = value.round() as i32;
        }
        if let Some(value) = get(5) {
            config.window.border_radius = value.round() as i32;
        }
        if let Some(value) = get(6) {
            config.security.paste_guard = value > 0.5;
        }
        if let Some(value) = get(7) {
            config.security.osc52 = value > 0.5;
        }
        if let Some(value) = get(8) {
            config.window.sink_when_unfocused = value > 0.5;
        }
        config.sanitize();
    }

    /// 最后一项（随 Windows 启动）当前是开还是关。
    ///
    /// 单独开一个访问器，是为了让保存路径显式处理它——它不是配置字段，
    /// 而是注册表状态，写错了用户下次开机就看不到窗口。
    pub fn autostart_requested(&self) -> Option<bool> {
        self.items
            .last()
            .filter(|item| item.toggle)
            .map(|item| item.value > 0.5)
    }

    /// 绘制用的行文本：(标签, 值)。
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        self.items
            .iter()
            .map(|item| (item.label, item.value_text()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_from_config_values() {
        let mut config = Config::default();
        config.window.opacity = 0.66;
        config.font.size = 18.0;
        config.security.paste_guard = false;
        let settings = Settings::from_config(&config);
        assert!((settings.items[0].value - 0.66).abs() < 1e-9);
        // 背景与文字透明度是两个独立滑块（用户要求分开）。
        assert_eq!(settings.items[1].value, 1.0);
        assert_eq!(settings.items[2].value, 18.0);
        assert_eq!(settings.items[6].value, 0.0);
        // 8 个可调项 + 沉底开关 + 自启动开关 = 10 行。
        assert_eq!(settings.rows().len(), 10);
    }

    #[test]
    fn adjust_respects_bounds_and_reports_change() {
        let mut config = Config::default();
        config.window.opacity = 1.0;
        let mut settings = Settings::from_config(&config);
        // 已经在上限，再往上不该报「改了」。
        assert!(!settings.adjust(true));
        assert!(!settings.dirty);
        assert!(settings.adjust(false));
        assert!(settings.dirty);
        assert!(settings.items[0].value < 1.0);
    }

    #[test]
    fn values_snap_to_the_step_grid() {
        let mut config = Config::default();
        config.window.opacity = 0.88;
        let mut settings = Settings::from_config(&config);
        for _ in 0..7 {
            settings.adjust(true);
        }
        // 0.88 + 7*0.02 = 1.02 → 被上限夹到 1.00；再往下三档是 0.94。
        settings.adjust(false);
        settings.adjust(false);
        settings.adjust(false);
        let value = settings.items[0].value;
        assert!((value - 0.94).abs() < 1e-9, "实际 {value}");
    }

    #[test]
    fn selection_wraps_around() {
        let mut settings = Settings::from_config(&Config::default());
        settings.move_selection(-1);
        assert_eq!(settings.selected, settings.len() - 1);
        settings.move_selection(1);
        assert_eq!(settings.selected, 0);
    }

    #[test]
    fn toggle_flips_and_round_trips_into_config() {
        let mut config = Config::default();
        let mut settings = Settings::from_config(&config);
        settings.selected = 6;
        // 默认粘贴保护是开的，翻一次变成「关」。
        assert!(settings.adjust(true));
        assert_eq!(settings.items[6].value_text(), "关");
        settings.apply(&mut config);
        assert!(!config.security.paste_guard);

        settings.selected = 7;
        assert!(settings.adjust(true));
        settings.apply(&mut config);
        assert!(config.security.osc52);
    }

    #[test]
    fn apply_leaves_untouched_fields_alone() {
        let mut config = Config::default();
        config.shell.program = "cmd.exe".to_owned();
        config.keys.quit = "Ctrl+Alt+X".to_owned();
        let settings = Settings::from_config(&config);
        settings.apply(&mut config);
        assert_eq!(config.shell.program, "cmd.exe");
        assert_eq!(config.keys.quit, "Ctrl+Alt+X");
    }
}
