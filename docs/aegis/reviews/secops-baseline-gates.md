# 安全运营基线门禁评审（提交入口 / 落盘路径 / shell 回退 / CI 门禁）

| 项 | 值 |
|---|---|
| 评审对象 | `b699ea41e9a00676b42ebabc10dd952543944433`（2026-10-22 15:58），工作区有 1 处未提交改动 `src/app.rs` |
| 评审范围 | `deny.toml`、`.github/workflows/ci.yml`、`rust-toolchain.toml`、`Cargo.toml`、`build.rs`、`TLShell.manifest`、`src/diag.rs`、`src/config/mod.rs`、`src/config/state.rs`、`src/term/shell.rs`、`src/win/single.rs`、`src/main.rs`、`src/app.rs`（日志点 / 热重载 / 会话启动）、`scripts/gate.ps1`、`README.md`（安全边界 + 已知限制） |
| 评审类型 | 只读基线评审（配置 + 代码证据 + 门禁设计）。**未修改任何源码** |
| 标准映射 | 内部安全标准 `security/17-security-pattern.md`：§5.1 密钥、§11.1 快速失败式配置、§12.2 日志敏感数据、§13 错误与响应、§15 注入、§17 阶段验收清单 |
| 结论 | 已有基线（deny.toml、锁版 toolchain、默认 warn 日志、paste_guard、OSC52 默认关、Job Object、asInvoker）**方向正确且大体落地**；但存在 **2 项 HIGH 级实现缺陷**（日志记录 IME 上屏字符、CI 无密钥扫描/动作未 pin）、**1 项 HIGH 级构建缺口**（manifest 可静默丢失），以及 **3 处基线自相矛盾**（路径三分法不一致、security 开关可热改、工具链三处声明不同源）。四项任务的答案都能落到可直接执行的命令上，见 §2–§5 |

---

## 0. 结论摘要

| 编号 | 问题 | 判定 | 严重度 | SLA | 主要 CWE | 标准 | 建议 |
|---|---|---|---|---|---|---|---|
| F-01 | `src/app.rs:1178-1183` 在 `WM_IME_CHAR` 分支用 `diag::debug` 记录**输入法上屏的真实字符**（`format!("输入法上屏 {ch:?}")`） | 直接违反 `src/diag.rs` 模块头「绝不记录键盘输入」的第 3 条约束，也违反 README「不记录键盘输入」的承诺。`TLSHELL_LOG=debug` 由任意父进程环境变量即可开启，无 UI 提示、无审计记录 | HIGH | 72h | CWE-532、CWE-778 | §12.2 | 删掉字符本身，只保留 `wparam` 十六进制与「已上屏」布尔（§2.2 给代码） |
| F-02 | `src/app.rs:311-315` `diag::info("启动 shell: {} {:?}", program, args)` **全量落盘 `shell.args`** | `[shell] args` 是用户自由配置，可含 `-Command "...token..."`、UNC/本地绝对路径。info 级一旦开启即长期留痕 4 MiB × 轮转 | MEDIUM | 1 周 | CWE-532 | §12.2 | 只记 `args.len()` 与每项字节数；保留 program 全路径（有必要，用于排障） |
| F-03 | `src/config/mod.rs:17-22` `config_dir()` 在 `APPDATA` 缺失时返回 `PathBuf::from(".")` | 配置/状态/句柄文件**退化到进程当前工作目录**。从共享目录、压缩包解包目录、`C:\Windows\Temp` 之类位置启动时，`config.toml`、`state.json`、`hwnd.txt` 全部落到可被他人预置的位置 | MEDIUM | 1 周 | CWE-427、CWE-426、CWE-73 | §11.1 | `APPDATA` 缺失 → 直接失败（快速失败），绝不落 CWD |
| F-04 | `src/win/single.rs` `wake_existing()` 读 `%APPDATA%\TLShell\hwnd.txt` → `parse::<isize>` → **只做 `IsWindow` 校验** → `ShowWindow(SW_SHOW)` + `SetForegroundWindow` + `PostMessageW(0x8001)` | 任意可写该文件者（含 F-03 的 CWD 退化场景）可让 TLShell 把**任意窗口**拉到前台。同一份 README 对 WorkerW 要求「类名 + `QueryFullProcessImageNameW` 双重校验」，此处零校验，标准不一致；`PostMessageW` 返回值也被丢弃 | MEDIUM | 1 周 | CWE-290、CWE-345、CWE-367 | §13 | 写入时同存 `pid` + 窗口类名 + 进程镜像全路径，唤醒前三项全部复核；长期改为 `RegisterWindowMessageW` 探活 + `WM_COPYDATA` 握手，不落盘 |
| F-05 | 落盘三分法不一致：`config.toml`/`state.json`/`hwnd.txt` 在 **`%APPDATA%`（Roaming）**，日志在 **`%LOCALAPPDATA%`（Local）** | 日志选得对，`state.json` 与 `hwnd.txt` 选得错：窗口坐标/DPI 属机器态，放 Roaming 会在域环境被同步到漫游服务器（写放大 + 跨机无意义 + 每次 `WM_EXITSIZEMOVE` 触发漫游写）；`hwnd.txt` 是纯运行态，根本不该持久化 | LOW | 1 迭代 | CWE-668、CWE-538 | §11.1 | `state.json` → `%LOCALAPPDATA%\TLShell\`；`hwnd.txt` 短期改 Local，长期删除（见 F-04） |
| F-06 | `src/diag.rs:22-27` `LOCALAPPDATA` 缺失时回退 `TEMP` | 回退目录可能是 `C:\Windows\Temp` 等**多用户可创建**的位置，日志里有 shell 全路径、字体族、错误串（含路径） | LOW | 1 迭代 | CWE-538、CWE-732 | §12.2 | 只用 `LOCALAPPDATA`；缺失则 `%USERPROFILE%\AppData\Local`，再缺失就不写日志（静默可接受，已有先例） |
| F-07 | `src/diag.rs:55-74` 轮转用 `fs::rename`，失败无降级；`SINK` 无单行写入上限 | 日志被 AV/索引器短暂占用时 rename 失败 → 继续 append 到超限文件；`escape()` 截断 512 字符是**字符**上限，多字节仍可控但无字节级保护 | LOW | 1 迭代 | CWE-400、CWE-778 | §12.2 | rename 失败时 `warn` 一次并保持 append（可接受），但要**记录轮转失败**；`write_line` 前按字节截断到 4 KiB |
| F-08 | `src/app.rs:431-492` 热重载整体替换 `self.config`（含 `security.*`） | `security.paste_guard` / `osc52` / `allow_custom_shell` 可在**运行中**被外部改文件关闭，安全控制不是启动期不变量。`allow_custom_shell` 虽因会话已建立而不立即可利用，但 `paste_guard=false` 立即生效（多行粘贴不再二次确认） | MEDIUM | 1 周 | CWE-15、CWE-367、CWE-668 | §11.1 | 热重载只接受 `window/font/colors/keys`，`security` 段变更一律 `warn` 并忽略，需重启生效 |
| F-09 | CI 无密钥扫描；`actions/checkout@v4`、`dtolnay/rust-toolchain@master`、`Swatinem/rust-cache@v2`、`taiki-e/install-action@v2`、`actions/upload-artifact@v4` **全部按 tag/branch 引用**；无顶层 `permissions` | 供应链面：`@master` 分支可被上游改写即入构建；无 `permissions` 时 `GITHUB_TOKEN` 取默认宽权限；密钥只能靠人工评审拦 | HIGH | 72h | CWE-1357、CWE-829、CWE-1104 | §11.1 | 全部 pin 到 SHA + `permissions: contents: read` + gitleaks（§5.2 给 YAML） |
| F-10 | `build.rs:14-22` manifest 缺失只 `warning`，**不让构建失败** | `asInvoker` / `PerMonitorV2` 可静默丢失而 CI 全绿：产物变成「无 manifest」的默认行为（无 `requestedExecutionLevel` 即 asInvoker 的等价物存在，但 DPI 感知与 `activeCodePage` 全丢，且不再有任何声明可审计）。基线最强的一条声明成了不可验证项 | HIGH | 72h | CWE-693、CWE-1053 | §17 | manifest 文件缺失 → `panic!` 让构建失败；CI 增加对 PE 内嵌 manifest 的字符串断言（§5.2） |
| F-11 | 工具链三处声明不一致：`rust-toolchain.toml` = `1.97.1`、`Cargo.toml` `rust-version = "1.85"`、`ci.yml` 又在 `dtolnay/rust-toolchain.with.toolchain` **硬编码 `1.97.1`** | 改 `rust-toolchain.toml` 不会带动 CI；`rust-version` 声明与实测编译器差 12 个小版本，MSRV 声明不可信 | MEDIUM | 1 周 | CWE-1104 | §17 | CI 去掉 `with.toolchain`，让 `dtolnay/rust-toolchain@stable` 读 `rust-toolchain.toml`；`rust-version` 改为 `1.97` |
| F-12 | `scripts/gate.ps1` 与 CI 不同源：本地**无 `--locked`、无 `cargo deny`** | 「本地绿 ⇒ CI 绿」不成立；依赖漂移与许可问题只在 pushed 后暴露 | MEDIUM | 1 周 | CWE-1104 | §17 | CI 直接调用 `scripts/gate.ps1`（加 `-Ci` 开关补齐 `--locked` + deny），单一真源 |
| F-13 | `src/term/shell.rs` 的 PowerShell 5.1 回退（详见 §4） | 默认 `program = "pwsh.exe"`，**未装 PS7 的机器会静默降级到 Windows PowerShell 5.1**；降级只在日志里可见、界面无提示；注入 `-Command` 与用户 `-File` / `-EncodedCommand` 语义冲突；无 `-NoProfile` | MEDIUM | 1 周 | CWE-693、CWE-477 | §13 | 界面显式提示降级；注入前先检测 `-File`/`-EncodedCommand`/`-EncodedArguments` 全部跳过注入；`ShellKind::PowerShell5` 追加 `-NoProfile`（可配置） |
| F-14 | `deny.toml` 的 `bans.multiple-versions = "warn"`、`advisories.yanked = "warn"`，且无 `unmaintained` 设置 | 双版本依赖（`windows` / `syn` 类）不阻断、yanked crate 不阻断、unmaintained 不报 | LOW | 1 迭代 | CWE-1104 | §17 | `multiple-versions = "deny"`（配 `skip` 白名单）或 CI 用 `cargo tree --duplicates` 做计数断言；`unmaintained = "workspace"` |

**最高优先级动作**：F-01 与 F-10 必须在下一次部署前修掉——前者让 `TLSHELL_LOG=debug` 变成键盘记录器，后者让「asInvoker 基线」在构建层面不可证。

---

## 1. 证据基线

| 文件 | 行 | 事实 |
|---|---|---|
| `deny.toml` | 1-3 | `targets = [{ triple = "x86_64-pc-windows-msvc" }]`；`[advisories] version = 2`、`yanked = "warn"`、`ignore = []` |
| | 6-20 | `[licenses] confidence-threshold = 0.9`；`allow` 13 项（MIT / MIT-0 / Apache-2.0 / Apache-2.0 WITH LLVM-exception / BSD-2 / BSD-3 / ISC / Unicode-3.0 / Unicode-DFS-2016 / Zlib / CC0-1.0 / BSL-1.0 / MPL-2.0） |
| | 23-27 | `[bans] multiple-versions = "warn"`、`wildcards = "deny"`、`deny = []` |
| | 30-34 | `[sources] unknown-registry = "deny"`、`unknown-git = "deny"`、`allow-registry = ["https://github.com/rust-lang/crates.io-index"]` |
| `.github/workflows/ci.yml` | 1-8 | `on: push(branches: [main]) / pull_request / workflow_dispatch`；`env` 仅 `CARGO_TERM_COLOR`、`CARGO_INCREMENTAL=0`；**无 `permissions`** |
| | 9-14 | 单 job `gate`（`name: windows`），`runs-on: windows-latest`，`timeout-minutes: 30`；`checkout@v4`；`dtolnay/rust-toolchain@master` + `with.toolchain: "1.97.1"` |
| | 16-19 | `Swatinem/rust-cache@v2`（`cache-on-failure: true`） |
| | 21-27 | `cargo fmt --all --check` → `cargo clippy --all-targets --locked -- -D warnings` → `cargo test --locked` |
| | 29-33 | `taiki-e/install-action@v2`（`tool: cargo-deny`）→ `cargo deny --all-features check` |
| | 35-42 | `cargo build --locked --release` → `upload-artifact@v4`（`tlshell-x86_64-pc-windows-msvc` / `target/release/tlshell.exe` / `if-no-files-found: error`） |
| `rust-toolchain.toml` | 1-5 | `channel = "1.97.1"`、`components = ["rustfmt", "clippy"]`、`profile = "minimal"`、`targets = ["x86_64-pc-windows-msvc"]` |
| `Cargo.toml` | — | `rust-version = "1.85"`、`license = "MIT OR Apache-2.0"`、`publish = false`；依赖全部 `=` 精确锁版（`alacritty_terminal 0.26.0`、`portable-pty 0.9.0`、`windows 0.62.2` 等 10 项） |
| | `[lints]` | `[lints.rust] unsafe_op_in_unsafe_fn = "deny"`；`[lints.clippy] undocumented_unsafe_blocks = "deny"` |
| `build.rs` | 12-22 | 仅 `CARGO_CFG_TARGET_ENV == "msvc"` 时加 `/MANIFEST:EMBED` 与 `/MANIFESTINPUT:{manifest}`；manifest 路径不存在时 `cargo:warning` 后 `return`，**不失败** |
| `TLShell.manifest` | 7-11 | `dpiAwareness = PerMonitorV2`、`dpiAware = true/pm`、`activeCodePage = UTF-8`、`longPathAware = true` |
| | 18-24 | `requestedExecutionLevel level="asInvoker" uiAccess="false"` |
| `src/diag.rs` | 3-11 | 模块头四约束：落 `%LOCALAPPDATA%\TLShell\logs`（非 Roaming）、默认 warn、**绝不记 PTY 字节流/键盘/剪贴板**、1 MiB × 3 |
| | 22-27 | `log_dir()`：`LOCALAPPDATA` **`or_else` `TEMP`**，再 `.join("TLShell").join("logs")` |
| | 30-53 | `init()` 读 `TLSHELL_LOG`（`info`/`debug`），`create_dir_all` 失败静默 `return`；`create(true).append(true)`；**无显式 DACL** |
| | 55-74 | `rotate_if_needed()`：`> MAX_BYTES` 时 `tlshell.log → .1 → .2`（`fs::rename`，失败仅 `let _`） |
| | 77-93 | `escape()` 转义 `\r\n\t\x1b` 与 `<0x20`，截断 512 **字符**（防日志行伪造，设计正确） |
| `src/config/mod.rs` | 17-22 | `config_dir()`：`APPDATA` 为 `Some` 且非空 → `%APPDATA%\TLShell`；**否则 `PathBuf::from(".")`** |
| | 25-27 | `config_path()` = `config_dir()/config.toml` |
| | 54-90 | `load()`：缺失即 `write_default`（`created = true`）；读/解析失败 → 默认值 + warning，**不覆盖**用户文件；成功后 `sanitize()`；serde **未开 `deny_unknown_fields`** |
| | 314-338 | `sanitize()` 只钳数值（宽高 200..16384 / 120..16384、padding·radius 0..64、opacity `clamp(0.10, 1.0)`、radius ≤ 短边/2、字号 6..72、行高 0.8..3.0）；**`security.*` 不做任何规范化或交叉校验** |
| `src/config/state.rs` | 86-88 | `state_path()` = `config_dir().join("state.json")` → **Roaming**，且随 F-03 的 `.` 退化 |
| | 90-215 | `save()` 走 `path.with_extension("json.tmp")` → `fs::write` → `fs::rename`，失败 `remove_file(temp)`（原子性正确） |
| `src/term/shell.rs` | 38-101 | `allowed_dirs()` 四个白名单目录；`resolve_program()` 裸名只在白名单内搜、带分隔符则 `canonicalize` 后复核 `is_allowed_path`；**不走 PATH、不走 CWD** |
| | 123-124 | `classify()` 用 `program.to_string_lossy().to_ascii_lowercase()` 后按**绝对路径后缀**判种 |
| | 146-170 | `inject_args()`：`has()` 只做 `eq_ignore_ascii_case` 全等匹配；PS5 无 `-command`/`-c` 时追加 `-NoLogo` + `-NoExit` + `-Command` + `PS5_UTF8_SETUP`；Cmd 无 `/k`·`/c` 时追加 `/K` + `chcp 65001 >nul`；**PS7/Other 不注入**；**无 `-NoProfile`** |
| | 173-205 | `resolve()`：`resolve_program` 失败 → `fallback_program()` → 成功则 `diag::warn` **静默换实现**；之后才做 `if !allow_custom && !is_allowed_path(&program)` 的 `bail!` |
| | 208-218 | `fallback_program()`：`file_stem` 小写后 `"pwsh" → "powershell.exe"`、`"powershell" → "pwsh.exe"`，其余 `None`（只有两版 PowerShell 互为退路，退路同样走白名单） |
| `src/win/single.rs` | — | `Local\TLShell.SingleInstance` 命名互斥体（`ERROR_ALREADY_EXISTS`）；`hwnd_file()` = `config_dir()/hwnd.txt`；`wake_existing()` 只 `IsWindow` 校验后 `ShowWindow` + `SetForegroundWindow` + `PostMessageW(0x8001)` |
| `src/main.rs` | 106-110 | `run_quit_request()` 注释「单实例握手在 M5 接入；当前直接以成功码退出」→ 恒 `Ok(0)`，与 README 宣称不符（功能缺口，非安全面） |
| | 148-161 | `run_selftest()` 默认报告 `std::env::temp_dir().join("tlshell-selftest.json")` → 可预测路径、无 `O_EXCL` |
| `src/app.rs` | 298-339 | `spawn_session()`：`shell::resolve(..., security.allow_custom_shell)`；`diag::info("启动 shell: {} {:?}", program, args)`；`cwd = USERPROFILE`；失败 `diag::error` + 界面提示 |
| | 574 | 复制：`diag::info("已复制 {} 个字符", …)` — 仅计数，合规 |
| | 830-875 | 粘贴：`diag::info("粘贴请求: {} 字符 {} 行，paste_guard={}")` / `"多行粘贴：等待二次确认"` / `"写入粘贴内容 {} 字节"` — 仅计数，合规 |
| | 1142 | `WM_LBUTTONDOWN` 只记坐标，无害 |
| | **1178-1183** | `WM_IME_CHAR` → `diag::debug("收到 WM_IME_CHAR wparam=0x{:x}")` + **`diag::debug("输入法上屏 {ch:?}")`** |
| | 431-492 | `watch_config()` 1 s 轮询 mtime；`reload_config()` 整体替换 `self.config` 并应用主题/字体/透明度/尺寸；文件消失仅 `warn` |
| `scripts/gate.ps1` | 13-16 | `cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test` → `cargo build --release`（**无 `--locked`、无 `cargo deny`**） |
| `README.md` | 安全边界 | 九条承诺，其中「日志落 `%LOCALAPPDATA%\TLShell\logs`，默认 warn，不记录 PTY 字节流/键盘输入/剪贴板内容」「命名互斥体 + `%APPDATA%\TLShell\hwnd.txt` 句柄唤醒」「WorkerW 需 `GetWindowThreadProcessId` + `QueryFullProcessImageNameW` 验 `explorer.exe`」三条与实现对不上（见 F-01、F-04） |

工作区无 `.env`、无密钥类文件入库；`.gitignore` 已忽略 `/target/`、`*.log`、`run.log`、`.vscode/`、`.idea/`。

---

## 2. 提交入口检查清单（逐条可执行）

目的：把「评审才看得见的东西」变成「commit 之前机器就能判红的东西」。以下命令在仓库根 `D:\Code\Rust\TLShell` 执行，**只用 PowerShell 原生 cmdlet 与 git/cargo**（本机未装 `rg`，CI 也不依赖它），可整段复制粘贴。

### 2.1 密钥与凭据（§5.1 / §11.1）

```powershell
$files = (Get-ChildItem src -Recurse -Filter *.rs).FullName

# 1) 密钥材料 / 云凭据模式：必须 0 命中（无命中时 exit=1，即通过）
git grep -nEI -e '-----BEGIN [A-Z ]*PRIVATE KEY-----' -e 'AKIA[0-9A-Z]{16}' -e 'AIza[0-9A-Za-z_\-]{35}' -- .
if ($LASTEXITCODE -eq 0) { throw '发现密钥材料或云凭据模式' }

# 2) 疑似硬编码凭据赋值（只打印，供人工复核）
Select-String -Path $files -Pattern '(?i)(password|passwd|secret|token|api[_-]?key|client_secret|access_key)\s*[:=]'

# 3) 环境变量读取与字符串兜底默认值（标准 §11.1 明令禁止后者，前者需逐条确认是否为可信输入）
Select-String -Path $files -Pattern 'var_os\(|env::var\('
Select-String -Path $files -Pattern 'unwrap_or\(\s*"|unwrap_or_default\(\)'

# 4) 敏感文件未入库
$bad = git ls-files | Select-String -Pattern '(^|/)\.env|\.pem$|\.pfx$|\.p12$|id_rsa|secrets?\.(toml|ya?ml|json)$'
if ($bad) { $bad; throw '敏感文件被入库' }
```

### 2.2 日志与敏感数据红线（§12.2）

```powershell
$files = (Get-ChildItem src -Recurse -Filter *.rs).FullName

# 1) 全部日志调用清单（人工复核用，不判红）
Select-String -Path $files -Pattern 'diag::(error|warn|info|debug)' |
  ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }

# 2) 硬红线（规则 A，必须 0 命中）：日志的 format! 里出现「内容型占位符」
$ruleA = 'diag::[a-z_]+\(\s*&?format!\(.*\{(ch|key|keys|text|clip|clipboard|input|payload|chars|bytes)([:}?])'
$hits = Select-String -Path $files -Pattern $ruleA
if ($hits) { $hits | ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }; throw '日志红线 A 命中：疑似把内容写进日志' }

# 3) 复核清单（规则 B，宽匹配，只打印不判红）
Select-String -Path $files -Pattern 'diag::(error|warn|info|debug)\([^\n]*(char|chars|text|clipboard|payload|bytes|input|key|ime)' |
  ForEach-Object { "review: $($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }
#   实测当前 3 命中：app.rs:574「已复制 {} 个字符」、app.rs:871「写入粘贴内容 {} 字节」为**只记计数**的合规项；
#   app.rs:1179「收到 WM_IME_CHAR wparam=」也只记 wparam。真正的违规在 app.rs:1181（`{ch:?}`），由规则 A 捕获。

# 4) 结构化兜底：stdout 调试输出不得进 src（main 的用法文档除外）
$stdio = Select-String -Path $files -Pattern '\b(println|eprintln|dbg)!\(' | Where-Object { $_.Path -notlike '*\main.rs' }
if ($stdio) { $stdio | ForEach-Object { "$($_.Filename):$($_.LineNumber)" }; throw 'src 内出现 stdout 调试输出' }

# 5) 编译期与测试期双保险（CI 同样跑）
cargo test --locked --lib
```

> 全部用 PowerShell 原生 `Select-String`，不依赖 `rg`（本机与 CI 均未安装）。想更快可自行换 `rg`，但规则 A 的正则与判定语义不要改。

### 2.3 依赖许可 / 公告 / 来源（§17 依赖阶段）

```powershell
cargo deny --locked --all-features check advisories bans licenses sources   # 分项跑，便于定位属于哪一类
cargo deny --all-features check                                              # 与 CI 完全一致的总门
cargo tree --duplicates                                                      # bans=warn 时的可视补充
cargo tree -e normal --prefix none | Sort-Object -Unique                     # 直接依赖清单（人工过一遍许可）
```

> `--locked` 属于 cargo-deny 的顶层参数位（放在 `check` 之前）；若本地 cargo-deny 版本不认，去掉它即可，CI 其余步骤已对 cargo 本身使用 `--locked`。

### 2.4 unsafe 纪律（§17）

项目必然存在 Win32 FFI，纪律不是「零 unsafe」而是「每条可解释、可定位、不增长」：

```powershell
# 1) 每条 unsafe 块必须有 SAFETY 注释（Cargo.toml 已把 clippy 的该项设为 deny，这里是显式复跑）
cargo clippy --all-targets --locked -- -D warnings -D clippy::undocumented_unsafe_blocks

# 2) unsafe 出现位置清单（评审时逐条核对调用契约）
Select-String -Path (Get-ChildItem src -Recurse -Filter *.rs).FullName -Pattern 'unsafe\s*(\{|fn|impl|extern)' |
  ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }

# 3) 增长预算：与基线比较（实测基线：unsafe 块 100 个 / 含 unsafe fn·impl·extern 共 109 处）
$files = (Get-ChildItem src -Recurse -Filter *.rs).FullName
$count = (Select-String -Path $files -Pattern 'unsafe\s*\{' -AllMatches |
            ForEach-Object { $_.Matches.Count } | Measure-Object -Sum).Sum
if ($count -gt 100) { throw "unsafe 块 $count 超出基线 100，需评审新增点并同步更新本预算" }

# 4) unsafe fn 内必须再包 unsafe（已由 lints.rust 强制，这里防 lints 被误删）
Select-String -Path Cargo.toml -Pattern 'unsafe_op_in_unsafe_fn\s*=\s*"deny"'
```

### 2.5 lints / 格式 / 锁文件（§17）

```powershell
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --locked --release
cargo metadata --locked --format-version 1 > $null    # 锁文件与 manifest 一致性
git diff --quiet -- Cargo.lock ; if ($LASTEXITCODE -ne 0) { Write-Host 'Cargo.lock 有改动：确认是有意的依赖变更' }
Select-String -Path Cargo.toml -Pattern 'edition\s*=\s*"2021"|rust-version\s*=\s*"'   # 声明与 toolchain 一致性人工确认
```

### 2.6 一键入口（推荐把 2.1–2.5 收进 `scripts/gate.ps1`，本地与 CI 同源）

```powershell
# 目标形态（F-12 的修复方向）：单脚本 + -Ci 开关，CI 直接调它
#   powershell -NoProfile -File scripts\gate.ps1 -Ci
# 顺序：secrets -> log-redline -> fmt -> clippy(-D warnings) -> test --locked -> deny -> build --locked --release -> manifest 断言
```

---

## 3. 日志与配置落盘路径的基线问题

### 3.1 现状三分法

| 产物 | 当前路径 | 目录类型 | 判定 |
|---|---|---|---|
| `tlshell.log` / `.1` / `.2` | `%LOCALAPPDATA%\TLShell\logs\` | Local（不漫游） | ✅ 选择正确 |
| `config.toml` | `%APPDATA%\TLShell\` | Roaming | ✅ 可辩护（用户偏好应随配置文件漫游） |
| `state.json`（窗口几何 + 显示器 DPI） | `%APPDATA%\TLShell\` | Roaming | ❌ 机器态放进了漫游目录（F-05） |
| `hwnd.txt`（上一次的窗口句柄） | `%APPDATA%\TLShell\` | Roaming | ❌ 纯运行态持久化，且被直接信任（F-04、F-05） |
| selftest 报告（默认） | `%TEMP%\tlshell-selftest.json` | 可预测路径 | ⚠️ 低危（仅自身产物，无 `O_EXCL`） |

结论：**「日志走 Local、配置走 Roaming」本身是正确且一致的分法**，问题不在两者不一致，而在 `state.json` 与 `hwnd.txt` 被错误归到 Roaming 侧，并且三个产物的目录解析共用同一个会退化成 `.` 的函数（F-03）。

### 3.2 泄漏面清单

| 面 | 机制 | 影响 | 处置 |
|---|---|---|---|
| 日志记录 IME 上屏字符 | `TLSHELL_LOG=debug` + `WM_IME_CHAR` 分支 | 中文/日文输入的全部上屏文本可被父进程一次性开启并长期留痕（4 MiB × 轮转） | F-01，**必须先修** |
| 日志记录 `shell.args` | info 级 `{:?}` | 用户把令牌写进 `-Command` 时直接落盘 | F-02 |
| 目录退化到 CWD | `APPDATA` 缺失 → `"."` | 从可写共享目录启动时，`config.toml`/`state.json`/`hwnd.txt` 可被他人预置 | F-03 |
| `hwnd.txt` 信任 | 只 `IsWindow` | 任意窗口被拉前台 + 收到 `0x8001` | F-04 |
| `TEMP` 回退日志 | `LOCALAPPDATA` 缺失 | 日志落可能多用户可创建的位置 | F-06 |
| 无显式 DACL | `create_dir_all` 默认继承 | 现实风险低（`%APPDATA%`/`%LOCALAPPDATA%` 已是 user-only ACL），但**基线文档没写这条依赖**，环境变量被改写时无兜底 | 写进 README 的「假设」一节，或 `init()` 里 `GetNamedSecurityInfo` 复核（可选） |
| `security.*` 可热改 | 1 s 轮询 + 整体替换 | `paste_guard=false` 立即生效，绕过「多行粘贴二次确认」 | F-08 |

### 3.3 与标准的一致性

- §11.1「缺失必需的密钥/路径时应用必须在启动时失败」：`config_dir()` 的 `.` 退化是**教科书式反例**（F-03）；`diag::log_dir()` 的 `TEMP` 回退是同一模式的弱化版（F-06）。
- §12.2「敏感数据绝不入日志」：F-01 是硬违规，F-02 是灰区（取决于用户配置）。
- §13「错误信息不泄漏内部细节」：`show_terminal_error` 与 `diag::error` 携带 `{err:#}` 全链（`anyhow` 上下文），本地终端程序可接受，但如果未来加崩溃上报需先脱敏。

---

## 4. shell 回退到 `powershell.exe` 的行为与风险

### 4.1 行为（代码事实，`src/term/shell.rs`）

| 步骤 | 行为 |
|---|---|
| 1 | 默认 `[shell] program = "pwsh.exe"`（`src/config/mod.rs` 默认值） |
| 2 | `resolve_program("pwsh.exe")`：裸名 → 只在 4 个白名单目录内找 → 命中则 `canonicalize` 再 `is_allowed_path` 复核 |
| 3 | 找不到（`%ProgramFiles%\PowerShell\7\pwsh.exe` 不存在，即**未安装 PS7 的机器**）→ 进入 `Err` 分支 |
| 4 | `fallback_program("pwsh.exe")` → `"powershell.exe"` → `resolve_program` 走白名单（`%SystemRoot%\System32\WindowsPowerShell\v1.0`） |
| 5 | 命中即 `diag::warn("`pwsh.exe` 不可用，改用 C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")`，**随后照常启动，界面无任何提示** |
| 6 | `classify()` 按绝对路径后缀判为 `ShellKind::PowerShell5` |
| 7 | `inject_args(PowerShell5, [])` → 追加 `-NoLogo -NoExit -Command "[Console]::OutputEncoding=[Console]::InputEncoding=[Text.UTF8Encoding]::new()"` |
| 8 | 反向场景对称：配 `powershell.exe` 而 PS7 存在时不会自动升级（`fallback` 只在解析**失败**时触发） |

### 4.2 风险

| 风险 | 说明 | 严重度 |
|---|---|---|
| 静默降级 | 用户以为在 PowerShell 7（`$PSVersionTable.PSVersion` 才看得出），实际是 5.1 + .NET Framework。日志里有一条 `warn`，但默认界面/终端首屏不显示。5.1 在 AppLocker/WDAC 环境中更容易落到 Constrained Language Mode，语法子集不同（三元、`&&`、`??` 全不支持） | MEDIUM |
| 5.1 会加载 profile | 没有 `-NoProfile`。`-NoExit -Command <setup>` 下 PS5 先加载 `$PROFILE`，用户/域下发的 profile 代码在每次启动时执行。与 Windows Terminal 的交互行为一致（可辩护），但**与项目「最小启动面」的定位不符**，且 profile 报错会污染首屏 | MEDIUM |
| 注入与用户参数冲突 | `has()` 只对 `-command` / `-c` 做全等大小写不敏感匹配。用户写 `-File script.ps1`、`-EncodedCommand …`、`-Command:"…"`（带冒号）、`-co`（前缀缩写）时**判定为「未给 -Command」并继续注入** `-NoExit -Command <setup>`。`-File` 与 `-Command` 同时出现时 `powershell.exe` 直接报参数冲突；`-EncodedCommand` 与 `-Command` 同时出现时取哪个随版本而异；`-Command:"…"` 的冒号形式让注入的裸 `-Command` 变成第二个命令，先执行的是注入的 setup | MEDIUM |
| 参数吞并 | 注入在末尾追加 `-Command <setup>`，用户原本写在后面的位置参数会被当作 setup 的参数或触发绑定错误（`-Command` 之后的一切都是命令串） | MEDIUM |
| 编码修复不对等 | PS7 分支**不注入任何东西**（合理，PS7 默认 UTF-8）；`cmd` 注入 `chcp 65001 >nul` 但**未加 `/D`**（忽略 AutoRun 注册表项），`HKCU\Software\Microsoft\Command Processor\AutoRun` 中的任意命令仍会在每个会话执行 | LOW |
| `Other` 种类无编码处理 | `allow_custom_shell = true` + 非 PowerShell/cmd 程序 → `ShellKind::Other` → 不注入 → 非 UTF-8 输出在 `alacritty_terminal` 侧按 UTF-8 解析，可能出现替换字符 | LOW |
| 错误信息透出搜索路径 | `bail!("在允许的目录中找不到 {program}，已搜索: …")` 把白名单目录全部列出（信息量低，可接受，但属 §13 的可选收敛项） | LOW |

### 4.3 建议

1. 降级时**必须**在终端首屏或 overlay 显示一行 `已降级到 Windows PowerShell 5.1（未找到 pwsh.exe）`（`set_overlay` 已有能力），并把 `warn` 升级为一次性的显式提示。
2. `inject_args` 的「是否已给命令」判定改为**前缀集合**：`-command` / `-c` / `-com` / `-comm…`（PowerShell 支持前缀缩写）、带冒号形式、以及 `-file` / `-f` / `-encodedcommand` / `-e` / `-ec` —— 命中任一即**完全跳过注入**（宁可丢 UTF-8 修复，也不要和用户参数打架）。
3. `ShellKind::PowerShell5` 增加 `-NoProfile`（默认开，配置项 `[shell] no_profile` 可关），并在文档里写明理由。
4. `cmd` 的注入改为 `/K "chcp 65001 >nul"` 前置 `/D`。
5. 为上述四种冲突写单测：`-File`、`-EncodedCommand`、`-Command:"x"`、`-co x` 均断言 `inject_args` 返回原参数不变。

---

## 5. CI 缺的门禁与可直接粘贴的步骤

### 5.1 缺口对照

| 门禁 | 现状 | 缺口 |
|---|---|---|
| 密钥扫描 | 无 | 需 gitleaks（或 `git grep` 兜底）+ 敏感文件清单断言 |
| 依赖许可/公告/来源 | `cargo deny --all-features check` ✅ | `bans=warn`、`yanked=warn`、无 `unmaintained`；建议分项跑以便定位 |
| 依赖双版本 | 仅 warn | 建议 `cargo tree --duplicates` 计数断言或 `multiple-versions = "deny"` + `skip` |
| unsafe 纪律 | `Cargo.toml [lints]` ✅ + clippy `-D warnings` ✅ | 无「增长预算」断言（新增 unsafe 块无人看）；无 `cargo fmt --check` 之外的形式化 |
| lints/格式 | `fmt --check` + `clippy -D warnings` ✅ | 无 `RUSTDOCFLAGS=-D warnings`；无 doc test |
| 锁文件 | `--locked` ✅ | 本地门缺（F-12） |
| toolchain 单一真源 | ❌ 三处硬编码/不一致 | CI 去掉 `with.toolchain`，改读 `rust-toolchain.toml` |
| **构建产物断言** | ❌ | ① manifest 是否真的嵌入（`asInvoker` / `PerMonitorV2`）；② `--selftest` smoke；③ 产物 SHA-256 记录 |
| **actions 供应链** | ❌ 全部 tag/branch | 全部 pin 到 commit SHA；`dtolnay/rust-toolchain@master` 尤其危险 |
| **最小权限** | ❌ 无 `permissions` | 顶层 `permissions: contents: read` |
| **定时重扫** | ❌ 只在 push/PR 触发 | `schedule: cron` 每周跑 advisories（新公告不会因代码不变而触发） |
| 时效性 | `timeout-minutes: 30` ✅ | `concurrency` 取消同分支旧任务（可选） |
| 覆盖矩阵 | 单 triple ✅ 与 `deny.toml` 一致 | 无缺失（如未来支持 aarch64 需同步 `targets`） |

### 5.2 可直接粘进 `.github/workflows/ci.yml` 的步骤

先在工作流顶层加最小权限与并发（**替换文件开头到 job 定义之间的部分**）：

```yaml
permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true
```

**动作 pin 示例**（把现有 ``uses:`` 逐个换成 SHA；下面是 `actions/checkout` 的写法，其余同理，SHA 请以官方 release 页为准）：

```yaml
      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
        with:
          persist-credentials: false
```

**在 `cargo fmt --all --check` 之前插入密钥与红线扫描**：

```yaml
      - name: secret scan
        shell: pwsh
        run: |
          choco install gitleaks -y --no-progress
          if ($LASTEXITCODE -ne 0) { throw "gitleaks 安装失败" }
          gitleaks detect --source . --redact --exit-code 1 --no-banner
          if ($LASTEXITCODE -ne 0) { throw "发现疑似密钥" }

      - name: sensitive-file guard
        shell: pwsh
        run: |
          $bad = git ls-files | Select-String -Pattern '(^|/)\.env|\.pem$|\.pfx$|\.p12$|id_rsa|secrets?\.(toml|ya?ml|json)$'
          if ($bad) { $bad; throw "敏感文件被入库" }

      - name: log red-line guard
        shell: pwsh
        run: |
          $files = (Get-ChildItem src -Recurse -Filter *.rs).FullName
          # 规则 A（阻断）：日志 format! 里不得出现「内容型占位符」
          $ruleA = 'diag::[a-z_]+\(\s*&?format!\(.*\{(ch|key|keys|text|clip|clipboard|input|payload|chars|bytes)([:}?])'
          $hits = Select-String -Path $files -Pattern $ruleA
          if ($hits) { $hits | ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }; throw "日志红线 A 命中" }
          # stdout 调试输出不得进 src（main.rs 的用法文档除外）
          $stdio = Select-String -Path $files -Pattern '\b(println|eprintln|dbg)!\(' | Where-Object { $_.Path -notlike '*\main.rs' }
          if ($stdio) { $stdio | ForEach-Object { "$($_.Filename):$($_.LineNumber)" }; throw "src 内出现 stdout 调试输出" }
          # 规则 B（只打印，供人复核）：宽匹配清单
          Select-String -Path $files -Pattern 'diag::(error|warn|info|debug)\([^\n]*(char|chars|text|clipboard|payload|bytes|input|key|ime)' |
            ForEach-Object { "review: $($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }
```

> 说明：`clippy` 的 `-D warnings` 抓不到 F-01（它是「合法」的 `format!` 调用），所以这条门禁是必要的**回归护栏**。规则 A 目前在 `src/app.rs:1181` 有且仅有 1 处命中——正是 F-01；修掉那一行后本步立刻转绿，不要为了让 CI 变绿而放宽规则 A。规则 B 只打印不阻断，作用是让评审者每次都能看到全部日志调用点的现状。
>
> 已知覆盖边界：规则 A 是**行级**正则，跨行书写的 `format!`（`app.rs:289`、`app.rs:311`、`app.rs:514`、`app.rs:540`、`app.rs:640`、`app.rs:843`、`app.rs:1046`、`app.rs:1142`、`main.rs:131`、`main.rs:141` 等 10 处）不会被它匹配，这部分靠规则 B 的清单 + 人工复核兜住（本次已逐条核对，均为坐标/尺寸/计数类，无内容泄漏）。若要机器覆盖跨行写法，改用 `Select-String -Raw` 配 `(?s)` 多行模式——本期不引入，避免误报。

**把工具链真源改成 `rust-toolchain.toml`**（删掉 `with.toolchain`）：

```yaml
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
```

**在 `cargo deny` 之后、`cargo build` 之后插入产物断言**：

```yaml
      - name: deny (per-category)
        shell: pwsh
        run: |
          cargo deny --locked --all-features check advisories
          if ($LASTEXITCODE -ne 0) { throw "advisories" }
          cargo deny --locked --all-features check licenses
          if ($LASTEXITCODE -ne 0) { throw "licenses" }
          cargo deny --locked --all-features check bans sources
          if ($LASTEXITCODE -ne 0) { throw "bans/sources" }

      - name: duplicate dependency budget
        shell: pwsh
        run: |
          $dups = cargo tree --locked --duplicates 2>$null
          if ($dups) { $dups | Write-Host }
          # 实测基线 6 条版本条目：bitflags 1.3.2/2.13.2、syn 2.0.119/3.0.9、windows-sys 0.59.0/0.61.2
          $count = ($dups | Select-String -Pattern '^[a-zA-Z0-9_-]+ v' | Measure-Object).Count
          Write-Host "duplicate version entries: $count"
          if ($count -gt 6) { throw "重复依赖条目 $count 超出基线 6，需评审新增点" }

      - name: unsafe budget
        shell: pwsh
        run: |
          $files = (Get-ChildItem src -Recurse -Filter *.rs).FullName
          $count = (Select-String -Path $files -Pattern 'unsafe\s*\{' -AllMatches |
                     ForEach-Object { $_.Matches.Count } | Measure-Object -Sum).Sum
          Write-Host "unsafe blocks: $count"
          if ($count -gt 100) { throw "unsafe 块 $count 超出基线 100" }
```

```yaml
      - name: embed-manifest assertion
        shell: pwsh
        run: |
          $exe = 'target/release/tlshell.exe'
          if (-not (Test-Path $exe)) { throw "缺少产物 $exe" }
          $text = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($exe))
          if ($text -notmatch 'requestedExecutionLevel') { throw "manifest 未嵌入：缺 requestedExecutionLevel" }
          if ($text -notmatch 'level="asInvoker"')       { throw "manifest 未嵌入：asInvoker 缺失或被改" }
          if ($text -match 'level="requireAdministrator"') { throw "manifest 声明提权，违反 asInvoker 基线" }
          if ($text -notmatch 'PerMonitorV2')            { throw "manifest 未嵌入：缺 PerMonitorV2" }
          $h = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower()
          "tlshell.exe sha256=$h" | Tee-Object -FilePath target/release/SHA256SUMS.txt
          $h

      - name: selftest smoke
        shell: pwsh
        run: |
          $report = Join-Path $env:RUNNER_TEMP 'tlshell-selftest.json'
          & target/release/tlshell.exe --selftest --report $report
          $code = $LASTEXITCODE
          if (Test-Path $report) { Get-Content $report -Raw | Write-Host }
          if ($code -ne 0) { throw "selftest 失败 (exit $code)" }
```

> `embed-manifest assertion` 已对 `b699ea4` 的 `target/release/tlshell.exe` 实测：`requestedExecutionLevel` / `level="asInvoker"` / `PerMonitorV2` 三项均命中，`requireAdministrator` 未命中，即当前产物正确嵌入了 manifest。这一步的价值是**防回归**（`build.rs` 的 warning-only 分支一旦触发，CI 立刻判红），不是修当下。
>
> `--selftest` 在无桌面的 runner 上是否稳定需实测（`run_selftest` 只写 JSON 并返回 0/1，不建窗口，风险低）。若实测不稳定，把该步改成 `continue-on-error: false` + 在 `windows-latest` 上手写最小断言（`--version` 输出 + 报告文件存在），不要直接删掉这一步。

**定时重扫**（`on:` 块追加）：

```yaml
  schedule:
    - cron: '0 3 * * 1'   # 每周一 03:00 UTC 重跑 advisories
```

**上传产物附带校验和**（现有 `upload-artifact` 的 `path` 加一行）：

```yaml
          path: |
            target/release/tlshell.exe
            target/release/SHA256SUMS.txt
```

**`build.rs` 的快速失败**（F-10，源码侧一处小改，不属于本次「不改源码」范围，列为待办）：

```rust
// build.rs：manifest 缺失时不再只 warning
let manifest = ...;
if !manifest.exists() {
    panic!("缺少 {manifest:?}：manifest 是安全基线的一部分（asInvoker / PerMonitorV2），构建必须失败");
}
```

### 5.3 落地顺序

1. **今天**：加 `permissions` + `concurrency` + actions pin SHA + `embed-manifest assertion`（这几步零源码改动，立刻提升基线可证性）。
2. **72h**：修 F-01（删一行日志）→ 打开 `log red-line guard`；`--selftest` smoke 实测后接入。
3. **1 周**：`deny.toml` 收紧（`multiple-versions`/`unmaintained`）+ 分项 deny + 重复依赖/unsafe 预算；`rust-version` 与 toolchain 对齐；热重载忽略 `security`（F-08）。
4. **1 迭代**：`state.json` 迁 `%LOCALAPPDATA%`（带迁移读旧路径一次的兼容逻辑）、`hwnd.txt` 加三重校验或删除、`diag` 的 `TEMP` 回退收紧、`gate.ps1` 与 CI 同源（F-12）。

---

## 6. 与已有基线文档的对账

| 已有承诺（README「安全边界」） | 实现 | 结论 |
|---|---|---|
| asInvoker 不提权 | `TLShell.manifest` 声明正确 | ⚠️ 声明正确但**嵌入不可验证**（F-10） |
| shell 按白名单绝对路径解析、无 CWD 回退 | `resolve_program` + `is_allowed_path` 实现正确 | ✅ |
| OSC52 默认关、粘贴剥离 C0/C1 | 默认 `osc52 = false`、`paste::sanitize` | ✅ |
| 日志落 `%LOCALAPPDATA%`、默认 warn、不记 PTY/键盘/剪贴板 | 路径与级别正确；粘贴/复制只记计数 | ❌ **键盘输入被 `WM_IME_CHAR` 的 debug 日志破坏**（F-01） |
| Job Object `KILL_ON_JOB_CLOSE` | 依赖项与 feature 齐备 | ✅（本次未展开） |
| 命名互斥体单实例 + `%APPDATA%\TLShell\hwnd.txt` 唤醒 | 互斥体正确；`hwnd.txt` 零校验 | ❌ 与 WorkerW 双校验标准不一致（F-04） |
| WorkerW 需 `QueryFullProcessImageNameW` 验 `explorer.exe` | 已有评审记录该要求 | 本次未复核（见 `security-input-boundary.md` Q1） |
| `deny.toml` + CI `cargo deny` | 存在且生效 | ⚠️ 覆盖率不足（F-14、§5.1） |

---

## 7. 待办（按 SLA）

| SLA | 项 | 类型 |
|---|---|---|
| 24h（CRITICAL 前置） | 无 CRITICAL；但 F-01 的修复仅一行删除，建议同日完成 | — |
| 72h | F-01 删 IME 字符日志 + 打开 `log red-line guard`；F-09 actions pin SHA + `permissions`；F-10 manifest 断言（CI 侧，源码侧待排期） | 代码 + CI |
| 1 周 | F-02 args 脱敏；F-03 目录快速失败；F-04 hwnd 三重校验；F-08 security 段冻结；F-11 工具链单一真源；F-12 gate 同源；F-13 回退提示与注入判定 | 代码 + 配置 |
| 1 迭代 | F-05 路径归位（state → Local、hwnd 去持久化）；F-06 TEMP 回退收紧；F-07 轮转失败告警 + 字节级截断；F-14 deny 收紧 | 代码 + 文档 |
