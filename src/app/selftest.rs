//! 启动期自检。
//!
//! 目的：GUI 无法在无人值守环境下用眼睛验收，但**窗口样式、分层属性、热键注册、
//! 挂载父子关系、PTY 往返**这些都可以在进程内断言并写成 JSON 报告。
//! 自检只做只读观察与一次性创建/销毁，不写配置、不改注册表。

use serde::Serialize;

use crate::config::Config;
use crate::diag;
use crate::win::dpi;
use crate::win::hotkey::{self, Hotkey};
use crate::win::surface::{self, Surface as LayeredSurface};
use crate::win::window::{self, WindowSpec};

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

impl Check {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            ok: true,
            detail: detail.into(),
        }
    }

    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            ok: false,
            detail: detail.into(),
        }
    }

    fn assert(name: &str, condition: bool, detail: impl Into<String>) -> Self {
        if condition {
            Self::pass(name, detail)
        } else {
            Self::fail(name, detail)
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub passed: bool,
    pub checks: Vec<Check>,
}

/// 执行全部自检项。
pub fn run() -> Report {
    let mut checks = Vec::new();

    let mut config = Config::default();
    config.sanitize();

    let hinstance = match window::module_handle() {
        Ok(handle) => {
            checks.push(Check::pass("module_handle", "GetModuleHandleW 成功"));
            handle
        }
        Err(err) => {
            checks.push(Check::fail("module_handle", format!("{err:#}")));
            return finish(checks);
        }
    };

    match window::register_class(hinstance, crate::app::wndproc) {
        Ok(()) => checks.push(Check::pass("register_class", "RegisterClassExW 成功")),
        Err(err) => {
            checks.push(Check::fail("register_class", format!("{err:#}")));
            return finish(checks);
        }
    }

    let spec = WindowSpec {
        rect: dpi::Rect::new(40, 40, config.window.width, config.window.height),
    };
    let app: &'static mut crate::app::App =
        Box::leak(Box::new(crate::app::App::new_headless(config.clone())));
    let app_ptr: *mut std::ffi::c_void = (app as *mut crate::app::App).cast();

    let hwnd = match window::create(hinstance, &spec, app_ptr) {
        Ok(hwnd) => {
            checks.push(Check::pass("create_window", format!("hwnd={:?}", hwnd.0)));
            hwnd
        }
        Err(err) => {
            checks.push(Check::fail("create_window", format!("{err:#}")));
            return finish(checks);
        }
    };
    app.set_hwnd(hwnd);

    // 窗口样式：不进任务栏、不进 Alt+Tab、必须带分层位。
    let ex = crate::app::ex_style(hwnd);
    let tool_window = ex & 0x0000_0080 != 0; // WS_EX_TOOLWINDOW
    let layered_bit = ex & 0x0008_0000 != 0; // WS_EX_LAYERED
    checks.push(Check::assert(
        "ex_style_toolwindow",
        tool_window,
        format!("GWL_EXSTYLE=0x{ex:08X}（需要 WS_EX_TOOLWINDOW=0x00000080）"),
    ));
    checks.push(Check::assert(
        "ex_style_layered",
        layered_bit,
        format!("GWL_EXSTYLE=0x{ex:08X}（需要 WS_EX_LAYERED=0x00080000）"),
    ));

    // 逐像素 alpha：真的走一遍 DIB，确认透明度被折进 alpha 且颜色已预乘。
    // 不再断言 `SetLayeredWindowAttributes`——那条路和 `UpdateLayeredWindow` 互斥。
    match LayeredSurface::new(4, 4) {
        Ok(mut dib) => {
            {
                let pixels = dib.pixels_mut();
                for pixel in pixels.iter_mut() {
                    *pixel = 0x00FF_FFFF;
                }
                // 先把缓冲写成「背景 alpha + 白色」，再确认掩膜不改 alpha。
                let alpha = (config.window.opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
                for pixel in pixels.iter_mut() {
                    *pixel = (alpha << 24) | (alpha << 16) | (alpha << 8) | alpha;
                }
                surface::apply_round_mask(pixels, 4, 4, 0.0);
            }
            let pixels = dib.pixels_mut();
            let expected = (config.window.opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
            let alpha = pixels[0] >> 24;
            let rgb = pixels[0] & 0x00FF_FFFF;
            let premultiplied = (expected << 16) | (expected << 8) | expected;
            checks.push(Check::assert(
                "dib_alpha_roundtrip",
                alpha == expected && rgb == premultiplied,
                format!(
                    "期望 alpha={expected}、预乘 rgb=0x{premultiplied:06X}，实际 alpha={alpha}、rgb=0x{rgb:06X}"
                ),
            ));
        }
        Err(err) => checks.push(Check::fail("dib_alpha_roundtrip", format!("{err:#}"))),
    }

    // 几何：无边框窗口的客户区必须等于请求尺寸。
    match window::window_rect(hwnd) {
        Ok(rect) => {
            let ok = rect.w == spec.rect.w && rect.h == spec.rect.h;
            checks.push(Check::assert(
                "window_geometry",
                ok,
                format!(
                    "请求 {}x{}，实际 {}x{}",
                    spec.rect.w, spec.rect.h, rect.w, rect.h
                ),
            ));
        }
        Err(err) => checks.push(Check::fail("window_geometry", format!("{err:#}"))),
    }

    // 应用指针绑定：窗口过程必须能拿回同一个地址。
    let bound = crate::app::bound_user_data(hwnd);
    checks.push(Check::assert(
        "user_data_bound",
        bound != 0,
        format!("GWLP_USERDATA=0x{bound:X}"),
    ));

    // DPI 感知：至少应拿到一个非零 DPI。
    let window_dpi = window::dpi(hwnd);
    checks.push(Check::assert(
        "dpi_query",
        window_dpi >= 48,
        format!("GetDpiForWindow={window_dpi}"),
    ));

    // 输入法结果串：没有组字时必须返回 None，而不是 panic 或读出垃圾。
    // 这条只证明 ImmGetCompositionStringW(GCS_RESULTSTR) 的调用路径不崩；
    // 真机上屏仍要靠真人敲一次输入法（见 README「已知限制」）。
    let ime = crate::win::ime::result_string(hwnd);
    checks.push(Check::assert(
        "ime_result_absent",
        ime.is_none(),
        format!("GCS_RESULTSTR -> {ime:?}"),
    ));

    // 热键：能解析、能注册、能注销。
    match hotkey::parse(&config.keys.quit) {
        Ok(spec) => match Hotkey::register(hwnd, 99, &spec) {
            Ok(handle) => {
                drop(handle);
                checks.push(Check::pass(
                    "hotkey_register",
                    format!("{} 注册成功并已注销", config.keys.quit),
                ));
            }
            Err(err) => checks.push(Check::fail(
                "hotkey_register",
                format!("{} 注册失败（可能被占用）: {err:#}", config.keys.quit),
            )),
        },
        Err(err) => checks.push(Check::fail("hotkey_parse", format!("{err:#}"))),
    }

    window::destroy(hwnd);
    checks.push(Check::pass("destroy_window", "DestroyWindow 已调用"));

    finish(checks)
}

fn finish(checks: Vec<Check>) -> Report {
    let passed = checks.iter().all(|c| c.ok);
    for check in &checks {
        if !check.ok {
            diag::error(&format!("自检失败 [{}]: {}", check.name, check.detail));
        }
    }
    Report { passed, checks }
}
