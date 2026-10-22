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
    config_path: Option<PathBuf>,
    /// 忽略 `state.json`，回到配置里的默认位置。
    reset_position: bool,
}

fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => return fail(&format!("参数错误: {err:#}")),
    };

    diag::init();

    // 单实例锁必须活到进程结束，所以绑在 main 里而不是 run_normal 里。
    let _instance = if options.selftest || options.quit {
        None
    } else {
        match win::single::ensure_single() {
            Ok(lock) => lock,
            Err(err) => return fail(&format!("单实例检测失败: {err:#}")),
        }
    };
    if _instance.is_none() && !options.selftest && !options.quit {
        // 已有实例在跑：唤醒它之后安静退出，不算错误。
        return ExitCode::SUCCESS;
    }

    let result = if options.selftest {
        run_selftest(&options)
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

fn fail(message: &str) -> ExitCode {
    diag::error(message);
    // 无控制台时用弹窗保证用户能看到致命错误，而不是静默什么都不发生。
    show_fatal(message);
    ExitCode::FAILURE
}

fn show_fatal(message: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text = HSTRING::from(message);
    let caption = HSTRING::from("TLShell 启动失败");
    // SAFETY: 只读调用，不持有任何指针。
    unsafe {
        let _ = MessageBoxW(None, &text, &caption, MB_OK | MB_ICONERROR);
    }
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
    let code = app::run(config, Some(rect))?;
    Ok(code.clamp(0, 255) as u8)
}

fn run_quit_request() -> Result<u8> {
    diag::info("--quit：请求已有实例退出");
    // 单实例握手在 M5 接入；当前直接以成功码退出，避免脚本误判。
    Ok(0)
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
        let path = config::state::state_path();
        if let Some(state) = config::state::load(&path) {
            rect = state.window.to_rect();
            diag::info(&format!(
                "恢复上次位置: {}x{}@{},{}",
                rect.w, rect.h, rect.x, rect.y
            ));
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
            "--quit" => options.quit = true,
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
            "--help" | "-h" => {
                pending.clear();
                return Err(anyhow::anyhow!(
                    "用法: TLShell.exe [--config <PATH>] [--quit] [--selftest [--report <PATH>]]"
                ));
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
