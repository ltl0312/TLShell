# TLShell 实现计划 — 桌面层常驻终端

- **日期**：2026-10-22
- **状态**：待执行（未开工）
- **项目根**：`D:\Code\Rust\TLShell\`（已存在，当前为空目录，无 git）
- **Aegis Visibility**：这是全新项目、无既有代码基线，但引入了新的窗口生命周期所有者（WorkerW 挂载）、持久化契约（config/state 双文件）和一条不可回退的交互边界（焦点语义），因此需要一份可执行的实现计划，而不是直接开工。

---

## 0. 一页摘要

TLShell 是一个 **Rust 写的、把自己挂进 Windows 桌面层（WorkerW）的终端模拟器**。它复用 Alacritty 的终端内核（`alacritty_terminal`）和 WezTerm 的 PTY 层（`portable-pty`），自己只负责三件 Alacritty 不做的事：**桌面层挂载、位置持久化、玻璃外观**。

预计 **12–18 个工作日**，分 6 个里程碑，每个里程碑有可判定的验收动作。

---

## 1. 目标与边界

### 1.1 必须做到（这五条就是验收标准）

| # | 需求 | 判定方式 |
|---|---|---|
| R1 | 终端**粘在桌面上**，不属于普通窗口 | 挂载后父窗口是 WorkerW，不是桌面根窗口 |
| R2 | 按 **Win+D 回到桌面，终端依然可见** | Win+D 后截屏能看到终端内容 |
| R3 | **位置与大小固定**，每次开机一致；且**支持手动拖动/改尺寸并记住** | 改位置 → 退出 → 重启 → 位置尺寸一致 |
| R4 | **背景颜色与透明度可配**，壁纸能透出 | 改 `opacity` 与 `background` 配置后肉眼可辨 |
| R5 | **完整支持 PowerShell**：Windows PowerShell 5.1 与 PowerShell 7+ 都能正常显示、输入、中文不乱码 | 两个版本各跑 `Get-ChildItem`、`vim`、中文输入 |

补充约束（沿用前序需求）：
- **不在任务栏显示、不在托盘显示**（因此本项目自身也没有托盘图标）
- 终端能跑 `powershell.exe` / `pwsh.exe` / `cmd.exe` / `wsl.exe` / `vim`，即完整的 VT 能力

### 1.2 明确不做（MVP 边界，防止范围蔓延）

- ❌ 分屏 / 标签页（单窗口单会话；要分屏在终端里跑 zellij）
- ❌ 真·模糊（亚克力/Mica）—— 嵌入 WorkerW 后 DWM 背景特效失效，MVP 只做 alpha
- ❌ 主题系统、插件、脚本扩展
- ❌ 跨平台（只做 Windows 10 1809+ / 11）
- ❌ 托盘图标、设置 GUI（配置走 `config.toml` 手改 + 热重载）

---

## 2. 需求就绪检查

| 检查项 | 结论 |
|---|---|
| 批准来源 | 用户 m00168：「B,给出计划书（TLShell），要求要明确」；m00184 补充：项目根 `D:\Code\Rust\TLShell`、须支持 PowerShell |
| 场景 | 单机、单窗口、单会话、桌面层常驻 |
| 验收证据 | 1.1 表格中的可观察动作，全部可人工执行 |
| 决策性缺口 | 无。字体、默认 shell、快捷键已在本计划中给定默认值，可改配置 |
| 未决项 | 无阻塞项 |

---

## 3. 为什么必须是代码（Change Necessity）

**现成终端没有一个能挂进 WorkerW。** Alacritty / WezTerm / Rio 都是标准顶层窗口：

- Alacritty 有 `window.position`，但只设初始位置，拖动后不保存，且 Windows 上时有失效（[issue #7267](https://github.com/alacritty/alacritty/issues/7267)）；
- 三者都没有 `SetParent(WorkerW)` 的挂载点，也没有「挂载后仍可交互」的窗口生命周期设计；
- 用宿主程序外挂 `SetParent` 到 Alacritty 窗口，会与 winit 的窗口管理冲突（resize / DPI 事件错乱），不可维护。

**最小代码边界**：只写「窗口层 + 渲染层 + 持久化」，VT 解析与 PTY 完全复用现成 crate。代码量集中在 `src/win/`、`src/render/`、`src/term/input.rs`、`src/config/`。

---

## 4. 技术栈

### 4.1 零件来源矩阵（复用什么 / 自己写什么）

**这是本项目的核心决策表：每一行都指明了零件来自哪个上游项目。**

| 上游项目 | 许可证 | 复用的部分 | 不用的部分 | 我们怎么接 |
|---|---|---|---|---|
| **Alacritty** | Apache-2.0 | `alacritty_terminal` crate（0.26.0）：`Term` 网格模型、ANSI/VT 解析状态机、滚动缓冲、选区、`EventListener` 回调 | 它的 winit 窗口层、OpenGL 渲染器、glyph cache、配置系统 | 只当「网格 + 状态机」用，自己把 `Term` 画出来 |
| **Alacritty** | Apache-2.0 | `alacritty/src/input.rs` 的**键盘编码逻辑**（移植源码，不是依赖） | 其中与 winit 耦合的事件匹配代码 | 移植为 `src/term/input.rs`，输入源换成 `WM_KEYDOWN` / `WM_CHAR`；**保留原版权头与 Apache-2.0 声明** |
| **WezTerm** | MIT | `portable-pty` crate（0.9.0）：`PtyPair` / `PtySystem`、Windows ConPTY 封装 | 它的 mux、渲染、Lua 配置 | 起 PTY、拿读写句柄，其余不管 |
| **rust-windowing** | MIT/Apache-2.0 | `softbuffer`（0.4.8）：裸窗口句柄 → 像素缓冲提交 | — | 帧绘制出口 |
| **fontdue** | MIT/Apache-2.0 | 字形解析 + 光栅化为 alpha 位图 | 它的排版能力（我们只需要等宽网格） | 自己算格位、自己 blit |
| **microsoft/windows-rs** | MIT/Apache-2.0 | 全部 Win32 绑定（`windows` 0.62.2） | — | 窗口、WorkerW、分层、热键 |

**完全自己写（约 2350–2550 行）**：

| 模块 | 行数估计 | 为什么不能复用 |
|---|---|---|
| `win/window.rs` — 窗口生命周期 + 消息循环 | ~600 | winit 抽象与 `SetParent` 冲突 |
| `win/window.rs` — 无边框 hit-test / 拖动 / 八向缩放 | ~300 | 无现成 crate |
| `win/workerw.rs` — 桌面层挂载 | ~200 | 无现成 crate |
| `render/*` — 帧绘制 + 字形缓存 | ~700 | Alacritty 渲染器绑 OpenGL，我们走 CPU |
| `term/input.rs` — 键盘编码 | ~300 | 从 Alacritty 移植 + 换输入源 |
| `config/*` — 配置与位置持久化 | ~250 | 项目专属 |

**依赖版本（已核验 2026-10-22）**：

| crate | 版本 | 下载量 |
|---|---|---|
| `alacritty_terminal` | 0.26.0 | 2.17M |
| `portable-pty` | 0.9.0 | 22.7M |
| `windows` | 0.62.2 | 372M |
| `softbuffer` | 0.4.8 | 29.3M |
| `fontdue` | 0.9.5 | 10.9M |
| `serde` / `serde_json` | 1.0.229 / 1.0.154 | — |
| `anyhow` | 1.0.104 | — |

**关键决策：不使用 winit。** 直接用 `windows` 建窗口、手写消息循环。理由：

1. winit 的窗口抽象与 `SetParent` 冲突，DPI/resize 逻辑会打架；
2. 需要自定义 `WM_NCHITTEST`（无边框拖动 + 边缘缩放）、`WM_EXITSIZEMOVE`（位置持久化）、分层窗口；
3. 少一层抽象 = 少一类无法调试的问题。

代价：消息循环、DPI 感知、键盘映射要自己写（约 900 行），但全程可控。

### 4.2 PowerShell 支持（两个版本都要过）

Windows 上「PowerShell」是两个不同程序，必须分别对待：

| | Windows PowerShell 5.1 | PowerShell 7+ |
|---|---|---|
| 可执行文件 | `powershell.exe` | `pwsh.exe` |
| 默认编码 | 跟随系统 ANSI 代码页（中文系统 = GBK / 936） | **UTF-8** |
| ConPTY 下的问题 | 中文输入输出易乱码；PSReadLine 版本较老，光标与刷新偶发异常 | 基本无问题 |
| 处理方式 | 启动参数强制 UTF-8：`-NoLogo -NoExit -Command "[Console]::OutputEncoding=[Console]::InputEncoding=[Text.UTF8Encoding]::new()"` | 直接启动即可 |

**实现要点**：

1. `config.toml` 的 `[shell] program` 可填任一；TLShell 启动时按**可执行文件名**判断是否注入 UTF-8 前置命令（`powershell.exe` → 注入，`pwsh.exe` → 不注入，`cmd.exe` → 注入 `chcp 65001`）。
2. ConPTY 管道输出按 UTF-8 传递（Windows Terminal 依赖此行为），因此**渲染侧固定按 UTF-8 解码**；乱码只会来自 shell 侧编码，由上面的启动参数消除。
3. 两个版本都要在 M1 验收里各跑一遍（见 §9）。

---

## 5. 架构

### 5.1 模块划分

```
D:\Code\Rust\TLShell\
├── Cargo.toml
├── build.rs                    # 嵌入 manifest（PerMonitorV2）
├── TLShell.manifest
├── src/
│   ├── main.rs                 # 入口：单实例、参数、错误出口
│   ├── app.rs                  # 消息循环、窗口生命周期、事件分发
│   ├── win/
│   │   ├── mod.rs
│   │   ├── workerw.rs          # 三级兜底查找 WorkerW
│   │   ├── window.rs           # CreateWindowEx、样式、hit-test、拖动/缩放
│   │   ├── layered.rs          # 透明度（SetLayeredWindowAttributes）
│   │   ├── hotkey.rs           # RegisterHotKey 全局热键
│   │   └── dpi.rs              # PerMonitorV2、坐标换算
│   ├── term/
│   │   ├── mod.rs
│   │   ├── session.rs          # portable-pty 会话 + 读线程
│   │   ├── grid.rs             # alacritty_terminal Term 封装
│   │   └── input.rs            # 键盘事件 → VT 字节序列（移植自 Alacritty）
│   ├── render/
│   │   ├── mod.rs
│   │   ├── painter.rs          # softbuffer 帧绘制
│   │   ├── glyph.rs            # fontdue 字形位图缓存
│   │   └── layout.rs           # 网格坐标 → 像素坐标
│   └── config/
│       ├── mod.rs              # config.toml 加载与校验
│       └── state.rs            # state.json 位置持久化
└── docs/aegis/plans/           # 本文件
```

### 5.2 运行时数据流

```
键盘/IME 事件
    │  input.rs 编码为 VT 字节
    ▼
  PTY 写端 ──► powershell.exe / pwsh.exe / cmd / wsl / vim
                    │
                    │ 输出字节（UTF-8）
                    ▼
              PTY 读线程 ──► vte::Parser ──► alacritty_terminal::Term（网格）
                                                    │
                                            grid.rs 暴露脏行
                                                    ▼
                                        painter.rs 逐字形 blit
                                                    ▼
                                          softbuffer → 窗口
```

### 5.3 关键不变量

1. **窗口父句柄必须是 WorkerW**，否则 R1/R2 不成立。挂载失败时降级为普通窗口并在日志告警。
2. **不加 `WS_EX_NOACTIVATE`**。终端必须能获得焦点才能打字；牺牲「点击不抢焦点」换取可用性（见 §7）。
3. **位置只在 `WM_EXITSIZEMOVE` 时落盘**，拖动过程中不写文件（避免高频 IO）。
4. **PTY 读线程与 UI 线程通过 channel 通信**，不共享可变状态。
5. **单实例**：命名 Mutex 保证；第二次启动只发送「显示/聚焦」信号。

---

## 6. 配置与状态模型

### 6.1 `config.toml`（用户手改）

```toml
[shell]
program = "pwsh.exe"          # 可换 powershell.exe / cmd.exe / wsl.exe / bash.exe
args    = ["-NoLogo"]         # powershell.exe 时 TLShell 会自动补 UTF-8 前置命令

[window]
width         = 900
height        = 420
x             = 40            # 首次启动位置；之后以 state.json 为准
y             = 40
opacity       = 0.88          # 0.0–1.0
padding       = 10
border_radius = 8

[font]
family      = "Cascadia Mono"
size        = 14.0
line_height = 1.25

[colors]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor     = "#f5e0dc"

[keys]
quit = "Ctrl+Alt+Q"
```

### 6.2 `state.json`（程序写，勿手改）

```json
{
  "version": 1,
  "window": { "x": 40, "y": 40, "width": 900, "height": 420 },
  "monitor": { "dpi": 144, "bounds": [0, 0, 2560, 1440] }
}
```

保存 `dpi` 与 `bounds` 是为了：换显示器或改缩放后，按比例还原而不是跑到屏幕外。

**路径**：`%APPDATA%\TLShell\`（config 与 state 同目录）。

---

## 7. 兼容边界与已知限制

| 项 | 结论 | 影响 |
|---|---|---|
| **焦点语义** | Win+D 后焦点丢失，需点击终端才能继续输入 | 系统行为，不可绕过（Rainmeter 输入框同样如此）。写入 README |
| **点击抢焦点** | 点击终端会激活它 | 这是能打字的必要条件；不影响其他程序运行 |
| **多显示器** | Win10+ 每个显示器一个 WorkerW | MVP 只挂主屏，多屏在 M5 评估 |
| **Explorer 重启** | 窗口会脱离 WorkerW | M5 监听 `TaskbarCreated` 消息重挂 |
| **管理员程序** | 目标程序提权时 PTY 仍可工作，挂载不受影响 | 无影响（挂载的是自己的窗口） |
| **全屏游戏** | 桌面被覆盖，终端不可见 | 符合预期 |
| **透明** | 只有 alpha，无模糊 | 见 §1.2 不做项 |
| **退出方式** | 无托盘、无关闭按钮 | 全局热键 `Ctrl+Alt+Q`；或 `TLShell.exe --quit` |

---

## 8. TDD Route

- **mode**：`auto`
- **decision**：`light`
- **authority**：用户未要求 TDD；本项目为绿地，无既有回归面
- **test posture**：
  - **单元测试**（`cargo test`）覆盖纯逻辑：配置反序列化与默认值、几何坐标钳制（防止窗口跑到屏幕外）、键盘事件 → VT 序列映射、shell 启动参数注入（`powershell.exe` / `pwsh.exe` / `cmd.exe` 分支）、WorkerW 候选筛选逻辑（用假 HWND 树测）
  - **手动冒烟**：窗口层、桌面层、渲染层无法自动化，按 §9 各里程碑的验收动作人工执行
- **verification**：`cargo test` + `cargo clippy -- -D warnings` + 里程碑验收表
- **reason**：GUI/系统集成部分写自动化测试的收益低于成本；把可测的纯逻辑抽出来单测，其余靠可判定的验收动作

---

## 9. 任务分解

每个里程碑独立可验收，**按顺序执行**（后者依赖前者的窗口与消息循环）。

### M0 — 骨架与窗口（约 2 天）

| 项 | 内容 |
|---|---|
| 文件 | `Cargo.toml`、`build.rs`、`TLShell.manifest`、`src/main.rs`、`src/app.rs`、`src/win/window.rs`、`src/win/hotkey.rs` |
| 目的 | 建立可运行的空壳窗口，验证消息循环与样式 |
| 最小改动 | `CreateWindowExW` 创建 `WS_POPUP` 窗口；`WS_EX_TOOLWINDOW`；手写 `GetMessage` 循环；`RegisterHotKey(Ctrl+Alt+Q)` 退出 |
| 兼容影响 | 无（新项目） |
| 验收 | ① `cargo run` 出现 900×420 无边框纯色窗口；② 任务栏无图标；③ Alt+Tab 列表中无此窗口；④ `Ctrl+Alt+Q` 进程以 exit 0 退出 |
| 命令 | `cargo run` / `cargo clippy -- -D warnings` |

### M1 — PTY 与渲染（约 4 天）

| 项 | 内容 |
|---|---|
| 文件 | `src/term/session.rs`、`src/term/grid.rs`、`src/term/input.rs`、`src/render/painter.rs`、`src/render/glyph.rs`、`src/render/layout.rs` |
| 目的 | 窗口里出现真正可交互的终端 |
| 最小改动 | `portable-pty` 起 shell（按 §4.2 注入编码参数）；读线程 → `vte::Parser` → `alacritty_terminal::Term`；`fontdue` 字形缓存 + `softbuffer` 绘制；`WM_KEYDOWN`/`WM_CHAR` → 字节写回 PTY |
| 兼容影响 | 无 |
| 验收 | ① 显示 `PS C:\...>` 提示符；② `dir` 输出正确、不闪不花；③ `vim test.txt` 能进入、`i` 插入、`:q!` 退出；④ `Get-ChildItem` 的 ANSI 颜色正确；⑤ 方向键/Home/End/Backspace 行为正常；⑥ **`pwsh.exe` 与 `powershell.exe` 各跑一遍**：中文路径不糊、中文输入不乱码、表格对齐正常；⑦ `cmd.exe` 下 `chcp` 显示 65001 |
| 命令 | `cargo test`（input 映射、启动参数注入单测）/ 手动冒烟 |

### M2 — 桌面层挂载（约 3 天）

| 项 | 内容 |
|---|---|
| 文件 | `src/win/workerw.rs`、`src/win/window.rs`（扩展） |
| 目的 | 满足 R1 / R2 |
| 最小改动 | 三级兜底查找 WorkerW（DefView 兄弟 → Progman 子 WorkerW → 顶层 WorkerW）；`SetParent`；`GetClientRect(WorkerW)` 后 `SetWindowPos` 重设；`WM_NCHITTEST` 实现无边框拖动（`HTCAPTION`）与八向缩放 |
| 兼容影响 | 窗口坐标语义从屏幕坐标变为 WorkerW 客户区坐标，M3 持久化必须按此存 |
| 验收 | ① **按 Win+D，终端仍在桌面可见**；② 点击终端后可继续输入；③ 拖动标题区可移动；④ 拖边缘可缩放；⑤ 挂载失败时降级为普通窗口且日志有记录 |
| 命令 | 手动冒烟（含 Win+D 动作） |

### M3 — 配置与位置持久化（约 2 天）

| 项 | 内容 |
|---|---|
| 文件 | `src/config/mod.rs`、`src/config/state.rs`、`src/app.rs`（扩展） |
| 目的 | 满足 R3 |
| 最小改动 | `config.toml` 加载 + 默认值 + 校验（越界值钳制）；`WM_EXITSIZEMOVE` 时 `GetWindowRect` → 转 WorkerW 相对坐标 → 写 `state.json`（500ms 防抖）；启动时优先读 state，按 DPI 比例还原 |
| 兼容影响 | 引入持久化契约 `state.json v1`，后续改动需版本号迁移 |
| 验收 | ① 拖到 (300,200)、尺寸改 1000×500 → 退出 → 重启，位置尺寸完全一致；② 改 `config.toml` 的 `font.size` → 重启生效；③ 删除 `state.json` → 按 config 的 x/y 启动；④ 把 state 里的坐标改成屏幕外 → 启动后被钳制回可见区 |
| 命令 | `cargo test`（config 反序列化、坐标钳制单测） |

### M4 — 玻璃外观（约 2 天）

| 项 | 内容 |
|---|---|
| 文件 | `src/win/layered.rs`、`src/render/painter.rs`（扩展） |
| 目的 | 满足 R4 |
| 最小改动 | `WS_EX_LAYERED` + `SetLayeredWindowAttributes(alpha)`；背景色/前景色/光标色从配置读取；自绘圆角边框与内边距 |
| 兼容影响 | 无 |
| 验收 | ① `opacity = 0.88` 时壁纸透出、文字清晰；② 改 `0.5` 明显更透；③ 改 `background` 颜色立即反映（热重载）；④ 圆角与边框在浅色/深色壁纸下都不糊 |
| 命令 | 手动冒烟 |

### M5 — 打磨与稳定性（约 3–5 天）

| 项 | 内容 |
|---|---|
| 文件 | 全部模块 |
| 目的 | 从「能跑」到「能天天用」 |
| 最小改动 | 滚轮滚动 scrollback；鼠标选中 + `Ctrl+C` 复制；IME 中文输入；Explorer 重启后 `TaskbarCreated` 重挂；多显示器选择；大输出时的重绘节流（16ms）；单实例 |
| 兼容影响 | IME 会改变输入路径，需回归 M1 的键盘验收 |
| 验收 | ① 滚轮可回看历史；② 选中文本 `Ctrl+C` 能粘到别处；③ **中文输入法能打出中文（PS 5.1 与 pwsh 各测一次）**；④ `taskkill /f /im explorer.exe && start explorer` 后终端自动回到桌面层；⑤ `cat` 大文件不卡死；⑥ 第二次启动不产生第二个窗口 |
| 命令 | 手动冒烟 + `cargo clippy -- -D warnings` |

---

## 10. 验证计划

| 层级 | 方式 | 覆盖 |
|---|---|---|
| 单元 | `cargo test` | 配置解析、坐标钳制、键映射、shell 启动参数注入、WorkerW 候选筛选 |
| 静态 | `cargo clippy -- -D warnings` | 全仓 |
| 集成 | 手动冒烟清单（§9 各里程碑验收列） | 窗口/桌面层/渲染/PowerShell 双版本 |
| 交付 | 用户可见验证：按 R1–R5 五条逐条演示 | 需求闭环 |

**完成定义**：R1–R5 五条全部通过 + `cargo test` 绿 + 连续使用 1 天无崩溃。

---

## 11. 风险与回退

| # | 风险 | 概率 | 影响 | 缓解 / 回退 |
|---|---|---|---|---|
| 1 | `alacritty_terminal` 0.26 API 变动（0.x 不稳定） | 中 | 编译失败 | 锁死版本；回退方案：`vte` + 自写网格（+3 天） |
| 2 | Win11 24H2+ WorkerW 结构再变 | 中 | R1/R2 失效 | 三级兜底 + 失败降级为普通窗口 + 日志；用户可上报 HWND 树 |
| 3 | `softbuffer` 透明表现不如预期 | 低 | R4 打折 | 全局 alpha 已够用；兜底为 color key |
| 4 | 键盘映射漏键（Ctrl 组合、功能键） | 高 | 输入异常 | 键映射单测覆盖常用集；遗漏逐条补 |
| 5 | CPU 渲染大输出卡顿 | 中 | 体验差 | 脏行重绘 + 16ms 节流；仍不行则换 wgpu（+4 天） |
| 6 | IME 与 WorkerW 子窗口冲突 | 中 | 中文无法输入 | M5 单独验证；兜底：IME 走 `WM_IME_*` 而非 `WM_CHAR` |
| 7 | **PowerShell 5.1 中文乱码** | **高** | 输入/显示异常 | 启动注入 UTF-8 前置命令（§4.2）；M1 强制双版本验收 |
| 8 | 移植 Alacritty `input.rs` 的许可证合规 | 低 | 法律风险 | 保留 Apache-2.0 版权头，NOTICE 文件列明来源 |

**回退原则**：M0–M4 任一步失败，`git reset` 到上一里程碑（每个里程碑一个 commit）；M5 的每项特性独立开关，失败只关该项。

---

## 12. 自检

- [x] 五条用户需求全部映射到验收动作（§1.1 → §9 各里程碑）
- [x] **上游零件来源、许可证、复用边界逐项列明**（§4.1）
- [x] **PowerShell 5.1 / 7+ 双版本差异与处理方式明确**（§4.2）
- [x] 项目根已修正为 `D:\Code\Rust\TLShell\`
- [x] 无占位符（字体、shell、快捷键均给了默认值）
- [x] 模块与文件一一对应，无孤儿模块
- [x] 持久化契约有版本号，兼容边界已声明
- [x] TDD route 已记录（auto → light），理由与测试范围明确
- [x] 每条风险都有回退路径
- [x] 明确列出「不做」清单，防止范围蔓延
- [x] 已知限制（焦点丢失、无模糊）已写进 README 计划

---

## 附：执行路线

- **路线**：`inline`（单人顺序执行，里程碑间有强依赖，并行收益低）
- **提交粒度**：一个里程碑一个 commit，完成后跑 `cargo test` + 对应验收
- **User confirmation required**：`no`（无付费资源、无外部动作、无不可逆操作；全部改动在 `D:\Code\Rust\TLShell\` 内）
