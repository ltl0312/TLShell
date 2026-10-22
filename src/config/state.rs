//! 窗口位置与尺寸的持久化。
//!
//! 位置只在 `WM_EXITSIZEMOVE` 落盘（计划书 §5.3）：拖动过程中每移动一像素都写文件，
//! 既没必要也会把磁盘刷爆。
//!
//! 写入必须原子：先写同目录的临时文件、再 `rename` 覆盖。中途崩溃或断电时，
//! 旧文件保持完整，不会留下半截 JSON 让下次启动解析失败。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::win::dpi::Rect;

/// 状态文件格式版本。字段不兼容时递增，读到旧版本直接丢弃。
pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowState {
    pub fn from_rect(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.w,
            height: rect.h,
        }
    }

    pub fn to_rect(self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorState {
    pub dpi: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl MonitorState {
    pub fn from_rect(dpi: u32, rect: Rect) -> Self {
        Self {
            dpi,
            x: rect.x,
            y: rect.y,
            width: rect.w,
            height: rect.h,
        }
    }

    #[allow(dead_code)] // M5 多显示器回中判断用
    pub fn bounds(self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub window: WindowState,
    pub monitor: MonitorState,
}

impl State {
    pub fn new(window: Rect, dpi: u32, monitor: Rect) -> Self {
        Self {
            version: STATE_VERSION,
            window: WindowState::from_rect(window),
            monitor: MonitorState::from_rect(dpi, monitor),
        }
    }
}

/// `%APPDATA%\TLShell\state.json`。配置目录不可定位时返回 `None`——
/// 宁可不记住窗口位置，也不要写进一个碰巧的当前目录。
pub fn state_path() -> Option<PathBuf> {
    crate::config::config_dir()
        .ok()
        .map(|dir| dir.join("state.json"))
}

/// 读取状态。文件缺失、损坏、版本不匹配都返回 `None`，由调用方回退到配置里的默认值。
pub fn load(path: &Path) -> Option<State> {
    let text = std::fs::read_to_string(path).ok()?;
    let state: State = match serde_json::from_str(&text) {
        Ok(state) => state,
        Err(err) => {
            crate::diag::warn(&format!("state.json 解析失败，忽略: {err}"));
            return None;
        }
    };
    if state.version != STATE_VERSION {
        crate::diag::warn(&format!(
            "state.json 版本 {} 与当前 {STATE_VERSION} 不符，忽略",
            state.version
        ));
        return None;
    }
    Some(state)
}

/// 原子写入：临时文件 + rename。
pub fn save(path: &Path, state: &State) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建状态目录失败: {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(state).context("序列化状态失败")?;

    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text.as_bytes())
        .with_context(|| format!("写入临时状态文件失败: {}", temp.display()))?;
    // Windows 上 `rename` 走 MoveFileEx + REPLACE_EXISTING，目标已存在也能覆盖。
    std::fs::rename(&temp, path).with_context(|| {
        let _ = std::fs::remove_file(&temp);
        format!("替换状态文件失败: {}", path.display())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tlshell-state-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    #[test]
    fn roundtrip_keeps_geometry() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("state.json");
        let state = State::new(
            Rect::new(40, 60, 900, 420),
            130,
            Rect::new(0, 0, 1920, 1080),
        );
        save(&path, &state).expect("写入");
        let loaded = load(&path).expect("读回");
        assert_eq!(loaded, state);
        assert_eq!(loaded.window.to_rect(), Rect::new(40, 60, 900, 420));
        assert_eq!(loaded.monitor.bounds(), Rect::new(0, 0, 1920, 1080));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = temp_dir("missing");
        assert!(load(&dir.join("nope.json")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_returns_none() {
        let dir = temp_dir("corrupt");
        let path = dir.join("state.json");
        std::fs::write(&path, b"{ this is not json").expect("写入");
        assert!(load(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn future_version_is_ignored() {
        let dir = temp_dir("future");
        let path = dir.join("state.json");
        let mut state = State::new(Rect::new(0, 0, 900, 420), 96, Rect::new(0, 0, 1920, 1080));
        state.version = STATE_VERSION + 1;
        save(&path, &state).expect("写入");
        assert!(load(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = temp_dir("no-temp");
        let path = dir.join("state.json");
        let state = State::new(Rect::new(1, 2, 3, 4), 96, Rect::new(0, 0, 800, 600));
        save(&path, &state).expect("写入");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("列目录")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_idempotent_across_versions() {
        let dir = temp_dir("overwrite");
        let path = dir.join("state.json");
        let first = State::new(Rect::new(0, 0, 900, 420), 96, Rect::new(0, 0, 1920, 1080));
        save(&path, &first).expect("第一次写入");
        let second = State::new(
            Rect::new(100, 200, 800, 400),
            120,
            Rect::new(0, 0, 2560, 1440),
        );
        save(&path, &second).expect("第二次写入");
        assert_eq!(load(&path).expect("读回"), second);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
