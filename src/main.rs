#![windows_subsystem = "windows"]
//! TLShell 入口。
//!
//! 因为是 windows 子系统，stdout/stderr 在正常启动时不可见，
//! 所以一切诊断走 `diag`（落盘）与 `--selftest`（写 JSON 报告）。

mod app;
mod config;
mod diag;
mod render;
mod term;
mod win;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use config::Config;
use win::dpi;

/// 窗口最小尺寸（逻辑像素），与 `app` 里的 `WM_GETMINMAXINFO` 保持一致。
const MIN_WIDTH: i32 = 480;
const MIN_HEIGHT: i32 = 240;

/// 命令行选项。刻意保持极小：这不是一个需要子命令的工具。
#[derive(Debug, Default)]
struct Options {
    selftest: bool,
    report: Option<PathBuf>,
    quit: bool,
    /// 当前以管理员权限运行时，用非提权方式重启自己。
    restart_unelevated: bool,
    config_path: Option<PathBuf>,
    /// 忽略 `state.json`，回到配置里的默认位置。
    reset_position: bool,
    /// 真机输入自检：启动后由本进程自己注入一次点击与一次按键。
    probe_input: bool,
    /// `--probe-real`：用 `SendInput` 产生真实鼠标/键盘事件（带前台守卫）。
    probe_real: bool,
    /// 把首帧绘制面落盘成 BMP（视觉验收取证用）。
    dump_frame: Option<std::path::PathBuf>,
    /// 落盘时的合成底色（`RRGGBB`）。白底是最差条件，黑底是另一种极端。
    dump_bg: Option<u32>,
    /// 启动即打开设置面板：给视觉取证拍面板截图用，正常运行不会带。
    open_settings: bool,
    /// UI 自动化测试模式：跳单实例锁、强制不沉底、不自动退出。
    /// 只给 `scripts/verify-*.ps1` 用；正常启动不会带这个参数。
    ui_test: bool,
    /// --autostart on|off|status：读写 HKCU\...\Run，不启动界面。
    autostart: Option<String>,
}

/// 用法说明。`--help` 走 stdout、退出码 0：它不是错误，不该弹模态框。
fn usage() -> &'static str {
    "用法: TLShell.exe [选项]

  （无参数）                启动终端
  --config <PATH>           使用指定配置文件（保存也写回这份）
  --reset-position          忘掉记住的位置，回到配置里的坐标
  --quit                    让已有实例退出
  --restart-unelevated      以管理员运行时，经 explorer 降权重启
  --autostart on|off|status 读写 HKCU 的 Run 项（随 Windows 启动）
  --open-settings           启动即展开设置面板
  --selftest [--report PATH] 无界面自检（可写 JSON 报告）
  --probe-input             合成消息自检：点击/拖选/ASCII/CJK/emoji
  --probe-real              真实 SendInput 自检（结束归还前台）
  --dump-frame <PATH>       首帧落盘为 24 位 BMP
  --dump-bg <RRGGBB>        落盘时的合成底色（默认白）
  --ui-test                 验收脚本用：跳单实例锁、不沉底、不自动退出
  --help, -h                打印这份说明"
}

fn main() -> ExitCode {
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--help" || arg == "-h")
    {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => return fail(&format!("参数错误: {err:#}")),
    };

    diag::init();

    // 环境前置校验（安全评审 F-03 / F-05）：配置目录与用户目录拿不到时显式失败。
    // 从前会退化到「当前目录」，那会让程序在被启动的任意目录里写配置和状态文件。
    if !options.selftest
        && !options.quit
        && !options.restart_unelevated
        && options.autostart.is_none()
    {
        if let Err(err) = preflight_environment() {
            return fail(&format!("运行环境不可用: {err:#}"));
        }
    }

    // 单实例锁必须活到进程结束，所以绑在 main 里而不是 run_normal 里。
    // 自检类运行（`--selftest` / `--probe-*` / `--dump-frame`）是独立的一次性诊断，
    // 不参与单实例互斥：否则用户开着终端时，验证跑不起来，只能拿到「已有实例」。
    let diagnostic_only = options.selftest
        || options.quit
        || options.restart_unelevated
        || options.autostart.is_some()
        || options.probe_input
        || options.probe_real
        || options.dump_frame.is_some()
        || options.ui_test;
    let _instance = if diagnostic_only {
        None
    } else {
        match win::single::ensure_single() {
            Ok(lock) => lock,
            Err(err) => return fail(&format!("单实例检测失败: {err:#}")),
        }
    };
    if _instance.is_none() && !diagnostic_only {
        // 已有实例在跑：唤醒它之后安静退出，不算错误。
        return ExitCode::SUCCESS;
    }

    let result = if options.selftest {
        run_selftest(&options)
    } else if options.restart_unelevated {
        run_restart_unelevated()
    } else if let Some(action) = options.autostart.as_deref() {
        // 命令行式用法不弹模态框：无人值守调用会挂死在 MessageBox 上。
        match run_autostart(action) {
            Ok(code) => return ExitCode::from(code),
            Err(err) => {
                diag::error(&format!("--autostart 失败: {err:#}"));
                eprintln!("--autostart 失败: {err:#}");
                return ExitCode::FAILURE;
            }
        }
    } else if options.quit {
        run_quit_request()
    } else {
        run_normal(&options)
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(err) => fail(&format!("{err:#}")),
    }
}

/// 启动前的环境前置校验：缺一个就明确失败，不做任何退化。
fn preflight_environment() -> Result<()> {
    config::config_dir()?;
    let has_profile = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .is_some();
    if !has_profile {
        anyhow::bail!("USERPROFILE 缺失：无法确定 shell 的启动目录");
    }
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    diag::error(message);
    // 无控制台时用弹窗保证用户能看到致命错误，而不是静默什么都不发生。
    show_fatal(message);
    ExitCode::FAILURE
}

fn show_fatal(message: &str) {
    // 无人值守环境（测试执行器、CI）不能弹窗：模态对话框会一直挂在那里，
    // 既挡路又让"启动失败"看起来像"启动成功还在运行"。
    // 这个变量只抑制弹窗，退出码与日志照旧。
    if std::env::var_os("TLSHELL_NO_MODAL").is_some() {
        eprintln!("TLShell 启动失败: {message}");
        return;
    }
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text = HSTRING::from(message);
    let caption = HSTRING::from("TLShell 启动失败");
    // SAFETY: 只读调用，不持有任何指针。
    unsafe {
        let _ = MessageBoxW(None, &text, &caption, MB_OK | MB_ICONERROR);
    }
}

/// `--autostart on|off|status`：只改用户级注册表项，不启动界面。
///
/// 用「读回来」判定成败：`reg.exe` 的退出码在值已存在时可能仍是 0，
/// 而用户真正在意的是「下次开机窗口还在不在」。
fn run_autostart(action: &str) -> Result<u8> {
    match action {
        "on" => {
            let exe = std::env::current_exe().context("取不到当前可执行文件路径")?;
            if !win::autostart::enable(&exe) {
                anyhow::bail!("写入 {} 失败", win::autostart::RUN_KEY);
            }
        }
        "off" => {
            if !win::autostart::disable() {
                anyhow::bail!("删除 {} 的值失败", win::autostart::RUN_KEY);
            }
        }
        "status" => {}
        other => anyhow::bail!("--autostart 只接受 on|off|status，收到 {other}"),
    }
    println!("{}", win::autostart::describe());
    Ok(0)
}

fn run_normal(options: &Options) -> Result<u8> {
    let loaded = load_config(options);
    for warning in &loaded.warnings {
        diag::warn(warning);
    }
    if loaded.created {
        diag::info(&format!("已生成默认配置: {}", loaded.path.display()));
    }
    let config = loaded.config;

    let rect = resolve_rect(&config, options.reset_position);
    let code = app::run(app::RunOptions {
        config,
        rect: Some(rect),
        input_probe: options.probe_input,
        real_probe: options.probe_real,
        dump_frame: options.dump_frame.clone(),
        dump_bg: options.dump_bg,
        config_path: options.config_path.clone(),
        open_settings: options.open_settings,
        ui_test: options.ui_test,
    })?;
    Ok(code.clamp(0, 255) as u8)
}

fn run_quit_request() -> Result<u8> {
    if crate::win::single::request_quit() {
        diag::info("--quit：已请求已有实例退出");
    } else {
        // 没有实例不算失败：脚本里 `--quit` 常用来做「先关掉再说」。
        diag::warn("--quit：没有找到正在运行的实例（或它不是本程序）");
    }
    Ok(0)
}

/// 以管理员权限运行时，改用非提权方式重新启动自己。
///
/// 做法是让 `explorer.exe` 代启：explorer 跑在普通完整性级别，它拉起来的
/// 子进程继承中完整性，于是得到一个不带管理员令牌的 TLShell。
fn run_restart_unelevated() -> Result<u8> {
    if !crate::win::elevation::is_elevated().unwrap_or(false) {
        diag::info("--restart-unelevated：当前不是管理员，无需降权重启");
        return Ok(0);
    }
    let exe = std::env::current_exe()?;
    diag::info(&format!("--restart-unelevated：准备重启 {}", exe.display()));
    // 先让旧实例退出，否则新进程会被单实例锁挡住。
    let _ = crate::win::single::request_quit();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let explorer = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("explorer.exe");
    match std::process::Command::new(&explorer).arg(&exe).spawn() {
        Ok(_) => {
            diag::info("--restart-unelevated：已经用非提权方式重新启动");
            Ok(0)
        }
        Err(err) => Err(anyhow::anyhow!(
            "无法通过 {} 重启: {err}",
            explorer.display()
        )),
    }
}

/// M0 阶段只有默认值；`config.toml` 的加载与校验在 M3 接入。
fn load_config(options: &Options) -> config::Loaded {
    config::load(options.config_path.as_deref())
}

/// 决定窗口初始矩形：优先上次落盘的位置，否则用配置里的默认值；
/// 无论来源，最终都要钳进显示器工作区，否则断电拔掉副屏之后窗口会跑到看不见的地方。
fn resolve_rect(config: &Config, reset_position: bool) -> dpi::Rect {
    let mut rect = dpi::Rect::new(
        config.window.x,
        config.window.y,
        config.window.width,
        config.window.height,
    );

    if !reset_position {
        if let Some(path) = config::state::state_path() {
            if let Some(state) = config::state::load(&path) {
                rect = state.window.to_rect();
                diag::info(&format!(
                    "恢复上次位置: {}x{}@{},{}",
                    rect.w, rect.h, rect.x, rect.y
                ));
            }
        }
    }

    let work = dpi::work_area_for_point(rect.x, rect.y).unwrap_or_else(dpi::primary_work_area);
    let clamped = dpi::clamp_rect(rect, work, MIN_WIDTH, MIN_HEIGHT);
    if clamped != rect {
        diag::warn(&format!(
            "窗口矩形 {rect:?} 超出显示器工作区 {work:?}，已钳回 {clamped:?}"
        ));
    }
    clamped
}

fn run_selftest(options: &Options) -> Result<u8> {
    let report_path = options
        .report
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("tlshell-selftest.json"));

    let outcome = app::selftest::run();
    let json = serde_json::to_string_pretty(&outcome).context("自检报告序列化失败")?;
    std::fs::write(&report_path, &json)
        .with_context(|| format!("写入自检报告失败: {}", report_path.display()))?;

    diag::info(&format!("自检报告: {}", report_path.display()));
    Ok(if outcome.passed { 0 } else { 1 })
}

fn parse_args<I: Iterator<Item = String>>(args: I) -> Result<Options> {
    let mut options = Options::default();
    let mut pending: Vec<String> = args.collect();
    let mut index = 0;

    while index < pending.len() {
        let arg = pending[index].clone();
        match arg.as_str() {
            "--selftest" => options.selftest = true,
            "--probe-input" => options.probe_input = true,
            "--probe-real" => options.probe_real = true,
            "--open-settings" => options.open_settings = true,
            "--ui-test" => options.ui_test = true,
            "--dump-frame" => {
                index += 1;
                let value = pending
                    .get(index)
                    .cloned()
                    .context("--dump-frame 需要一个路径参数")?;
                options.dump_frame = Some(PathBuf::from(value));
            }
            "--dump-bg" => {
                index += 1;
                let value = pending
                    .get(index)
                    .cloned()
                    .context("--dump-bg 需要一个 RRGGBB 底色（如 000000）")?;
                options.dump_bg = Some(
                    crate::win::window::parse_rgb(&value)
                        .with_context(|| format!("--dump-bg 不是 RRGGBB：{value}"))?,
                );
            }
            "--autostart" => {
                index += 1;
                let value = pending
                    .get(index)
                    .cloned()
                    .context("--autostart 需要 on|off|status")?;
                options.autostart = Some(value);
            }
            "--quit" => options.quit = true,
            "--restart-unelevated" => options.restart_unelevated = true,
            "--reset-position" => options.reset_position = true,
            "--report" => {
                index += 1;
                let value = pending
                    .get(index)
                    .cloned()
                    .context("--report 需要一个路径参数")?;
                options.report = Some(PathBuf::from(value));
            }
            "--config" => {
                index += 1;
                let value = pending
                    .get(index)
                    .cloned()
                    .context("--config 需要一个路径参数")?;
                options.config_path = Some(PathBuf::from(value));
            }
            // `main` 在解析之前就拦下 `--help`，这里只防御直接调用 parse_args 的场合。
            "--help" | "-h" => {
                pending.clear();
                return Ok(options);
            }
            other => {
                return Err(anyhow::anyhow!("未知参数 `{other}`"));
            }
        }
        index += 1;
    }

    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(items: &[&str]) -> Result<Options> {
        parse_args(items.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn defaults_are_all_off() {
        let options = parse(&[]).unwrap();
        assert!(!options.selftest);
        assert!(!options.quit);
        assert!(options.config_path.is_none());
    }

    #[test]
    fn parses_selftest_with_report() {
        let options = parse(&["--selftest", "--report", "out.json"]).unwrap();
        assert!(options.selftest);
        assert_eq!(options.report.unwrap().to_string_lossy(), "out.json");
    }

    #[test]
    fn parses_config_path() {
        let options = parse(&["--config", "D:/x.toml"]).unwrap();
        assert_eq!(options.config_path.unwrap().to_string_lossy(), "D:/x.toml");
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn rejects_missing_value() {
        assert!(parse(&["--report"]).is_err());
        assert!(parse(&["--config"]).is_err());
    }
}
