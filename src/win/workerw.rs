//! 把窗口挂到桌面的 WorkerW 层，让它成为“壁纸的一部分”。
//!
//! 背景：`Progman`（桌面）下面挂着 `SHELLDLL_DefView`（图标层）与 `WorkerW`（壁纸层）。
//! 把窗口 `SetParent` 到壁纸层之后，按 `Win+D` 不会把它最小化，因为它根本不是顶层窗口。
//!
//! 候选策略按可靠性排序，任一成功即止：
//! 1. `Progman` 直接子窗口里的 `WorkerW`（本机实测的拓扑，最稳）。
//! 2. 顶层 `WorkerW` 中“没有 `SHELLDLL_DefView` 子窗口”的那个（经典写法）。
//! 3. 都找不到 → 保持顶层窗口，只记一条告警（计划书 §5.3 的降级要求）。
//!
//! 失败绝不 panic：挂不上桌面是功能降级，不是崩溃理由。

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetParent, GetWindowLongPtrW, GetWindowThreadProcessId, SetParent,
    SetWindowLongPtrW, SetWindowPos, GWL_STYLE, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    WS_CHILD, WS_POPUP,
};

/// 挂载结果。`Fallback` 里带原因，供日志与自检使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// 成功挂到 WorkerW 上，附带找到它的策略名。
    Attached {
        workerw: HWND,
        strategy: &'static str,
    },
    /// 挂不上，窗口保持顶层。
    Fallback { reason: String },
}

impl Attach {
    #[cfg(test)]
    pub fn is_attached(&self) -> bool {
        matches!(self, Attach::Attached { .. })
    }
}

/// 取窗口所属进程的可执行文件名（小写，不含路径）。
///
/// 拿不到（权限、进程已退出、查询失败）返回 `None`——调用方必须把 `None`
/// 当成“未知”而不是“通过”。
fn owner_process_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    // SAFETY: 只读查询窗口对应的线程/进程号。
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return None;
    }
    // SAFETY: 只申请查询权限，句柄在 CloseHandle 前有效。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 512];
    let mut len = buffer.len() as u32;
    // SAFETY: 缓冲区长度由 len 给出，返回实际写入长度。
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: 与 OpenProcess 配对。
    unsafe {
        let _ = CloseHandle(process);
    }
    ok.ok()?;
    let path = String::from_utf16_lossy(&buffer[..len as usize]);
    path.rsplit(['\\', '/'])
        .next()
        .map(|name| name.to_ascii_lowercase())
}

/// 这个窗口是不是桌面本身（`explorer.exe`）的。
///
/// 安全评审 S-02：三级兜底只按窗口类名筛选，同完整性级别的任意进程都能注册
/// 一个叫 `WorkerW` 的窗口，然后我们就把终端 `SetParent` 到别人的窗口里。
/// 按类名找完之后必须再验一次宿主进程。
fn is_desktop_owner(hwnd: HWND) -> bool {
    match owner_process_name(hwnd) {
        Some(name) if name == "explorer.exe" => true,
        Some(name) => {
            crate::diag::warn(&format!(
                "WorkerW 候选 0x{:x} 属于 {name}，不是 explorer.exe，跳过",
                hwnd.0 as isize
            ));
            false
        }
        None => {
            crate::diag::warn(&format!(
                "WorkerW 候选 0x{:x} 的宿主进程无法确认，按不可信跳过",
                hwnd.0 as isize
            ));
            false
        }
    }
}

/// 找壁纸层窗口。
pub fn find_workerw() -> Option<(HWND, &'static str)> {
    // 策略 1：Progman 的直接子窗口。
    // SAFETY: 只读窗口枚举，句柄由系统返回。
    unsafe {
        if let Ok(progman) = FindWindowW(w!("Progman"), None) {
            if !progman.0.is_null() {
                if let Ok(defview) =
                    FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None)
                {
                    if let Ok(workerw) =
                        FindWindowExW(Some(progman), Some(defview), w!("WorkerW"), None)
                    {
                        if !workerw.0.is_null() && is_desktop_owner(workerw) {
                            return Some((workerw, "progman-child"));
                        }
                    }
                }
                // 有些系统里 WorkerW 排在 DefView 之前，直接找一遍。
                if let Ok(workerw) = FindWindowExW(Some(progman), None, w!("WorkerW"), None) {
                    if !workerw.0.is_null() && is_desktop_owner(workerw) {
                        return Some((workerw, "progman-child-first"));
                    }
                }
            }
        }
    }

    // 策略 2：顶层 WorkerW 里挑一个不带 SHELLDLL_DefView 子窗口的。
    // SAFETY: 只读窗口枚举。
    unsafe {
        let mut previous: Option<HWND> = None;
        loop {
            let Ok(candidate) = FindWindowExW(None, previous, w!("WorkerW"), None) else {
                break;
            };
            if candidate.0.is_null() {
                break;
            }
            previous = Some(candidate);
            let has_defview = FindWindowExW(Some(candidate), None, w!("SHELLDLL_DefView"), None)
                .map(|handle| !handle.0.is_null())
                .unwrap_or(false);
            if !has_defview && is_desktop_owner(candidate) {
                return Some((candidate, "top-level-workerw"));
            }
        }
    }

    None
}

/// 把 `hwnd` 挂到壁纸层。调用方负责在失败时继续按普通窗口跑。
pub fn attach(hwnd: HWND) -> Attach {
    let Some((workerw, strategy)) = find_workerw() else {
        return Attach::Fallback {
            reason: "找不到 WorkerW 壁纸层窗口".to_owned(),
        };
    };

    // `SetParent` 成功时也可能返回 NULL（原父窗口本来就是桌面），
    // 所以不能拿返回值判空；`windows` 已经把「返回 0 且 GetLastError != 0」折成 Err，
    // 这里只认 Ok/Err。
    // SAFETY: 两个句柄都来自系统；不涉及调用方指针。
    let first = unsafe { SetParent(hwnd, Some(workerw)) };
    if let Err(err) = first {
        // SAFETY: 读取本线程最近一次错误码，仅用于日志。
        let code = unsafe { GetLastError() };
        return Attach::Fallback {
            reason: format!("SetParent 失败: {err}（Win32 错误码 {code:?}）"),
        };
    }
    // `SetParent` 可能返回 Ok 却没真正改父窗口（返回值判空本身不可靠），
    // 所以以「回读到的父窗口」为准。
    if parent_of(hwnd).is_none() {
        // 退一步：把样式从 WS_POPUP 换成 WS_CHILD 再挂一次。
        // 系统对「弹出窗口」的 SetParent 会解释成设置 owner，而 owner 依然会被
        // Win+D 最小化，必须变成真正的子窗口。
        // SAFETY: hwnd 有效，只改样式位。
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let child_style = (style & !(WS_POPUP.0 as isize)) | WS_CHILD.0 as isize;
            SetWindowLongPtrW(hwnd, GWL_STYLE, child_style);
            let _ = SetParent(hwnd, Some(workerw));
        }
    }

    // 挂上去之后必须显式提到兄弟窗口的最前面：WorkerW 里通常还躺着壁纸程序
    // （本机是 Wallpaper Engine）的窗口，不提层级就会被它盖住。
    // 注意不能带 SWP_NOZORDER——那会让 hWndInsertAfter 失效。
    // SAFETY: hwnd 已挂到 workerw 下，只调整 Z 序。
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }

    // 立刻回读一次，确认挂载真的生效——`SetParent` 跨进程时可能被系统拒绝，
    // 而返回值本身不足以判断。
    let actual = parent_of(hwnd);
    crate::diag::info(&format!(
        "attach 回读: 期望父窗口={:?}, 实际父窗口={:?}, visible={}",
        workerw.0,
        actual.map(|handle| handle.0),
        // SAFETY: 只读查询。
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool() }
    ));

    Attach::Attached { workerw, strategy }
}

/// 当前父窗口，供自检断言“确实挂上去了”。
pub fn parent_of(hwnd: HWND) -> Option<HWND> {
    // SAFETY: 只读查询。
    let parent = unsafe { GetParent(hwnd) }.ok()?;
    if parent.0.is_null() {
        None
    } else {
        Some(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_variant_reports_state() {
        let attached = Attach::Attached {
            workerw: HWND(std::ptr::null_mut()),
            strategy: "test",
        };
        assert!(attached.is_attached());

        let fallback = Attach::Fallback {
            reason: "no workerw".to_owned(),
        };
        assert!(!fallback.is_attached());
    }
}
