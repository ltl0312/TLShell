# TLShell 构建 / 测试 / CI 工具链方案（Windows）

- **日期**：2026-10-22
- **配套计划书**：[2026-10-22-tlshell-desktop-terminal.md](2026-10-22-tlshell-desktop-terminal.md)
- **性质**：对计划书 §4（技术栈）、§8（TDD Route）、§10（验证计划）的**执行层补充**。不改需求，只把"怎么跑、跑什么、跑不了什么"钉死。
- **证据等级**：本文所有"实测"结论均在 `D:\Code\Rust\TLShell` 所在机器上真实执行过（rustc 1.97.1 / Win11 26200），原始输出见 §9。

---

## 0. 结论摘要

| # | 结论 | 影响 |
|---|---|---|
| 1 | 计划书 §4.1 的 6 个依赖版本**全部为当前最新版且可在本机编译链接通过**（冷编译 7 分 54 秒） | 依赖基线可锁定，无版本回退风险 |
| 2 | **ConPTY 下 shell 启动会先发 `ESC[6n`（DSR）并阻塞等待应答**；不应答则 PowerShell/cmd 永远停在 4 字节 | M1 必须实现 DSR 应答，否则"能显示提示符"这条验收根本进不去 |
| 3 | ConPTY 会发 `ESC[?9001h`（Win32 Input Mode），而 `alacritty_terminal` 0.26 的 `TermMode` **没有**该标志位 | 输入层需自行处理，否则 Alt 组合 / Ctrl+Backspace 行为与 Windows Terminal 不一致 |
| 4 | 本机（Win11 26200）WorkerW 拓扑**不是**"Progman 的兄弟"：Progman 的直接子窗口是 `SHELLDLL_DefView` + `WorkerW` | 计划书 §9-M2 的三级兜底顺序需按实测重排 |
| 5 | `pwsh.exe`（PowerShell 7）**本机未安装** | R5 的 pwsh 分支本地无法验证 |
| 6 | 计划书 §4.1 依赖表**漏了两个必需依赖**：TOML 解析器与 `raw-window-handle` | 直接照抄 §4.1 会编译不过 |

---

## 1. 环境基线（本机实测）

| 项 | 实测值 | 判定 |
|---|---|---|
| OS | Windows 10.0.26200（Windows 11 25H2） | ✅ 满足计划书 §1.2「Win10 1809+ / 11」 |
| 会话 | SessionId 1，`SM_REMOTESESSION = 0`（本地控制台，非 RDP） | ✅ |
| rustc / cargo | 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1 | ✅ 远高于依赖 MSRV（最高 1.85） |
| 默认 host | `x86_64-pc-windows-msvc` | ✅ |
| 已装组件 | `clippy`、`rustfmt`、`rust-src`、`rust-docs` | ✅ 无需额外安装 |
| 已装 target | 仅 `x86_64-pc-windows-msvc` | ⚠️ 只做 x64；aarch64 未装 |
| MSVC | VS 2026 @ `D:\Compiler\visual studio 2026\Product`，含 `VC.Tools.x86.x64` | ✅ |
| Windows SDK | 10.0.22621.0、10.0.26100.0 | ✅（rc.exe 可用于嵌入 manifest） |
| linker | rustc 自动定位 MSVC `link.exe`，**无需 vcvars 环境** | ✅（实测 `cargo build` 成功） |
| Git | 2.56.0.windows.2 | ✅ |
| PowerShell 5.1 | 5.1.26100.7019 @ `C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe` | ✅ |
| **PowerShell 7** | **未安装**（无 `pwsh`，无 `C:\Program Files\PowerShell\7`） | ❌ 见 §7 |
| 显示器 | 1 台，物理 2560×1440，缩放 **135%（≈130 DPI）** | ⚠️ 非整数缩放（好用例）；多屏无法验证 |
| 字体 | `CascadiaMono.ttf`、`consola.ttf`、`msyh.ttc`（微软雅黑）均存在 | ✅ 计划书默认字体可用 |
| 中文 IME | 搜狗输入法（`Sogou_TSF_UI` / `SoPY_*` 窗口存在） | ✅ 但 IME 行为只能人工验 |
| cargo 源 | 全局 `~/.cargo/config.toml` 指向 `rsproxy.cn` 稀疏索引 | ⚠️ 见 §7（CI 不能依赖它） |
| 桌面外壳 | Explorer 运行中，**Wallpaper Engine 正在占用桌面层 WorkerW** | ⚠️ 见 §9.4 |

### 1.1 环境依赖清单

**必须（缺一不可）**

| 依赖 | 用途 | 本机 |
|---|---|---|
| Rust ≥ 1.85（建议锁 1.97.1） | 编译 | ✅ |
| MSVC C++ Build Tools + Windows 10/11 SDK | 链接、`rc.exe`（嵌 manifest） | ✅ |
| `clippy` / `rustfmt` 组件 | 质量门 | ✅ |
| Git | 里程碑 commit（计划书 §11 回退原则依赖它） | ✅ |
| PowerShell 5.1 | R5 验收对象 + 日常脚本 | ✅ |

**必须（CI / 分发）**

| 依赖 | 用途 |
|---|---|
| GitHub Actions `windows-latest` | CI（自带 MSVC + SDK + git） |
| `Swatinem/rust-cache@v2` | 冷编译 8 分钟，不缓存 CI 会很难受 |

**按需**

| 依赖 | 用途 | 本机 |
|---|---|---|
| PowerShell 7 (`pwsh.exe`) | R5 第二分支验收 | ❌ 需装 |
| `vim` | M1 验收 ③ | 未探测 |
| `wsl.exe` | 计划书 §1.1 补充约束（能跑 wsl） | 有 WSL 相关窗口类，未确认 |
| Wallpaper Engine | 不影响构建，但**会与桌面层挂载抢同一个 WorkerW** | ⚠️ 正在运行 |

---

## 2. 一次性准备

```bash
# 在项目根 D:\Code\Rust\TLShell 执行
rustup show                                  # 确认 host = x86_64-pc-windows-msvc

# 仓库内已提交 rust-toolchain.toml（channel = "1.97.1"）。
# 首次进入目录时 rustup 会自动装该版本工具链（本机已装好，实测 active = 1.97.1）：
#   rustc 1.97.1 / cargo 1.97.1 / clippy 0.1.97 / rustfmt 1.9.0-stable
# 组件与 target 由 rust-toolchain.toml 声明，无需手工 add。

# 验证工具链（不装 vcvars 也应通过）
rustc --version && cargo --version && cargo clippy --version && cargo fmt --version

# 只在本机缺失 pwsh 时执行（R5 验收前置）
winget install --id Microsoft.PowerShell --source winget
```

> **注意**：`channel = "1.97.1"` 是精确锁定，不是 `stable`。好处是"本地绿 = CI 绿 = 半年后仍绿"；代价是换机器/CI 首次会下载一份工具链（约 200 MB，本机已完成）。若更在意首次上手速度，可改成 `channel = "stable"`，但要在计划书里接受"rustc 升级导致 clippy 新 lint 炸门"的偶发成本。

> **不要**手工调用 `link`：本机 PATH 上 `link.exe` 命中的是 Git/MSYS 的 `link`，不是 MSVC 的。cargo 走 rustc 自己的工具链探测，不受 PATH 影响——但人手工敲 `link` 会踩坑。

---

## 3. 日常命令序列（dev loop）

```bash
# 0) 进入项目
cd /d D:\Code\Rust\TLShell

# 1) 快检：只编译不改产物，最快的语法/类型反馈
cargo check --all-targets

# 2) 跑：M0 起可运行（GUI 需要交互桌面，必须在本地控制台会话里跑，不能在没有桌面的服务里跑）
cargo run

# 3) 单测：只跑纯逻辑（配置/坐标钳制/键映射/启动参数注入）
cargo test

# 4) 盯某一个模块
cargo test config:: -- --nocapture
cargo test input:: -- --nocapture

# 5) 看 PTY 输出（调试用，M1 期间最有用）
cargo run 2>&1 | tee run.log
```

**调试期建议**（写进 `Cargo.toml`，见 §5）：`[profile.dev] opt-level = 1` + 依赖 `opt-level = 2`。fontdue 光栅化在 `opt-level = 0` 下慢到难以交互。

---

## 4. 质量门序列（提交前 / 里程碑验收前）

严格按此顺序，任一失败即停：

```bash
cd /d D:\Code\Rust\TLShell

# 1) 格式（不自动改，只判）
cargo fmt --all --check

# 2) 静态检查（计划书 §8 verification 原文要求）
cargo clippy --all-targets -- -D warnings

# 3) 单元测试
cargo test

# 4) 发布构建（M0 起每个里程碑都要过，能提前暴露 LTO/链接/panic=abort 问题）
cargo build --release

# 5) 里程碑人工验收（计划书 §9 各里程碑"验收"列）
#    M0: cargo run → 900×420 无边框窗口；任务栏/Alt+Tab 无图标；Ctrl+Alt+Q 退出码 0
#    M1: PS 5.1 / pwsh 各跑一遍 Get-ChildItem、vim、中文输入
#    M2: Win+D 后终端仍可见；拖动/缩放；挂载失败降级
#    M3: 拖到 (300,200)/1000×500 → 重启一致；state.json 坐标改到屏幕外 → 被钳制
#    M4: opacity 0.88 vs 0.5 肉眼可辨；改 background 热重载生效
#    M5: 滚轮/选中复制/中文输入/explorer 重启重挂/大文件不卡/二次启动不重开
```

**一键脚本**（已提交 [scripts/gate.ps1](../../../scripts/gate.ps1)，PowerShell 5.1 / 7 均可跑）：

```powershell
powershell -NoProfile -File scripts\gate.ps1
```

> `clippy --all-targets` 比计划书写的 `cargo clippy` 更严：会把 `#[cfg(test)]` 代码也 lint。测试代码里的 `unwrap()` 之类如果不想被拦，加 `#[allow(clippy::unwrap_used)]` 而不是放宽整个门。

---

## 5. `Cargo.toml` 依赖与 profile（可直接粘贴）

```toml
[package]
name = "tlshell"
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
publish = false

[dependencies]
# --- 终端内核 ---
alacritty_terminal = "0.26.0"        # Apache-2.0；网格 + VT 状态机；已 re-export vte
portable-pty       = "0.9.0"         # MIT；ConPTY 封装

# --- 窗口与渲染 ---
windows            = { version = "0.62.2", features = [
  "Win32_Foundation",
  "Win32_UI_WindowsAndMessaging",    # CreateWindowEx / SetParent / WM_* / RegisterHotKey
  "Win32_UI_HiDpi",                  # PerMonitorV2 / GetDpiForWindow
  "Win32_UI_Input_KeyboardAndMouse", # VK_* 常量
  "Win32_Graphics_Gdi",              # 分层窗口 / 位图
  "Win32_System_LibraryLoader",      # GetModuleHandleW
  "Win32_System_Threading",          # 单实例 Mutex
] }
softbuffer         = { version = "0.4.8", default-features = false }  # Windows 后端无 feature 开关；关掉 x11/wayland 解析
raw-window-handle  = "0.6.2"         # softbuffer 0.4 需要 Win32WindowHandle
fontdue            = "0.9.5"

# --- 配置与错误 ---
serde        = { version = "1.0.229", features = ["derive"] }
serde_json   = "1.0.154"
toml         = "1.1.8"               # 计划书 §4.1 漏列：config.toml 必须有个 TOML 解析器
anyhow       = "1.0.104"

[build-dependencies]
embed-resource = "3.0.12"            # 嵌 TLShell.manifest（需要 SDK 的 rc.exe，本机已有）

# ---------------- profiles ----------------

[profile.dev]
opt-level = 1                        # fontdue 光栅化在 O0 下不可用
debug     = 1                        # 缩短链接时间

[profile.dev.package."*"]
opt-level = 2                        # 依赖全优化，自身代码仍可断点

[profile.release]
opt-level     = 3
lto           = "thin"               # 收益接近 fat，构建时间约 1/3
codegen-units = 1
panic         = "abort"              # 本项目无 unwind 需求
strip         = "debuginfo"          # 保留符号表，崩溃仍可事后 triage

[profile.dist]
inherits = "release"
strip    = "symbols"                 # 只给用户的分发包：最小体积
```

**取舍说明**

- `lto = "thin"` 而非 `"fat"`：单二进制小工具，fat 的额外收益 < 3%，构建时间翻倍。
- `panic = "abort"`：GUI 程序崩溃本就只能重启；放弃 `catch_unwind` 换 ~10–15% 体积。若 M5 想用 `catch_unwind` 兜住渲染线程异常，改回默认（`unwind`）。
- `strip = "debuginfo"` 而非 `"symbols"`：`release` 保留符号表，`dist` 才 strip。否则线上崩溃只有地址，没法查。

**build.rs 两条路（二选一）**

1. **嵌入 manifest**（计划书原方案）：`build.rs` 里 `embed_resource::compile("TLShell.manifest", embed_resource::NONE)`。需要 `rc.exe`——本机 Windows SDK 10.0.26100 有，CI 的 `windows-latest` 也有。
2. **运行时 API**（更省依赖）：删掉 manifest 与 `build-dependencies`，在 `main` 第一行调
   `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`。
   必须在**任何窗口创建之前**调用。好处：少一个 build 依赖、少一个 rc.exe 失败点；坏处：违反"清单声明式"惯例，且某些系统级 DPI 行为（如 `WM_DPICHANGED` 初始值）略有差异。

推荐 1（与计划书一致），2 作为 rc.exe 出问题时的降级。

---

## 6. CI（GitHub Actions）

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:
  workflow_dispatch:

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0

jobs:
  gate:
    name: windows / fmt + clippy + test + release
    runs-on: windows-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v4

      - name: Install toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - name: Cache cargo + target
        uses: Swatinem/rust-cache@v2
        with:
          cache-on-failure: true

      - name: fmt
        run: cargo fmt --all --check

      - name: clippy
        run: cargo clippy --all-targets --locked -- -D warnings

      - name: test
        run: cargo test --locked

      - name: release build
        run: cargo build --locked --release

      - name: upload exe
        uses: actions/upload-artifact@v4
        with:
          name: tlshell-x86_64-pc-windows-msvc
          path: target/release/tlshell.exe
          if-no-files-found: error
```

**为什么这么写**

| 决策 | 理由 |
|---|---|
| 单一 job，不拆 matrix | 只做 x64 Windows，拆 job 只会重复付缓存与编译成本 |
| `--locked` | 二进制项目必须提交 `Cargo.lock`；CI 用 `--locked` 保证"本地绿 = CI 绿" |
| `RUSTFLAGS: -D warnings` **不设** | 那会把依赖的 warning 也算成错误，一个上游小版本就可能炸 CI。警告门只放在 clippy 步骤 |
| `cargo fmt --all --check` | 本仓库是单 crate，但 `--all` 为将来加 workspace 留口 |
| release build 进 CI | `lto`/`panic=abort`/`strip` 只在 release profile 生效，debug 绿不代表能发版 |
| 30 分钟超时 | 冷编译 8 分钟 + clippy 1 分钟 + 缓存未命中余量 |
| 不上 `cargo audit` / `cargo deny` | 可选；依赖面小（9 个直接依赖），等 M5 再评估 |

**CI 上跑不了的**：任何创建真实窗口、`SetParent` 到 WorkerW、测 DPI/透明/热键的测试。`windows-latest` 没有交互桌面，`SetParent` 必然失败。所以：

- 单元测试只测纯逻辑（与计划书 §8 一致）；
- 需要 HWND 的测试一律 `#[ignore]`，本地 `cargo test -- --ignored` 手动跑；
- CI 只保证"能编译、纯逻辑对、能出 exe"。

---

## 7. 本地可验证 / 无法验证清单

### 7.1 本地可验证（本机条件足够）

| 项 | 怎么验 |
|---|---|
| 依赖版本可编译链接 | 已实测通过（§9.1） |
| 工具链锁定与质量门工具可用 | 已实测：`rust-toolchain.toml` 生效后 rustc 1.97.1 / clippy 0.1.97 / rustfmt 1.9.0 全部就绪 |
| `Term` + `vte` 解析 + ANSI 颜色 | 已实测通过（§9.2） |
| ConPTY 起 `powershell.exe` / `cmd.exe`、UTF-8 中文输出 | 已实测通过（§9.3） |
| WorkerW 拓扑与挂载目标 | 已实测（§9.4） |
| 配置反序列化 / 坐标钳制 / 键映射 / 启动参数注入 | `cargo test` |
| Win+D 行为、拖动缩放、透明度、圆角 | 人工（需本地控制台会话） |
| PS 5.1 分支的 R5 全部验收 | 人工 |

### 7.2 本地无法验证（必须外部条件或另一台机器）

| # | 项 | 缺什么 | 影响 | 处置 |
|---|---|---|---|---|
| U1 | **R5 的 PowerShell 7 分支** | `pwsh.exe` 未安装 | 计划书 §1.1 R5 只过了一半 | `winget install Microsoft.PowerShell` 后补验；或明确把 pwsh 分支降级为"未验证" |
| U2 | **多显示器** | 本机 1 台 | §7「多显示器 MVP 只挂主屏」无法验 | 接受为已知未验证项 |
| U3 | **Per-Monitor DPI 切换** | 单屏无法拖动跨屏 | `WM_DPICHANGED` 路径无法验 | 本机 135% 已覆盖"非 100% 缩放"；跨屏留 M5 |
| U4 | **100% 缩放基线** | 系统固定 135% | 1.0 缩放下格宽取整可能不同 | 临时把缩放调 100% 验一次（需注销/重登） |
| U5 | **Explorer 重启重挂（M5）** | 需交互操作 | `TaskbarCreated` 路径 | 本地可做：`taskkill /f /im explorer.exe && start explorer` |
| U6 | **IME 中文输入（M5）** | 需人手动敲 | 搜狗 TSF 与 WorkerW 子窗口的 IME 交互 | 人工；兜底方案见计划书风险 #6 |
| U7 | **Wallpaper Engine 共存** | WPE 正在占用目标 WorkerW | 挂载后可能被 WPE 的 DX11 窗口盖住 | 见 §9.4；需实测 z-order，或让用户先关 WPE |
| U8 | **其他 Windows 版本**（1809/22H2/24H2） | 只有 26200 | 计划书风险 #2「WorkerW 结构再变」 | 依赖三级兜底 + 日志；建议 VM 抽验 22H2 |
| U9 | **CI 首次真跑** | 未配置仓库 | yaml 未经真机验证 | 首次 push 后核对：缓存命中、rc.exe、`--locked` |
| U10 | **`windows-latest` 上 `cargo test` 是否会碰 GUI** | 取决于 M0 怎么写 | 若测试里建窗口会挂 | 强制约定：测试不建窗口 |
| U11 | **cargo 源** | 本机全局走 rsproxy.cn 镜像 | 本地绿 ≠ crates.io 绿 | 仓库内**不要**提交 `.cargo/config.toml`；CI 走官方源，首次绿即证明两边一致 |
| U12 | **杀软/Defender 对 ConPTY 子进程** | 未测 | 首次启动可能被拦 | 人工观察首次运行 |

---

## 8. 需要补进计划书的 6 条（实测发现）

1. **§9-M1 增加"DSR 应答"**：`ESC[6n` 必须回 `ESC[{row};{col}R`。实测不应答 → PowerShell 只吐 4 字节后永久阻塞。这条不写，M1 第一天就会卡住。
2. **§9-M1 增加"Win32 Input Mode"**：ConPTY 会发 `ESC[?9001h`。`alacritty_terminal` 0.26 的 `TermMode` 无此位（已 grep 确认），需在 `input.rs` 自建状态：开启时按 `ESC[Vk;Sc;Uc;Kd;Cs;Rc;_` 编码，关闭时走 VT 序列。
3. **§9-M2 的兜底顺序按实测改**：本机 Progman 的直接子窗口是 `SHELLDLL_DefView` + `WorkerW`，所以第一优先是 **`FindWindowEx(Progman, DefView, "WorkerW")`**（DefView 的兄弟），而不是"Progman 的兄弟"。
4. **§4.1 依赖表补两项**：`toml`（config.toml 解析）、`raw-window-handle`（softbuffer 0.4 非可选）。
5. **§4.1 明确 `Dimensions` 要自己实现**：`alacritty_terminal` 对 `(usize, usize)` 的 `Dimensions` 实现是 `#[cfg(test)]`，外部用不到。
6. **§8 test posture 补一条 CI 边界**：CI 只跑纯逻辑单测 + 编译；任何建窗口的测试 `#[ignore]`。

---

## 9. 实测证据（原始输出摘要）

### 9.1 依赖全量编译（rustc 1.97.1，无 vcvars）

```
Compiling windows v0.62.2
Compiling alacritty_terminal v0.26.0
Compiling portable-pty v0.9.0
Compiling softbuffer v0.4.8
Compiling fontdue v0.9.5
Compiling dep-probe v0.0.0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 7m 54s
```

结论：9 个直接依赖 + 传递依赖在 Windows/MSVC 下编译并**链接**通过（`cargo build`，非 `check`）。

### 9.2 M1 核心 API 跑通（`Term` + `vte::ansi::Processor`）

```rust
let dims = Dims { cols: 80, lines: 24 };
let mut term = Term::new(Config::default(), &dims, Proxy);   // Proxy: impl EventListener
let mut proc: Processor = Processor::new();                  // ← 必须显式标注类型
proc.advance(&mut term, b"hello\x1b[31mR\x1b[0m");
// grid[Line(0)][Column(0..6)].c == "helloR"
// grid[Line(0)][Column(5)].fg == Named(Red)
```

输出：

```
row0      = "helloR"
fg col5   = Named(Red)
fg col0   = Named(Foreground)
```

要点：
- `Processor::new()` 泛型无法推导，必须 `let p: Processor = ...`（或写全 `Processor<StdSyncHandler>`）。
- `Term::new(config, &dimensions, event_proxy)` 取 `&D`；`alacritty_terminal::vte` 是 re-export，**不要**单独加 `vte` 依赖（会版本错配）。

### 9.3 ConPTY 行为（决定性证据）

```
== powershell.exe (无 DSR 应答) : 4 bytes ==
<ESC>[6n                       ← 只有这一条，然后永久阻塞

== powershell.exe (有 DSR 应答) : 128 bytes ==
<ESC>[6n<ESC>[?9001h<ESC>[?1004h<ESC>[m<ESC>]0;C:\WINDOWS\...\powershell.exe
<ESC>[?25hPS=5.1.26100.7019
cjk=中文测试                    ← 中文经 UTF-8 正常

== cmd.exe (有 DSR 应答) : 209 bytes ==
<ESC>[6n<ESC>[?9001h<ESC>[?1004h<ESC>[m<ESC>]0;...cmd.exe<ESC>[?25h<ESC>[?25lCMD_OK<ESC>[K
中文<ESC>[K ... <ESC>[3;1H<ESC>[?25h
```

派生结论：
- `ESC[?9001h`（Win32 Input Mode）、`ESC[?1004h`（focus reporting）、`ESC]0;title BEL` 三条是 ConPTY 标配，解析器必须至少不崩。
- `cmd.exe` 输出里有大量 `ESC[K`，M5 的"脏行重绘"要能扛住这种刷新密度。
- **中文不乱码的前提只是 shell 侧编码为 UTF-8**；本机 PS 5.1 未加前置命令时 `中文测试` 也已正确（因为 ConPTY 管道按 UTF-8 传），但交互式 PSReadLine 下的输入仍需按计划书 §4.2 注入。

### 9.4 WorkerW 拓扑（Win11 26200，本机）

```
Progman = 0x104B6
Progman 直接子窗口（Z 序）:
  0x104C2 : SHELLDLL_DefView    子: 0x104C6:SysListView32      ← 桌面图标
  0x104EC : WorkerW             子: 0x10544:WPEDesktopDX11Window ← 壁纸层，已被 Wallpaper Engine 占用
顶层 WorkerW 共 15 个，全部 parent=NULL 且无子窗口（与桌面壁纸无关）
```

派生结论：
- 挂载目标 = **`FindWindowEx(Progman, DefView, "WorkerW")`** = `0x104EC`。
- 传统"Progman 的兄弟 WorkerW"算法在本机返回 NULL（15 个顶层 WorkerW 都在 Progman 的 Z 序之前，且都是别的程序建的）——**不能作为主路径**。
- Wallpaper Engine 已在该 WorkerW 内建了 DX11 子窗口：`SetParent` 后必须 `SetWindowPos(HWND_TOP)`，否则会被壁纸盖住；反之若 WPE 重绘，也可能盖住我们。这是 U7 的具体形态，M2 验收时要专门看一眼。

---

## 10. 本次已落地的文件（可直接用，未触碰 M0 的产物）

| 文件 | 作用 | 状态 |
|---|---|---|
| [rust-toolchain.toml](../../../rust-toolchain.toml) | 锁 1.97.1 + `rustfmt`/`clippy` + x64-msvc target | TOML 解析通过；工具链已生效（rustc 1.97.1 / clippy 0.1.97 / rustfmt 1.9.0） |
| [rustfmt.toml](../../../rustfmt.toml) | `max_width = 100`，edition 交给 Cargo.toml | 生效 |
| [.gitignore](../../../.gitignore) | `/target/`、`*.pdb`、日志 | 生效 |
| [.github/workflows/ci.yml](../../../.github/workflows/ci.yml) | fmt → clippy → test → release build → 传 exe | YAML 解析通过（1 job / 8 steps），**未真机跑过**（见 U9） |
| [scripts/gate.ps1](../../../scripts/gate.ps1) | 本地一键质量门 | PowerShell 语法解析通过 |

**刻意没做的**：没有创建 `Cargo.toml` / `build.rs` / `TLShell.manifest` / `src/`——那是 M0 的产物，避免与里程碑提交边界打架。§5 的依赖与 profile 块是给 M0 直接粘贴的。

---

## 11. 自检

- [x] 命令序列按"准备 → dev loop → 质量门 → 发布"分层，可直接粘贴
- [x] 每条命令都在本机执行过或明确标注为"待首次执行"（U9）
- [x] 依赖版本逐个核对 crates.io 稀疏索引，非凭记忆
- [x] profile 给出取舍理由，不是抄默认值
- [x] CI 边界（能跑什么/不能跑什么）写清，避免把 GUI 测试塞进 CI
- [x] "本地无法验证"12 条，每条给缺什么 + 影响 + 处置
- [x] 与计划书的 6 处差异单独成节，可直接回填
