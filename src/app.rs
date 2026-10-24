//! 应用装配与消息循环。
//!
//! 所有权约定（解决计划书 §5.2 与 §5.3-4 的冲突）：
//! - `Term` 由 UI 线程独占，PTY 读线程只把字节塞进 channel，绝不共享可变状态。
//! - 窗口过程里的一切 panic 都被 `catch_unwind` 拦住，转交 `DefWindowProcW`，
//!   因为 unwind 穿过 `extern "system"` 边界是未定义行为。

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Osc52, TermMode};
use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, PAINTSTRUCT};
use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmReleaseContext, ImmSetCompositionWindow, CFS_POINT, COMPOSITIONFORM,
    GCS_RESULTSTR,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, IsWindow, KillTimer,
    LoadCursorW, PostQuitMessage, RegisterWindowMessageW, SetCursor, SetTimer, ShowWindow,
    TranslateMessage, GWLP_USERDATA, GWL_EXSTYLE, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION,
    HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IDC_ARROW, IDC_HAND, IDC_IBEAM,
    IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, MA_ACTIVATE, MSG, SW_HIDE,
    SW_SHOW, SW_SHOWNOACTIVATE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_CREATE,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_HOTKEY,
    WM_IME_CHAR, WM_IME_COMPOSITION, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
    WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER,
};

use crate::config::{Config, FONT_SIZE_MAX, FONT_SIZE_MIN};
use crate::diag;
use crate::render::settings::Settings;
use crate::render::{Blink, FontSet, Layout, Painter, Theme};
use crate::term::input::{self, Key, Modifiers};
use crate::term::paste;
use crate::term::session::{Session, SpawnOptions};
use crate::term::{shell, TermSize, Terminal};
use crate::win::hotkey::{self, Hotkey};
use crate::win::ime;
use crate::win::surface::Surface as LayeredSurface;
use crate::win::window::{self, WindowSpec};
use crate::win::{clipboard, dock, dpi, elevation, layered, probe, single};

pub mod selftest;

/// 退出热键的注册 id。
pub const HOTKEY_ID_QUIT: i32 = 1;
/// `WM_MOUSELEAVE`（0x02A3）：鼠标离开窗口。
///
/// `windows` crate 把它放在要额外 feature 的 `UI::Controls` 里，这里直接写常量值。
const WM_MOUSELEAVE: u32 = 0x02A3;
/// 隐藏/清屏热键的注册 id。
pub const HOTKEY_ID_HIDE: i32 = 2;
/// 「提到最前 + 拿焦点」热键的注册 id。
///
/// 它存在的理由：窗口可能被别的窗口盖住（包括沉底模式下的必然情形），
/// 这时鼠标点不到它，只剩一个纯键盘的入口。
pub const HOTKEY_ID_RAISE: i32 = 3;

/// 判定「这一次上屏是不是上一次的重复」：内容相同、来源不同、时间相邻。
///
/// 抽成纯函数是为了能直接测：真实输入法上屏没法自动化，但这条规则可以。
fn ime_is_duplicate(
    last: Option<&(String, Instant, ImeSource)>,
    text: &str,
    source: ImeSource,
    now: Instant,
) -> bool {
    match last {
        Some((prev, at, prev_source)) => {
            prev == text && *prev_source != source && now.duration_since(*at) < IME_DUP_WINDOW
        }
        None => false,
    }
}

/// 输入法上屏消息来自哪条路。同一次上屏两条路都会来，只有内容比对才知道是不是重复。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImeSource {
    /// `WM_IME_COMPOSITION` + `GCS_RESULTSTR`（组字型输入法的主路径）。
    Composition,
    /// `WM_IME_CHAR`（逐码元，非 BMP 字符由高低代理项两发）。
    Char,
}

/// 唯一的定时器 id：同时驱动 PTY 泵与光标闪烁。
const TIMER_TICK: usize = 1;
/// 定时器周期。16ms ≈ 60Hz，是“内容驱动重绘”的节流下限。
const TICK_MS: u32 = 16;
/// 两次实际重绘之间的最小间隔，避免高频输出把 CPU 打满。
const RENDER_INTERVAL: Duration = Duration::from_millis(16);

/// 底部提示条（粘贴确认、告警）的存活时间。
const OVERLAY_TTL: Duration = Duration::from_secs(3);

/// 多行粘贴的二次确认窗口：超过这个时间再按 Ctrl+V 会重新提示。
const PASTE_CONFIRM_TTL: Duration = Duration::from_secs(3);

/// 启动告警（管理员终端、热键失败）留得久一点。
const ADMIN_NOTICE_TTL: Duration = Duration::from_secs(30);
/// `VK_PACKET`：字符装在扫描码里的虚拟键，系统不会为它补 `WM_CHAR`。
const VK_PACKET: u32 = 0xe5;

/// 抑制重复 `WM_CHAR` 的时间窗。
///
/// `TranslateMessage` 在同一轮消息泵里就把 `WM_CHAR` 排进队列，实际间隔在毫秒级；
/// 50ms 足够覆盖，又短到不会误伤用户的下一次真实输入。
const SUPPRESS_CHAR_WINDOW: Duration = Duration::from_millis(50);
/// 同一次输入法上屏可能先后走 `GCS_RESULTSTR` 和 `WM_IME_CHAR` 两条路。
/// 谁先谁后没有保证，所以去重按「内容相同 + 来源不同 + 时间相邻」判定。
const IME_DUP_WINDOW: Duration = Duration::from_millis(400);
/// `--dump-frame` 落盘前至少先画这么多帧：等 shell 首屏输出已经进网格，
/// 否则截到的是空白帧，看不出文字锐度（V-02）。
const DUMP_DELAY: Duration = Duration::from_millis(1500);

/// 边缘拖拽缩放的命中宽度（物理像素）。
const RESIZE_BAND: i32 = 5;
/// 四角拖拽缩放的命中尺寸（物理像素），比边宽一些才好抓。
const CORNER_BAND: i32 = 16;
/// 窗口最小尺寸（逻辑像素），来自 UI 规范。
const MIN_WIDTH: i32 = 480;
const MIN_HEIGHT: i32 = 240;

/// 一个终端标签：自己的网格、事件通道与 PTY 会话。
///
/// 输出通道按会话隔离（`Session::drain` 只取自己那条队列），所以切标签既不用给
/// 输出打标记，也不可能串台——这是「多终端可以叠放」能廉价做对的原因。
struct Tab {
    title: String,
    terminal: Option<Terminal>,
    events: Option<Receiver<Event>>,
    session: Option<Session>,
    /// 当前标签实际跑起来的 shell 类型（解析失败时是 `None`）。
    ///
    /// 清屏要按类型分派：PowerShell / bash 的 `Ctrl+L` 由 readline 自己清屏并重画提示符，
    /// 而 `cmd.exe` 不认 `Ctrl+L`，只能由我们清网格。
    shell_kind: Option<shell::ShellKind>,
}

impl Tab {
    fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            terminal: None,
            events: None,
            shell_kind: None,
            session: None,
        }
    }
}

/// 切换终端的一个候选项：写进配置的名字 + 弹窗上给人看的名字。
#[derive(Debug, Clone, Copy)]
struct ShellChoice {
    /// 写进 `shell.program` 的裸文件名，交给 [`shell::resolve`] 的白名单解析。
    program: &'static str,
    /// 弹窗里显示的说明（带上版本，用户才知道 pwsh 与 powershell 的区别）。
    label: &'static str,
}

/// 候选集合：新 → 旧 → 最旧，也是「前一个终端」这个直觉的顺序。
const SHELL_CHOICES: [ShellChoice; 3] = [
    ShellChoice {
        program: "pwsh.exe",
        label: "pwsh · PowerShell 7",
    },
    ShellChoice {
        program: "powershell.exe",
        label: "powershell · Windows 5.1",
    },
    ShellChoice {
        program: "cmd.exe",
        label: "cmd · 命令提示符",
    },
];

/// 切换终端弹窗：先选终端类型，再选「新开标签」还是「原地切换」。
///
/// 为什么要有弹窗：标签栏那个按钮原来是点一下就悄悄重启 shell，
/// 用户既不知道会切到哪一类终端，也不知道现有记录会不会被清掉。
/// 这两件事都必须让用户当场看得见、点得动。
struct ShellPicker {
    /// 本机真正能起来的候选（解析失败的直接不列，避免选了必然失败）。
    choices: Vec<ShellChoice>,
    /// 选中的候选下标。
    choice: usize,
    /// 是否新开标签：`false` = 原地换 shell，保留当前记录。
    new_session: bool,
    /// 焦点行：0 = 终端类型，1 = 切换方式。
    focus: usize,
}

impl ShellPicker {
    /// 按可用性过滤候选；一个都不剩时返回 `None`（不弹空窗）。
    fn available(shell_config: &crate::config::ShellConfig, allow_custom: bool) -> Option<Self> {
        let choices: Vec<ShellChoice> = SHELL_CHOICES
            .iter()
            .copied()
            .filter(|candidate| {
                // 只改一份副本去探测：别人的 args 保持原样，当前配置一个字都不动。
                let mut probe = shell_config.clone();
                probe.program = candidate.program.to_owned();
                shell::resolve(&probe, allow_custom).is_ok()
            })
            .collect();
        if choices.is_empty() {
            return None;
        }
        // 默认停在当前类型上：多数人打开弹窗只是想确认「现在是什么」。
        let choice = choices
            .iter()
            .position(|candidate| {
                candidate
                    .program
                    .eq_ignore_ascii_case(shell_config.program.as_str())
            })
            .unwrap_or(0);
        Some(Self {
            choices,
            choice,
            new_session: false,
            focus: 0,
        })
    }

    /// 面板要画的两行（与 `draw_panel` 的行序一致）。
    fn rows(&self) -> Vec<(&'static str, String)> {
        let label = self
            .choices
            .get(self.choice)
            .map_or("—", |candidate| candidate.label);
        let mode = if self.new_session {
            "新开标签（另起会话）"
        } else {
            "原地切换（保留记录）"
        };
        vec![("终端", label.to_owned()), ("方式", mode.to_owned())]
    }

    /// 当前选中的候选；下标越界时退化成第一个。
    fn selected(&self) -> Option<ShellChoice> {
        self.choices
            .get(self.choice)
            .copied()
            .or_else(|| self.choices.first().copied())
    }

    fn move_focus(&mut self, delta: i32) {
        let last = self.rows().len().saturating_sub(1) as i32;
        let next = self.focus as i32 + delta;
        self.focus = next.clamp(0, last) as usize;
    }

    /// 改当前行的值：第 0 行换候选，第 1 行换方式。
    fn adjust(&mut self, forward: bool) {
        if self.focus == 0 {
            self.cycle_choice(forward);
        } else {
            self.new_session = !self.new_session;
        }
    }

    fn cycle_choice(&mut self, forward: bool) {
        let len = self.choices.len();
        if len == 0 {
            return;
        }
        self.choice = if forward {
            (self.choice + 1) % len
        } else {
            (self.choice + len - 1) % len
        };
    }

    /// 鼠标点某一行：先移焦点，再像点一次那样把该行的值推到下一个。
    fn click_row(&mut self, row: usize) {
        self.focus = row.min(1);
        self.adjust(true);
    }
}

/// 一次弹窗的渲染数据：设置面板与切换终端弹窗共用同一条绘制路径。
struct PanelView {
    title: &'static str,
    rows: Vec<(&'static str, String)>,
    selected: usize,
    footer: &'static str,
    dirty: bool,
}

pub struct App {
    hwnd: HWND,
    config: Config,
    quit_hotkey: Option<Hotkey>,
    raise_hotkey: Option<Hotkey>,
    hide_hotkey: Option<Hotkey>,
    /// 记录热键注册失败的原因，供自检与日志使用。
    pub startup_warnings: Vec<String>,

    /// 自检路径不启动 shell、不建渲染面。
    headless: bool,
    /// 打开的终端标签；至少一个。每个标签自带网格、事件通道与 PTY 会话。
    tabs: Vec<Tab>,
    /// 当前显示的标签下标（永远落在 `0..tabs.len()`）。
    active_tab: usize,
    painter: Option<Painter>,
    surface: Option<LayeredSurface>,
    surface_size: (u32, u32),
    layout: Layout,
    blink: Blink,
    focused: bool,
    dirty: bool,
    last_render: Instant,
    /// 抑制随后那条重复 `WM_CHAR` 的截止时刻。
    ///
    /// 只对「字符键 + 已经被 `WM_KEYDOWN` 编码送走」这一种情况置位，而且带时间窗：
    /// 上一版用一个裸 bool，方向键/F 键根本不产生 `WM_CHAR`，标志就一直挂着，
    /// 把用户之后敲的第一个真实字符吃掉（代码审查 B4）。
    suppress_char_until: Option<Instant>,
    /// 最近一次输入法上屏写了什么、什么时候、走哪条路（只用于去重，不写日志）。
    last_ime_result: Option<(String, Instant, ImeSource)>,
    /// 输入法上屏时等待配对的 UTF-16 高代理项（非 BMP 字符由两条 `WM_IME_CHAR` 组成）。
    ime_high_surrogate: Option<u16>,
    /// 这个输入法是否用 `WM_IME_COMPOSITION` 通知上屏。
    ///
    /// 底部提示条（粘贴确认、配置告警），到点自动消失。
    overlay: Option<(String, Instant)>,
    /// 自检开始前的前台窗口，结束时还回去（不把用户踢出正在用的窗口）。
    probe_previous_foreground: isize,
    /// 真实输入探针（`--probe-real`）：用 `SendInput` 产生系统级事件。
    real_probe: bool,
    /// 真实探针的守卫状态（光标、前台还原所需的现场）。
    real_guard: Option<probe::RealGuard>,
    /// 真实探针期间收到的真实事件计数：(点击, 按键, 字符)。
    real_events: (u32, u32, u32),
    /// 设置面板：`Some` 表示面板开着，终端不再接收按键。
    settings: Option<Settings>,
    /// 切换终端弹窗：`Some` 表示弹窗开着，按键与鼠标点击都先归它。
    shell_picker: Option<ShellPicker>,
    /// 进程是否处于提权态：为真时边框内侧常驻一圈警示色（S-01）。
    elevated: bool,
    /// 常驻状态行：退出码、致命错误这类「说完就不该消失」的信息。
    /// 与 `overlay` 的区别是它没有 TTL，只有重新起 shell 或成功输入才清掉。
    status: Option<String>,
    /// 提示条到期时刻（`Instant` 是该条消失的时间点）。
    /// 待确认的多行粘贴内容与首次按键时间。
    pending_paste: Option<(String, Instant)>,
    /// 实际使用的配置文件路径（`--config` 可覆盖）。
    config_path: Option<std::path::PathBuf>,
    /// 上次看到的配置文件修改时间，用于热重载判定。
    config_mtime: Option<std::time::SystemTime>,
    /// 上次轮询配置文件的时间。
    last_watch: Instant,
    /// 是否正在拖拽选择文字。
    selecting: bool,
    /// `TaskbarCreated` 的消息号（`RegisterWindowMessageW` 的返回值，0 表示没注册上）。
    taskbar_created: u32,
    /// 可见性看门狗（安全评审 S-01b）。
    watchdog: dock::Watchdog,
    /// `--probe-input`：启动后由本进程注入一次点击与按键，验证输入链路。
    input_probe: bool,
    /// 自检的当前步骤与下一次推进的时刻。
    probe: Option<ProbeState>,
    /// 首帧落盘路径（`--dump-frame`），只为视觉验收取证。
    dump_frame: Option<std::path::PathBuf>,
    /// 落盘合成底色；默认白（最差对比条件）。
    dump_bg: u32,
    dump_done: bool,
    /// 落盘时间点：首帧上屏后延后一点，等 shell 的第一屏输出进网格。
    dump_due: Option<Instant>,
    /// 自检期间写进 PTY 的字节数。
    probe_writes: usize,
    /// 自检收尾后的退出时刻（诊断进程不常驻）。
    quit_due: Option<Instant>,
    /// `--open-settings`：启动即展开设置面板（视觉取证用）。
    open_panel_at_startup: bool,
    /// 标签栏上鼠标悬停的格子。`Blank` 表示没停在任何可点处。
    hover: crate::render::layout::TabHit,
    /// 是否已经向系统登记过「鼠标离开这个窗口要通知我」。
    hover_tracking: bool,
}

/// 真机输入自检的状态机。
#[derive(Debug, Clone, Copy)]
struct ProbeState {
    phase: probe::Phase,
    next_at: Instant,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self::build(config, false)
    }

    /// 自检用：只建窗口，不起 shell。
    pub fn new_headless(config: Config) -> Self {
        Self::build(config, true)
    }

    /// 打开真机输入自检（`--probe-input`）。必须在窗口创建前调用。
    pub fn enable_input_probe(&mut self) {
        self.input_probe = true;
        self.probe_previous_foreground = probe::save_foreground();
    }

    /// 启用真实输入探针（`--probe-real`）。
    ///
    /// 与 `--probe-input` 的区别只有一点：它发的是**系统级**事件，
    /// 因此能验证命中测试与激活，也因此在每一步发送前都要求前台是自己的窗口。
    pub fn enable_real_probe(&mut self) {
        self.input_probe = true;
        self.real_probe = true;
        self.probe_previous_foreground = probe::save_foreground();
    }

    fn build(config: Config, headless: bool) -> Self {
        // 初始化失败前的兜底布局：8×18 单元格、10px 内边距。
        let layout = Layout::compute(8, 18, config.window.padding, 14, 900, 420);
        let mut app = Self {
            hwnd: HWND::default(),
            config,
            quit_hotkey: None,
            raise_hotkey: None,
            hide_hotkey: None,
            startup_warnings: Vec::new(),
            headless,
            tabs: vec![Tab::new("shell 1")],
            active_tab: 0,
            painter: None,
            surface: None,
            surface_size: (0, 0),
            layout,
            blink: Blink::new(),
            // 启动用 `SW_SHOWNOACTIVATE`，首帧确实没有焦点。上一版写死 true，
            // 于是焦点描边和实心光标在「其实没焦点」的状态下撒谎（代码审查 B7）。
            focused: false,
            elevated: false,
            probe_previous_foreground: 0,
            real_probe: false,
            real_guard: None,
            real_events: (0, 0, 0),
            settings: None,
            shell_picker: None,
            dirty: false,
            last_render: Instant::now(),
            suppress_char_until: None,
            ime_high_surrogate: None,
            last_ime_result: None,
            overlay: None,
            status: None,
            pending_paste: None,
            config_path: crate::config::config_path().ok(),
            config_mtime: None,
            last_watch: Instant::now(),
            selecting: false,
            taskbar_created: 0,
            watchdog: dock::Watchdog::new(),
            input_probe: false,
            probe: None,
            dump_frame: None,
            dump_done: false,
            dump_bg: 0x00FF_FFFF,
            dump_due: None,
            probe_writes: 0,
            quit_due: None,
            open_panel_at_startup: false,
            hover: crate::render::layout::TabHit::Blank,
            hover_tracking: false,
        };
        // 光标闪烁同样只在真正有焦点时开（B7 的另一半）。
        app.blink.set_enabled(false);
        app
    }

    // ------------------------------------------------------------ 标签

    /// 当前标签下标，永远落在范围内。
    ///
    /// `tabs` 不为空时 `active_tab` 才可信；用 `min` 做一次兜底，避免关标签之后
    /// 有一个瞬间下标越界就 panic（用户点得比程序快）。
    fn active_index(&self) -> usize {
        self.active_tab.min(self.tabs.len().saturating_sub(1))
    }

    fn tab_terminal(&self) -> Option<&Terminal> {
        self.tabs.get(self.active_index())?.terminal.as_ref()
    }

    fn tab_terminal_mut(&mut self) -> Option<&mut Terminal> {
        let index = self.active_index();
        self.tabs.get_mut(index)?.terminal.as_mut()
    }

    fn tab_events(&self) -> Option<&Receiver<Event>> {
        self.tabs.get(self.active_index())?.events.as_ref()
    }

    fn tab_session(&self) -> Option<&Session> {
        self.tabs.get(self.active_index())?.session.as_ref()
    }

    fn tab_session_mut(&mut self) -> Option<&mut Session> {
        let index = self.active_index();
        self.tabs.get_mut(index)?.session.as_mut()
    }

    /// 标签栏上每个标签的标题（绘制与命中判定共用同一份）。
    fn tab_titles(&self) -> Vec<&str> {
        self.tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>()
    }

    /// 新建一个标签并切过去。会话在 `apply_size` 里按新尺寸拉起。
    fn open_tab(&mut self, hwnd: HWND) {
        // 序号取最小可用值：关掉 shell 2 之后新建，不该又叫 shell 2。
        let mut index = self.tabs.len() + 1;
        while self
            .tabs
            .iter()
            .any(|tab| tab.title == format!("shell {index}"))
        {
            index += 1;
        }
        self.tabs.push(Tab::new(&format!("shell {index}")));
        self.active_tab = self.tabs.len() - 1;
        diag::info(&format!("新建标签: {}", self.tabs[self.active_tab].title));
        // 新标签没有网格，先按当前尺寸建一个，再拉会话。
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("新标签测量失败: {err:#}"));
        }
        if let Err(err) = self.spawn_session(hwnd) {
            diag::warn(&format!("新标签启动 shell 失败: {err:#}"));
        }
        if let Some(report) = Some(dock::focus_state(hwnd)) {
            self.focused = report.focus_set;
            self.blink.set_enabled(report.focus_set);
        }
        self.log_tab_geometry();
        self.dirty = true;
    }

    /// 关掉一个标签。关掉最后一个时保留一个空标签并写明怎么重开。
    fn close_tab(&mut self, index: usize, hwnd: HWND) {
        if index >= self.tabs.len() {
            return;
        }
        let title = self.tabs[index].title.clone();
        let mut removed = self.tabs.remove(index);
        diag::info(&format!("关闭标签: {title}"));
        // `Session::drop` 会 kill 并 join 读线程，而那个 join 没有超时：
        // 直接在 UI 线程里丢，读线程一旦卡在 ReadFile 上，整个窗口就冻死
        // （实测：点 × 之后点击全部无响应，日志也停了）。丢到后台线程去收尾。
        if let Some(session) = removed.session.take() {
            std::thread::spawn(move || drop(session));
        }
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new("shell 1"));
            self.active_tab = 0;
            self.status = Some("没有终端了：Ctrl+Shift+T 新建".to_owned());
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("切换标签后重排失败: {err:#}"));
        }
        // 顶上来的标签可能从没起过会话（关掉最后一个标签时的 shell 1），
        // 不补这一下，打字就没有进程接收。
        if self.tab_session().is_none() {
            if let Err(err) = self.spawn_session(hwnd) {
                diag::warn(&format!("标签关闭后启动 shell 失败: {err:#}"));
            }
        }
        self.log_tab_geometry();
        self.dirty = true;
    }

    /// 切到指定标签。
    fn switch_tab(&mut self, index: usize, hwnd: HWND) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.active_tab = index;
        diag::info(&format!("切到标签: {}", self.tabs[index].title));
        // 新标签可能还没有网格/会话（从没显示过），补建并同步尺寸。
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("切换标签后重排失败: {err:#}"));
        }
        if self.tab_session().is_none() {
            if let Err(err) = self.spawn_session(hwnd) {
                diag::warn(&format!("切换标签后启动 shell 失败: {err:#}"));
            }
        }
        self.log_tab_geometry();
        self.dirty = true;
    }

    /// 字号加一档/减一档：立刻重排网格并写回配置。
    ///
    /// 步长 1px 而不是按比例缩放：`font.size` 本身就是像素值，整数步长让
    /// 「按 5 下变成多少」是可预测的，也不会因为浮点累积漂到 13.999 这种值。
    fn zoom_font(&mut self, step: i32, hwnd: HWND) {
        let size = next_font_size(self.config.font.size, step);
        if (size - self.config.font.size).abs() < f64::EPSILON {
            self.set_overlay_for("字号已经到头了", OVERLAY_TTL);
            return;
        }
        self.config.font.size = size;
        self.apply_font_change(hwnd, format!("字号 {size:.0}px"));
    }

    /// 把字号复位到出厂值。
    fn reset_font_size(&mut self, hwnd: HWND) {
        let default = crate::config::FontConfig::default().size;
        if (default - self.config.font.size).abs() < f64::EPSILON {
            self.set_overlay_for("字号已经是默认值", OVERLAY_TTL);
            return;
        }
        self.config.font.size = default;
        self.apply_font_change(hwnd, format!("字号复位为 {default:.0}px"));
    }

    /// 字号变了的共同收尾：重新加载字体、重排网格、写回配置。
    fn apply_font_change(&mut self, hwnd: HWND, message: String) {
        if let Err(err) = self.reload_fonts(hwnd) {
            diag::error(&format!("换字号时重新加载字体失败: {err:#}"));
            self.set_overlay_for(format!("换字号失败：{err}"), ADMIN_NOTICE_TTL);
            return;
        }
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("换字号后重排失败: {err:#}"));
        }
        if let Some(path) = self.config_path.clone() {
            if let Err(err) = crate::config::save_atomic(&self.config, &path) {
                diag::warn(&format!("字号没有写回配置: {err:#}"));
                self.set_overlay_for(format!("字号已生效，但没写回配置：{err}"), ADMIN_NOTICE_TTL);
                self.dirty = true;
                return;
            }
        }
        diag::info(&message);
        self.set_overlay_for(message, OVERLAY_TTL);
        self.dirty = true;
    }

    /// 清屏：把屏幕和滚动缓冲都清掉，但**保留 shell 的提示符**。
    ///
    /// 用户实测缺陷：原来无条件发 `CSI 3J CSI 2J CSI H`，网格清空后 shell 并不知道
    /// 自己该重画提示符，于是连 `PS C:\\Users\\ZhuanZ>` 一起消失，屏幕全黑像卡死。
    ///
    /// 正确做法是把清屏交给 readline：PowerShell（PSReadLine）与 bash 收到 `Ctrl+L`
    /// 会自己清屏并重画提示符，这才是用户在别的终端里看到的「cls 之后提示符还在」。
    /// `cmd.exe` 不认 `Ctrl+L`，只能由我们清网格（它自己发 `cls` 时同样会重画提示符，
    /// 所以这里清完网格等它的下一条输出即可）。
    /// 当前标签的 shell 类型缩写，画在标签栏按钮上。
    fn shell_label(&self) -> &'static str {
        match self
            .tabs
            .get(self.active_index())
            .and_then(|tab| tab.shell_kind)
        {
            Some(shell::ShellKind::PowerShell7) => "pwsh",
            Some(shell::ShellKind::PowerShell5) => "ps5",
            Some(shell::ShellKind::Cmd) => "cmd",
            Some(shell::ShellKind::Other) => "shell",
            None => "—",
        }
    }

    /// 用当前标签标题与客户区宽度算出整条标签栏的几何。
    fn tab_bar_now(&self) -> crate::render::layout::TabBar {
        let titles = self.tab_titles();
        let client_w = crate::win::window::client_rect(self.hwnd).map_or(0, |rect| rect.w);
        crate::render::layout::tab_bar(self.layout.cell_w, &titles, client_w)
    }

    /// 把标签栏几何写进日志。
    ///
    /// 验收脚本靠这几行拿坐标，不在脚本里重算宽度——两份几何必然漂移，
    /// 而「点不准」的 bug 就藏在第二份里。
    fn log_tab_geometry(&self) {
        let bar = self.tab_bar_now();
        diag::info(&format!(
            "标签栏: 单元格={} 栏高={} 新建中心={} shell中心={} 清屏中心={} 标签数={} 可绘右界={}",
            self.layout.cell_w,
            crate::render::layout::tab_bar_height(self.layout.cell_h),
            bar.new_center,
            bar.shell_center,
            bar.clear_center,
            bar.slots.len(),
            bar.clip_end
        ));
        let titles = self.tab_titles();
        for (index, slot) in bar.slots.iter().enumerate() {
            let title = titles.get(index).copied().unwrap_or("?");
            diag::info(&format!(
                "标签[{index}]: {title} 起={} 止={} 叉={}",
                slot.start, slot.end, slot.close_center
            ));
        }
    }

    /// 鼠标在标签栏上移动时更新悬停格，并登记「离开时通知我」。
    fn track_hover(&mut self, lparam: LPARAM) {
        let (x, y) = client_point(lparam);
        let bar_h = crate::render::layout::tab_bar_height(self.layout.cell_h);
        let hit = if y >= 0 && y < bar_h {
            let bar = self.tab_bar_now();
            crate::render::layout::hit_tab_bar(&bar, self.layout.cell_w, x)
        } else {
            crate::render::layout::TabHit::Blank
        };
        if hit != self.hover {
            self.hover = hit;
            self.dirty = true;
            // 用 info 而不是 debug：验收脚本按 info 级别跑，悬停反馈必须有据可查。
            diag::info(&format!("悬停: {hit:?}"));
        }
        if !self.hover_tracking {
            // SAFETY: 只填一个全零的 TRACKMOUSEEVENT 结构体，交给系统登记通知。
            unsafe {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            self.hover_tracking = true;
        }
    }

    /// 指针下面那一点「点得动吗」：点得动给手型，其余按文本区处理。
    /// 这一行是不是标签栏（导航条，不是终端网格）。
    fn in_tab_bar(&self, y: i32) -> bool {
        let bar_h = crate::render::layout::tab_bar_height(self.layout.cell_h);
        y >= 0 && y < bar_h
    }

    fn over_clickable(&self, x: i32, y: i32) -> bool {
        if self.shell_picker.is_some() {
            // 弹窗占满整块客户区：只有行是可点的，别处不是。
            return self.shell_picker_row_at(y).is_some();
        }
        if self.settings_visible() {
            return false;
        }
        if !self.in_tab_bar(y) {
            return false;
        }
        let bar = self.tab_bar_now();
        !matches!(
            crate::render::layout::hit_tab_bar(&bar, self.layout.cell_w, x),
            crate::render::layout::TabHit::Blank
        )
    }

    /// 按指针位置刷新系统光标。
    ///
    /// 为什么必须自己刷：窗口类只带一个光标，而这块客户区里既有按钮也有终端网格。
    /// 之前类光标固定成 I-beam，于是每个按钮上都显示「文本插入符」——看起来像可以
    /// 框选文字，谁都不敢点。现在按钮给手型，终端区仍旧是 I-beam（那里本来就能选字）。
    fn refresh_cursor(&self, hwnd: HWND) {
        let mut point = POINT::default();
        // SAFETY: point 是栈上可写结构；hwnd 有效，只取指针位置。
        unsafe {
            if GetCursorPos(&mut point).is_err() {
                return;
            }
        }
        // 命中判定走与 WM_NCHITTEST 完全同一条路径（同一套边距），
        // 这样边框、拖动带、按钮不会各算一套边界。
        let packed = (((point.y as isize) & 0xffff) << 16) | ((point.x as isize) & 0xffff);
        let code = self.hit_test(hwnd, LPARAM(packed)).0 as u32;
        // SAFETY: 同上，只做屏幕到客户的坐标换算。
        unsafe {
            let _ = ScreenToClient(hwnd, &mut point);
        }
        let id = match code {
            HTLEFT | HTRIGHT => IDC_SIZEWE,
            HTTOP | HTBOTTOM => IDC_SIZENS,
            HTTOPLEFT | HTBOTTOMRIGHT => IDC_SIZENWSE,
            HTTOPRIGHT | HTBOTTOMLEFT => IDC_SIZENESW,
            HTCAPTION => IDC_SIZEALL,
            HTCLIENT if self.over_clickable(point.x, point.y) => IDC_HAND,
            // 标签栏是导航条：不可点的空白处也不该摆出文本插入符。
            HTCLIENT if self.in_tab_bar(point.y) => IDC_ARROW,
            HTCLIENT => IDC_IBEAM,
            _ => IDC_ARROW,
        };
        // SAFETY: id 是系统预定义光标资源名，只读。
        let handle = unsafe { LoadCursorW(None, id) }.unwrap_or_default();
        // SAFETY: handle 是刚取得的（最坏是空）光标句柄，交给系统只影响外观。
        unsafe {
            SetCursor(Some(handle));
        }
    }

    /// 当前该画哪个弹窗：切换终端优先于设置面板。
    fn modal_panel(&self) -> Option<PanelView> {
        if let Some(picker) = self.shell_picker.as_ref() {
            return Some(PanelView {
                title: "切换终端",
                rows: picker.rows(),
                selected: picker.focus,
                footer: "↑↓ 选择 ←→ 切换 Enter 确认 Esc 取消 · 也可以点行切换",
                dirty: false,
            });
        }
        let panel = self.settings.as_ref().filter(|panel| !panel.is_empty())?;
        Some(PanelView {
            title: "设置",
            rows: panel.rows(),
            selected: panel.selected,
            footer: if panel.dirty {
                "未保存 · ↑↓ 选择 ←→ 调整 Enter 保存 Esc 取消"
            } else {
                "↑↓ 选择 ←→ 调整 Enter 保存 Esc 取消"
            },
            dirty: panel.dirty,
        })
    }

    /// 点标签栏的 shell 按钮：弹出切换终端的选择框。
    ///
    /// 原来的实现是「点一下就悄悄切到下一个类型并重启 shell」：用户既看不见会切到
    /// 哪一类终端，也不知道手上这些记录会不会被清掉。现在一律先弹窗，两个决定
    /// （换哪一类 / 新开还是原地）都由用户当场点明白。
    fn open_shell_picker(&mut self) {
        let allow_custom = self.config.security.allow_custom_shell;
        match ShellPicker::available(&self.config.shell, allow_custom) {
            Some(picker) => {
                let candidates = picker.choices.len();
                let current = self.config.shell.program.clone();
                self.shell_picker = Some(picker);
                self.dirty = true;
                // 行的竖直中心一并打出来：验收脚本按这条日志点行，
                // 脚本里不再自己算一遍几何（那是最容易和产品漂移的地方）。
                let centers = self.picker_row_centers();
                diag::info(&format!(
                    "打开切换终端弹窗: {candidates} 个候选，当前 {current}，行中心={centers:?}，行高={}",
                    self.layout.cell_h
                ));
            }
            None => {
                diag::warn("切换终端: 没有可用候选");
                self.set_overlay_for("没有可切换的 shell，检查 shell.program 配置", OVERLAY_TTL);
            }
        }
    }

    /// 弹窗每一行的竖直中心（像素），供日志与验收脚本点击使用。
    fn picker_row_centers(&self) -> Vec<i32> {
        let rows = self
            .shell_picker
            .as_ref()
            .map_or(0, |picker| picker.rows().len());
        (0..rows)
            .map(|index| {
                let (_, top) = self.layout.cell_origin(0, index + 2);
                top + self.layout.cell_h / 2
            })
            .collect()
    }

    fn close_shell_picker(&mut self) {
        if self.shell_picker.take().is_some() {
            diag::info("关闭切换终端弹窗（未确认）");
            self.dirty = true;
        }
    }

    /// 弹窗开着时接管按键：Esc 取消、Enter 确认、↑↓ 换行、←→ 改值。
    fn handle_shell_picker_key(&mut self, vk: u32) {
        const VK_ESCAPE: u32 = 0x1b;
        const VK_RETURN: u32 = 0x0d;
        const VK_LEFT: u32 = 0x25;
        const VK_UP: u32 = 0x26;
        const VK_RIGHT: u32 = 0x27;
        const VK_DOWN: u32 = 0x28;
        match vk {
            VK_ESCAPE => self.close_shell_picker(),
            VK_RETURN => {
                let hwnd = self.hwnd;
                self.confirm_shell_picker(hwnd);
            }
            VK_UP | VK_DOWN => {
                if let Some(picker) = self.shell_picker.as_mut() {
                    picker.move_focus(if vk == VK_DOWN { 1 } else { -1 });
                }
                self.dirty = true;
            }
            VK_LEFT | VK_RIGHT => {
                if let Some(picker) = self.shell_picker.as_mut() {
                    picker.adjust(vk == VK_RIGHT);
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// 弹窗里的鼠标点击：点哪一行，就把那一行的值推到下一个。
    fn handle_shell_picker_click(&mut self, y: i32) {
        let Some(row) = self.shell_picker_row_at(y) else {
            // 点在空白处什么也不做：误触不该改掉用户已经选好的东西。
            return;
        };
        let Some(picker) = self.shell_picker.as_mut() else {
            return;
        };
        picker.click_row(row);
        let choice = picker.selected().map_or("—", |choice| choice.program);
        let mode = if picker.new_session {
            "新开标签"
        } else {
            "原地切换"
        };
        self.dirty = true;
        diag::info(&format!(
            "切换终端弹窗: 点第 {} 行 → 终端={choice}，方式={mode}",
            row + 1
        ));
    }

    /// 弹窗第 `y` 像素落在哪一行（与 `draw_panel` 的行序一致：标题占第 0 行，行从第 2 行起）。
    fn shell_picker_row_at(&self, y: i32) -> Option<usize> {
        let rows = self
            .shell_picker
            .as_ref()
            .map(|picker| picker.rows().len())?;
        (0..rows).find(|index| {
            let (_, top) = self.layout.cell_origin(0, index + 2);
            y >= top && y < top + self.layout.cell_h
        })
    }

    /// 确认弹窗：写配置，然后按用户选的「新开标签 / 原地切换」执行。
    fn confirm_shell_picker(&mut self, hwnd: HWND) {
        let Some(picker) = self.shell_picker.take() else {
            return;
        };
        let Some(choice) = picker.selected() else {
            return;
        };
        self.dirty = true;
        let program_changed = !self
            .config
            .shell
            .program
            .eq_ignore_ascii_case(choice.program);
        if program_changed {
            self.config.shell.program = choice.program.to_owned();
            self.save_config(&format!("shell 已切换为 {}，配置已保存", choice.program));
        }
        if picker.new_session {
            self.open_tab(hwnd);
            diag::info(&format!("切换终端: 新开标签 {}", choice.program));
            self.set_overlay_for(format!("已新开标签：{}", choice.label), OVERLAY_TTL);
        } else if !program_changed && self.tab_session().is_some() {
            // 选回来的就是当前这一类，而且会话还活着：重启只会白丢进程状态。
            diag::info(&format!("原地切换: 当前已经是 {}，未重启", choice.program));
            self.set_overlay_for(format!("当前已经是 {}", choice.label), OVERLAY_TTL);
        } else {
            self.switch_shell_in_place(hwnd, choice);
        }
    }

    /// 原地切换：只换 PTY 会话，**保留**当前屏幕与回滚缓冲。
    ///
    /// 用户的原话是「不新开会话切换终端之后之前的记录不能清空」，所以这里绝对不能走
    /// `terminal = None` 那条路——那会把滚动历史和当前屏幕一起丢掉，等于顺手清屏。
    fn switch_shell_in_place(&mut self, hwnd: HWND, choice: ShellChoice) {
        let index = self.active_index();
        // 切换前的回滚深度：切换后要把它原样报出来，这是「记录没被清掉」的证据。
        let history = self.tab_terminal().map_or(0, Terminal::history_size);
        if let Some(tab) = self.tabs.get_mut(index) {
            if let Some(session) = tab.session.take() {
                // 会话回收可能要等 ConPTY 读线程退出，不能卡在 UI 线程上。
                std::thread::spawn(move || drop(session));
            }
            tab.shell_kind = None;
        }
        self.clear_status();
        // 一条分界线：让用户分得清「以上是切换前的输出」。
        self.show_terminal_error(&format!(
            "[TLShell] 已切换为 {}（上面这些记录没有清空）",
            choice.label
        ));
        if let Err(err) = self.spawn_session(hwnd) {
            diag::error(&format!("原地切换终端失败: {err:#}"));
        }
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("切换终端后重排失败: {err:#}"));
        }
        let kept = self.tab_terminal().map_or(0, Terminal::history_size);
        diag::info(&format!(
            "原地切换: {}，屏幕与回滚历史保留（历史={} 行，切换前={history} 行）",
            choice.program, kept
        ));
        self.set_overlay_for(format!("已原地切换到 {}", choice.label), OVERLAY_TTL);
    }

    /// 把当前内存里的配置写盘；失败必须在提示条上说出来，不能静默。
    fn save_config(&mut self, note: &str) {
        let Some(path) = self.config_path.clone() else {
            diag::warn("配置目录不可定位，改动只在本次运行有效");
            self.set_overlay_for("配置目录不可用，切换只对本次运行有效", OVERLAY_TTL);
            return;
        };
        match crate::config::save_atomic(&self.config, &path) {
            Ok(()) => diag::info(note),
            Err(err) => {
                diag::error(&format!("写配置失败: {err:#}"));
                self.set_overlay_for(format!("切换没有写进配置：{err}"), OVERLAY_TTL);
            }
        }
    }

    fn clear_screen(&mut self) {
        let kind = self
            .tabs
            .get(self.active_index())
            .and_then(|tab| tab.shell_kind);
        let readline_clears = !matches!(kind, Some(shell::ShellKind::Cmd));
        if readline_clears {
            // Ctrl+L：由 shell 自己清屏并重画提示符。
            self.write_to_pty(&[0x0c]);
            diag::info("清屏: 已向 shell 发送 Ctrl+L（由 shell 重画提示符）");
        } else {
            if let Some(term) = self.tab_terminal_mut() {
                term.feed(b"\x1b[3J\x1b[2J\x1b[H");
                term.scroll_to_bottom();
            }
            diag::info("清屏: cmd 不认 Ctrl+L，改为本地清网格");
        }
        self.dirty = true;
    }

    #[allow(dead_code)] // M5 单实例与重挂使用
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 自检路径里窗口先于 `App` 的字段就绪，需要补写一次。
    pub fn set_hwnd(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    /// 注册全局热键。失败只告警：`--quit` 永远是可用的退路。
    fn register_hotkeys(&mut self, hwnd: HWND) {
        match hotkey::parse(&self.config.keys.quit) {
            Ok(spec) => match Hotkey::register(hwnd, HOTKEY_ID_QUIT, &spec) {
                Ok(handle) => self.quit_hotkey = Some(handle),
                Err(err) => {
                    let message = format!("退出热键注册失败，请使用 `TLShell.exe --quit`: {err:#}");
                    diag::warn(&message);
                    self.startup_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("退出热键配置非法: {err:#}");
                diag::warn(&message);
                self.startup_warnings.push(message);
            }
        }

        match hotkey::parse(&self.config.keys.raise) {
            Ok(spec) => match Hotkey::register(hwnd, HOTKEY_ID_RAISE, &spec) {
                Ok(handle) => self.raise_hotkey = Some(handle),
                Err(err) => {
                    let message =
                        format!("唤起热键注册失败（{}）: {err:#}", self.config.keys.raise);
                    diag::warn(&message);
                    self.startup_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("唤起热键解析失败（{}）: {err:#}", self.config.keys.raise);
                diag::warn(&message);
                self.startup_warnings.push(message);
            }
        }
        match hotkey::parse(&self.config.keys.hide) {
            Ok(spec) => match Hotkey::register(hwnd, HOTKEY_ID_HIDE, &spec) {
                Ok(handle) => self.hide_hotkey = Some(handle),
                Err(err) => {
                    let message = format!("隐藏热键注册失败: {err:#}");
                    diag::warn(&message);
                    self.startup_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("隐藏热键配置非法: {err:#}");
                diag::warn(&message);
                self.startup_warnings.push(message);
            }
        }
    }

    // ---------------------------------------------------------------- 初始化

    /// `WM_CREATE` 里做一次性的重量级初始化。
    /// 注册 `TaskbarCreated`，Explorer 重启后能收到通知（见 `reattach`）。
    fn register_taskbar_message(&mut self) {
        // SAFETY: 参数是静态宽字符串。
        let id = unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) };
        self.taskbar_created = id;
        if id == 0 {
            diag::warn("注册 TaskbarCreated 失败，Explorer 重启后无法自动重挂");
        }
    }

    fn initialize(&mut self, hwnd: HWND) -> Result<()> {
        if self.headless {
            return Ok(());
        }
        self.register_taskbar_message();
        self.ensure_surface(hwnd)?;
        self.reload_fonts(hwnd)?;
        self.apply_size(hwnd)?;
        self.report_startup_warnings();
        self.spawn_session(hwnd)?;
        // SAFETY: hwnd 有效；定时器在 WM_DESTROY 里注销。
        unsafe {
            SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);
        }
        if self.open_panel_at_startup {
            // 视觉取证：面板本身也要落一张图，所以启动就展开它。
            self.open_settings();
            self.dirty = true;
        }
        if self.input_probe {
            // 等 shell 起完、首帧画完再注入，否则点击落在还没准备好的窗口上。
            // 真实探针从「抬到最前」开始：不先拿到前台，后续事件会打进别人的窗口。
            let phase = if self.real_probe {
                probe::Phase::RealRaise
            } else {
                probe::Phase::Click
            };
            self.probe = Some(ProbeState {
                phase,
                next_at: Instant::now() + Duration::from_millis(800),
            });
        }
        Ok(())
    }

    fn ensure_surface(&mut self, _hwnd: HWND) -> Result<()> {
        if self.surface.is_some() {
            return Ok(());
        }
        // 逐像素 alpha 的绘制面（V-19）：一块 32 位 DIB，画完由 `UpdateLayeredWindow` 推送。
        let (width, height) = self.surface_size;
        let surface = LayeredSurface::new(width.max(1) as usize, height.max(1) as usize)
            .map_err(|err| anyhow::anyhow!("创建 DIB 绘制面失败: {err:#}"))?;
        self.surface = Some(surface);
        Ok(())
    }

    fn reload_fonts(&mut self, hwnd: HWND) -> Result<()> {
        let window_dpi = dpi::window_dpi(hwnd);
        let fonts = FontSet::load(&self.config.font, window_dpi)?;
        diag::info(&format!(
            "字体就绪: {} / {:?} @ {}px, 单元格 {}x{}",
            fonts.primary_family, fonts.fallback_family, fonts.ascii_px, fonts.cell_w, fonts.cell_h
        ));
        let theme = Theme::from_config(&self.config.colors);
        self.painter = Some(Painter::new(fonts, theme));
        Ok(())
    }

    /// 推进输入自检（`--probe-input`）：只往自己的窗口投递消息。
    ///
    /// 每步只做一件事就返回，等待交给下一次 `WM_TIMER`，这样消息循环一直在转，
    /// 刚投递的消息才能被自己处理完。
    fn advance_probe(&mut self) {
        let Some(state) = self.probe else {
            return;
        };
        if Instant::now() < state.next_at {
            return;
        }
        match state.phase {
            probe::Phase::Click => {
                let posted = probe::post_click(self.hwnd);
                probe::note(
                    probe::Phase::Click,
                    &format!("已投递点击={posted} 命中={}", dock::hit_report(self.hwnd)),
                );
                self.schedule_probe(probe::Phase::Drag, 200);
            }
            probe::Phase::Drag => {
                let posted = probe::post_drag(self.hwnd);
                let selection = self
                    .tab_terminal()
                    .and_then(|term| term.selection_range())
                    .map(|(start, end)| {
                        format!(
                            "{}:{}..{}:{}",
                            start.line.0, start.column.0, end.line.0, end.column.0
                        )
                    })
                    .unwrap_or_else(|| "无".to_owned());
                probe::note(
                    probe::Phase::Drag,
                    &format!("已投递拖选={posted} 选区={selection}"),
                );
                self.schedule_probe(probe::Phase::Type, 300);
            }
            probe::Phase::Type => {
                let posted = probe::post_text(self.hwnd, "a中😀");
                probe::note(
                    probe::Phase::Type,
                    &format!("已投递 3 个字符（a / 中 / 😀），成功 {posted} 个"),
                );
                self.schedule_probe(probe::Phase::Report, 600);
            }
            probe::Phase::RealRaise => match probe::begin_real(self.hwnd) {
                Some(guard) => {
                    self.real_guard = Some(guard);
                    self.schedule_probe(probe::Phase::RealClick, 300);
                }
                None => {
                    // 守卫失败就直接收工：一个事件都不发，避免打进别人的窗口。
                    probe::note(
                        probe::Phase::RealReport,
                        "真实探针中止：前台没拿到（未发送任何输入事件）",
                    );
                    self.real_probe = false;
                    self.probe = None;
                    // 必须收尾退出：诊断进程若在这里停住，调用方 `-Wait` 会永远挂。
                    self.finish_probe_run();
                }
            },
            probe::Phase::RealClick => {
                let sent = self
                    .real_guard
                    .as_ref()
                    .is_some_and(|guard| probe::real_click(self.hwnd, guard));
                diag::info(&format!("真实点击已发送={sent}"));
                self.schedule_probe(probe::Phase::RealDrag, 400);
            }
            probe::Phase::RealDrag => {
                let sent = self
                    .real_guard
                    .as_ref()
                    .is_some_and(|guard| probe::real_drag(self.hwnd, guard));
                diag::info(&format!("真实拖选已发送={sent}"));
                self.schedule_probe(probe::Phase::RealType, 400);
            }
            probe::Phase::RealType => {
                let units = self
                    .real_guard
                    .as_ref()
                    .map(|guard| probe::real_type(self.hwnd, guard))
                    .unwrap_or(0);
                diag::info(&format!("真实按键已发送 unicode 事件={units}"));
                self.schedule_probe(probe::Phase::RealSink, 500);
            }
            probe::Phase::RealSink => {
                if let Some(guard) = self.real_guard.take() {
                    probe::end_real(self.hwnd, guard, self.sinks());
                }
                self.schedule_probe(probe::Phase::RealReport, 400);
            }
            probe::Phase::RealReport => {
                let (clicks, keydowns, _chars) = self.real_events;
                let written = self.probe_writes;
                // 不报「字符数」：真实键盘走 `text_for`/`VK_PACKET` 翻译后直接写 PTY，
                // 不会产生 `WM_CHAR`，把那个计数印出来只会让人以为打字失败了。
                // 判据用「按键 → 写入 PTY 字节」，两端都是实测值。
                probe::note(
                    probe::Phase::RealReport,
                    &format!("真实输入结算: 点击={clicks} 按键={keydowns} 写入 PTY={written} 字节"),
                );
                // 输入法状态是「用户本机装了什么输入法」的事实，自动化替代不了。
                // 印出来之后，用户切到搜狗/微软拼音各跑一次就能确认判据读到的值。
                let key = |value: Option<u32>| match value {
                    Some(mode) => format!("0x{mode:04x}"),
                    None => "不可读".to_owned(),
                };
                probe::note(
                    probe::Phase::RealReport,
                    &format!(
                        "输入法状态: 转换模式={} 原生输入={}",
                        key(crate::win::ime::conversion_mode(self.hwnd)),
                        crate::win::ime::wants_native_input(self.hwnd)
                    ),
                );
                self.real_probe = false;
                self.probe_previous_foreground = 0;
                self.probe = None;
                // 输入法去重的端到端自检：同一段文本先后从两条消息路径进来，
                // 第二次必须被吞掉。这条走的是真实 `commit_ime_text`，
                // 不是另写一份判据；真实输入法上屏无法自动化，这一条是能做到的最强验证。
                let before = self.probe_writes;
                self.commit_ime_text("好", ImeSource::Composition);
                let first = self.probe_writes - before;
                let before = self.probe_writes;
                self.commit_ime_text("好", ImeSource::Char);
                let second = self.probe_writes - before;
                diag::info(&format!(
                    "IME 去重自检: 第一次写入={first} 字节 第二次写入={second} 字节 去重={}",
                    second == 0
                ));
                self.finish_probe_run();
            }
            probe::Phase::Report => {
                let state = dock::focus_state(self.hwnd);
                probe::note(
                    probe::Phase::Report,
                    &format!(
                        "focused={} 已写入 PTY {} 字节 状态={}",
                        self.focused,
                        self.probe_writes,
                        state.describe()
                    ),
                );
                probe::restore_foreground(self.probe_previous_foreground);
                self.probe = None;
                self.finish_probe_run();
            }
        }
    }

    /// 自检跑完就退出。
    ///
    /// 诊断进程没有常驻的理由：留着只会让执行器等超时，还会占住前台还原的时机。
    /// 延后一拍再退出，让最后一帧（含 `--dump-frame` 的落盘）先画完。
    fn finish_probe_run(&mut self) {
        self.probe = None;
        self.dirty = true;
        self.quit_due = Some(Instant::now() + Duration::from_millis(600));
    }

    fn schedule_probe(&mut self, phase: probe::Phase, delay_ms: u64) {
        self.probe = Some(ProbeState {
            phase,
            next_at: Instant::now() + Duration::from_millis(delay_ms),
        });
    }

    fn spawn_session(&mut self, hwnd: HWND) -> Result<()> {
        if self.tab_session().is_some() {
            return Ok(());
        }
        let resolved =
            match shell::resolve(&self.config.shell, self.config.security.allow_custom_shell) {
                Ok(resolved) => resolved,
                Err(err) => {
                    diag::error(&format!("shell 解析失败: {err:#}"));
                    self.show_terminal_error(&format!("启动 shell 失败: {err}"));
                    return Ok(());
                }
            };
        // 日志里不再落盘完整参数（安全评审 F-02）：参数里可能带令牌或路径。
        let args_bytes: usize = resolved.args.iter().map(|arg| arg.len()).sum();
        diag::info(&format!(
            "启动 shell: {}（{} 个参数，共 {args_bytes} 字节，内容不记录）",
            resolved.program.display(),
            resolved.args.len()
        ));

        // 启动目录与环境变量都要先过闸门（安全评审 F-05 / S-03）：
        // 不再继承进程自身的 cwd 与整份环境。
        let cwd = match shell::resolve_cwd(std::env::var_os("USERPROFILE")) {
            Ok(cwd) => cwd,
            Err(err) => {
                diag::error(&format!("启动目录不可用: {err:#}"));
                self.show_terminal_error(&format!("拒绝启动 shell: {err}"));
                return Ok(());
            }
        };
        let env = match shell::child_env() {
            Ok(env) => env,
            Err(err) => {
                diag::error(&format!("子进程环境构造失败: {err:#}"));
                self.show_terminal_error(&format!("拒绝启动 shell: {err}"));
                return Ok(());
            }
        };
        diag::info(&format!(
            "子进程环境: {} 项，启动目录 {}",
            env.len(),
            cwd.display()
        ));
        if let Some(from) = resolved.fallback_from.clone() {
            let message = format!("`{from}` 不可用，已改用 {}", resolved.program.display());
            self.startup_warnings.push(message);
            self.set_overlay_for(
                format!("`{from}` 不可用，已改用其他 PowerShell"),
                ADMIN_NOTICE_TTL,
            );
        }

        // `resolved` 会整个搬进 `SpawnOptions`，先把它属于哪种 shell 记下来。
        let shell_kind = resolved.kind;
        let options = SpawnOptions {
            shell: resolved,
            cols: self.layout.cols as u16,
            rows: self.layout.rows as u16,
            pixel_width: (self.layout.cols * self.layout.cell_w as usize).min(u16::MAX as usize)
                as u16,
            pixel_height: (self.layout.rows * self.layout.cell_h as usize).min(u16::MAX as usize)
                as u16,
            cwd,
            env,
        };

        match Session::spawn(options) {
            Ok(session) => {
                diag::info(&format!("shell pid={}", session.pid()));
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.session = Some(session);
                    tab.shell_kind = Some(shell_kind);
                }
            }
            Err(err) => {
                diag::error(&format!("PTY 启动失败: {err:#}"));
                self.show_terminal_error(&format!("启动 shell 失败: {err}"));
            }
        }
        let _ = hwnd;
        Ok(())
    }

    /// 把错误写进终端网格本身——比弹窗温和，也不会挡住桌面。
    fn show_terminal_error(&mut self, message: &str) {
        let Some(term) = self.tab_terminal_mut() else {
            // 终端还没建好（启动早期失败）时不能静默丢弃：退化成常驻状态行，
            // 否则用户只看到一个空白窗口，不知道发生了什么（V-32）。
            self.set_status(message.to_owned());
            return;
        };
        term.feed(message.replace('\n', "\r\n").as_bytes());
        term.feed(b"\r\n");
        self.dirty = true;
    }

    /// 重新计算布局，并把尺寸同步给绘制面、终端与 PTY。
    fn apply_size(&mut self, hwnd: HWND) -> Result<()> {
        let rect = window::client_rect(hwnd).context("读取客户区尺寸失败")?;
        let (width, height) = (rect.w, rect.h);
        if width <= 0 || height <= 0 {
            return Ok(());
        }

        if width > 0 && height > 0 && (width as u32, height as u32) != self.surface_size {
            self.surface_size = (width as u32, height as u32);
            // 尺寸变了就换一块 DIB：搬家比原地重建更简单，也没有中间态。
            match LayeredSurface::new(width as usize, height as usize) {
                Ok(surface) => self.surface = Some(surface),
                Err(err) => diag::warn(&format!("重建 DIB 绘制面失败: {err:#}")),
            }
        }

        let Some((cell_w, cell_h, ascent)) = self
            .painter
            .as_ref()
            .map(|p| (p.fonts.cell_w, p.fonts.cell_h, p.fonts.ascent))
        else {
            return Ok(());
        };

        let window_dpi = dpi::window_dpi(hwnd);
        let padding = dpi::scale(self.config.window.padding, dpi::BASE_DPI, window_dpi);
        // 顶部要留出标签栏；它不属于网格，但「第 0 行在哪」必须一次算清。
        let bar_h = crate::render::layout::tab_bar_height(cell_h);
        let layout =
            Layout::compute_with_inset(cell_w, cell_h, padding, ascent, width, height, bar_h);
        self.layout = layout;

        let size = TermSize::new(layout.cols, layout.rows);
        match self.tab_terminal_mut() {
            Some(term) => term.resize(size),
            None => {
                let (term, rx) = Terminal::new(size, 10_000, osc52_mode(&self.config));
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.terminal = Some(term);
                    tab.events = Some(rx);
                }
            }
        }

        if let Some(session) = self.tab_session() {
            let pixel_width = (layout.cols * cell_w as usize).min(u16::MAX as usize) as u16;
            let pixel_height = (layout.rows * cell_h as usize).min(u16::MAX as usize) as u16;
            if let Err(err) = session.resize(
                layout.cols as u16,
                layout.rows as u16,
                pixel_width,
                pixel_height,
            ) {
                diag::warn(&format!("PTY resize 失败: {err:#}"));
            }
        }

        self.dirty = true;
        Ok(())
    }

    // -------------------------------------------------------------- 配置热重载

    /// 每秒轮询一次 `config.toml` 的修改时间，变了就重载。
    ///
    /// 用轮询而不是 `ReadDirectoryChangesW`：后者要额外线程和缓冲协议，
    /// 而配置文件一年改不了几次，1 秒的延迟完全够用。
    fn watch_config(&mut self) {
        const WATCH_INTERVAL: Duration = Duration::from_secs(1);

        let Some(config_path) = self.config_path.clone() else {
            // 配置目录不可定位时没有可监视的文件；启动告警里已经说明过。
            return;
        };

        let now = Instant::now();
        if now.duration_since(self.last_watch) < WATCH_INTERVAL {
            return;
        }
        self.last_watch = now;

        let mtime = std::fs::metadata(&config_path)
            .and_then(|meta| meta.modified())
            .ok();

        // 第一次只记录基线，不算“变更”。
        if self.config_mtime.is_none() {
            self.config_mtime = mtime;
            return;
        }
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;
        if mtime.is_none() {
            // 文件被删掉了，保留当前配置，等它重新出现。
            diag::warn("配置文件不见了，暂时沿用当前配置");
            return;
        }
        self.reload_config();
    }

    /// 重新读取配置并应用到运行中的窗口。
    fn reload_config(&mut self) {
        let Some(config_path) = self.config_path.clone() else {
            return;
        };
        let loaded = crate::config::load(Some(&config_path));
        for warning in &loaded.warnings {
            diag::warn(warning);
        }
        if loaded.config == self.config {
            diag::debug("配置文件有改动但内容等价，跳过");
            return;
        }

        let font_changed = loaded.config.font != self.config.font;
        let mut next = loaded.config;
        // 安全策略在启动时冻结：热重载不得在运行中放松 `paste_guard` / `osc52` /
        // `allow_custom_shell`（安全评审 F-08）。否则「配置可写」就等于「策略可改」，
        // 计划书里那些「默认关闭」的结论会被一次热重载推翻。
        if next.security != self.config.security {
            diag::warn("security.* 改动已忽略：安全策略只在启动时读取，重启后生效");
            next.security = self.config.security.clone();
        }
        self.config = next;

        if let Some(painter) = self.painter.as_mut() {
            painter.theme = Theme::from_config(&self.config.colors);
        }
        let hwnd = self.hwnd;
        if font_changed {
            if let Err(err) = self.reload_fonts(hwnd) {
                diag::warn(&format!("热重载字体失败: {err:#}"));
            }
        }
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("热重载重排失败: {err:#}"));
        }
        self.dirty = true;
        diag::info("配置已热重载");
    }

    // ------------------------------------------------------ 鼠标：选择与滚动

    /// 把客户区坐标换算成网格坐标（行号已扣掉历史回滚偏移）。
    fn cell_at(&self, x: i32, y: i32) -> Option<Point> {
        let (column, row) = self.layout.hit(x, y)?;
        let offset = self.tab_terminal().map_or(0, |term| term.display_offset());
        let line = row as i32 - offset as i32;
        Some(Point::new(Line(line), Column(column)))
    }

    /// 按下左键：起一个选区并捕获鼠标。
    fn begin_selection(&mut self, hwnd: HWND, lparam: LPARAM) {
        let (x, y) = client_point(lparam);
        let Some(point) = self.cell_at(x, y) else {
            diag::debug(&format!("({x},{y}) 落在网格外，忽略"));
            return;
        };
        diag::debug(&format!(
            "开始选择 @ ({x},{y}) -> line={} col={}",
            point.line.0, point.column.0
        ));
        if let Some(term) = self.tab_terminal_mut() {
            term.start_selection(point);
        }
        self.selecting = true;
        // SAFETY: 捕获鼠标，拖到窗口外也能继续收到 mousemove；由 end_selection 释放。
        unsafe {
            SetCapture(hwnd);
        }
        self.dirty = true;
    }

    fn drag_selection(&mut self, lparam: LPARAM) {
        if !self.selecting {
            return;
        }
        let (x, y) = client_point(lparam);
        let Some(point) = self.cell_at(x, y) else {
            diag::debug(&format!("({x},{y}) 落在网格外，忽略"));
            return;
        };
        if let Some(term) = self.tab_terminal_mut() {
            term.update_selection(point);
            diag::debug(&format!(
                "扩展选择 -> line={} col={} 选区={:?}",
                point.line.0,
                point.column.0,
                term.selection_range()
            ));
        }
        self.dirty = true;
    }

    fn end_selection(&mut self) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        // SAFETY: 与 SetCapture 配对；没捕获成功时释放是无害的。
        unsafe {
            let _ = ReleaseCapture();
        }
    }

    /// 把当前选区写进剪贴板，然后清掉选区。
    fn copy_selection(&mut self) {
        let Some(text) = self.tab_terminal().and_then(|term| term.selection_text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        match clipboard::set_text(&text) {
            Ok(()) => diag::info(&format!("已复制 {} 个字符", text.chars().count())),
            Err(err) => diag::warn(&format!("复制到剪贴板失败: {err:#}")),
        }
        if let Some(term) = self.tab_terminal_mut() {
            term.clear_selection();
        }
        self.dirty = true;
    }

    /// 滚轮回看历史。`lines` 正数向上（看历史）。
    fn scroll_view(&mut self, lines: i32) {
        if lines == 0 {
            return;
        }
        let mut state = None;
        if let Some(term) = self.tab_terminal_mut() {
            term.scroll(lines);
            state = Some((term.display_offset(), term.history_size()));
        }
        if let Some((offset, history)) = state {
            // 回滚是「界面不再跟随输出」的状态，必须有据可查（也方便验收脚本断言）。
            diag::info(&format!("回滚: 偏移={offset} 历史={history}"));
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ 输入法

    /// 把输入法的候选窗口挪到光标处，否则它会固定出现在窗口左上角。
    fn position_ime(&self, hwnd: HWND) {
        let (Some(term), Some(painter)) = (self.tab_terminal(), self.painter.as_ref()) else {
            return;
        };
        let point = term.cursor_point();
        let row = point.line.0 + term.display_offset() as i32;
        let (x, y) = self.layout.cell_origin(point.column.0, row.max(0) as usize);
        // SAFETY: hwnd 有效；himc 用完立刻释放，不跨消息持有。
        unsafe {
            let himc = ImmGetContext(hwnd);
            if himc.is_invalid() {
                return;
            }
            let form = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT {
                    x,
                    y: y + painter.fonts.ascent,
                },
                rcArea: RECT::default(),
            };
            let _ = ImmSetCompositionWindow(himc, &form);
            let _ = ImmReleaseContext(hwnd, himc);
        }
    }

    /// 直接把一个字符送进 PTY。
    fn send_char(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        self.write_to_pty(encoded.as_bytes());
    }

    /// 输入法上屏的单位是 UTF-16 码元，非 BMP 字符（emoji 等）由高低代理项两发送达。
    ///
    /// 上一版直接把每个码元当 `char` 转换，代理项转换必然失败，于是整字丢失
    /// （代码审查 B5）。这里做一对缓冲。
    /// 输入法上屏的唯一出口：两条消息路径都从这里写，顺便去重。
    ///
    /// 微软拼音上屏时 `WM_IME_COMPOSITION`(`GCS_RESULTSTR`) 与 `WM_IME_CHAR`
    /// 会各来一条、内容相同，两条都写就是「打一遍、回车又一遍」。
    /// 只看「见过 COMPOSITION 就永远忽略 IME_CHAR」把正确性押在先后顺序上，
    /// 而顺序并没有保证：先到的那条已经写进 PTY 了。这里改为比对内容。
    fn commit_ime_text(&mut self, text: &str, source: ImeSource) {
        let now = Instant::now();
        if ime_is_duplicate(self.last_ime_result.as_ref(), text, source, now) {
            diag::debug("输入法上屏重复（同内容、另一条消息），已丢弃");
            return;
        }
        if self.real_probe {
            self.real_events.2 += 1;
        }
        self.write_to_pty(text.as_bytes());
        // 只记字节数，不记内容：日志里出现用户输入等于内置键盘记录器（F-01）。
        diag::debug(&format!("输入法上屏 {} 字节（内容不记录）", text.len()));
        self.last_ime_result = Some((text.to_owned(), now, source));
        self.dirty = true;
    }

    fn commit_ime_unit(&mut self, unit: u16) {
        const HIGH_START: u16 = 0xD800;
        const HIGH_END: u16 = 0xDBFF;
        const LOW_START: u16 = 0xDC00;
        const LOW_END: u16 = 0xDFFF;

        if (HIGH_START..=HIGH_END).contains(&unit) {
            // 高代理项先存着，等低代理项配对。
            self.ime_high_surrogate = Some(unit);
            return;
        }
        if (LOW_START..=LOW_END).contains(&unit) {
            match self.ime_high_surrogate.take() {
                Some(high) => {
                    let code = 0x1_0000
                        + (((high as u32) - HIGH_START as u32) << 10)
                        + ((unit as u32) - LOW_START as u32);
                    if let Some(ch) = char::from_u32(code) {
                        self.commit_ime_text(&ch.to_string(), ImeSource::Char);
                    }
                }
                None => diag::warn("输入法上屏出现孤立低代理项，已丢弃"),
            }
            return;
        }
        // 普通码元：清掉可能残留的半对代理项，避免串到下一个字符。
        self.ime_high_surrogate = None;
        if let Some(ch) = char::from_u32(unit as u32) {
            self.commit_ime_text(&ch.to_string(), ImeSource::Char);
        }
    }

    // -------------------------------------------------- Explorer 重启后重申

    /// `Explorer` 重启后只需要重申 z-order。
    ///
    /// 顶层方案下不再有「父窗口被销毁」这回事，因此不做任何重挂：
    /// 只把窗口压回最底，别的什么都不动。
    fn reassert_bottom(&mut self, hwnd: HWND) {
        diag::info("收到 TaskbarCreated，重申桌面层 z-order（不重挂）");
        if !dock::is_top_level(hwnd) {
            // 防御：真出现父窗口说明有人改过样式，明确写日志而不是装作没事。
            diag::warn("窗口竟然有父窗口，桌面层前置条件被破坏");
        }
        dock::send_to_bottom(hwnd);
        if !dock::is_visible(hwnd) {
            // SAFETY: hwnd 有效；恢复显示但不抢焦点。
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.dirty = true;
    }

    // ---------------------------------------------------------------- 数据泵

    /// 取走 PTY 输出并喂给 VT 状态机，然后处理终端事件。
    fn pump_pty(&mut self) {
        let chunks = match self.tab_session() {
            Some(session) => session.drain(),
            None => return,
        };
        if !chunks.is_empty() {
            if let Some(term) = self.tab_terminal_mut() {
                for chunk in chunks {
                    term.feed(&chunk);
                }
            }
            self.dirty = true;
        }

        let mut events = Vec::new();
        if let Some(rx) = self.tab_events() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            self.handle_term_event(event);
        }
    }

    fn handle_term_event(&mut self, event: Event) {
        match event {
            // 这条事件里装着 DSR 应答（`ESC[6n` → `ESC[行;列R`）。
            // PowerShell 在 ConPTY 下会阻塞等它，丢掉就等于永久卡死。
            Event::PtyWrite(text) => {
                self.write_to_pty(text.as_bytes());
            }
            Event::Wakeup => {
                self.dirty = true;
            }
            Event::Exit | Event::ChildExit(_) => {
                // 直接 `PostQuitMessage` 会让窗口瞬间消失，用户看不到退出码，
                // 也看不到最后那几行输出（视觉验收 V-30 / V-32）。
                // 现在改成：留下窗口、把退出码写成常驻状态行、等用户 `Ctrl+Shift+R`。
                let code = self.tab_session_mut().and_then(Session::try_exit_code);
                diag::info(&format!("shell 已退出，退出码 {code:?}"));
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.session = None;
                }
                self.set_status(match code {
                    Some(code) => format!(
                        "shell 已退出：{} — Ctrl+Shift+R 重新打开",
                        describe_exit_code(code)
                    ),
                    None => "shell 已退出（退出码未知）— Ctrl+Shift+R 重新打开".to_owned(),
                });
            }
            // OSC 52 在 `security.osc52 = false` 时根本不会产生事件；
            // 即使打开也只允许写入剪贴板（只读方向永久禁用）。
            Event::ClipboardStore(_, _) => {
                diag::info("忽略 OSC 52 剪贴板写入");
            }
            _ => {}
        }
    }

    fn write_to_pty(&mut self, bytes: &[u8]) {
        if self.input_probe {
            self.probe_writes += bytes.len();
            diag::debug(&format!("自检写入 {} 字节: {:?}", bytes.len(), bytes));
        }
        // 一边往回翻历史一边敲键盘，说明用户想回到当前输出，先跳回底部。
        if let Some(term) = self.tab_terminal_mut() {
            if term.display_offset() > 0 {
                term.scroll_to_bottom();
                self.dirty = true;
            }
        }
        let Some(session) = self.tab_session_mut() else {
            return;
        };
        if let Err(err) = session.write(bytes) {
            diag::warn(&format!("写入 PTY 失败: {err:#}"));
            return;
        }
        self.blink.reset(Instant::now());
    }

    // ---------------------------------------------------------------- 输入

    /// 打开设置面板。
    fn open_settings(&mut self) {
        if self.settings.is_some() {
            return;
        }
        self.settings = Some(Settings::from_config(&self.config));
        self.dirty = true;
        let count = self.settings.as_ref().map_or(0, Settings::len);
        // 面板是一个新画面：`--dump-frame` 时重新挂一次落盘，验收截图才有面板，
        // 否则只会拍到启动时的终端首帧。
        if self.dump_frame.is_some() {
            self.dump_done = false;
            self.dump_due = None;
            self.dirty = true;
        }
        diag::info(&format!("打开设置面板（{count} 项）"));
    }

    /// 面板是否还值得显示：空面板直接关掉，避免留一块只有标题的空白。
    fn settings_visible(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|panel| !panel.is_empty())
    }

    /// 关掉面板（不保存）。
    fn close_settings(&mut self) {
        if self.settings.take().is_some() {
            self.dirty = true;
            diag::info("关闭设置面板（未保存）");
        }
    }

    /// 保存面板上的值：先写盘，成功才换内存里的配置。
    ///
    /// 顺序不能反：写盘失败时内存与文件必须仍然一致，否则用户看到的值
    /// 会在下次启动时"自己变回去"。
    fn save_settings(&mut self) {
        let Some(panel) = self.settings.clone() else {
            return;
        };
        let mut next = self.config.clone();
        panel.apply(&mut next);
        // 「随 Windows 启动」不是配置文件里的字段，而是注册表状态：按面板当前值
        // 去登记/取消，失败要在提示条上说清楚（自启动没开成，用户下次开机会发现
        // 窗口不见了，这必须当场可见）。
        let mut autostart_note: Option<String> = None;
        if let Some(want) = panel.autostart_requested() {
            if want != crate::win::autostart::is_enabled() {
                let changed = if want {
                    match std::env::current_exe() {
                        Ok(exe) => crate::win::autostart::enable(&exe),
                        Err(err) => {
                            diag::error(&format!("取不到当前可执行文件路径: {err}"));
                            false
                        }
                    }
                } else {
                    crate::win::autostart::disable()
                };
                let now = crate::win::autostart::is_enabled();
                if now == want && changed {
                    diag::info(&crate::win::autostart::describe());
                } else {
                    diag::warn("自启动登记失败，注册表未按预期改变");
                    autostart_note = Some(format!(
                        "自启动没有改成功（现在：{}）",
                        if now { "开" } else { "关" }
                    ));
                }
            }
        }
        match self.config_path.clone() {
            Some(path) => match crate::config::save_atomic(&next, &path) {
                Ok(()) => {
                    diag::info(&format!("设置已写入 {}", path.display()));
                    self.config = next;
                    if let Err(err) = self.reload_fonts(self.hwnd) {
                        diag::warn(&format!("应用字号失败: {err:#}"));
                    }
                    if let Err(err) = self.apply_size(self.hwnd) {
                        diag::warn(&format!("应用尺寸失败: {err:#}"));
                    }
                    self.settings = None;
                    self.dirty = true;
                    // 沉底开关刚变过：立刻生效，别让用户以为没反应。
                    // 关闭沉底时把窗口提到最前并拿焦点——这正是"点不动"的解药。
                    if self.sinks() {
                        self.maybe_sink(self.hwnd);
                    } else {
                        let report = dock::take_keyboard_focus(self.hwnd);
                        self.focused = report.focus_set;
                        self.blink.set_enabled(report.focus_set);
                        diag::info(&format!("设置后提到最前: {}", report.describe()));
                    }
                    self.set_overlay(match autostart_note {
                        Some(note) => note,
                        None => "设置已保存".to_owned(),
                    });
                }
                Err(err) => {
                    // 写盘失败要说出来，并且**不**关面板：用户还能改回去重试。
                    diag::error(&format!("写配置失败: {err:#}"));
                    self.set_overlay(format!("设置没有保存：{err}"));
                }
            },
            None => {
                diag::error("配置目录不可用，设置无法保存");
                self.set_overlay("配置目录不可用，设置无法保存");
            }
        }
    }

    /// 面板开着时接管按键。返回 `true` 表示这个键已经被面板吃掉。
    fn handle_settings_key(&mut self, vk: u32) -> bool {
        if self.settings.is_none() {
            return false;
        }
        const VK_ESCAPE: u32 = 0x1b;
        const VK_RETURN: u32 = 0x0d;
        const VK_LEFT: u32 = 0x25;
        const VK_UP: u32 = 0x26;
        const VK_RIGHT: u32 = 0x27;
        const VK_DOWN: u32 = 0x28;
        match vk {
            VK_ESCAPE => self.close_settings(),
            VK_RETURN => self.save_settings(),
            VK_UP => {
                if let Some(panel) = self.settings.as_mut() {
                    panel.move_selection(-1);
                }
                self.dirty = true;
            }
            VK_DOWN => {
                if let Some(panel) = self.settings.as_mut() {
                    panel.move_selection(1);
                }
                self.dirty = true;
            }
            VK_LEFT | VK_RIGHT => {
                if let Some(panel) = self.settings.as_mut() {
                    panel.adjust(vk == VK_RIGHT);
                }
                self.dirty = true;
            }
            _ => {}
        }
        // 面板开着时所有按键都归面板，绝不能漏给终端——否则在设置界面上
        // 打字会把字符写进 shell。
        true
    }

    fn handle_keydown(&mut self, vk: u32, mods: Modifiers, scan: u16) {
        // 切换终端弹窗在最上层：它先吃掉按键，绝不漏给终端或设置面板。
        if self.shell_picker.is_some() {
            self.handle_shell_picker_key(vk);
            return;
        }
        // 面板开着就先给面板；它吃掉所有按键。
        if self.settings_visible() {
            // 面板开着就吃掉所有按键：Esc 关、Enter 存、↑↓ 选、←→ 调。
            self.handle_settings_key(vk);
            return;
        }
        // 输入法开着的时候键盘归输入法：拼音字母必须由它组成候选词，
        // 我们一边写 PTY 一边让它组词就是「输入一遍、回车再输一遍」。
        // 关掉输入法（英文模式）才轮到自己翻译，这条判据与系统行为一致。
        if !mods.ctrl && !mods.alt && crate::win::ime::is_composing(self.hwnd) {
            return;
        }

        // 打开设置：Ctrl+, 或 F2（不需要记组合键）。
        const VK_COMMA: u32 = 0xbc;
        const VK_F2: u32 = 0x71;
        if (vk == VK_COMMA && mods.ctrl) || vk == VK_F2 {
            self.open_settings();
            return;
        }

        // `VK_PACKET`（0xE5）：`SendInput(KEYEVENTF_UNICODE)`、部分输入法与
        // 远程桌面会话把字符装在扫描码里用这个虚拟键送进来，系统**不会**再补
        // 一条 `WM_CHAR`。不在这里接手，这些字符就静默丢失
        // （真实探针实测：点击与焦点都正常，但 `字符=0`、写入 0 字节）。
        if vk == VK_PACKET {
            // 两条来源共用这一个虚拟键，必须分开处理：
            // 1. `SendInput(KEYEVENTF_UNICODE)` / 部分 IME：扫描码字段直接是
            //    UTF-16 码元（会 ≥ 0x20），当成字符写出去；
            // 2. 某些注入路径把所有键都送成 `VK_PACKET`，但扫描码是真实扫描码
            //    （例如 A 键是 0x1e），这时按扫描码反查虚拟键再翻译，
            //    否则物理键盘在这里会「打得出来却变成控制字符」。
            // 顺序不能反：真键盘走虚拟化/注入路径时扫描码是**真实扫描码**
            // （C 键 0x2E、N 键 0x31），若先按「UTF-16 码元」解释，用户敲
            // `ipconfig` 会看到 `ip.o1!i"ipconfig`——每个 ≥0x20 的键变成
            // 扫描码对应的标点/数字。只有扫描码解释失败（≥0x80，不可能是
            // 扫描码）才退回 UTF-16 码元，那条路给 CJK 与 emoji 用。
            let text = crate::win::text::text_for_packet(scan);
            if !text.is_empty() {
                for ch in text.chars() {
                    self.send_char(ch);
                }
                // 同一个键随后还会来一条 `WM_CHAR`（真键盘路径实测如此），
                // 不设抑制窗就会写两遍：乱码 + 正确字符各一份。
                self.suppress_char_until = Some(Instant::now() + SUPPRESS_CHAR_WINDOW);
            } else if scan >= 0x20 {
                self.commit_ime_unit(scan);
            }
            return;
        }
        let Some(key) = map_vk(vk) else {
            return;
        };
        // 普通字符键在没有 Ctrl/Alt 时留给 WM_CHAR——那里能拿到正确的
        // Shift 状态与输入法组合结果。
        if matches!(key, Key::Char(_)) && !mods.ctrl && !mods.alt {
            // 自己翻译一次再等 `WM_CHAR`：`WM_CHAR` 依赖 `TranslateMessage`
            // 从扫描码推出字符，注入或某些 IME 路径下它根本不出现，
            // 用户侧就是「打字没反应」。翻译成功就写，并在窗口内抑制紧随
            // 其后的那条 `WM_CHAR`，避免同一个键写两遍。
            let text = crate::win::text::text_for(vk as u16, scan);
            if text.is_empty() {
                return;
            }
            // 输入法处在中文/假名模式时，这几个字母是它的拼音素材，不是终端输入。
            // 搜狗不写 `GCS_COMPSTR`，只靠 `is_composing` 判不出来——那正是
            // 「拼音打一遍、回车中文又一遍」的成因。
            if crate::win::ime::wants_native_input(self.hwnd) {
                diag::debug("输入法中文模式，字母不写 PTY");
                return;
            }
            for ch in text.chars() {
                self.send_char(ch);
            }
            self.suppress_char_until = Some(Instant::now() + SUPPRESS_CHAR_WINDOW);
            return;
        }
        // Ctrl+Shift+R 重开 shell：只在没有会话时生效，避免误杀正在跑的命令。
        if mods.ctrl && mods.shift && !mods.alt && matches!(key, Key::Char('r') | Key::Char('R')) {
            self.restart_session();
            return;
        }
        // Ctrl+C 有双重语义：有选区时是「复制」，没选区时才是中断。
        // 计划书与安全评审 S-13 都要求这两件事不共用同一次按键语义。
        if mods.ctrl && !mods.alt && matches!(key, Key::Char('c') | Key::Char('C')) {
            let has_selection = self.tab_terminal().is_some_and(|term| term.has_selection());
            if has_selection {
                self.copy_selection();
                return;
            }
        }
        // Ctrl+V 走剪贴板净化 + 多行确认 + bracketed paste 三条前置。
        if mods.ctrl && !mods.alt && matches!(key, Key::Char('v') | Key::Char('V')) {
            self.paste_from_clipboard();
            return;
        }
        // 回滚快捷键：Shift+PageUp/PageDown 翻页、Shift+Home/End 到顶/到底。
        // 必须带 Shift——裸 PageUp 是 shell 的按键（less/more 要用）。
        if mods.shift && !mods.ctrl && !mods.alt {
            let page = (self.layout.rows.saturating_sub(1)).max(1) as i32;
            match key {
                Key::PageUp => {
                    self.scroll_view(page);
                    return;
                }
                Key::PageDown => {
                    self.scroll_view(-page);
                    return;
                }
                Key::Home => {
                    if let Some(term) = self.tab_terminal_mut() {
                        term.scroll_to_top();
                    }
                    self.dirty = true;
                    return;
                }
                Key::End => {
                    if let Some(term) = self.tab_terminal_mut() {
                        term.scroll_to_bottom();
                    }
                    self.dirty = true;
                    return;
                }
                _ => {}
            }
        }

        // 标签与清屏：都放在编码之前拦下，别把 Ctrl+T 变成 `0x14` 写进 shell。
        if mods.ctrl && !mods.alt {
            match key {
                Key::Char('t') | Key::Char('T') => {
                    let hwnd = self.hwnd;
                    self.open_tab(hwnd);
                    return;
                }
                Key::Char('w') | Key::Char('W') => {
                    let hwnd = self.hwnd;
                    let index = self.active_index();
                    self.close_tab(index, hwnd);
                    return;
                }
                Key::Char('l') | Key::Char('L') => {
                    self.clear_screen();
                    return;
                }
                // 字号缩放：终端里最常用的一个调节，不该逼用户去改配置文件。
                Key::Char('=') => {
                    let hwnd = self.hwnd;
                    self.zoom_font(1, hwnd);
                    return;
                }
                Key::Char('-') => {
                    let hwnd = self.hwnd;
                    self.zoom_font(-1, hwnd);
                    return;
                }
                Key::Char('0') => {
                    let hwnd = self.hwnd;
                    self.reset_font_size(hwnd);
                    return;
                }
                Key::Tab => {
                    let hwnd = self.hwnd;
                    let count = self.tabs.len();
                    if count > 1 {
                        let next = if mods.shift {
                            (self.active_index() + count - 1) % count
                        } else {
                            (self.active_index() + 1) % count
                        };
                        self.switch_tab(next, hwnd);
                    }
                    return;
                }
                _ => {}
            }
        }
        let mode = self
            .tab_terminal()
            .map(|term| term.mode())
            .unwrap_or_else(TermMode::empty);
        if let Some(bytes) = input::encode(key, mods, mode) {
            self.write_to_pty(&bytes);
            // 只有「字符键」才可能紧跟一条重复的 `WM_CHAR`（Ctrl+空格、Ctrl+数字这类
            // 会被 `TranslateMessage` 再翻译一次）。导航键/F 键根本不产生 WM_CHAR，
            // 给它们置位就会把用户的下一个真实字符吞掉（代码审查 B4）。
            if matches!(key, Key::Char(_)) {
                self.suppress_char_until = Some(Instant::now() + SUPPRESS_CHAR_WINDOW);
            }
        }
    }

    fn handle_char(&mut self, code: u32) {
        // 无论是否命中都清掉：过期的抑制标记绝不能留到下一次按键。
        let deadline = self.suppress_char_until.take();
        if char_is_repeat(deadline, Instant::now()) {
            return;
        }
        let Some(ch) = char::from_u32(code) else {
            return;
        };
        // 控制字符已经由 WM_KEYDOWN 处理过，丢掉以免重复。
        if code < 0x20 || code == 0x7f {
            return;
        }
        let mut buffer = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buffer).as_bytes().to_vec();
        self.write_to_pty(&encoded);
    }

    // ---------------------------------------------------------------- 定时

    /// 启动阶段的告警走底部提示条露出来。
    ///
    /// 没有标题栏、没有托盘、没有控制台，热键注册失败和挂载降级如果只写日志，
    /// 用户永远不知道（UI 规范 G5/G6）。这里把最关键的一条摆到画面上。
    fn report_startup_warnings(&mut self) {
        // 提权态在本机没有 UAC 就走不到（会自动提权等于帮用户提权），
        // 所以给 debug 构建留一个只读开关，让这条视觉线索能被验证而不是"写完就算"。
        // 只影响外观判定，不改任何权限：真实权限仍是 `asInvoker`。
        let forced = cfg!(debug_assertions) && std::env::var_os("TLSHELL_FORCE_ELEVATED").is_some();
        if forced {
            diag::info("TLSHELL_FORCE_ELEVATED 生效：按提权态渲染（仅调试构建）");
        }
        match if forced {
            Ok(true)
        } else {
            elevation::is_elevated()
        } {
            Ok(true) => {
                diag::warn("进程以管理员权限运行：从管理员 shell 启动会继承高完整性级别");
                // 提权态不能只靠 30 秒提示条：提示条过去之后用户手里还是一个
                // 看不出任何区别的窗口，而它跑的是高完整性进程（S-01）。
                // 两条常驻线索：边框内侧警示圈 + 状态行。
                self.elevated = true;
                self.set_overlay_for(
                    "以管理员权限运行：此窗口里的操作会改动系统",
                    ADMIN_NOTICE_TTL,
                );
                return;
            }
            Ok(false) => {}
            Err(err) => diag::warn(&format!("提权状态自检失败: {err}")),
        }
        // 透明度低于可读阈值：钳制照做，但必须留痕。`OPACITY_WARN_BELOW` 这个常量
        // 上一轮只存在、没有任何代码读它，等于告警没实现（V-04）。
        self.note_opacity_warning();
        if let Some(first) = self.startup_warnings.first() {
            let text = format!("启动告警：{first}");
            self.set_overlay_for(text, ADMIN_NOTICE_TTL);
        }
    }

    /// 热键、沉底这些告警在 `WM_CREATE` **之后**才产生，
    /// 需要在窗口显示前再补一次提示条，否则只落日志、用户看不见（UI 规范 G5）。
    /// 是否启用「失焦沉底」。
    fn sinks(&self) -> bool {
        // 自检期间不沉底：探针要把窗口压到最上层才能把合成事件送到自己身上，
        // 一沉底，后面每一步的 `SetForegroundWindow` 都会被前台锁挡掉。
        self.config.window.sink_when_unfocused && !self.real_probe
    }

    /// 按配置沉底；关闭时什么都不做（窗口留在普通 z-order 里）。
    fn maybe_sink(&self, hwnd: HWND) {
        if self.sinks() {
            let ok = dock::send_to_bottom(hwnd);
            diag::info(&format!(
                "沉底: ok={ok} 下方窗口={} 紧邻下方={}",
                dock::windows_below(hwnd),
                dock::next_below_class(hwnd)
            ));
        }
    }

    fn report_late_warnings(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        if let Some(first) = self.startup_warnings.first() {
            let text = format!("启动告警：{first}");
            self.set_overlay_for(text, ADMIN_NOTICE_TTL);
        }
    }

    /// `OPACITY_WARN_BELOW` 的告警行为：日志 + 提示条素材两处都留痕。
    fn note_opacity_warning(&mut self) {
        let opacity = self.config.window.opacity;
        if opacity >= crate::config::OPACITY_WARN_BELOW {
            return;
        }
        let message = format!(
            "透明度 {opacity:.2} 低于可读阈值 {:.2}，正文对比度可能不足",
            crate::config::OPACITY_WARN_BELOW
        );
        diag::warn(&message);
        self.startup_warnings.push(message);
    }

    /// 显示一条底部提示，`OVERLAY_TTL` 后自动消失。
    /// 写一条常驻状态行（退出码、致命错误）。
    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some(text.into());
        self.dirty = true;
    }

    fn clear_status(&mut self) {
        if self.status.take().is_some() {
            self.dirty = true;
        }
    }

    /// 重新起一个 shell（`Ctrl+Shift+R`）。
    ///
    /// 退出码留在滚动缓冲区里不擦，所以重开之后还能看见上一条命令的结果。
    fn restart_session(&mut self) {
        if self.tab_session().is_some() {
            return;
        }
        self.clear_status();
        self.show_terminal_error("[TLShell] 重新启动 shell");
        if let Err(err) = self.spawn_session(self.hwnd) {
            diag::error(&format!("重开 shell 失败: {err:#}"));
        }
    }

    fn set_overlay(&mut self, text: impl Into<String>) {
        self.set_overlay_for(text, OVERLAY_TTL);
    }

    /// 显示一条存活 `ttl` 的提示（管理员提示这类需要更久的用这个）。
    fn set_overlay_for(&mut self, text: impl Into<String>, ttl: Duration) {
        self.overlay = Some((text.into(), Instant::now() + ttl));
        self.dirty = true;
    }

    /// 粘贴剪贴板内容。
    ///
    /// 三道闸门（计划书 §5.5 与安全评审 P0-4）：
    /// 1. 净化：只留可打印字符与换行/制表符，控制字符一律丢弃；
    /// 2. 多行确认：`paste_guard` 打开时，含多行的内容需要 3 秒内再按一次；
    /// 3. bracketed paste：终端支持时用 `ESC[200~ ... ESC[201~` 包住，
    ///    让 shell 分清「粘贴」与「键入」，避免粘贴即执行。
    fn paste_from_clipboard(&mut self) {
        let raw = match clipboard::get_text() {
            Ok(text) => text,
            Err(err) => {
                self.set_overlay(format!("粘贴失败：{err}"));
                return;
            }
        };
        let clean = paste::sanitize(&raw);
        if clean.is_empty() {
            self.set_overlay("剪贴板里没有可粘贴的文本");
            return;
        }

        diag::info(&format!(
            "粘贴请求: {} 字符 {} 行，paste_guard={}",
            clean.chars().count(),
            paste::line_count(&clean),
            self.config.security.paste_guard
        ));
        if self.config.security.paste_guard && paste::needs_confirm(&clean) {
            let now = Instant::now();
            let approved = self.pending_paste.as_ref().is_some_and(|(text, at)| {
                text == &clean && now.duration_since(*at) < PASTE_CONFIRM_TTL
            });
            if !approved {
                self.pending_paste = Some((clean.clone(), now));
                diag::info("多行粘贴：等待二次确认");
                self.set_overlay(format!(
                    "剪贴板含 {} 行，3 秒内再按一次 Ctrl+V 确认粘贴",
                    paste::line_count(&clean)
                ));
                return;
            }
            self.pending_paste = None;
        }

        let bracketed = self
            .tab_terminal()
            .is_some_and(|term| term.mode().contains(TermMode::BRACKETED_PASTE));
        let bytes = paste::bytes_for_pty(&clean, bracketed);
        diag::info(&format!("写入粘贴内容 {} 字节", bytes.len()));
        self.write_to_pty(&bytes);
        self.overlay = None;
        self.status = None;
        self.dirty = true;
    }

    fn tick(&mut self) {
        // 自检收尾：把最后一帧画完再退出，别让执行器干等超时。
        if let Some(due) = self.quit_due {
            if Instant::now() >= due {
                self.quit_due = None;
                // SAFETY: PostQuitMessage 只往本线程队列投一条 WM_QUIT。
                unsafe { PostQuitMessage(0) };
                return;
            }
            self.dirty = true;
        }
        // 待落盘期间保持重绘：否则网格内容变了也不会再走 render，落盘会停在旧帧。
        if self.dump_frame.is_some() && !self.dump_done {
            self.dirty = true;
        }
        self.pump_pty();
        self.advance_probe();
        self.watch_config();
        // 可见性看门狗：被 Win+D 之类的操作收走后自己找回来（S-01b）。
        if !self.headless {
            if let Some(reason) = self.watchdog.poll(self.hwnd).filter(|_| self.sinks()) {
                diag::warn(&reason);
                self.set_overlay(reason);
            }
        }

        let now = Instant::now();
        if let Some((_, until)) = self.overlay {
            if now >= until {
                self.overlay = None;
                self.dirty = true;
            }
        }
        if let Some((_, at)) = &self.pending_paste {
            if now.duration_since(*at) >= PASTE_CONFIRM_TTL {
                self.pending_paste = None;
            }
        }
        if self.blink.tick(now) {
            self.dirty = true;
        }

        // 子进程退出不再关窗：留下窗口、把退出码写成常驻状态行（V-30）。
        // 关闭动作交给 `Ctrl+Alt+Q`，用户还能看清最后几行与退出码。
        if let Some(code) = self.tab_session_mut().and_then(Session::try_exit_code) {
            diag::info(&format!("检测到子进程退出，退出码 {code}"));
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.session = None;
            }
            self.set_status(format!(
                "shell 已退出（退出码 {code}）— Ctrl+Shift+R 重新打开"
            ));
        }

        if self.dirty && now.duration_since(self.last_render) >= RENDER_INTERVAL {
            self.render();
            self.last_render = now;
            self.dirty = false;
        }
    }

    fn render(&mut self) {
        // 面板/标签数据必须在借出 painter/surface 之前取好：它们都是 `self` 的字段，
        // 一起可变借用以后就不能再回头读 `self.settings`。
        let panel_view = self.modal_panel();
        // 标题取一份副本：绘制期间还要可变借用 `self.tabs`（借的是同一块内存，
        // 留着 `&str` 会被借用检查器当成冲突）。
        let tab_titles: Vec<(String, bool)> = {
            let active = self.active_index();
            self.tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| (tab.title.clone(), index == active))
                .collect()
        };
        let bar_h = crate::render::layout::tab_bar_height(self.layout.cell_h);
        // 临时提示优先于常驻状态行：告警过去之后状态行自己会回来。
        let overlay_text = self
            .overlay
            .as_ref()
            .map(|(text, _)| text.clone())
            .or_else(|| self.status.clone());
        let flags = crate::render::painter::DrawFlags {
            cursor_visible: self.blink.visible(),
            focused: self.focused,
            elevated: self.elevated,
        };
        let radius = dpi::scale(
            self.config.window.border_radius,
            dpi::BASE_DPI,
            dpi::window_dpi(self.hwnd),
        ) as f32;
        // 背景透明度与文字透明度分开：前者只铺底，后者只乘字形覆盖度。
        let bg_alpha = (self.config.window.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        let text_alpha = (self.config.window.text_opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        let layout = self.layout;
        let hwnd = self.hwnd;
        let hover = self.hover;
        let shell_label = self.shell_label();
        let scrollback = self
            .tab_terminal()
            .map(|term| (term.display_offset(), term.history_size()));

        // 逐个字段借，别借整个 `self`：`tab_views` 里还拿着 `self.tabs` 的引用。
        let App {
            painter,
            surface,
            tabs,
            active_tab,
            ..
        } = self;
        let (Some(painter), Some(surface)) = (painter.as_mut(), surface.as_mut()) else {
            return;
        };
        let (width, height) = surface.size();
        if width == 0 || height == 0 {
            return;
        }
        let active = (*active_tab).min(tabs.len().saturating_sub(1));
        let term = tabs.get(active).and_then(|tab| tab.terminal.as_ref());
        {
            let pixels: &mut [u32] = surface.pixels_mut();
            let mut frame = crate::render::Frame {
                pixels,
                width,
                height,
            };
            painter.bg_alpha = bg_alpha;
            painter.text_alpha = text_alpha;
            if let Some(view) = panel_view {
                painter.draw_panel(
                    &mut frame,
                    &layout,
                    &crate::render::painter::Panel {
                        title: view.title,
                        rows: view.rows,
                        selected: view.selected,
                        footer: view.footer,
                        dirty: view.dirty,
                    },
                );
            } else {
                match term {
                    Some(term) => {
                        painter.draw(&mut frame, &layout, term, flags, overlay_text.as_deref());
                        if let Some((offset, history)) = scrollback {
                            painter.draw_scrollback(&mut frame, &layout, offset, history);
                        }
                    }
                    // shell 退出后没有网格：仍然画一屏，别留一块没刷过的透明面。
                    None => painter.draw_idle(&mut frame, &layout, flags, overlay_text.as_deref()),
                }
                let tab_views: Vec<crate::render::painter::TabView<'_>> = tab_titles
                    .iter()
                    .map(|(title, active)| crate::render::painter::TabView {
                        title: title.as_str(),
                        active: *active,
                    })
                    .collect();
                painter.draw_tab_bar(
                    &mut frame,
                    &layout,
                    bar_h,
                    &tab_views,
                    flags.focused,
                    crate::render::painter::TabBarState { hover, shell_label },
                );
            }
        }
        if let Err(err) = surface.present(hwnd, radius) {
            diag::warn(&format!("UpdateLayeredWindow 失败: {err:#}"));
            return;
        }
        let was_done = self.dump_done;
        Self::maybe_dump(
            self.dump_frame.as_ref(),
            self.dump_bg,
            &mut self.dump_done,
            &mut self.dump_due,
            surface,
            width,
            height,
        );
        // 取证跑完就退出：`--dump-frame` 是一次性诊断，不该常驻桌面。
        // （自检类运行自己会设 quit_due，这里只补「单纯落盘」那条路径。）
        if !was_done
            && self.dump_done
            && self.quit_due.is_none()
            && !self.real_probe
            && !self.input_probe
        {
            self.quit_due = Some(Instant::now() + Duration::from_millis(600));
        }
    }

    /// 到点就把刚推给系统的同一份像素写成 BMP（`--dump-frame`）。
    ///
    /// 白底合成是最差条件，正好用来验 V-19（圆角抗锯齿）、V-02（文字稀释）、
    /// V-38（白底选区）。面板也走这条路径，验收截图才拍得到面板。
    fn maybe_dump(
        dump_frame: Option<&std::path::PathBuf>,
        background: u32,
        dump_done: &mut bool,
        dump_due: &mut Option<Instant>,
        surface: &mut LayeredSurface,
        width: usize,
        height: usize,
    ) {
        if dump_frame.is_none() || *dump_done {
            return;
        }
        let due = *dump_due.get_or_insert_with(|| Instant::now() + DUMP_DELAY);
        if Instant::now() < due {
            return;
        }
        let Some(path) = dump_frame else {
            return;
        };
        match crate::win::dump::write_bmp(path, surface.pixels_mut(), width, height, background) {
            Ok(()) => diag::info(&format!("首帧已落盘: {}", path.display())),
            Err(err) => diag::warn(&format!("首帧落盘失败: {err:#}")),
        }
        *dump_done = true;
    }

    /// 非客户区命中测试。
    ///
    /// 窗口没有标题栏，所以拖动与缩放全靠这里：
    /// - 四角 → 对角缩放；四边 → 单向缩放
    /// - 内边距那一圈 → 整窗拖动
    /// - 其余（正文区）→ 交给终端，绝不吃掉点击
    /// - 按住 Alt 时正文区也可拖动，作为窄内边距下的退路
    fn hit_test(&self, hwnd: HWND, lparam: LPARAM) -> LRESULT {
        let screen_x = (lparam.0 & 0xffff) as i16 as i32;
        let screen_y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        let mut point = POINT {
            x: screen_x,
            y: screen_y,
        };
        // SAFETY: point 是栈上可写结构，hwnd 有效。
        unsafe {
            let _ = ScreenToClient(hwnd, &mut point);
        }
        let Ok(rect) = window::client_rect(hwnd) else {
            return LRESULT(HTCLIENT as isize);
        };
        let (width, height) = (rect.w, rect.h);
        if width <= 0 || height <= 0 {
            return LRESULT(HTCLIENT as isize);
        }

        let (x, y) = (point.x, point.y);
        let corner = CORNER_BAND.min(width / 3).min(height / 3);
        let band = RESIZE_BAND.min(width / 3).min(height / 3).max(1);

        let near_left = x < corner;
        let near_right = x >= width - corner;
        let near_top = y < corner;
        let near_bottom = y >= height - corner;

        // 四角优先，否则内边距那一圈永远够不到对角。
        let corner_hit = match (near_left, near_right, near_top, near_bottom) {
            (true, _, true, _) => Some(HTTOPLEFT),
            (_, true, true, _) => Some(HTTOPRIGHT),
            (true, _, _, true) => Some(HTBOTTOMLEFT),
            (_, true, _, true) => Some(HTBOTTOMRIGHT),
            _ => None,
        };
        if let Some(hit) = corner_hit {
            return LRESULT(hit as isize);
        }

        if x < band {
            return LRESULT(HTLEFT as isize);
        }
        if x >= width - band {
            return LRESULT(HTRIGHT as isize);
        }
        if y < band {
            return LRESULT(HTTOP as isize);
        }
        if y >= height - band {
            return LRESULT(HTBOTTOM as isize);
        }

        // 拖动带至少留 12 物理像素：`padding` 配成 0 时窗口不能变得没法拖。
        let window_dpi = dpi::window_dpi(hwnd);
        let padding = dpi::scale(self.config.window.padding, dpi::BASE_DPI, window_dpi).max(12);
        if x < padding || y < padding || x >= width - padding || y >= height - padding {
            return LRESULT(HTCAPTION as isize);
        }

        // 内边距太窄时，Alt + 拖动是备用把手。
        if current_modifiers().alt {
            return LRESULT(HTCAPTION as isize);
        }

        LRESULT(HTCLIENT as isize)
    }

    /// 把当前几何写进 `state.json`。
    ///
    /// 只在 `WM_EXITSIZEMOVE` 调用（计划书 §5.3）：拖动过程中每个像素都写盘毫无必要，
    /// 而且会把磁盘刷爆。写失败只告警——记不住位置不影响这次使用。
    fn persist_state(&self, hwnd: HWND) {
        let Ok(rect) = window::window_rect(hwnd) else {
            diag::warn("保存位置失败：读不到窗口矩形");
            return;
        };
        let window_dpi = dpi::window_dpi(hwnd);
        let monitor =
            dpi::work_area_for_point(rect.x, rect.y).unwrap_or_else(dpi::primary_work_area);
        let state = crate::config::state::State::new(rect, window_dpi, monitor);
        let Some(path) = crate::config::state::state_path() else {
            diag::warn("保存位置跳过：配置目录不可定位");
            return;
        };
        if let Err(err) = crate::config::state::save(&path, &state) {
            diag::warn(&format!("保存位置失败: {err:#}"));
        } else {
            diag::debug(&format!(
                "已保存位置: {}x{}@{},{} dpi={window_dpi}",
                rect.w, rect.h, rect.x, rect.y
            ));
        }
    }

    /// 限制最小尺寸，避免缩到没法用。
    fn apply_min_size(&self, hwnd: HWND, lparam: LPARAM) -> LRESULT {
        let window_dpi = dpi::window_dpi(hwnd);
        let min_width = dpi::scale(MIN_WIDTH, dpi::BASE_DPI, window_dpi).max(1);
        let min_height = dpi::scale(MIN_HEIGHT, dpi::BASE_DPI, window_dpi).max(1);
        // SAFETY: WM_GETMINMAXINFO 的 lparam 指向可写的 MINMAXINFO。
        let info =
            unsafe { &mut *(lparam.0 as *mut windows::Win32::UI::WindowsAndMessaging::MINMAXINFO) };
        info.ptMinTrackSize.x = min_width;
        info.ptMinTrackSize.y = min_height;
        LRESULT(0)
    }

    // ---------------------------------------------------------------- 消息

    /// 消息分发。返回 `LRESULT`；未处理的消息由调用方转交默认过程。
    unsafe fn handle(
        &mut self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                if let Err(err) = self.initialize(hwnd) {
                    diag::error(&format!("初始化失败: {err:#}"));
                    self.show_terminal_error(&format!("初始化失败: {err}"));
                }
                Some(LRESULT(0))
            }
            WM_PAINT => {
                // SAFETY: 标准 BeginPaint/EndPaint 配对，用于验证无效区域。
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    let _ = BeginPaint(hwnd, &mut ps);
                    let _ = EndPaint(hwnd, &ps);
                }
                self.dirty = true;
                self.render();
                self.last_render = Instant::now();
                self.dirty = false;
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(window::ERASE_HANDLED),
            WM_NCHITTEST => Some(self.hit_test(hwnd, lparam)),
            WM_SETCURSOR => {
                // 整个窗口的光标都由自己分派：类光标只有一个（以前是 I-beam），
                // 一旦让默认过程参与，边框与按钮就会各自退回那个错误的光标。
                // lparam 里的命中码不用——`refresh_cursor` 自己按指针位置重算，
                // 这样「鼠标没动、只是弹窗开合」的时机也能拿到正确结果。
                self.refresh_cursor(hwnd);
                Some(LRESULT(1))
            }
            WM_MOUSEACTIVATE => {
                // 顶层窗口：允许激活，键盘才拿得到（子窗口方案下这句等同于空操作）。
                diag::debug("收到 WM_MOUSEACTIVATE");
                Some(LRESULT(MA_ACTIVATE as isize))
            }
            WM_ACTIVATE => {
                let active = wparam.0 & 0xffff;
                diag::debug(&format!("收到 WM_ACTIVATE wparam={active}"));
                // 只在「失去激活」时重申沉底：用户正在输入时不去抢 z-order。
                if active == 0 {
                    // 失焦：沉底模式回最底层，普通模式只取消 topmost 落回普通层。
                    if self.sinks() {
                        self.maybe_sink(hwnd);
                    } else {
                        dock::drop_topmost(hwnd);
                    }
                }
                None
            }
            WM_EXITSIZEMOVE => {
                self.persist_state(hwnd);
                None
            }
            WM_GETMINMAXINFO => Some(self.apply_min_size(hwnd, lparam)),
            WM_SIZE => {
                if let Err(err) = self.apply_size(hwnd) {
                    diag::warn(&format!("尺寸同步失败: {err:#}"));
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let new_dpi = (wparam.0 & 0xffff) as u32;
                diag::info(&format!("DPI 变更为 {new_dpi}"));
                if let Err(err) = self.reload_fonts(hwnd) {
                    diag::warn(&format!("DPI 变更后重载字体失败: {err:#}"));
                }
                if let Err(err) = self.apply_size(hwnd) {
                    diag::warn(&format!("DPI 变更后重排失败: {err:#}"));
                }
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                diag::info("获得键盘焦点");
                self.focused = true;
                self.blink.set_enabled(true);
                self.dirty = true;
                // 焦点到手但窗口仍留在最底层（沉底模式）。激活与叠放次序是两件事，
                // 用户明确要求「始终在所有窗口之下」。
                if self.sinks() {
                    dock::send_to_bottom(hwnd);
                }
                Some(LRESULT(0))
            }
            WM_KILLFOCUS => {
                diag::info("失去键盘焦点");
                self.focused = false;
                self.blink.set_enabled(false);
                // 失焦即沉底（仅沉底模式）：这是「桌面层」在顶层方案下的锚点。
                // 普通模式则取消 topmost，别长期压在别的窗口上面。
                if self.sinks() {
                    self.maybe_sink(hwnd);
                } else {
                    dock::drop_topmost(hwnd);
                }
                self.dirty = true;
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                diag::debug(&format!(
                    "收到 WM_LBUTTONDOWN lparam=0x{:x} -> {:?}",
                    lparam.0,
                    client_point(lparam)
                ));
                if self.real_probe {
                    self.real_events.0 += 1;
                }
                // 弹窗开着的时候点击只归弹窗：点行 = 选中并把该行的值推到下一个，
                // 点空白什么也不做。绝不能让点击漏给下面的选字逻辑。
                if self.shell_picker.is_some() {
                    let (_, click_y) = client_point(lparam);
                    self.handle_shell_picker_click(click_y);
                    return Some(LRESULT(0));
                }
                // 标签栏先判：那是导航条，不是网格。落在栏上就不该起选区，
                // 否则点标签会顺手把第一行文字选中。
                let (click_x, click_y) = client_point(lparam);
                let bar_h = crate::render::layout::tab_bar_height(self.layout.cell_h);
                if click_y >= 0 && click_y < bar_h {
                    let hit = {
                        let bar = self.tab_bar_now();
                        crate::render::layout::hit_tab_bar(&bar, self.layout.cell_w, click_x)
                    };
                    match hit {
                        crate::render::layout::TabHit::Select(index) => {
                            self.switch_tab(index, hwnd);
                        }
                        crate::render::layout::TabHit::Close(index) => {
                            self.close_tab(index, hwnd);
                        }
                        crate::render::layout::TabHit::New => self.open_tab(hwnd),
                        crate::render::layout::TabHit::Shell => self.open_shell_picker(),
                        crate::render::layout::TabHit::Clear => self.clear_screen(),
                        crate::render::layout::TabHit::Blank => {}
                    }
                    // 点完导航条仍然要拿到键盘：用户的下一动作一定是打字。
                    let report = dock::take_keyboard_focus(hwnd);
                    self.focused = report.focus_set;
                    self.blink.set_enabled(report.focus_set);
                    diag::info(&format!(
                        "点击标签栏 @({click_x},{click_y}) 栏高={bar_h} 单元格={}: {:?} {}",
                        self.layout.cell_w,
                        hit,
                        report.describe()
                    ));
                    return Some(LRESULT(0));
                }
                // 点一下就把前台与键盘都拿回来，然后立刻回读校验。
                let report = dock::take_keyboard_focus(hwnd);
                diag::info(&format!("点击取焦点: {}", report.describe()));
                // 沉底模式：拿到键盘之后立刻把 z-order 压回最底。
                // 用户要的是「它永远在所有窗口之下」，而不是「点一下就把桌面挡住」；
                // 两者可以同时成立——前台窗口照样收键盘，叠放次序仍在最底。
                if self.sinks() {
                    dock::send_to_bottom(hwnd);
                }
                self.focused = report.focus_set;
                self.blink.set_enabled(report.focus_set);
                self.begin_selection(hwnd, lparam);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.drag_selection(lparam);
                self.track_hover(lparam);
                // 光标跟着指针走：光靠 WM_SETCURSOR 不够，弹窗开合、按钮互切这些
                // 状态变化发生时指针并没有移动，样式就会停在旧的那一种。
                self.refresh_cursor(hwnd);
                None
            }
            WM_MOUSELEAVE => {
                // 鼠标离开窗口：把悬停高亮撤掉，否则按钮会一直亮着。
                self.hover_tracking = false;
                if self.hover != crate::render::layout::TabHit::Blank {
                    self.hover = crate::render::layout::TabHit::Blank;
                    self.dirty = true;
                }
                None
            }
            WM_LBUTTONUP => {
                self.end_selection();
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                // 鼠标被别人抢走（比如弹了系统菜单），别把 selecting 挂在那儿。
                self.selecting = false;
                None
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
                // 一格 120，一次滚 3 行；正数（滚轮向前）看历史。
                self.scroll_view(delta / 120 * 3);
                Some(LRESULT(0))
            }
            WM_IME_STARTCOMPOSITION => {
                self.position_ime(hwnd);
                None
            }
            WM_IME_COMPOSITION => {
                // 上屏结果串。微软拼音的部分版本只走这条路，不发 `WM_IME_CHAR`；
                // 只接后者会让「选完候选词按空格、字没进终端」看起来像输入法坏了。
                //
                // 只记录**字节数**，不记录内容：日志里出现用户输入的字符等于
                // 内置键盘记录器（安全评审 F-01）。
                let flags = lparam.0 as u32;
                if flags & GCS_RESULTSTR.0 != 0 {
                    if let Some(text) = ime::result_string(hwnd) {
                        if !text.is_empty() {
                            self.commit_ime_text(&text, ImeSource::Composition);
                        }
                    }
                }
                Some(LRESULT(0))
            }
            WM_IME_CHAR => {
                // 组字型输入法（微软拼音实测如此）上屏时会同时发
                // `WM_IME_COMPOSITION`(`GCS_RESULTSTR`) 与这条 `WM_IME_CHAR`。
                // 去重放在 `commit_ime_text` 里按「内容 + 来源 + 时间」判，
                // 不依赖两条消息谁先到。
                // 输入法上屏。**不记录字符内容**：那等于给自己装了一个键盘记录器
                // （安全评审 F-01）。
                //
                // 这里也**不再**用「抑制下一条 WM_CHAR」来防重复：我们不调用
                // `DefWindowProc`，系统根本不会把 WM_IME_CHAR 翻译成 WM_CHAR，
                // 那条抑制只会误伤用户的下一个真实按键（代码审查 B5）。
                if self.real_probe {
                    self.real_events.2 += 1;
                }
                self.commit_ime_unit(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = wparam.0 as u32;
                let mods = current_modifiers();
                if self.real_probe {
                    self.real_events.1 += 1;
                }
                // VK_PACKET 把 UTF-16 单元放在 16 位扫描码字段里，所以这里取
                // 16 位而不是 8 位——取 8 位会把 CJK 与 emoji 裁成半个码元。
                let scan = ((lparam.0 >> 16) & 0xffff) as u16;
                if self.real_probe {
                    diag::debug(&format!(
                        "真实探针: WM_KEYDOWN vk={vk:#x} scan={scan:#x} lparam={:#x}",
                        lparam.0
                    ));
                }
                self.handle_keydown(vk, mods, scan);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                if self.real_probe {
                    self.real_events.2 += 1;
                    diag::debug(&format!("真实探针: WM_CHAR code={:#x}", wparam.0));
                }
                self.handle_char(wparam.0 as u32);
                Some(LRESULT(0))
            }
            WM_TIMER => {
                if wparam.0 == TIMER_TICK {
                    self.tick();
                }
                Some(LRESULT(0))
            }
            WM_HOTKEY => {
                match wparam.0 as i32 {
                    HOTKEY_ID_QUIT => {
                        diag::info("收到退出热键");
                        // SAFETY: PostQuitMessage 只投递线程消息。
                        unsafe { PostQuitMessage(0) };
                    }
                    HOTKEY_ID_RAISE => {
                        diag::info("收到唤起热键");
                        // SAFETY: hwnd 有效；这三个调用都只影响本窗口。
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_SHOW);
                        }
                        // 热键是用户主动动作，`SetForegroundWindow` 在这里通常被允许；
                        // 万一被前台锁挡住，`raise_and_focus` 会用 topmost +
                        // `SwitchToThisWindow` 兜底，保证窗口真的看得见。
                        let report = dock::raise_and_focus(hwnd);
                        diag::info(&format!("唤起: {}", report.describe()));
                        self.focused = report.focus_set;
                        self.blink.set_enabled(report.focus_set);
                        if report.foreground {
                            dock::drop_topmost(hwnd);
                        }
                        self.dirty = true;
                    }
                    HOTKEY_ID_HIDE => {
                        diag::info("收到隐藏热键");
                        // SAFETY: hwnd 有效。
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                // 无边框窗口没有关闭按钮；外部 WM_CLOSE 不结束会话（安全评审 SYS-6）。
                diag::info("忽略外部 WM_CLOSE");
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: hwnd 仍有效，注销本窗口的定时器。
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_TICK);
                }
                for tab in &mut self.tabs {
                    // 同上：退出路径也不能在 UI 线程 join 读线程。
                    if let Some(session) = tab.session.take() {
                        std::thread::spawn(move || drop(session));
                    }
                }
                // SAFETY: PostQuitMessage 只投递线程消息。
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            other if other == self.taskbar_created && other != 0 => {
                self.reassert_bottom(hwnd);
                Some(LRESULT(0))
            }
            other if other == single::QUIT_MESSAGE => {
                // `tlshell.exe --quit` 落到这里：它必须是真正的退出请求。
                // `WM_CLOSE` 被有意忽略（外部程序不该能顺手关掉终端），所以
                // 命令行退出得有自己的消息。
                diag::info("收到退出请求（--quit）");
                // SAFETY: PostQuitMessage 只投递线程消息。
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            other if other == single::WAKE_MESSAGE => {
                // 第二个实例启动时叫醒我们：显示并真正拿下键盘焦点
                // （顶层窗口的 `SetForegroundWindow` 才有效，子窗口时是空操作）。
                // SAFETY: hwnd 有效。
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
                let report = dock::take_keyboard_focus(hwnd);
                diag::info(&format!("第二实例唤醒: {}", report.describe()));
                self.dirty = true;
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// 从 `LPARAM` 里取出客户区坐标（低 16 位 x、高 16 位 y，各自按 16 位符号扩展）。
fn client_point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xffff) as u16 as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
    (x, y)
}

/// OSC 52 策略：默认关闭；打开也只允许写入剪贴板，读方向永久禁用。
fn osc52_mode(config: &Config) -> Osc52 {
    if config.security.osc52 {
        Osc52::OnlyCopy
    } else {
        Osc52::Disabled
    }
}

fn current_modifiers() -> Modifiers {
    // 用 `GetAsyncKeyState` 而不是 `GetKeyState`：桌面层窗口可能通过投递消息、
    // 输入法或热键路径拿到按键，此时线程键盘状态未必被更新过，
    // 读全局物理按键状态才不会把 Ctrl 判成没按。
    // SAFETY: 三个参数都是常量虚拟键码，只读全局键状态。
    unsafe {
        Modifiers {
            shift: GetAsyncKeyState(VK_SHIFT.0 as i32) < 0,
            ctrl: GetAsyncKeyState(VK_CONTROL.0 as i32) < 0,
            alt: GetAsyncKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

/// 虚拟键码 → 终端语义按键。返回 `None` 表示这个键不产生输入。
fn map_vk(vk: u32) -> Option<Key> {
    let key = match vk {
        0x08 => Key::Backspace,
        0x09 => Key::Tab,
        0x0d => Key::Enter,
        0x1b => Key::Escape,
        0x20 => Key::Char(' '),
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x23 => Key::End,
        0x24 => Key::Home,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x2d => Key::Insert,
        0x2e => Key::Delete,
        0x30..=0x39 => Key::Char(char::from(b'0' + (vk - 0x30) as u8)),
        0x41..=0x5a => Key::Char(char::from(b'a' + (vk - 0x41) as u8)),
        0x70..=0x7b => Key::Function((vk - 0x6f) as u8),
        // OEM 标点：无 Ctrl/Alt 时走 WM_CHAR，这里只为 Ctrl+<标点> 这类组合服务
        // （例如 Ctrl+\ 的 0x1c）。缺了它们，Ctrl+标点在终端里没有任何反应。
        0xba => Key::Char(';'),
        0xbb => Key::Char('='),
        0xbc => Key::Char(','),
        0xbd => Key::Char('-'),
        0xbe => Key::Char('.'),
        0xbf => Key::Char('/'),
        0xc0 => Key::Char('`'),
        0xdb => Key::Char('['),
        0xdc => Key::Char('\\'),
        0xdd => Key::Char(']'),
        0xde => Key::Char('\''),
        _ => return None,
    };
    Some(key)
}

/// 这条 `WM_CHAR` 是不是刚才那次 `WM_KEYDOWN` 的重复投递。
///
/// 只看时间窗：命中也清、不命中也清，标记不会跨按键残留。
/// 把退出码说成人话。
///
/// Windows 的退出码是 32 位的：`0` 正常，`1`.. 是普通错误，
/// `>= 0x8000_0000` 是 NTSTATUS（`0xC000_013A` 就是 Ctrl+C 中断），
/// 而 `Stop-Process` 之类的强制结束会给出 `0xFFFF_FFFF`。
/// 直接打印十进制 `4294967295` 只会让人困惑（实测踩过）。
fn describe_exit_code(code: u32) -> String {
    if code == 0 {
        return "正常退出".to_owned();
    }
    if code == u32::MAX {
        return "异常结束（0xFFFFFFFF，进程被强制终止）".to_owned();
    }
    if code >= 0x8000_0000 {
        if code == 0xC000_013A {
            return "被中断（0xC000013A，Ctrl+C / 控制台关闭）".to_owned();
        }
        return format!("异常结束（0x{code:08X}）");
    }
    format!("退出码 {code}")
}

/// 字号加减一档，并夹在允许范围内。
///
/// 抽成自由函数是为了能单测：`zoom_font` 要 HWND 才能重排，测不到边界。
fn next_font_size(current: f64, step: i32) -> f64 {
    (current + f64::from(step)).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX)
}

fn char_is_repeat(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|deadline| now < deadline)
}

/// 窗口过程。任何 panic 都退化为默认处理，绝不跨 FFI 边界 unwind。
pub unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 由调用方（窗口过程）保证在正确的线程与窗口上下文中。
        unsafe { dispatch(hwnd, msg, wparam, lparam) }
    }));

    match result {
        Ok(value) => value,
        Err(_) => {
            diag::error("窗口过程 panic，已降级为默认处理");
            // SAFETY: 透传系统默认过程。
            unsafe { window::default_proc(hwnd, msg, wparam, lparam) }
        }
    }
}

unsafe fn dispatch(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: WM_NCCREATE 的 lparam 是 CREATESTRUCTW。
        unsafe { window::bind_app(hwnd, lparam) };
        // SAFETY: 透传默认过程以完成非客户区创建。
        return unsafe { window::default_proc(hwnd, msg, wparam, lparam) };
    }

    // SAFETY: 只读窗口绑定的应用指针。
    let ptr = unsafe { window::app_from_window(hwnd) };
    let Some(ptr) = ptr else {
        // SAFETY: 应用指针尚未绑定，交给默认过程。
        return unsafe { window::default_proc(hwnd, msg, wparam, lparam) };
    };

    // SAFETY: 指针由 `run` 通过 `Box::leak` 提供，存活到进程结束；
    // 且窗口过程只在该 UI 线程上被调用，不存在别名竞争。
    let app = unsafe { &mut *(ptr as *mut App) };

    // SAFETY: 同上，处于窗口过程上下文。
    if let Some(result) = unsafe { app.handle(hwnd, msg, wparam, lparam) } {
        return result;
    }

    // SAFETY: 未处理的消息交给默认过程。
    unsafe { window::default_proc(hwnd, msg, wparam, lparam) }
}

/// 一次运行的输入参数。
///
/// 打包成结构体是因为诊断开关一直在增加：八个位置参数既难看也容易被调换，
/// clippy 的 `too_many_arguments` 也在这儿拦了一道。
pub struct RunOptions {
    pub config: Config,
    pub rect: Option<dpi::Rect>,
    /// `--probe-input`：进程内 `PostMessage` 自检。
    pub input_probe: bool,
    /// `--probe-real`：`SendInput` 真实事件自检。
    pub real_probe: bool,
    /// `--dump-frame`：把首帧落盘成 BMP。
    pub dump_frame: Option<std::path::PathBuf>,
    /// `--dump-bg`：落盘时的合成底色。
    pub dump_bg: Option<u32>,
    /// `--config` 指定的保存目标。
    pub config_path: Option<std::path::PathBuf>,
    /// `--open-settings`：启动即展开面板（视觉取证）。
    pub open_settings: bool,
    /// UI 自动化测试模式：不沉底、不自动退出，只给验收脚本用。
    pub ui_test: bool,
}

/// 启动消息循环，返回进程退出码。
pub fn run(options: RunOptions) -> Result<i32> {
    let RunOptions {
        config,
        rect,
        input_probe,
        real_probe,
        dump_frame,
        dump_bg,
        config_path,
        open_settings,
        ui_test,
    } = options;
    let hinstance = window::module_handle()?;
    window::register_class(hinstance, wndproc)?;

    // 恢复矩形可能来自上次的 state.json，也可能是配置里的默认值；
    // 调用方已经把完全落在屏幕外的矩形钳回来了。
    let spec = WindowSpec {
        rect: rect.unwrap_or_else(|| {
            dpi::Rect::new(
                config.window.x,
                config.window.y,
                config.window.width,
                config.window.height,
            )
        }),
    };

    // 泄漏而不是回收：`App` 必须比窗口活得更久，且窗口过程可能在任意时刻回访它。
    // 进程退出时由操作系统回收，不存在累积泄漏。
    let mut app_owned = App::new(config.clone());
    app_owned.dump_frame = dump_frame;
    if let Some(bg) = dump_bg {
        app_owned.dump_bg = bg;
    }
    // `--config` 指定的文件就是保存目标：否则面板里的值会写回默认位置，
    // 用户看到的和落盘的会变成两个文件。
    if let Some(path) = config_path {
        app_owned.config_path = Some(path);
    }
    app_owned.open_panel_at_startup = open_settings;
    if ui_test {
        // 验收脚本用 PostMessage 点击控件；沉底状态下真实点击拿不到前台，
        // 所以测试模式强制不沉底。这不是产品默认行为，正常启动不会走到这里。
        app_owned.config.window.sink_when_unfocused = false;
        diag::info("UI 测试模式：强制不沉底、不自动退出");
    }
    if real_probe {
        app_owned.enable_real_probe();
    } else if input_probe {
        app_owned.enable_input_probe();
    }
    let app: &'static mut App = Box::leak(Box::new(app_owned));
    let app_ptr: *mut c_void = (app as *mut App).cast();

    let hwnd = window::create(hinstance, &spec, app_ptr)?;
    app.hwnd = hwnd;
    // 记下句柄，第二个实例靠它唤醒我们。
    single::remember(hwnd.0 as isize);

    // 桌面层方案（M6 架构裁决）：顶层窗口 + z-order 沉底。
    // **绝不再 SetParent**：一旦变成子窗口，键盘焦点就落在 explorer 的线程输入
    // 队列里，`SetFocus` 与 `SetForegroundWindow` 都作用于不到本窗口，终端从此
    // 只收得到鼠标、收不到按键。
    if !dock::is_top_level(hwnd) {
        diag::warn("窗口不是顶层窗口：键盘焦点可能永远拿不到");
    }
    if app.sinks() {
        // 「沉底」不能只看 SetWindowPos 的返回值：它成功不代表真的到了最底。
        // `windows_below` 数的是自己下面还剩几个窗口，真到底时必须是 0，
        // 这行日志就是执行器能断言的证据。
        let ok = dock::send_to_bottom(hwnd);
        diag::info(&format!(
            "启动沉底: ok={ok} 下方窗口={} 紧邻下方={}",
            dock::windows_below(hwnd),
            dock::next_below_class(hwnd)
        ));
        if !ok {
            app.startup_warnings.push("桌面层沉底失败".to_owned());
        }
    }

    // `UpdateLayeredWindow` 的前置条件：窗口必须带 WS_EX_LAYERED（`create` 已设，
    // 这里再确认一次，样式被外部改掉时能立刻发现）。
    layered::ensure_layered(hwnd)?;

    app.register_hotkeys(hwnd);
    // 热键/沉底的告警产生于 `WM_CREATE` 之后，显示前补一次提示条。
    app.report_late_warnings();
    window::show(hwnd);

    // 普通 z-order 下必须主动激活：`SW_SHOWNOACTIVATE` 只把窗口画出来，
    // 它会停在普通窗口的最下层 —— 用户桌面上只要有最大化窗口，就等于看不见、
    // 点不到，这正是上一版「无法点击和输入」的第二个成因。
    // 桌面层模式（sink）相反：故意不抢前台，停回最底层。
    if app.sinks() {
        diag::info("启动：桌面层模式，不抢前台");
    } else {
        let report = dock::raise_and_focus(hwnd);
        app.focused = report.focus_set;
        app.blink.set_enabled(report.focus_set);
        diag::info(&format!("启动取焦点: {}", report.describe()));
        if report.foreground {
            // 前台已经拿到：立刻退回普通 z-order，不需要继续压在最上层。
            // 被前台锁挡住时保持 topmost —— 否则窗口会掉回最大化窗口下面，
            // 用户既看不见也点不到（这正是上一版的症状）；等用户点一下窗口、
            // 触发 WM_ACTIVATE 后再由失焦分支退下来。
            dock::drop_topmost(hwnd);
        }
    }

    diag::info(&format!("启动完成: {}", window::describe(hwnd)));
    app.log_tab_geometry();
    // 遮挡自检：外部探针（受限桌面）看不到真实窗口，因此「点得到吗」只能由本进程回答。
    diag::info(&format!(
        "命中自检: {} 可见={} 沉底={}",
        dock::hit_report(hwnd),
        dock::is_visible(hwnd),
        app.sinks()
    ));
    Ok(message_loop())
}

fn message_loop() -> i32 {
    let mut msg = MSG::default();
    loop {
        // SAFETY: msg 是栈上可写结构，GetMessageW 会在本线程阻塞。
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match ret.0 {
            -1 => {
                diag::error("GetMessageW 失败");
                return 1;
            }
            0 => return msg.wParam.0 as i32,
            _ => {
                // SAFETY: msg 由上一步填充。
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// 给已存在的窗口发关闭请求。单实例场景下由第二个进程调用。
#[allow(dead_code)] // M5 单实例握手使用
pub fn request_close(hwnd: HWND) -> bool {
    // SAFETY: 只读查询 + 投递消息。
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(hwnd),
            WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        )
        .is_ok()
    }
}

/// 读取窗口扩展样式，供自检断言 `WS_EX_TOOLWINDOW`。
pub fn ex_style(hwnd: HWND) -> u32 {
    // SAFETY: hwnd 有效。
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
}

/// 供自检使用：把用户数据指针读回来，确认绑定成功。
pub fn bound_user_data(hwnd: HWND) -> isize {
    // SAFETY: hwnd 有效。
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_exit_code_reads_like_a_sentence() {
        assert_eq!(describe_exit_code(0), "正常退出");
        assert_eq!(describe_exit_code(1), "退出码 1");
        assert_eq!(
            describe_exit_code(0xC000_013A),
            "被中断（0xC000013A，Ctrl+C / 控制台关闭）"
        );
        assert_eq!(
            describe_exit_code(u32::MAX),
            "异常结束（0xFFFFFFFF，进程被强制终止）"
        );
        assert_eq!(describe_exit_code(0x8000_0002), "异常结束（0x80000002）");
    }

    #[test]
    fn maps_letters_to_lowercase() {
        assert_eq!(map_vk(0x41), Some(Key::Char('a')));
        assert_eq!(map_vk(0x5a), Some(Key::Char('z')));
    }

    #[test]
    fn maps_digits() {
        assert_eq!(map_vk(0x30), Some(Key::Char('0')));
        assert_eq!(map_vk(0x39), Some(Key::Char('9')));
    }

    #[test]
    fn maps_function_keys() {
        assert_eq!(map_vk(0x70), Some(Key::Function(1)));
        assert_eq!(map_vk(0x7b), Some(Key::Function(12)));
    }

    #[test]
    fn maps_navigation_keys() {
        assert_eq!(map_vk(0x25), Some(Key::Left));
        assert_eq!(map_vk(0x26), Some(Key::Up));
        assert_eq!(map_vk(0x28), Some(Key::Down));
        assert_eq!(map_vk(0x2e), Some(Key::Delete));
    }

    #[test]
    fn ignores_unmapped_keys() {
        assert_eq!(map_vk(0x10), None); // Shift
        assert_eq!(map_vk(0x14), None); // CapsLock
    }

    #[test]
    fn osc52_is_disabled_by_default() {
        let config = Config::default();
        assert!(matches!(osc52_mode(&config), Osc52::Disabled));
    }

    #[test]
    fn osc52_enabled_only_allows_copy() {
        let mut config = Config::default();
        config.security.osc52 = true;
        assert!(matches!(osc52_mode(&config), Osc52::OnlyCopy));
    }
}

#[cfg(test)]
mod font_zoom_tests {
    use super::{next_font_size, FONT_SIZE_MAX, FONT_SIZE_MIN};

    #[test]
    fn one_step_moves_exactly_one_pixel() {
        assert_eq!(next_font_size(14.0, 1), 15.0);
        assert_eq!(next_font_size(14.0, -1), 13.0);
    }

    #[test]
    fn zoom_stops_at_the_configured_bounds() {
        assert_eq!(next_font_size(FONT_SIZE_MIN, -1), FONT_SIZE_MIN);
        assert_eq!(next_font_size(FONT_SIZE_MAX, 1), FONT_SIZE_MAX);
    }

    #[test]
    fn bounds_match_the_sanitizer() {
        // sanitize() 与缩放共用同一组常量，越界值必须被同一处夹住。
        let mut config = crate::config::Config::default();
        config.font.size = 999.0;
        config.sanitize();
        assert_eq!(config.font.size, FONT_SIZE_MAX);
        config.font.size = 1.0;
        config.sanitize();
        assert_eq!(config.font.size, FONT_SIZE_MIN);
    }
}

mod ime_dedupe_tests {
    use super::*;
    use std::time::Instant;

    #[allow(dead_code)]
    fn last(text: &str, source: ImeSource, at: Instant) -> (String, Instant, ImeSource) {
        (text.to_owned(), at, source)
    }

    #[test]
    fn same_text_from_the_other_path_is_a_duplicate() {
        let at = Instant::now();
        let previous = last("你好", ImeSource::Composition, at);
        assert!(ime_is_duplicate(
            Some(&previous),
            "你好",
            ImeSource::Char,
            at + Duration::from_millis(5)
        ));
        let previous = last("hello", ImeSource::Char, at);
        assert!(ime_is_duplicate(
            Some(&previous),
            "hello",
            ImeSource::Composition,
            at + Duration::from_millis(300)
        ));
    }

    #[test]
    fn the_same_path_may_repeat_the_same_text() {
        // 连打两个一样的字（比如「中中」）走的是同一条路，不能被当重复吞掉。
        let at = Instant::now();
        let previous = last("中", ImeSource::Composition, at);
        assert!(!ime_is_duplicate(
            Some(&previous),
            "中",
            ImeSource::Composition,
            at + Duration::from_millis(1)
        ));
        let previous = last("a", ImeSource::Char, at);
        assert!(!ime_is_duplicate(
            Some(&previous),
            "a",
            ImeSource::Char,
            at + Duration::from_millis(1)
        ));
    }

    #[test]
    fn different_text_is_never_a_duplicate() {
        let at = Instant::now();
        let previous = last("你好", ImeSource::Composition, at);
        assert!(!ime_is_duplicate(
            Some(&previous),
            "你们",
            ImeSource::Char,
            at + Duration::from_millis(1)
        ));
    }

    #[test]
    fn duplicates_expire_with_the_window() {
        let at = Instant::now();
        let previous = last("ok", ImeSource::Composition, at);
        assert!(!ime_is_duplicate(
            Some(&previous),
            "ok",
            ImeSource::Char,
            at + IME_DUP_WINDOW
        ));
    }

    #[test]
    fn the_first_commit_is_never_a_duplicate() {
        assert!(!ime_is_duplicate(
            None,
            "你好",
            ImeSource::Composition,
            Instant::now()
        ));
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;

    fn picker(choice: usize, new_session: bool, focus: usize) -> ShellPicker {
        ShellPicker {
            choices: SHELL_CHOICES.to_vec(),
            choice,
            new_session,
            focus,
        }
    }

    #[test]
    fn picker_rows_are_exactly_two_and_name_both_modes() {
        let rows = picker(0, false, 0).rows();
        assert_eq!(rows.len(), 2, "弹窗只有两行：终端、方式");
        assert_eq!(rows[0].0, "终端");
        assert_eq!(rows[0].1, SHELL_CHOICES[0].label);
        assert_eq!(rows[1].0, "方式");
        assert_eq!(rows[1].1, "原地切换（保留记录）");

        // 同一份数据切成「新开标签」只改第二行的值，行数不变。
        let rows = picker(0, true, 1).rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].1, "新开标签（另起会话）");
    }

    #[test]
    fn picker_adjust_touches_only_the_focused_row() {
        // 第 0 行：换候选。
        let mut p = picker(0, false, 0);
        p.adjust(true);
        assert_eq!(p.choice, 1 % SHELL_CHOICES.len());
        assert!(!p.new_session, "换终端不该顺手改方式");

        // 第 1 行：换方式，候选不动。
        let mut p = picker(1, false, 1);
        p.adjust(true);
        assert!(p.new_session);
        assert_eq!(p.choice, 1, "换方式不该顺手改终端");
    }

    #[test]
    fn picker_choice_cycles_in_both_directions() {
        let mut p = picker(0, false, 0);
        p.adjust(false);
        assert_eq!(p.choice, SHELL_CHOICES.len() - 1, "往回是环形的");
        p.adjust(true);
        assert_eq!(p.choice, 0);
    }

    #[test]
    fn picker_focus_stays_inside_the_two_rows() {
        let mut p = picker(0, false, 0);
        p.move_focus(-5);
        assert_eq!(p.focus, 0);
        p.move_focus(99);
        assert_eq!(p.focus, 1);
    }

    #[test]
    fn picker_click_selects_the_row_before_adjusting() {
        let mut p = picker(0, false, 0);
        p.click_row(1);
        assert_eq!(p.focus, 1);
        assert!(p.new_session, "点行=选中并把这一行的值推到下一个");

        let mut p = picker(0, false, 1);
        p.click_row(0);
        assert_eq!(p.focus, 0);
        assert_eq!(p.choice, 1 % SHELL_CHOICES.len());
    }

    #[test]
    fn picker_lists_at_least_one_usable_shell_on_this_machine() {
        // 候选是「真正能起来」的那几个：本机至少该有 cmd.exe。
        let config = crate::config::ShellConfig::default();
        let picker = ShellPicker::available(&config, false).expect("至少 cmd.exe 该能解析");
        assert!(!picker.choices.is_empty());
        assert!(
            picker.choices.len() <= SHELL_CHOICES.len(),
            "候选只能来自固定白名单"
        );
        assert!(picker.selected().is_some());
    }
}
