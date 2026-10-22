//! 桌面层停靠：顶层窗口 + z-order 沉底。
//!
//! M6 架构裁决（`docs/aegis/reviews/security-arch-layer-decision.md`）：
//! 放弃 `SetParent` 到 `WorkerW`。一旦变成子窗口，键盘焦点就永远留在 explorer 的
//! 线程输入队列里——`SetFocus` 只能作用于同一队列的窗口，`SetForegroundWindow(子窗口)`
//! 是空操作，于是 `WM_SETFOCUS` 永远不来，终端「点得动、打不了字」。
//!
//! 现在的做法：
//! - 窗口保持顶层（`parent == NULL`），这样点击激活与键盘焦点都是正常的；
//! - 用 z-order 把窗口压到最底（壁纸之上、所有普通窗口之下）近似「桌面层」；
//! - **只在失去焦点时重申**沉底，用户正在输入时不去抢 z-order；
//! - `Watchdog` 定期检查可见性，被「显示桌面」之类的操作收走后自己找回来。
//!
//! 代价：失去 Win+D 免疫（子窗口方案有，但它换来的是键盘彻底失效），
//! 以及不再需要 `WS_EX_NOACTIVATE`（那会让窗口永远拿不到焦点）。

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetActiveWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetClientRect, GetForegroundWindow, GetParent, GetWindowThreadProcessId,
    IsWindowVisible, SetForegroundWindow, SetWindowPos, ShowWindow, SwitchToThisWindow,
    WindowFromPoint, HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SW_SHOWNOACTIVATE,
};

use crate::diag;

/// 看门狗的检查间隔。2s 足够快，又不至于每秒打扰合成器。
const CHECK_INTERVAL: Duration = Duration::from_secs(2);
/// 两次找回之间的最小间隔，避免和用户手里的窗口来回抢。
const RESTORE_COOLDOWN: Duration = Duration::from_secs(3);
/// 一轮里最多找回几次；超过就只记日志、不再动手。
const MAX_RESTORES: u32 = 3;

/// 把窗口压到 z-order 最底部。
///
/// 只改 z-order，不动位置、尺寸、激活状态与所有者关系。
pub fn send_to_bottom(hwnd: HWND) -> bool {
    // SAFETY: hwnd 由 CreateWindowExW 创建且在调用期间有效；
    // 四个标志位都声明「不改动」对应属性，因此不会触发 WM_SIZE/WM_MOVE。
    let result = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    };
    if let Err(err) = result {
        diag::warn(&format!("沉底失败: {err}"));
        return false;
    }
    true
}

/// 三种焦点相关事实的快照，用于「点击后到底有没有拿到键盘」的现场取证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusReport {
    /// 本窗口现在是前台窗口。
    pub foreground: bool,
    /// `GetFocus` 确实返回了本窗口。
    pub focus_set: bool,
}

impl FocusReport {
    /// 键盘输入能否到达本窗口的最小条件。
    pub fn keyboard_ready(&self) -> bool {
        self.foreground && self.focus_set
    }

    pub fn describe(&self) -> String {
        format!(
            "foreground={} focus_set={} keyboard_ready={}",
            self.foreground,
            self.focus_set,
            self.keyboard_ready()
        )
    }
}

/// 主动拿下前台与键盘焦点。
///
/// 顺序不能反：先 `SetForegroundWindow`（顶层窗口才有效），再 `SetActiveWindow`
/// 把激活状态落到本线程，最后 `SetFocus` 才会被接受。三步之后立刻回读校验，
/// 而不是假定成功——「假定成功」正是上一版把键盘弄丢的原因。
pub fn take_keyboard_focus(hwnd: HWND) -> FocusReport {
    // SAFETY: 五处调用都只作用于本进程自己的窗口，不涉及指针与生命周期。
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let _ = SetActiveWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        FocusReport {
            foreground: GetForegroundWindow() == hwnd,
            focus_set: GetFocus() == hwnd,
        }
    }
}

/// 抬到最前并取键盘焦点：启动时和 `Ctrl+Alt+T` 用。
///
/// 只调 `SetForegroundWindow` 是不够的：Windows 的前台锁会拒绝后台进程抢前台，
/// 实测 `focus_set=true` 而 `foreground=false`，窗口仍然停在最大化窗口下面，
/// 用户既看不见也点不到。因此分三步：
/// 1. `SetWindowPos(HWND_TOP)` 先抬 z-order —— 这一步不受前台锁限制；
/// 2. 正常取焦点；
/// 3. 还没拿到前台，就用 `SwitchToThisWindow`（任务栏点击用的同一条路径）再来一次。
///
/// 三步都只作用于本进程自己的窗口，不碰 `AttachThreadInput`（那会污染
/// `GetAsyncKeyState` 并可能卡死 explorer 的输入队列）。
pub fn raise_and_focus(hwnd: HWND) -> FocusReport {
    // SAFETY: 只改本窗口的 z-order 与激活状态，不改位置尺寸。
    //
    // 先 `HWND_TOPMOST`：实测 `HWND_TOP` 抬不过当前前台窗口（前台线程有额外特权，
    // 后台进程的普通 z-order 调整会被压回前台窗口下面），而 topmost 可以。
    // 抬上去之后立刻尝试取焦点；一旦失去焦点会 `drop_topmost` 落回普通层，
    // 所以不会长期霸占屏幕最上层。
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
    let report = take_keyboard_focus(hwnd);
    if report.foreground {
        return report;
    }
    // SAFETY: SwitchToThisWindow 只作用于传入的本进程窗口。
    unsafe {
        SwitchToThisWindow(hwnd, true);
    }
    let retried = take_keyboard_focus(hwnd);
    diag::info(&format!(
        "前台锁拦住了 SetForegroundWindow，SwitchToThisWindow 重试: {}",
        retried.describe()
    ));
    retried
}

/// 取消 topmost，落回普通 z-order 层（不改变激活状态）。
///
/// 与 `raise_and_focus` 配对：抬起来是为了让用户看见并点得到，
/// 一旦失焦就退回普通层，避免长期盖在别的窗口上面。
pub fn drop_topmost(hwnd: HWND) {
    // SAFETY: 只改本窗口的 z-order 属性。
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// 窗口是否仍然可见（被 Win+D、任务栏「显示桌面」收走后为 false）。
pub fn is_visible(hwnd: HWND) -> bool {
    // SAFETY: 只读查询，hwnd 无效时返回 false。
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// 窗口是不是顶层窗口（没有父窗口）。
pub fn is_top_level(hwnd: HWND) -> bool {
    // SAFETY: 只读查询。`GetParent` 在子窗口返回 `Ok(父句柄)`，顶层窗口返回 `Err`。
    unsafe { GetParent(hwnd).is_err() }
}

/// 只读地报告当前前台/焦点状态，不改变任何东西。
pub fn focus_state(hwnd: HWND) -> FocusReport {
    // SAFETY: 三个都是只读查询。
    unsafe {
        FocusReport {
            foreground: GetForegroundWindow() == hwnd,
            focus_set: GetFocus() == hwnd,
        }
    }
}

/// 自检：窗口客户区中心点「实际命中」的是谁。
///
/// 这是判断遮挡唯一可靠的手段。外部探针（自动化脚本）常常跑在受限的桌面里，
/// 枚举不到也点不到真实窗口，结论全是假的；只有本进程问自己才有意义。
/// 返回 `自己:TlShellWindow` 说明窗口在可见层且可点击，返回别的类名说明被盖住了。
pub fn hit_report(hwnd: HWND) -> String {
    let mut rect = RECT::default();
    // SAFETY: rect 是栈上可写结构；hwnd 无效时返回 false。
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return "客户区读取失败".to_owned();
    }
    let mut point = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    // SAFETY: point 是栈上可写结构。
    unsafe {
        let _ = ClientToScreen(hwnd, &mut point);
    }
    // SAFETY: 只读查询；point 由上面的 `ClientToScreen` 填好。
    let hit = unsafe { WindowFromPoint(point) };
    if hit.0.is_null() {
        return format!("({}, {}) 没有任何窗口", point.x, point.y);
    }
    let mut buf = [0u16; 128];
    // SAFETY: buf 是足够长的可写缓冲区，返回写入的字符数。
    let len = unsafe { GetClassNameW(hit, &mut buf) }.max(0) as usize;
    let class = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
    let mut pid = 0u32;
    // SAFETY: pid 是栈上可写变量。
    unsafe {
        GetWindowThreadProcessId(hit, Some(&mut pid));
    }
    let owner = if hit == hwnd {
        "自己"
    } else {
        "其他窗口"
    };
    format!(
        "({}, {}) -> {owner}:{class} pid={pid} 句柄=0x{:x}",
        point.x, point.y, hit.0 as isize
    )
}

/// 找回策略的纯逻辑部分，便于单测（真实 HWND 不好在单测里造）。
#[derive(Debug, Default)]
struct RestorePolicy {
    restores: u32,
    last_restore: Option<Instant>,
}

impl RestorePolicy {
    fn should_restore(&self, now: Instant, hidden: bool) -> bool {
        if !hidden || self.restores >= MAX_RESTORES {
            return false;
        }
        match self.last_restore {
            Some(at) => now.duration_since(at) >= RESTORE_COOLDOWN,
            None => true,
        }
    }

    fn record_restore(&mut self, now: Instant) {
        self.restores += 1;
        self.last_restore = Some(now);
    }
}

/// 可见性看门狗（安全评审 S-01b）。
#[derive(Debug)]
pub struct Watchdog {
    last_check: Instant,
    policy: RestorePolicy,
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl Watchdog {
    pub fn new() -> Self {
        Self {
            last_check: Instant::now(),
            policy: RestorePolicy::default(),
        }
    }

    /// 周期性调用。窗口被收走且限速允许时把它找回来，返回一句可供日志的原因。
    pub fn poll(&mut self, hwnd: HWND) -> Option<String> {
        let now = Instant::now();
        if now.duration_since(self.last_check) < CHECK_INTERVAL {
            return None;
        }
        self.last_check = now;

        if !self.policy.should_restore(now, !is_visible(hwnd)) {
            return None;
        }
        self.policy.record_restore(now);
        // SAFETY: hwnd 有效；SW_SHOWNOACTIVATE 不抢焦点，符合「恢复但不打扰」。
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        send_to_bottom(hwnd);
        Some(format!(
            "窗口被隐藏，已恢复显示（第 {} 次，最多 {MAX_RESTORES} 次）",
            self.policy.restores
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_restores_only_when_hidden() {
        let now = Instant::now();
        let policy = RestorePolicy::default();
        assert!(!policy.should_restore(now, false));
        assert!(policy.should_restore(now, true));
    }

    #[test]
    fn policy_respects_cooldown() {
        let mut policy = RestorePolicy::default();
        let start = Instant::now();
        assert!(policy.should_restore(start, true));
        policy.record_restore(start);
        // 冷却期内不再动手。
        assert!(!policy.should_restore(start + Duration::from_secs(1), true));
        // 过了冷却期可以再来一次。
        assert!(policy.should_restore(start + RESTORE_COOLDOWN, true));
    }

    #[test]
    fn policy_stops_after_max_restores() {
        let mut policy = RestorePolicy::default();
        let mut now = Instant::now();
        for _ in 0..MAX_RESTORES {
            assert!(policy.should_restore(now, true));
            policy.record_restore(now);
            now += RESTORE_COOLDOWN;
        }
        // 次数用完后只记日志，不再和用户抢窗口。
        assert!(!policy.should_restore(now + RESTORE_COOLDOWN, true));
    }

    #[test]
    fn keyboard_ready_needs_both_foreground_and_focus() {
        let report = FocusReport {
            foreground: true,
            focus_set: false,
        };
        assert!(!report.keyboard_ready());
        assert!(FocusReport {
            foreground: true,
            focus_set: true
        }
        .keyboard_ready());
    }
}
