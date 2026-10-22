//! DPI 换算与矩形钳制。
//!
//! 这里只有纯算术：不调用任何 Win32 API，因此可以完整单测。
//! 物理坐标 ↔ 逻辑坐标的换算点在全仓唯一存在于本文件。

/// Windows 的逻辑基准 DPI。
pub const BASE_DPI: u32 = 96;

/// 一个以左上角为原点的整数矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    #[allow(dead_code)] // M3 持久化接线后由 app 使用
    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    #[allow(dead_code)] // M3 持久化接线后由 app 使用
    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }

    #[allow(dead_code)] // M3 持久化接线后由 app 使用
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// 两矩形是否有正面积交集。
    #[allow(dead_code)] // M3 持久化接线后由 app 使用
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// 按 DPI 比例缩放一个长度值，中间量用 `i64` 避免溢出。
#[allow(dead_code)] // M3 持久化接线后由 app 使用
pub fn scale(value: i32, from_dpi: u32, to_dpi: u32) -> i32 {
    if from_dpi == 0 || to_dpi == 0 {
        return value;
    }
    let scaled = i64::from(value) * i64::from(to_dpi) / i64::from(from_dpi);
    scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// 逻辑像素 → 物理像素。
#[allow(dead_code)] // M3 持久化接线后由 app 使用
pub fn logical_to_physical(logical: i32, dpi: u32) -> i32 {
    scale(logical, BASE_DPI, dpi)
}

/// 物理像素 → 逻辑像素。
#[allow(dead_code)] // M3 持久化接线后由 app 使用
pub fn physical_to_logical(physical: i32, dpi: u32) -> i32 {
    scale(physical, dpi, BASE_DPI)
}

/// 把 `rect` 钳制进 `container`，并保证尺寸不小于 `min_w` × `min_h`。
///
/// 容器本身比最小尺寸还小时以容器为准（窗口不会比屏幕更大）。
#[allow(dead_code)] // M3 持久化接线后由 app 使用
pub fn clamp_rect(rect: Rect, container: Rect, min_w: i32, min_h: i32) -> Rect {
    let max_w = container.w.max(1);
    let max_h = container.h.max(1);
    let w = rect.w.max(min_w).min(max_w).max(1);
    let h = rect.h.max(min_h).min(max_h).max(1);

    let max_x = container.x.saturating_add(max_w - w);
    let max_y = container.y.saturating_add(max_h - h);

    Rect {
        x: rect.x.clamp(container.x, max_x.max(container.x)),
        y: rect.y.clamp(container.y, max_y.max(container.y)),
        w,
        h,
    }
}

/// 窗口所在显示器的 DPI。`GetDpiForWindow` 在 Win10 1607+ 可用。
pub fn window_dpi(hwnd: windows::Win32::Foundation::HWND) -> u32 {
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    // SAFETY: 只读查询，hwnd 由调用方保证有效。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi > 0 {
        dpi
    } else {
        BASE_DPI
    }
}

/// 包含指定屏幕坐标的显示器的工作区（不含任务栏）。
pub fn work_area_for_point(x: i32, y: i32) -> Option<Rect> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    let point = POINT { x, y };
    // SAFETY: 只读查询，point 是栈上值。
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info 已填好 cbSize，monitor 有效。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let work = info.rcWork;
    Some(Rect::new(
        work.left,
        work.top,
        work.right - work.left,
        work.bottom - work.top,
    ))
}

/// 主显示器工作区。多屏或查询失败时给一个保守的 1920x1080 兜底。
pub fn primary_work_area() -> Rect {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    let point = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    // SAFETY: 只读查询。
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY) };
    if !monitor.0.is_null() {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: info 已填好 cbSize，monitor 有效。
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            let work = info.rcWork;
            return Rect::new(
                work.left,
                work.top,
                work.right - work.left,
                work.bottom - work.top,
            );
        }
    }
    Rect::new(0, 0, 1920, 1080)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_identity_at_base_dpi() {
        assert_eq!(logical_to_physical(100, BASE_DPI), 100);
        assert_eq!(physical_to_logical(100, BASE_DPI), 100);
    }

    #[test]
    fn scale_handles_150_percent() {
        assert_eq!(logical_to_physical(100, 144), 150);
        assert_eq!(physical_to_logical(150, 144), 100);
    }

    #[test]
    fn scale_tolerates_zero_dpi() {
        // state.json 里的 dpi 可能是 0，这里必须退化为恒等而不是除零 panic。
        assert_eq!(scale(42, 0, 144), 42);
        assert_eq!(scale(42, 144, 0), 42);
    }

    #[test]
    fn scale_does_not_overflow() {
        assert_eq!(logical_to_physical(i32::MAX, 144), i32::MAX);
        assert_eq!(logical_to_physical(i32::MIN, 144), i32::MIN);
    }

    #[test]
    fn clamp_keeps_inside_container() {
        let container = Rect::new(0, 0, 1920, 1080);
        let clamped = clamp_rect(Rect::new(5000, 5000, 900, 420), container, 200, 120);
        assert_eq!(clamped, Rect::new(1020, 660, 900, 420));
    }

    #[test]
    fn clamp_handles_negative_coordinates() {
        // 副屏位于主屏左侧时 WorkerW 相对坐标为负，必须按容器下界收进来。
        let container = Rect::new(0, 0, 1920, 1080);
        let clamped = clamp_rect(Rect::new(-400, -300, 900, 420), container, 200, 120);
        assert_eq!(clamped, Rect::new(0, 0, 900, 420));
    }

    #[test]
    fn clamp_enforces_min_size() {
        let container = Rect::new(0, 0, 1920, 1080);
        let clamped = clamp_rect(Rect::new(10, 10, 1, 1), container, 200, 120);
        assert_eq!(clamped.w, 200);
        assert_eq!(clamped.h, 120);
    }

    #[test]
    fn clamp_never_exceeds_small_container() {
        let container = Rect::new(0, 0, 150, 90);
        let clamped = clamp_rect(Rect::new(0, 0, 900, 420), container, 200, 120);
        assert_eq!(clamped, Rect::new(0, 0, 150, 90));
    }

    #[test]
    fn intersects_detects_overlap() {
        let a = Rect::new(0, 0, 100, 100);
        assert!(a.intersects(&Rect::new(50, 50, 100, 100)));
        assert!(!a.intersects(&Rect::new(200, 0, 100, 100)));
        // 只接触边缘不算交集。
        assert!(!a.intersects(&Rect::new(100, 0, 100, 100)));
    }
}
