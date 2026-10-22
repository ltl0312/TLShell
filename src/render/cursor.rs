//! 光标闪烁：时间驱动，与 PTY 内容解耦。
//!
//! 硬边界（UI 规范）：闪烁由 `WM_TIMER` 驱动，**不**因为收到新字节就重置计时器。
//! 否则一边输出一边闪，看起来像在抖。
//!
//! 只在窗口有焦点时闪；失焦时光标保持常亮（空心框），符合“无焦点 = 不闪”的约定。

use std::time::{Duration, Instant};

/// 半周期。530ms 是 Windows 默认光标闪烁间隔，用户不会觉得陌生。
pub const BLINK_INTERVAL: Duration = Duration::from_millis(530);

#[derive(Debug)]
pub struct Blink {
    on: bool,
    last: Instant,
    enabled: bool,
}

impl Default for Blink {
    fn default() -> Self {
        Self::new()
    }
}

impl Blink {
    pub fn new() -> Self {
        Self {
            on: true,
            last: Instant::now(),
            enabled: true,
        }
    }

    /// 有焦点时开启闪烁，失焦时固定在“亮”。
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.on = true;
        self.last = Instant::now();
    }

    /// 现在光标该不该显示。
    pub fn visible(&self) -> bool {
        if !self.enabled {
            return true;
        }
        self.on
    }

    /// 推进计时器；返回 `true` 表示状态翻转、需要重绘。
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.enabled {
            return false;
        }
        if now.duration_since(self.last) < BLINK_INTERVAL {
            return false;
        }
        self.on = !self.on;
        self.last = now;
        true
    }

    /// 输入发生时把光标立刻点亮——用户刚敲了键，光标不该是灭的。
    pub fn reset(&mut self, now: Instant) {
        if self.enabled && !self.on {
            self.on = true;
            self.last = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_visible() {
        assert!(Blink::new().visible());
    }

    #[test]
    fn does_not_flip_before_interval() {
        let mut blink = Blink::new();
        let start = Instant::now();
        assert!(!blink.tick(start + Duration::from_millis(100)));
        assert!(blink.visible());
    }

    #[test]
    fn flips_after_interval() {
        let mut blink = Blink::new();
        let start = Instant::now();
        assert!(blink.tick(start + BLINK_INTERVAL));
        assert!(!blink.visible());
    }

    #[test]
    fn flips_back_on_next_interval() {
        let mut blink = Blink::new();
        let start = Instant::now();
        blink.tick(start + BLINK_INTERVAL);
        blink.tick(start + BLINK_INTERVAL * 2);
        assert!(blink.visible());
    }

    #[test]
    fn disabled_blink_stays_visible() {
        let mut blink = Blink::new();
        blink.set_enabled(false);
        let start = Instant::now();
        assert!(!blink.tick(start + BLINK_INTERVAL * 3));
        assert!(blink.visible());
    }

    #[test]
    fn reenabling_resets_to_visible() {
        let mut blink = Blink::new();
        let start = Instant::now();
        blink.tick(start + BLINK_INTERVAL);
        assert!(!blink.visible());
        blink.set_enabled(false);
        blink.set_enabled(true);
        assert!(blink.visible());
    }

    #[test]
    fn reset_lights_cursor_immediately() {
        let mut blink = Blink::new();
        let start = Instant::now();
        blink.tick(start + BLINK_INTERVAL);
        assert!(!blink.visible());
        blink.reset(start + BLINK_INTERVAL + Duration::from_millis(1));
        assert!(blink.visible());
    }
}
