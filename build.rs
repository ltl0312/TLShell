//! 把 `TLShell.manifest` 嵌入可执行文件，声明 PerMonitorV2 DPI 感知。
//!
//! 说明：只在 MSVC 目标上生效（link.exe 的 `/MANIFESTINPUT`）。
//! 清单文件缺失属于构建错误：清单决定 DPI 感知与 `asInvoker` 提权级别，
//! 静默跳过会让可执行文件在真机上降级，而构建仍然"成功"。

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
        // 早期是 warning，结果清单没进 exe 也没人发现：DPI 感知就此静默降级。
        // 现在缺失即失败——清单是硬依赖，不是锦上添花。
        panic!(
            "TLShell.manifest 缺失，无法嵌入 DPI 感知清单: {}",
            manifest.display()
        );
    }

    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
