//! 应用装配与消息循环。
//!
//! 所有权约定（解决计划书 §5.2 与 §5.3-4 的冲突）：
//! - `Term` 由 UI 线程独占，PTY 读线程只把字节塞进 channel，绝不共享可变状态。
//! - 窗口过程里的一切 panic 都被 `catch_unwind` 拦住，转交 `DefWindowProcW`，
//!   因为 unwind 穿过 `extern "system"` 边界是未定义行为。

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event;
use alacritty_terminal::term::{Osc52, TermMode};
use anyhow::{Context, Result};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, Win32WindowHandle, WindowHandle, WindowsDisplayHandle,
};
use softbuffer::{Context as SoftContext, Surface};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowLongPtrW, IsWindow, KillTimer, PostQuitMessage,
    SetTimer, ShowWindow, TranslateMessage, GWLP_USERDATA, GWL_EXSTYLE, HTBOTTOM, HTBOTTOMLEFT,
    HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, MSG,
    SW_HIDE, WM_CHAR, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS, WM_NCCREATE,
    WM_NCHITTEST, WM_PAINT, WM_SETFOCUS, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER,
};

use crate::config::Config;
use crate::diag;
use crate::render::{Blink, FontSet, Layout, Painter, Theme};
use crate::term::input::{self, Key, Modifiers};
use crate::term::session::{Session, SpawnOptions};
use crate::term::{shell, TermSize, Terminal};
use crate::win::hotkey::{self, Hotkey};
use crate::win::window::{self, WindowSpec};
use crate::win::{dpi, layered};

pub mod selftest;

/// 退出热键的注册 id。
pub const HOTKEY_ID_QUIT: i32 = 1;
/// 隐藏/清屏热键的注册 id。
pub const HOTKEY_ID_HIDE: i32 = 2;

/// 唯一的定时器 id：同时驱动 PTY 泵与光标闪烁。
const TIMER_TICK: usize = 1;
/// 定时器周期。16ms ≈ 60Hz，是“内容驱动重绘”的节流下限。
const TICK_MS: u32 = 16;
/// 两次实际重绘之间的最小间隔，避免高频输出把 CPU 打满。
const RENDER_INTERVAL: Duration = Duration::from_millis(16);

/// 边缘拖拽缩放的命中宽度（物理像素）。
const RESIZE_BAND: i32 = 5;
/// 四角拖拽缩放的命中尺寸（物理像素），比边宽一些才好抓。
const CORNER_BAND: i32 = 16;
/// 窗口最小尺寸（逻辑像素），来自 UI 规范。
const MIN_WIDTH: i32 = 480;
const MIN_HEIGHT: i32 = 240;

/// 把 HWND 包成 raw-window-handle 需要的样子，供 softbuffer 使用。
#[derive(Clone, Copy)]
struct SoftWindow(HWND);

impl HasWindowHandle for SoftWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let Some(raw_hwnd) = std::num::NonZeroIsize::new(self.0 .0 as isize) else {
            return Err(HandleError::Unavailable);
        };
        let handle = Win32WindowHandle::new(raw_hwnd);
        // SAFETY: 句柄来自真实存在的窗口，且 SoftWindow 不延长其生命周期。
        unsafe { Ok(WindowHandle::borrow_raw(RawWindowHandle::Win32(handle))) }
    }
}

impl HasDisplayHandle for SoftWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: Windows 的 display handle 没有附加状态，构造一个即可。
        unsafe {
            Ok(DisplayHandle::borrow_raw(RawDisplayHandle::Windows(
                WindowsDisplayHandle::new(),
            )))
        }
    }
}

pub struct App {
    hwnd: HWND,
    config: Config,
    quit_hotkey: Option<Hotkey>,
    hide_hotkey: Option<Hotkey>,
    /// 记录热键注册失败的原因，供自检与日志使用。
    pub hotkey_warnings: Vec<String>,

    /// 自检路径不启动 shell、不建渲染面。
    headless: bool,
    terminal: Option<Terminal>,
    term_events: Option<Receiver<Event>>,
    session: Option<Session>,
    painter: Option<Painter>,
    context: Option<SoftContext<SoftWindow>>,
    surface: Option<Surface<SoftWindow, SoftWindow>>,
    surface_size: (u32, u32),
    layout: Layout,
    blink: Blink,
    focused: bool,
    dirty: bool,
    last_render: Instant,
    /// `WM_KEYDOWN` 已经处理过的键，抑制随后那条重复的 `WM_CHAR`。
    suppress_next_char: bool,
    /// 实际使用的配置文件路径（`--config` 可覆盖）。
    config_path: std::path::PathBuf,
    /// 上次看到的配置文件修改时间，用于热重载判定。
    config_mtime: Option<std::time::SystemTime>,
    /// 上次轮询配置文件的时间。
    last_watch: Instant,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self::build(config, false)
    }

    /// 自检用：只建窗口，不起 shell。
    pub fn new_headless(config: Config) -> Self {
        Self::build(config, true)
    }

    fn build(config: Config, headless: bool) -> Self {
        // 初始化失败前的兜底布局：8×18 单元格、10px 内边距。
        let layout = Layout::compute(8, 18, config.window.padding, 14, 900, 420);
        Self {
            hwnd: HWND::default(),
            config,
            quit_hotkey: None,
            hide_hotkey: None,
            hotkey_warnings: Vec::new(),
            headless,
            terminal: None,
            term_events: None,
            session: None,
            painter: None,
            context: None,
            surface: None,
            surface_size: (0, 0),
            layout,
            blink: Blink::new(),
            focused: true,
            dirty: false,
            last_render: Instant::now(),
            suppress_next_char: false,
            config_path: crate::config::config_path(),
            config_mtime: None,
            last_watch: Instant::now(),
        }
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
                    self.hotkey_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("退出热键配置非法: {err:#}");
                diag::warn(&message);
                self.hotkey_warnings.push(message);
            }
        }

        match hotkey::parse(&self.config.keys.hide) {
            Ok(spec) => match Hotkey::register(hwnd, HOTKEY_ID_HIDE, &spec) {
                Ok(handle) => self.hide_hotkey = Some(handle),
                Err(err) => {
                    let message = format!("隐藏热键注册失败: {err:#}");
                    diag::warn(&message);
                    self.hotkey_warnings.push(message);
                }
            },
            Err(err) => {
                let message = format!("隐藏热键配置非法: {err:#}");
                diag::warn(&message);
                self.hotkey_warnings.push(message);
            }
        }
    }

    // ---------------------------------------------------------------- 初始化

    /// `WM_CREATE` 里做一次性的重量级初始化。
    fn initialize(&mut self, hwnd: HWND) -> Result<()> {
        if self.headless {
            return Ok(());
        }
        self.ensure_surface(hwnd)?;
        self.reload_fonts(hwnd)?;
        self.apply_size(hwnd)?;
        self.spawn_session(hwnd)?;
        // SAFETY: hwnd 有效；定时器在 WM_DESTROY 里注销。
        unsafe {
            SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);
        }
        Ok(())
    }

    fn ensure_surface(&mut self, hwnd: HWND) -> Result<()> {
        if self.surface.is_some() {
            return Ok(());
        }
        // 注意：`WM_CREATE` 期间 `App::hwnd` 还没写回（`run` 在 create 返回后才赋值），
        // 所以这里必须用传入的 hwnd，否则会拿到空句柄。
        let window = SoftWindow(hwnd);
        let context = SoftContext::new(window)
            .map_err(|err| anyhow::anyhow!("softbuffer 上下文创建失败: {err:?}"))?;
        let surface = Surface::new(&context, window)
            .map_err(|err| anyhow::anyhow!("softbuffer 绘制面创建失败: {err:?}"))?;
        self.context = Some(context);
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

    fn spawn_session(&mut self, hwnd: HWND) -> Result<()> {
        if self.session.is_some() {
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
        diag::info(&format!(
            "启动 shell: {} {:?}",
            resolved.program.display(),
            resolved.args
        ));

        let options = SpawnOptions {
            shell: resolved,
            cols: self.layout.cols as u16,
            rows: self.layout.rows as u16,
            pixel_width: (self.layout.cols * self.layout.cell_w as usize).min(u16::MAX as usize)
                as u16,
            pixel_height: (self.layout.rows * self.layout.cell_h as usize).min(u16::MAX as usize)
                as u16,
            cwd: std::env::var_os("USERPROFILE").map(std::path::PathBuf::from),
        };

        match Session::spawn(options) {
            Ok(session) => {
                diag::info(&format!("shell pid={}", session.pid()));
                self.session = Some(session);
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
        let Some(term) = self.terminal.as_mut() else {
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

        if let Some(surface) = self.surface.as_mut() {
            if let (Some(w), Some(h)) = (
                NonZeroU32::new(width as u32),
                NonZeroU32::new(height as u32),
            ) {
                let _ = surface.resize(w, h);
                self.surface_size = (width as u32, height as u32);
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
        let layout = Layout::compute(cell_w, cell_h, padding, ascent, width, height);
        self.layout = layout;

        let size = TermSize::new(layout.cols, layout.rows);
        match self.terminal.as_mut() {
            Some(term) => term.resize(size),
            None => {
                let (term, rx) = Terminal::new(size, 10_000, osc52_mode(&self.config));
                self.terminal = Some(term);
                self.term_events = Some(rx);
            }
        }

        if let Some(session) = self.session.as_ref() {
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

        self.apply_shape(hwnd);
        self.dirty = true;
        Ok(())
    }

    /// 按当前配置把窗口裁成圆角。
    ///
    /// 只在尺寸/圆角变化时调；`SetWindowRgn` 是有成本的系统调用。
    fn apply_shape(&self, hwnd: HWND) {
        let Ok(rect) = window::client_rect(hwnd) else {
            return;
        };
        let window_dpi = dpi::window_dpi(hwnd);
        let radius = dpi::scale(self.config.window.border_radius, dpi::BASE_DPI, window_dpi);
        if let Err(err) = layered::apply_round_corners(hwnd, rect.w, rect.h, radius) {
            diag::warn(&format!("圆角裁剪失败: {err:#}"));
        }
    }

    // -------------------------------------------------------------- 配置热重载

    /// 每秒轮询一次 `config.toml` 的修改时间，变了就重载。
    ///
    /// 用轮询而不是 `ReadDirectoryChangesW`：后者要额外线程和缓冲协议，
    /// 而配置文件一年改不了几次，1 秒的延迟完全够用。
    fn watch_config(&mut self) {
        const WATCH_INTERVAL: Duration = Duration::from_secs(1);

        let now = Instant::now();
        if now.duration_since(self.last_watch) < WATCH_INTERVAL {
            return;
        }
        self.last_watch = now;

        let mtime = std::fs::metadata(&self.config_path)
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
        let loaded = crate::config::load(Some(&self.config_path));
        for warning in &loaded.warnings {
            diag::warn(warning);
        }
        if loaded.config == self.config {
            diag::debug("配置文件有改动但内容等价，跳过");
            return;
        }

        let font_changed = loaded.config.font != self.config.font;
        self.config = loaded.config;

        if let Some(painter) = self.painter.as_mut() {
            painter.theme = Theme::from_config(&self.config.colors);
        }
        let hwnd = self.hwnd;
        if font_changed {
            if let Err(err) = self.reload_fonts(hwnd) {
                diag::warn(&format!("热重载字体失败: {err:#}"));
            }
        }
        if let Err(err) = layered::set_opacity(hwnd, self.config.window.opacity) {
            diag::warn(&format!("热重载透明度失败: {err:#}"));
        }
        if let Err(err) = self.apply_size(hwnd) {
            diag::warn(&format!("热重载重排失败: {err:#}"));
        }
        self.dirty = true;
        diag::info("配置已热重载");
    }

    // ---------------------------------------------------------------- 数据泵

    /// 取走 PTY 输出并喂给 VT 状态机，然后处理终端事件。
    fn pump_pty(&mut self) {
        let chunks = match self.session.as_ref() {
            Some(session) => session.drain(),
            None => return,
        };
        if !chunks.is_empty() {
            if let Some(term) = self.terminal.as_mut() {
                for chunk in chunks {
                    term.feed(&chunk);
                }
            }
            self.dirty = true;
        }

        let mut events = Vec::new();
        if let Some(rx) = self.term_events.as_ref() {
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
                diag::info("shell 已退出");
                // SAFETY: 只投递线程消息。
                unsafe { PostQuitMessage(0) };
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
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Err(err) = session.write(bytes) {
            diag::warn(&format!("写入 PTY 失败: {err:#}"));
            return;
        }
        self.blink.reset(Instant::now());
    }

    // ---------------------------------------------------------------- 输入

    fn handle_keydown(&mut self, vk: u32, mods: Modifiers) {
        let Some(key) = map_vk(vk) else {
            return;
        };
        // 普通字符键在没有 Ctrl/Alt 时留给 WM_CHAR——那里能拿到正确的
        // Shift 状态与输入法组合结果。
        if matches!(key, Key::Char(_)) && !mods.ctrl && !mods.alt {
            return;
        }
        let mode = self
            .terminal
            .as_ref()
            .map(|term| term.mode())
            .unwrap_or_else(TermMode::empty);
        if let Some(bytes) = input::encode(key, mods, mode) {
            self.write_to_pty(&bytes);
            // 同一个键会先来 WM_KEYDOWN 再来 WM_CHAR，这里打个标记避免发两遍。
            self.suppress_next_char = true;
        }
    }

    fn handle_char(&mut self, code: u32) {
        if self.suppress_next_char {
            self.suppress_next_char = false;
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

    fn tick(&mut self) {
        self.pump_pty();
        self.watch_config();

        let now = Instant::now();
        if self.blink.tick(now) {
            self.dirty = true;
        }

        if self
            .session
            .as_mut()
            .and_then(|session| session.try_exit_code())
            .is_some()
        {
            diag::info("检测到子进程退出，关闭窗口");
            // SAFETY: 只投递线程消息。
            unsafe { PostQuitMessage(0) };
            return;
        }

        if self.dirty && now.duration_since(self.last_render) >= RENDER_INTERVAL {
            self.render();
            self.last_render = now;
            self.dirty = false;
        }
    }

    fn render(&mut self) {
        let (Some(painter), Some(surface), Some(term)) = (
            self.painter.as_mut(),
            self.surface.as_mut(),
            self.terminal.as_ref(),
        ) else {
            return;
        };
        let (width, height) = self.surface_size;
        if width == 0 || height == 0 {
            return;
        }

        let Ok(mut buffer) = surface.buffer_mut() else {
            diag::warn("softbuffer buffer_mut 失败");
            return;
        };
        {
            let pixels: &mut [u32] = &mut buffer;
            let mut frame = crate::render::Frame {
                pixels,
                width: width as usize,
                height: height as usize,
            };
            painter.draw(
                &mut frame,
                &self.layout,
                term,
                self.blink.visible(),
                self.focused,
            );
        }
        if let Err(err) = buffer.present() {
            diag::warn(&format!("softbuffer present 失败: {err}"));
        }
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
        if let Err(err) = crate::config::state::save(&crate::config::state::state_path(), &state) {
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
                self.focused = true;
                self.blink.set_enabled(true);
                self.dirty = true;
                Some(LRESULT(0))
            }
            WM_KILLFOCUS => {
                self.focused = false;
                self.blink.set_enabled(false);
                self.dirty = true;
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = wparam.0 as u32;
                let mods = current_modifiers();
                self.handle_keydown(vk, mods);
                Some(LRESULT(0))
            }
            WM_CHAR => {
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
                self.session = None;
                // SAFETY: PostQuitMessage 只投递线程消息。
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
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
    // SAFETY: GetKeyState 只读当前线程的键盘状态。
    unsafe {
        Modifiers {
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
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
        _ => return None,
    };
    Some(key)
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

/// 启动消息循环，返回进程退出码。
pub fn run(config: Config, rect: Option<dpi::Rect>) -> Result<i32> {
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
    let app: &'static mut App = Box::leak(Box::new(App::new(config.clone())));
    let app_ptr: *mut c_void = (app as *mut App).cast();

    let hwnd = window::create(hinstance, &spec, app_ptr)?;
    app.hwnd = hwnd;

    // 顺序很重要：先改父子关系，再设置分层属性。
    // 反过来做的话，`SetLayeredWindowAttributes` 记录的是“顶层窗口”的分层表面，
    // 窗口变成子窗口之后不会再被合成，屏幕上就什么都看不到（实测如此）。
    //
    // 挂到壁纸层。失败只降级为普通窗口——功能少了，但终端照样能用。
    match crate::win::workerw::attach(hwnd) {
        crate::win::workerw::Attach::Attached { workerw, strategy } => {
            diag::info(&format!("已挂到 WorkerW（{strategy}）: {:?}", workerw.0));
        }
        crate::win::workerw::Attach::Fallback { reason } => {
            diag::warn(&format!("挂桌面层失败，降级为普通窗口: {reason}"));
            app.hotkey_warnings
                .push(format!("桌面层挂载失败: {reason}"));
        }
    }

    layered::set_opacity(hwnd, app.config.window.opacity)?;
    app.register_hotkeys(hwnd);
    window::show(hwnd);

    diag::info(&format!("启动完成: {}", window::describe(hwnd)));
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
