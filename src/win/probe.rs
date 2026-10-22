//! 真机输入自检：只往**自己的窗口**投递消息，绝不碰全局输入队列。
//!
//! 历史教训（必须保留）：上一版用 `mouse_event` / `SendInput` 往交互桌面注入真实事件，
//! 结果是「窗口被 Chrome 盖住 → 点击与按键全打进用户正在用的 Chrome」，
//! 等于程序抢了用户的鼠标和键盘。任何自动化都不许再走这条路：
//! 注入真实输入需要前台权限、会把事件投给当前焦点窗口，且无法限定只落在自己身上。
//!
//! 现在只做**消息级**自检：`PostMessage(WM_LBUTTONDOWN/WM_CHAR/WM_IME_CHAR)`。
//! 它验证的是本进程的消息处理链路（命中测试 → 取焦点 → 键序列化 → 写 PTY），
//! 不验证操作系统的真实路由——那一层只能由用户在真实桌面上点一下来确认。
//!
//! 只在 `--probe-input` 下启用。窗口被遮挡时不会影响任何其他程序。

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, MOUSE_EVENT_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow, SetForegroundWindow};

use crate::diag;
use crate::win::dock::hit_report;

/// 自检的推进步骤，由主线程按定时器调用。
#[derive(Debug, Clone, Copy)]
pub enum Phase {
    /// 投递一次点击。
    Click,
    /// 按住左键拖出一段选区（不抬起，让选区留在画面上）。
    Drag,
    /// 投递几个字符（涵盖 ASCII、CJK 与代理对）。
    Type,
    /// 结算。
    Report,
    // ---- 真实输入探针（`--probe-real`）----
    /// 抬到最前并确认自己拿到前台。
    RealRaise,
    /// 真实左键单击（`SendInput`）。
    RealClick,
    /// 真实按住拖选。
    RealDrag,
    /// 真实按键（虚拟键 + Unicode 两种）。
    RealType,
    /// 沉回底层、还原光标与前台。
    RealSink,
    /// 真实输入结算。
    RealReport,
}

/// 客户区中心点的屏幕坐标。窗口无效时返回 `None`。
pub fn center_of(hwnd: HWND) -> Option<POINT> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    let mut rect = RECT::default();
    // SAFETY: rect 是栈上可写结构；hwnd 无效时返回 Err。
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    if rect.right <= 0 || rect.bottom <= 0 {
        return None;
    }
    let mut point = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    // SAFETY: point 是栈上可写结构。
    unsafe {
        let _ = ClientToScreen(hwnd, &mut point);
    }
    Some(point)
}

/// 往自己的窗口投递一次左键按下+抬起（屏幕坐标，`hit_test` 自己会转客户区）。
pub fn post_click(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP};

    let Some(point) = center_of(hwnd) else {
        return false;
    };
    let lparam = LPARAM(((point.y as isize) << 16) | (point.x as isize & 0xffff));
    // SAFETY: 只投递消息，不触碰任何全局输入状态。
    unsafe {
        let down = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), lparam);
        let up = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), lparam);
        down.is_ok() && up.is_ok()
    }
}

/// 按住左键从客户区中心拖到右侧（屏幕坐标，不抬起——选区会留在画面上，
/// 用来给「选区在深/浅壁纸下都可见」留一份可复现的证据）。
pub fn post_drag(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, PostMessageW, WM_LBUTTONDOWN, WM_MOUSEMOVE,
    };

    let mut rect = RECT::default();
    // SAFETY: rect 是栈上可写结构。
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() || rect.right <= 0 {
        return false;
    }
    let mut start = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    let mut end = POINT {
        x: start.x + rect.right / 4,
        y: start.y,
    };
    // SAFETY: 两个点都是栈上可写结构。
    unsafe {
        let _ = ClientToScreen(hwnd, &mut start);
        let _ = ClientToScreen(hwnd, &mut end);
    }
    let lparam_of = |point: POINT| LPARAM(((point.y as isize) << 16) | (point.x as isize & 0xffff));
    // SAFETY: 只往自己的窗口队列里塞消息，不碰全局输入。
    unsafe {
        let down = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), lparam_of(start));
        let moved = PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(1), lparam_of(end));
        down.is_ok() && moved.is_ok()
    }
}

/// 往自己的窗口投递字符：ASCII 走 `WM_CHAR`，CJK 走 `WM_IME_CHAR`，非 BMP 拆代理对。
///
/// 返回投递成功的字符数，便于和 PTY 字节数对账。
pub fn post_text(hwnd: HWND, text: &str) -> usize {
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CHAR, WM_IME_CHAR};

    let mut posted = 0;
    for ch in text.chars() {
        if (ch as u32) < 0x80 {
            // SAFETY: 只往自己的窗口队列里塞消息。
            if unsafe { PostMessageW(Some(hwnd), WM_CHAR, WPARAM(ch as usize), LPARAM(1)) }.is_ok()
            {
                posted += 1;
            }
            continue;
        }
        // 真实 IME 对非 BMP 字符就是发两条 `WM_IME_CHAR`（高位/低位代理项），
        // 这里照原样发，才能验证 `commit_ime_unit` 的配对逻辑。
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf) {
            // SAFETY: 同上。
            if unsafe { PostMessageW(Some(hwnd), WM_IME_CHAR, WPARAM(*unit as usize), LPARAM(1)) }
                .is_ok()
            {
                posted += 1;
            }
        }
    }
    posted
}

// ---------------------------------------------------------------- 真实输入探针
//
// 为什么必须有这一层：`PostMessage` 只验证「消息处理」正确，绕过了两个真正
// 容易坏掉的环节——**命中测试**（点击有没有落到本窗口）与**激活**（前台有没有
// 交给我）。真机上「点不动、打不了字」恰好只可能坏在这两处，所以必须有一次
// 带真实 `SendInput` 的验证。
//
// 安全约束：真实输入会动光标与键盘，一旦前台不是自己，事件就打进别人的窗口。
// 因此每一步发送之前都重新确认 `GetForegroundWindow() == 本窗口`，
// 不满足就立刻中止（不发送任何事件）；结束时还原光标位置与前台窗口。
// 这条约束是上一轮「抢了用户鼠标键盘」事故的直接产物，不许删。

/// 真实输入探针的守卫状态。
pub struct RealGuard {
    previous_foreground: isize,
    cursor: (i32, i32),
}

fn foreground_is(hwnd: HWND) -> bool {
    // SAFETY: 只读查询，无副作用。
    unsafe { GetForegroundWindow() == hwnd }
}

/// 抬起窗口到最前、抢到前台，并记录还原所需的状态。
///
/// 返回 `None` 表示前台没拿到——此时**一个事件都没发**，调用方必须中止整个探针。
pub fn begin_real(hwnd: HWND) -> Option<RealGuard> {
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    let previous_foreground = save_foreground();
    let mut cursor = POINT::default();
    // SAFETY: cursor 是栈上可写结构。
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }

    // SAFETY: 只改本窗口的 z-order，不激活、不改几何。
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    // 抬到最前之后再要前台：顺序反了会被前台锁挡掉。
    // 走 `dock::raise_and_focus`，它在 `SetForegroundWindow` 被前台锁拒绝时
    // 会用 `SwitchToThisWindow` 再试一次（任务栏点击用的同一条路径）。
    let _ = crate::win::dock::raise_and_focus(hwnd);
    let mut ok = foreground_is(hwnd);
    if !ok {
        // 前台锁拒绝后台进程抢前台，唯一合法的破法就是「真点一下自己的窗口」：
        // 用户输入会授予前台权。点击前必须确认该坐标上的窗口就是自己，
        // 否则宁可不点 —— 上一轮抢用户鼠标的事故就是这么来的。
        ok = click_self(hwnd) && foreground_is(hwnd);
    }
    note(
        Phase::RealRaise,
        &format!(
            "抬到最前 foreground={ok} 命中={} 光标=({},{})",
            hit_report(hwnd),
            cursor.x,
            cursor.y
        ),
    );
    if !ok {
        return None;
    }
    Some(RealGuard {
        previous_foreground,
        cursor: (cursor.x, cursor.y),
    })
}

/// 真实点击自己窗口的中心，用来从「后台进程」变成「前台进程」。
///
/// 只在那一点确实属于本窗口时才点；任何不确定的情况都返回 false 并且一个事件都不发。
fn click_self(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, WindowFromPoint};

    let mut client = RECT::default();
    // SAFETY: client 是栈上可写结构。
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return false;
    }
    let mut center = POINT {
        x: (client.right - client.left) / 2,
        y: (client.bottom - client.top) / 2,
    };
    // SAFETY: center 是可写结构，hwnd 有效。
    if !unsafe { ClientToScreen(hwnd, &mut center) }.as_bool() {
        return false;
    }
    // SAFETY: 只读查询该坐标上的窗口。
    let target = unsafe { WindowFromPoint(center) };
    if target != hwnd {
        note(
            Phase::RealRaise,
            &format!("拒绝点击：({},{}) 上的窗口不是自己", center.x, center.y),
        );
        return false;
    }
    if !move_cursor_to(center.x, center.y) {
        return false;
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    let down = send_input(&[mouse_input(
        windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_LEFTDOWN,
    )]);
    std::thread::sleep(std::time::Duration::from_millis(40));
    let up = send_input(&[mouse_input(
        windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_LEFTUP,
    )]);
    std::thread::sleep(std::time::Duration::from_millis(150));
    let ok = down == 1 && up == 1;
    note(
        Phase::RealRaise,
        &format!("点自己一下取前台: down={down} up={up} ok={ok}"),
    );
    ok
}

fn send_input(inputs: &[INPUT]) -> usize {
    use windows::Win32::UI::Input::KeyboardAndMouse::SendInput;
    if inputs.is_empty() {
        return 0;
    }
    // SAFETY: inputs 是有效切片，长度与指针匹配；SendInput 只读它。
    let sent = unsafe { SendInput(inputs, core::mem::size_of::<INPUT>() as i32) };
    sent as usize
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT};
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn moved_input(nx: i32, ny: i32) -> INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEINPUT,
        MOUSE_EVENT_FLAGS,
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: MOUSE_EVENT_FLAGS(MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 把虚拟键换算成扫描码。
///
/// 真实键盘事件一定带扫描码，`TranslateMessage` 靠它（而不是虚拟键）生成
/// `WM_CHAR`。注入时把 `wScan` 留成 0，窗口收到的 `WM_KEYDOWN` 就是
/// `lparam=0x1`（scan=0），于是**永远不会有字符**——这曾让真实探针误判
/// 「打字不通」，其实是探针自己不像真键盘。
fn scan_for(vk: u16) -> u16 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC};
    // SAFETY: 纯换算，无副作用。
    unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) as u16 }
}

fn key_input(vk: u16, up: bool) -> INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan_for(vk),
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 把光标移到屏幕坐标（绝对定位需要归一化到 0..=65535）。
fn move_cursor_to(x: i32, y: i32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    // SAFETY: 只读查询。
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    if width <= 1 || height <= 1 {
        return false;
    }
    let nx = ((i64::from(x) * 65535) / (i64::from(width) - 1)) as i32;
    let ny = ((i64::from(y) * 65535) / (i64::from(height) - 1)) as i32;
    send_input(&[moved_input(nx, ny)]) == 1
}

/// 真实点击：光标移到客户区中心 → 按下 → 抬起。
pub fn real_click(hwnd: HWND, _guard: &RealGuard) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSE_EVENT_FLAGS,
    };
    if !foreground_is(hwnd) {
        note(Phase::RealClick, "守卫拦截：前台不是本窗口，未发送任何事件");
        return false;
    }
    let Some(point) = center_of(hwnd) else {
        return false;
    };
    let moved = move_cursor_to(point.x, point.y);
    let down = send_input(&[mouse_input(MOUSE_EVENT_FLAGS(MOUSEEVENTF_LEFTDOWN.0))]) == 1;
    let up = send_input(&[mouse_input(MOUSE_EVENT_FLAGS(MOUSEEVENTF_LEFTUP.0))]) == 1;
    note(
        Phase::RealClick,
        &format!(
            "真实点击@({},{}) moved={moved} down={down} up={up}",
            point.x, point.y
        ),
    );
    moved && down && up
}

/// 真实拖选：中心左侧按下 → 分四步右移 → 抬起。
pub fn real_drag(hwnd: HWND, _guard: &RealGuard) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSE_EVENT_FLAGS,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    if !foreground_is(hwnd) {
        note(Phase::RealDrag, "守卫拦截：前台不是本窗口，未发送任何事件");
        return false;
    }
    let mut rect = RECT::default();
    // SAFETY: rect 是栈上可写结构。
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() || rect.right <= 0 {
        return false;
    }
    let row = rect.bottom / 3;
    let mut start = POINT {
        x: rect.right / 8,
        y: row,
    };
    let mut end = POINT {
        x: rect.right / 2,
        y: row,
    };
    // SAFETY: 两个点都是栈上可写结构。
    unsafe {
        let _ = ClientToScreen(hwnd, &mut start);
        let _ = ClientToScreen(hwnd, &mut end);
    }
    let moved = move_cursor_to(start.x, start.y);
    let down = send_input(&[mouse_input(MOUSE_EVENT_FLAGS(MOUSEEVENTF_LEFTDOWN.0))]) == 1;
    let mut steps = 0;
    for step in 1..=4 {
        let x = start.x + (end.x - start.x) * step / 4;
        if move_cursor_to(x, start.y) {
            steps += 1;
        }
    }
    let up = send_input(&[mouse_input(MOUSE_EVENT_FLAGS(MOUSEEVENTF_LEFTUP.0))]) == 1;
    note(
        Phase::RealDrag,
        &format!(
            "真实拖选 ({},{})->({},{}) moved={moved} steps={steps} down={down} up={up}",
            start.x, start.y, end.x, end.y
        ),
    );
    moved && down && up
}

/// 真实按键：一个虚拟键（走 `WM_KEYDOWN`）+ 三个 Unicode 字符（走 `WM_CHAR`/`WM_IME_CHAR`）。
pub fn real_type(hwnd: HWND, _guard: &RealGuard) -> usize {
    if !foreground_is(hwnd) {
        note(Phase::RealType, "守卫拦截：前台不是本窗口，未发送任何事件");
        return 0;
    }
    // 只用虚拟键：这是物理键盘唯一的送法。`KEYEVENTF_UNICODE` 注入在本机
    // 会以 `scan=0` 到达窗口（字符丢失），拿它当判据会把环境缺陷算到产品头上。
    // 三个键覆盖三条不同的翻译路径：字母 A、OEM 分号 0xBA、OEM 斜杠 0xBF。
    let mut sent = 0;
    for vk in [0x41u16, 0xba, 0xbf] {
        if send_input(&[key_input(vk, false), key_input(vk, true)]) == 2 {
            sent += 1;
        }
    }
    note(Phase::RealType, &format!("真实按键 vk_sent={sent}/3"));
    sent
}

/// 还原：光标回原位、沉回底层、前台还给原窗口。
/// `sink` = 是否把窗口放回最底层（沉底模式）；普通模式只还原光标与前台。
pub fn end_real(hwnd: HWND, guard: RealGuard, sink: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_BOTTOM, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let _ = move_cursor_to(guard.cursor.0, guard.cursor.1);
    if sink {
        // SAFETY: 只改本窗口 z-order，不动位置与尺寸。
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_BOTTOM),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    restore_foreground(guard.previous_foreground);
    // 沉底之后再测一次命中：这一步回答的是「常驻最底层之后，用户还能不能点到它」。
    // 如果这里报的是 Progman/Chrome 而不是自己，说明窗口在桌面之上但被别的窗口
    // 盖住（正常），或者落到了桌面之下（缺陷）。
    note(
        Phase::RealSink,
        &format!(
            "已还原 光标=({},{}) 前台={:#x} 沉底后命中={}",
            guard.cursor.0,
            guard.cursor.1,
            guard.previous_foreground,
            hit_report(hwnd)
        ),
    );
}

/// 打印一行自检结论。
/// 自检开始前记下前台窗口，结算时还回去。
///
/// 自检为了验证「点击 → 取焦点」这条链会真的去抢前台，
/// 不还回去就等于每次跑测试都把用户从正在用的窗口里踢出来。
pub fn save_foreground() -> isize {
    // SAFETY: 只读查询，无参数无生命周期问题。
    unsafe { GetForegroundWindow().0 as isize }
}

/// 把前台还给 `previous`。目标已销毁或本来就是自己就什么都不做。
pub fn restore_foreground(previous: isize) {
    // 没有可归还的前台时也要留一行日志：回归脚本靠这条断言「自检收尾干净」，
    // 静默 return 会让断言在某些轮次随机失败（踩过一次）。
    if previous == 0 {
        diag::info("输入自检: 没有可归还的前台（启动时就没有前台窗口）");
        return;
    }
    let hwnd = HWND(previous as *mut core::ffi::c_void);
    // SAFETY: `IsWindow` 只检查句柄有效性；无效句柄不会进入 `SetForegroundWindow`。
    if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
        diag::info(&format!(
            "输入自检: 原前台 {previous:#x} 已不存在，跳过归还"
        ));
        return;
    }
    // SAFETY: 句柄已确认有效；失败也只是没还回去，不影响自检结论。
    let restored = unsafe { SetForegroundWindow(hwnd).as_bool() };
    diag::info(&format!("输入自检: 前台已还给 {previous:#x}（{restored}）"));
}

pub fn note(phase: Phase, detail: &str) {
    diag::info(&format!("输入自检[{}]: {detail}", {
        match phase {
            Phase::RealRaise => "真实抬起",
            Phase::RealClick => "真实点击",
            Phase::RealDrag => "真实拖选",
            Phase::RealType => "真实按键",
            Phase::RealSink => "真实还原",
            Phase::RealReport => "真实结算",
            Phase::Click => "点击",
            Phase::Drag => "拖选",
            Phase::Type => "按键",
            Phase::Report => "结算",
        }
    }));
}
