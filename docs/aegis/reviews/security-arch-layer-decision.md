# 桌面层挂载方式决策：子窗口挂载（方案 A） vs 顶层沉底（方案 B）

| 项 | 值 |
|---|---|
| 评审对象 | `b699ea41e9a00676b42ebabc10dd952543944433`（2026-10-22 16:24），工作区有 1 处未提交改动 `src/app.rs`（既有，非本评审引入） |
| 评审范围 | `src/win/workerw.rs`、`src/win/window.rs`、`src/win/layered.rs`、`src/win/single.rs`、`src/app.rs`（`run` / `wndproc` / `reattach` / `handle` / `current_modifiers`）、`docs/design/2026-10-22-tlshell-security-architecture.md`（§0.1-0.2、§1、§2、§4 D 表、§5.3-5.4、§6 S 表、§8、§9、§11）、`docs/aegis/plans/2026-10-22-tlshell-desktop-terminal.md`（§1.1-1.2、§4.1、§5.3、§7、§9） |
| 评审类型 | 只读架构决策评审（信任边界视角）。**未修改任何源码** |
| 决策类型 | 架构级取舍。迁移工作量中等，但**不接受运行时开关回退**（见 §2.7），故对"回到 A"的可逆性为低 |
| 关联 S 项 | 处置结论：**S-01、S-02、S-03**；连带影响：S-06、S-07、S-08、S-14 |
| 关联 D 项 | D5（`SetWindowDisplayAffinity`）、D13（env 全量继承，与本决策无关） |
| 结论 | **选方案 B（顶层沉底）**。方案 A 不成立：其"能用"的版本必须依赖 `AttachThreadInput`，而该调用已在 `security-input-boundary.md` §2.4 判为禁止引入 |

---

## 0. 结论摘要

| 项 | 判定 | 理由（一句话） |
|---|---|---|
| 选哪个 | **方案 B：本进程顶层 `WS_POPUP` + `WS_EX_TOOLWINDOW` + `WS_EX_LAYERED` + z-order 沉底** | 把窗口的输入权威、生命周期权威、坐标权威全部收回本进程；S-02 由"加固"变"消除" |
| 否决哪个 | **方案 A：保持 `WS_CHILD` + `AttachThreadInput`/`SetFocus` 抢焦点** | A 的前提在 Win32 上不成立，唯一补救手段是禁用项；等于用一个输入完整性缺陷换一个可用性缺陷 |
| 功能交换 | "Win+D 不消失" → "Win+D 隐藏后 ≤1.5s 自恢复可见，且不抢前台" | 用户可感知结果一致（回到桌面看得见终端），安全代价从**跨进程权威共享**降为**一次本进程自恢复动作** |
| 运行时开关 | **不设**（不加 `desktop_layer = workerw\|bottom`） | config 可写 = 执行策略可改；开关会把 S-02 的攻击面原样接回来，并制造两个生命周期 owner |
| S-01 | **不因本决策关闭**；新增 **S-01b（恢复可见性看门狗）** | B 引入系统主动隐藏通道，恢复失败即退化为"看不见的提权 shell"，正是 S-01 的核心场景 |
| S-02 | **关闭（设计消除）**，条件是整条挂载路径退役而非留用 | 无挂载则无归属判定；连带关闭 S-02b/c 与 F-01~F-04 |
| S-03 | **保持原状，按 M1 独立关闭**，与本决策解耦 | 进程创建路径与窗口父子链无关；不得因"不再是 explorer 子窗口"放宽 `env` 白名单或 `USERPROFILE` 强制 |
| 连带 | S-07/D5 从"重写/不可达"变"可落地"；S-14 降级为纯 z-order 重申；S-08 的 `space` 字段取消 | 见 §3、§4 |

---

## 1. 证据基线

| 文件 | 行 | 事实 |
|---|---|---|
| `src/win/window.rs` | 81-82 | `style = WS_POPUP`、`ex_style = WS_EX_TOOLWINDOW \| WS_EX_LAYERED`、`hWndParent = None` —— **B 所需的顶层形态已经是现成默认值，A 是在此之后把它改成子窗口** |
| `src/win/window.rs` | 107 | `show()` 用 `SW_SHOWNOACTIVATE` + `UpdateWindow`，首次出现不抢焦点 |
| `src/win/workerw.rs` | 106 | `find_workerw` 三级兜底只按类名筛选，未校验工作区覆盖 |
| `src/win/workerw.rs` | 169 | 第一次 `SetParent(hwnd, Some(workerw))` 只判 `Ok/Err` |
| `src/win/workerw.rs` | 179-188 | 父为 `None` 时把 `GWL_STYLE` 从 `WS_POPUP` 改成 `WS_CHILD` 后再次 `SetParent`，**返回值被 `let _` 丢弃** |
| `src/win/workerw.rs` | 196-206 | `SetWindowPos(hwnd, HWND_TOP, …, SWP_NOMOVE\|SWP_NOSIZE\|SWP_NOACTIVATE)`，故意带 z-order |
| `src/win/workerw.rs` | 219 | 回读 `parent_of` 只写 `diag::info`，**无条件返回 `Attach::Attached`** |
| `src/win/workerw.rs` | 26-34 | `Attach` 只有 `Attached`/`Fallback`，缺"样式已改但未挂上"第三态 |
| `src/app.rs` | 636 | `reattach` 直接复用 `workerw::attach`，无去抖、无新父校验、失败不重试 |
| `src/app.rs` | 1237 | `TaskbarCreated` 是**唯一**的重挂触发点 |
| `src/app.rs` | 1147-1151 | `WM_LBUTTONDOWN` 里 `SetForegroundWindow(hwnd)`，注释自称"点一下就把焦点拿回来" |
| `src/app.rs` | 1241-1250 | `WAKE_MESSAGE` → `ShowWindow(SW_SHOW)` + `SetForegroundWindow(hwnd)` |
| `src/app.rs` | 1272 | `current_modifiers` 用 `GetAsyncKeyState` 读全局物理键态（注释已承认线程键态未必更新） |
| `src/app.rs` | 1390-1393 | 必须 `attach` 先于 `layered::set_opacity`，注释记录 M2 踩过的"屏幕全空"事故 |
| `src/win/layered.rs` | 1-11 | 现状是 `SetLayeredWindowAttributes(LWA_ALPHA)` 整窗 alpha + `SetWindowRgn(CreateRoundRectRgn)` 硬边圆角；注释明确"逐像素 alpha 只能走 `UpdateLayeredWindow`" |
| `src/win/single.rs` | 1-5 | `hwnd.txt` 存在的原因就是"子窗口 `FindWindowW` 永远找不到"——跨进程父子链直接催生了一个文件信任依赖 |
| `docs/design/…security-architecture.md` | §1 | B1 输入/IME、B2 窗口消息；不变式①"B1/B2 → B3 是唯一命令注入点" |
| 同上 | §2 | 降级路径只有一行："WorkerW 挂载失败 → 普通无边框窗口（分辨率最高、行为最可预测）" |
| 同上 | §4 | D5："`SetWindowDisplayAffinity` 对子窗口无效，如实写 README" |
| 同上 | §6 | S-01 冻结；S-02 待落；S-03 待落；S-07 重写；S-14 广播重挂风暴 |
| 同上 | §8 | M2 验收含"同 IL 注册 Progman/WorkerW 同名类窗口 → 不挂载"与"广播 ×10 只重挂 1 次" |
| `docs/aegis/plans/…desktop-terminal.md` | §1.1 | R1「挂载后父窗口是 WorkerW」、R2「Win+D 后截屏能看到终端内容」是硬验收 |
| 同上 | §5.3 | 不变量 1「窗口父句柄必须是 WorkerW」；不变量 2「不加 `WS_EX_NOACTIVATE`」 |
| 同上 | §7 | 「Win+D 后焦点丢失，需点击才能继续输入。系统行为，不可绕过」 |
| 同上 | §9 M2 | 三级兜底 + `SetParent` + `GetClientRect(WorkerW)`；承认"坐标语义从屏幕坐标变为 WorkerW 客户区坐标" |

---

## 2. 决策：方案 B

### 2.1 A 的前提在 Win32 上不成立

方案 A 的描述是"保持子窗口 + `SetFocus` 抢焦点"。这条路径不可实现：**键盘焦点属于线程输入队列**，`SetFocus` 只能把焦点设给与调用线程同一输入队列的窗口。窗口一旦被 `SetParent` 挂到 explorer 的窗口树（`workerw.rs:169`、`:188`），它所在的输入队列就是 explorer 的 UI 线程，本进程调用 `SetFocus` 会失败（返回 NULL）。同理，`app.rs:1150` 的 `SetForegroundWindow(hwnd)` 与 `app.rs:1246` 的唤醒路径对子窗口都是**无操作**——子窗口不能成为前台窗口。今天的症状因此不是"焦点不稳"，而是"点击与唤醒静默失效"，最容易被误判成渲染卡死。

结论：A 只有两种状态——**焦点不可靠**，或**引入 `AttachThreadInput`**。没有第三种。

### 2.2 唯一能让 A 可用的手段是禁用项

`AttachThreadInput` 把本进程 UI 线程的输入队列与 explorer 的合并。后果全部落在 B1 上，而 B1 是唯一命令注入点的上游：

- `GetAsyncKeyState`（`app.rs:29`、`app.rs:1272`）读到的是**共享键态**，explorer 侧的处理（菜单模式、Alt 单按键、粘滞键）会被本进程误读成用户修饰键，剪贴板粘贴路径的 `Ctrl+V` 语义随之可被污染；
- 反向同样成立：本进程的键态对 explorer 可见，`SendInput` 的投递目标可在两窗口间漂移；
- 队列共享意味着**对方的阻塞成为我方输入停滞**（explorer 短暂无响应时终端打不出字）；
- 队列合并后焦点归属由"共享后的队列"决定，等于把"我是否被聚焦"交给一个不归本进程管理的调度器。

`security-input-boundary.md` §2.4 已经就同一条给出"禁止引入"的结论，本决策引用而不重复：**A 的可用版本必须走进禁区，故 A 被否决**。附带一句：`F-10`（`GetAsyncKeyState` 读修饰键）本身也有替代实现（`WM_KEYDOWN` 的 `lParam` bit 29 + 自绘 focus ring），但那条修复的价值恰恰是**不引入 ATI**——把它当成 A 的可行性论据是反向使用。

### 2.3 B 把权威收回本进程，是"消除"而非"加固"

S-02 属于**归属判定**类问题。判定型加固永远留有一个可被挤压的窗口：`workerw.rs:106` 选出候选、`:169` 才 `SetParent`，两者之间没有 `IsWindow` 复查、没有 HWND 复用防护、没有工作区覆盖校验，`:188` 的第二次 `SetParent` 连返回值都丢弃，`:219` 无条件宣告成功。即便把这三项全部补齐，判定仍在"跨进程句柄 + 时间窗"里做，本质是 CWE-367 的结构。

B 把这条判定整条删掉：没有候选、没有归属比较、没有挂载动作，攻击者无法通过"注册同名窗口类"影响本进程的窗口树，因为本进程从不查询别人的窗口树。这是设计级消除，可用一条静态断言守住（§4 V-9）。

顺带三项收益：

1. **前景与焦点诚实**：`GetForegroundWindow`/`GetFocus` 的语义恢复为"本进程窗口是否真的在前"，S-06 的判定第一次可以落实验收，而不是"看起来设过"。
2. **D5/S-07 从不可达变可落地**：`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 对顶层窗口有效，对子窗口无效（设计文档 §4 已如实记录）。
3. **坐标与 DPI 自洽**：`GetClientRect(WorkerW)` + explorer 客户区坐标系（计划书 §9 M2 已承认的语义变化）是"110 列恒等"这类网格不变量的直接威胁——跨进程父子窗口在双方 DPI 感知不一致时会被系统重解释，字体度量与 DIB 尺寸随之错配。B 下客户区就是屏幕坐标，DPI 只有本进程一个权威。

### 2.4 方案 A 的失败模式

| 编号 | 失败模式 | 触发条件 | 后果 | CWE |
|---|---|---|---|---|
| A-1 | 焦点前提不成立 | 任何一次 `SetParent` 成功 | 键盘输入不可靠；点击与唤醒静默失效 | CWE-653 |
| A-2 | 被迫使用 `AttachThreadInput` | 为修 A-1 | 输入队列共享，B1 被污染/被观测，输入停滞 | CWE-653、CWE-441、CWE-362 |
| A-3 | 归属判定可被冒充 | 同 IL 进程注册 `Progman`/`WorkerW` 同名类 + 复制 `explorer.exe` basename（`workerw.rs:85`） | 输入改投、`WM_*` 被截获、生命周期被第三方持有 | CWE-290、CWE-345、CWE-940、CWE-668、CWE-1021 |
| A-4 | 挂载状态不可信 | `SetParent` 返回 `Ok` 但被系统拒绝（跨进程常见） | `Attached` 与实际不符 → 尺寸/分层/自检全部基于错误前提，表现为空白或错位 | CWE-367、CWE-672、CWE-682 |
| A-5 | explorer 重启即失父 | 外壳重启、崩溃、更新 | 子窗口随父销毁或成孤儿；进程与（可能提权的）shell 继续存在但无可用窗口 → **放大 S-01** | CWE-672、CWE-664 |
| A-6 | 分层顺序脆弱 | 每次 `attach`/`reattach` 后必须重设分层属性（`app.rs:1390-1393` 已记录事故） | 任一遗漏即"屏幕全空"；故障态与"进程已退出"不可区分 | CWE-682 |
| A-7 | 视觉规格被锁死 | 子窗口分层只支持整窗 alpha（`layered.rs:1-11`） | `UpdateLayeredWindow` 逐像素 alpha 与抗锯齿圆角永久不可达，UI §2.6 的升级路径被封 | — (设计冻结) |
| A-8 | 派生文件信任 | `FindWindowW` 找不到子窗口（`single.rs:1-5`） | 引入 `hwnd.txt`：可写文件决定"唤醒谁"，成为 B6 之外的新信任依赖 | CWE-73、CWE-367 |

### 2.5 方案 B 的新失败模式（必须由设计承接）

| 编号 | 失败模式 | 说明 | 承接要求 |
|---|---|---|---|
| B-1 | 自恢复失败 → 隐形运行 | Win+D 隐藏后恢复逻辑失败，提权 shell 继续跑且无痕 | §3 的 **S-01b 看门狗**：限次尝试 + `diag` + 可见时叠加 attention 环直至下一次按键 |
| B-2 | 无条件沉底与激活冲突 | 用户点击终端 → 系统将其升起 → 若在 `WM_WINDOWPOSCHANGING` 里无条件压回 `HWND_BOTTOM`，窗口在点击瞬间"沉到别的窗口后面"，表现为"点了就没了" | L3 的 z-order 状态机：**未激活/失焦才重申沉底**；激活期间允许升到同级顶层之上；永不 `HWND_TOPMOST` |
| B-3 | 恢复与系统互相拉扯 | 恢复 → 系统再次最小化 → 再恢复，形成循环 | L4：限速 ≥300ms、最多 3 次、失败即停并置 `resume_failed` |
| B-4 | 恢复抢焦点（S-06 回归） | 用 `SW_SHOW` + `SetForegroundWindow` 恢复会打断用户输入 | L5：恢复/唤醒一律 `SW_SHOWNOACTIVATE`；仅当 `GetForegroundWindow()` 已属本进程时才允许置前 |
| B-5 | 唤醒目标可被冒充 | 退役 `hwnd.txt` 后若改用类名查找，同 IL 进程可注册同名类并截走 `WAKE_MESSAGE` | §4 V-8：查找后必须校验目标进程镜像路径与本进程一致，不一致则不投递、不唤醒 |

### 2.6 B 的实现不变量（写进设计文档，不是建议）

- **L1** 窗口恒为本进程顶层窗口；`GetParent(hwnd) == NULL`；**代码中不得出现 `SetParent` 调用**（由 V-9 静态断言守住）。
- **L2** 不加 `WS_EX_NOACTIVATE`（沿用计划书 §5.3 不变量 2，并升级为 B 的核心不变量——激活是输入的唯一自然入口）。
- **L3** z-order 状态机：未激活/失焦 → 沉底（在桌面之上、普通窗口之下）；激活 → 允许升至同级顶层之上；失焦 → 回底；任何情况不得 `HWND_TOPMOST`。
- **L4** Win+D：仅在收到 `WM_SIZE(SIZE_MINIMIZED)`（本程序无用户最小化入口）时调度一次恢复；限速 ≥300ms、最多 3 次；失败置 `resume_failed` 并留痕。
- **L5** 恢复与唤醒禁止 `SetForegroundWindow`，除非前台已属本进程。
- **L6** 分层仍走现状（`LWA_ALPHA` 整窗 alpha + `SetWindowRgn` 圆角）。逐像素 alpha 是独立任务，**不在本次迁移里做**——一次只动一层。
- **L7** `TaskbarCreated` 只做 z-order 重申与分层属性重放，不做任何信任决策；去抖 ≥2s，单次尝试。
- **L8** 提权可见性不受恢复路径影响：恢复后必须重绘 `elevated_ring`，alpha 固定 1.0（S-01 冻结决策不变）。

### 2.7 明确不采纳的折中

| 折中 | 否决理由 |
|---|---|
| 运行时开关 `desktop_layer = workerw\|bottom` | config 可写即可执行策略可改，等于把 S-02 的攻击面重新暴露给一个更弱的位置；同时制造两个窗口生命周期 owner |
| `GWLP_HWNDPARENT = Progman`（只设 owner，不改 style） | 仍是跨进程所有权句柄；Win+D 分组行为未验证（U-03 未结）；只允许未来作为"纯 z-order 提示"单独立项，且不得承载输入/生命周期语义 |
| 保留挂载代码、只是"默认不调用" | 死路径即"看起来能用"的诱因，下一个里程碑一定会被复用；退役即删除 |

---

## 3. S-01 / S-02 / S-03 处置结论

| S 项 | 处置 | 关闭条件 | 残余 |
|---|---|---|---|
| **S-01**（提权继承且不可见） | **维持冻结决策，不因层决策修改内容**（常驻 `elevated_ring` + `ADMIN`、固定 L4、alpha 1.0、不随 opacity 稀释）；**新增 S-01b** | S-01 仍按 M4 关闭；S-01b 需 L4 看门狗落地 + V-6/V-7 通过 | B 引入"系统主动隐藏"通道：恢复失败 = 看不见的提权 shell。这是 S-01 的高危场景，必须由看门狗覆盖，不能以"系统行为"了结 |
| **S-02**（WorkerW 母窗口冒充） | **关闭（设计消除）**。条件：`workerw::attach` / `find_workerw` / `is_desktop_owner` / `owner_process_name` 整条路径删除；`app.rs:636` 的 `reattach` 改写为 z-order 重申；`Attach` 枚举与 `parent_of` 一并退役 | 删除 + V-9 静态断言通过 | S-02b/c 与 F-01~F-04 随之关闭。若 v2 要真正的"壁纸层"，必须作为**新决策**重新引入并重开 S-02，不得在本次框架内悄悄复活 |
| **S-03**（`CreateProcessW` 路径 / env / cwd） | **与本决策解耦，保持原计划 M1 独立关闭** | 沿用 `security-input-boundary.md` §3 的绝对路径 + `env_clear` + 白名单 + 固定 `%USERPROFILE%` + `cmd /D` | 无新增。唯一提醒：不得因为"窗口不再是 explorer 子窗口"而推断"继承风险消失"——两者无关 |
| S-07 / D5 | 从"重写/不可达"变"可落地"：M4 启用 `WDA_EXCLUDEFROMCAPTURE`，README 表述从"无法防录屏"改为"已排除主流截屏 API，不影响外部摄像头" | M4 | `WS_EX_TOOLWINDOW` 意味着不进任务栏/Alt+Tab，"驻留可见性"仍需 ring + README 明示 |
| S-14 | **降级**：广播只触发 z-order 重申，不再做归属判定；去抖 ≥2s 保留（防 CPU 抖动，不再是信任决策） | M5 | 残留为可用性/DoS 级别 |
| S-08 | `space` 字段**取消**：只有屏幕坐标一种空间；历史值读入时忽略并按屏坐标重解释 | M5 | 位置持久化在 DPI/显示器变更下的钳制仍需实测 |

---

## 4. 迁移必须同步更新的验收项

| 编号 | 文档 / 条目 | 现内容 | 迁移后 | 判定手段 |
|---|---|---|---|---|
| V-1 | 计划书 §1.1 **R1** | 「挂载后父窗口是 WorkerW，不是桌面根窗口」 | 「窗口为顶层（`GetParent == NULL`）、不进任务栏与 Alt+Tab、z-order 恒在普通窗口之下」 | 句柄断言 + z-order 序数比较 |
| V-2 | 计划书 §1.1 **R2** | 「Win+D 后截屏能看到终端内容」 | 「Win+D 后 **≤1.5s 自动恢复可见**，截屏可见终端内容」 | 截屏（定时）+ 计时 |
| V-3 | 计划书 §1.1 新增 **R2'** | — | 「恢复过程不抢前台：恢复前后 `GetForegroundWindow()` 不变」 | 前台句柄前后对比 |
| V-4 | 计划书 §5.3 不变量 1 | 「窗口父句柄必须是 WorkerW」 | **反转为**「不得存在父窗口；出现 `SetParent` 调用即为缺陷」 | V-9 |
| V-5 | 计划书 §5.3 不变量 2 | 「不加 `WS_EX_NOACTIVATE`」 | 保留，并标注为"输入唯一入口" | 样式断言 |
| V-6 | 计划书 §7 兼容边界 | 「Win+D 后焦点丢失，需点击才能继续输入」 | 「焦点原生可用；Win+D 后经自恢复回到可输入态，不抢前台」 | 手动 + 前台句柄断言 |
| V-7 | 计划书 §9 M2 验收 | ①Win+D 可见 ②点击可输入 ③拖动 ④缩放 ⑤失败降级 | **⑤删除**（无挂载即无降级）；新增 ⑥z-order 恒底（新建普通窗口后仍在其下）⑦explorer 重启后 z-order 自恢复 ⑧同 IL 注册 `Progman`/`WorkerW` 同名类窗口对本窗口无任何影响 | 逐项 |
| V-8 | 计划书 §9 M5 + `single.rs` | `hwnd.txt` 写文件 + `IsWindow` + `PostMessage` | `hwnd.txt` **退役**；改类名查找 + **目标进程镜像路径与本进程一致**校验；不一致则不投递、不唤醒 | 伪造同名类窗口 → 不唤醒 |
| V-9 | 新增架构守卫（`cargo test`） | — | 扫描 `src/` 断言无 `SetParent` 调用，`agent` 侧无 `AttachThreadInput` | `tests/` 静态断言，纳入 `scripts/gate.ps1` 的 `cargo test` 段 |
| V-10 | 设计文档 §2 降级路径 | 「WorkerW 挂载失败 → 普通无边框窗口」 | 「层策略唯一：顶层沉底。无挂载分支、无降级分支」 | 文档一致性 |
| V-11 | 设计文档 §1 **B1** | 「输入/IME（入，用户可控…）」 | 追加：「B1 的输入源自本进程窗口的输入队列；**不接受跨进程队列共享**」 | 文档 + V-9 |
| V-12 | 设计文档 §1 **B2** | 「窗口消息（入，不可信…）」 | 追加：「B2 的放大器（跨进程父链提供的确定性枚举锚点）已随层决策消除」 | 文档 |
| V-13 | 设计文档 §8 M2 验收 | 「同 IL 注册 Progman/WorkerW 同名类窗口 → 不挂载，降级普通窗口」 | 「无挂载路径：注册同名类窗口后本窗口 `parent` 仍为 `NULL`，不产生任何子窗口依赖」（保留为回归守卫） | V-7⑧ + V-9 |
| V-14 | 设计文档 §6 | S-02「待落」、S-14「重挂风暴」、S-08「`space` 字段」、S-07「重写」 | S-02 → 「已消除（设计），条件见 §3」；S-14 → 「降级为 z-order 重申」；S-08 → 「`space` 取消」；S-07 → 「可落地」；新增 S-01b | 文档一致性 |
| V-15 | 设计文档 §9 评审边界 | 「不评估 WorkerW/SetParent 跨版本兼容性」 | 「不评估顶层沉底与第三方壁纸/桌面整理程序的视觉叠加顺序」（纯外观，不影响安全） | 文档 |
| V-16 | 设计文档 §11 台账 | M2 验收补「父窗口必须属 explorer.exe」 | M2 验收补「`parent == NULL` 且 z-order 在普通窗口之下」；M5 补 V-8 的进程校验 | 文档 |

---

## 5. 决策边界与遗留未知

- **U-03 收束**：`security-input-boundary.md` 留下"Win+D 免疫替代实现可行性未验证 → 决定能否切方案 B"。本决策**解除该前置依赖**：切 B 不以"功能等价"为前提，而以"可见性可观测等价"为前提（V-2/V-3）。原 U-03 降级为"若未来要求绝对不消失"的可选研究项，且其正确方向是 desktop-level 宿主（v2 独立决策），不是回到 A。
- **U-04（新）**：`WM_SIZE(SIZE_MINIMIZED)` 在本程序无任务栏按钮、无最小化框的前提下是否稳定作为"Win+D 发生"的判据，需 M2 实测（含 Win+D 连按、Win+逗号预览、任务视图切换）。
- **U-05（新）**：L3 的 z-order 状态机与第三方壁纸/桌面整理/多桌面工具同时运行时的可见性交互，未验证；只影响视觉，不进入信任边界。
- **U-06（新）**：`WDA_EXCLUDEFROMCAPTURE` 在旧版 Windows 的降级行为（返回失败而非静默降级）需实测，失败必须留痕而不是假装生效。

---

## 6. 台账同步清单

| 文档 | 动作 |
|---|---|
| `docs/aegis/plans/2026-10-22-tlshell-desktop-terminal.md` | §1.1（R1/R2 改判，新增 R2'）、§5.3（不变量 1 反转、不变量 2 升级）、§7（焦点语义）、§9（M2 验收删改增、M5 唤醒改造） |
| `docs/design/2026-10-22-tlshell-security-architecture.md` | §1（B1/B2 追加）、§2（降级路径删除）、§4（D5 改判）、§6（S-02/S-07/S-14/S-08 改判，新增 S-01b）、§8（M2 验收改写）、§9（边界改写）、§11（台账） |
| `README.md` | 焦点语义、Win+D 行为、D5/S-07 表述 |
| 代码（后续里程碑） | `workerw.rs` 整文件退役、`app.rs:636` 改写、`single.rs` 唤醒改造、`window.rs` z-order 状态机 —— **本评审不改源码** |
