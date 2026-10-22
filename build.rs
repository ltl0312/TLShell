//! 把 `TLShell.manifest` 嵌入可执行文件，声明 PerMonitorV2 DPI 感知。
//!
//! 说明：只在 MSVC 目标上生效（link.exe 的 `/MANIFESTINPUT`）。
//! 即使嵌入失败，运行时也会调用 `SetProcessDpiAwarenessContext` 兜底，
//! 所以这里**不让构建失败**——manifest 是锦上添花，不是硬依赖。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=TLShell.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    let target = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target != "msvc" {
        return;
    }

    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("TLShell.manifest");
    if !manifest.exists() {
        println!("cargo:warning=TLShell.manifest 缺失，跳过清单嵌入");
        return;
    }

    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
