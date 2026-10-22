# 代码审查：输入路径（SetParent 到 WorkerW 后的焦点与键盘）

- 审查对象：`D:\Code\Rust\TLShell`（**未修改任何源码**）
- 审查范围：`src/app.rs`、`src/win/window.rs`、`src/win/workerw.rs`、`src/win/layered.rs`、`src/term/session.rs`、`src/term/input.rs`、`src/win/single.rs`、`src/render/{cursor,painter}.rs`（焦点视觉）
- 审查重点：正确性（激活/焦点、样式状态机、输入去重、分层合成），不涉及风格
- 方法：静态阅读 + 运行期只读实测（运行中的 `target/debug/tlshell.exe`，16:06 构建，晚于 `src/app.rs` 16:05、`src/win/workerw.rs` 15:56，与被审代码一致）
- 结论摘要：症状成立、可定位。根因是一个**完整缺失的焦点链路**（B1/B2）；父子关系的建立过程**靠失败当信号**，检测判据与它要防的场景互相矛盾（B3）；两处输入去重会真实吞字符（B4/B5）；另有启动期 `&'static mut` 重入 UB（B6）与焦点视觉恒定“有焦点”掩盖问题（B7）。

## 一、运行期实测证据

探针（可重跑）：`target/probe/probe-window.ps1`、`target/probe/probe-focus.ps1`

| 观测项 | 实测值 | 含义 |
| --- | --- | --- |
| 窗口 / 进程 | `0xE10B2` / pid 10428 | 被审实例 |
| `GWL_STYLE` | `0x54000000`（`WS_CHILD=1`, `WS_POPUP=0`, `WS_VISIBLE=1`） | 样式改写**已执行**，当前是真子窗口 |
| `GWL_EXSTYLE` | `0x00080080`（`LAYERED`, `TOOLWINDOW`，**无 `NOACTIVATE`**） | 未声明不可激活，见 B2 |
| `GetParent` / `GA_PARENT` | `0x104EC` `WorkerW` pid=15236(explorer) | 真父窗口，不是 owner |
| `GA_ROOT` / `GA_ROOTOWNER` | `0x104B6` `Progman` pid=15236 | 根祖先属 explorer 线程 |
| `GW_OWNER` | `null` | 确认不是“popup + owner”的降级态 |
| 窗口线程 vs 父线程 | 19860（本进程） vs explorer | **跨线程父子**；`SetParent` 不迁移窗口线程 |
| `GetForegroundWindow` | `Chrome_WidgetWin_1` pid 42356 | 前台从未落到 TLShell 相关窗口 |
| `GetGUIThreadInfo(19860)` | `hwndActive=NULL`, `hwndFocus=NULL`, `hwndCapture=NULL` | **本线程输入队列既无 active 也无 focus** → 键盘无可投递对象 |

日志 `%LOCALAPPDATA%\TLShell\logs\tlshell.log` 佐证：`attach 回读: 期望父窗口=0x104ec, 实际父窗口=Some(0x104ec), visible=false` → `已挂到 WorkerW（progman-child）`；启动时一条 `WM_ACTIVATE wparam=1`、退出时 `wparam=0`，点击期间没有任何激活事件。

## 二、逐项判定

### 1) 激活/焦点链路：缺 3 步，且第一步的语义选错（判定：确认）

`WM_MOUSEACTIVATE` 返回 `MA_ACTIVATE`（`src/app.rs:1099-1102`，只打一行日志）只表示“照常做默认激活”。默认激活的对象是命中的**根祖先**——`Progman`，属 explorer 的输入队列；而我们的 hwnd 属本进程线程 19860 的另一个队列。焦点是**输入队列内部**的概念，实测该队列 `hwndActive=NULL`、`hwndFocus=NULL`，因此后续 `WM_KEYDOWN`/`WM_CHAR` 无处投递，`WM_SETFOCUS` 分支（`src/app.rs:1129-1140`）从未进入（日志中确实没有它的痕迹）。

`MA_ACTIVATE` 之后还差的步骤（按最小可用序）：

1. `AttachThreadInput(GetCurrentThreadId(), GetWindowThreadProcessId(GetForegroundWindow()), true)` —— 把本队列并入前台队列；否则在“非前台进程”限制下 `SetForegroundWindow` 被忽略。
2. `SetForegroundWindow(GetAncestor(hwnd, GA_ROOT))` —— 必须让**根**成为前台。`src/app.rs:1150` 直接对子窗口调 `SetForegroundWindow(hwnd)` 是 no-op（该 API 只对顶层窗口生效），实测前台仍是 Chrome。
3. `SetActiveWindow(hwnd)` + `SetFocus(Some(hwnd))` —— 让本队列产生 active/focus，`WM_SETFOCUS`/`WM_KILLFOCUS` 才会真正流动。

缺任一步都会复现当前症状：只补 `SetForegroundWindow` 无效（第 2 步对子窗口本来就不生效），只补 `SetFocus` 也无效（队列未激活/未挂接时跨队列 `SetFocus` 失败）。全仓库 `grep` 确认 `SetFocus`/`AttachThreadInput`/`SetActiveWindow`/`GetAncestor` **一个都没有**；`SetCapture`（`src/app.rs:524`）只用于选区。

### 2) `WS_POPUP`→`WS_CHILD` 改写：样式位干净，检测判据自相矛盾（判定：部分确认，矛盾点不在样式位）

样式位本身没留下矛盾状态（实测 `WS_CHILD=1, WS_POPUP=0, GW_OWNER=null`）。问题在**它为什么成立**：

`src/win/workerw.rs:169` 先在 `WS_POPUP` 状态下 `SetParent`；`:177-182` 的注释已经写明“系统对弹出窗口的 `SetParent` 会解释成设置 owner”，于是用了“回读父窗口为空”作为改写触发条件（`:179`）。但这两件事互斥：**若系统真按注释那样设了 owner，`GetParent` 就会返回 owner（非空）**，`parent_of(hwnd).is_none()` 为假 → `:184-189` 的样式改写被跳过 → 窗口永久停在 `WS_POPUP + owner`（owner 依然会被 Win+D 最小化，正是注释要避免的结果）。本次实测能挂成功，是因为这台机器上首次 `SetParent` **什么都没改**（`GetParent` 回 NULL），才让 `is_none()` 成立——即“靠失败当信号”。

且 `parent_of` 用 `GetParent`（`src/win/workerw.rs:223-231`），对非 `WS_CHILD` 窗口返回的是 **owner**，无法区分 parent/owner，天生不能做这个判据。`:210-219` 的回读只 `diag::info` 打印，`:220` **无条件** `return Attach::Attached`——判据失败也没有任何降级路径。

次要项：`:184-189` 改样式后没有 `SWP_FRAMECHANGED`（框架缓存不刷新）；`:196-206` 的 `SetWindowPos(HWND_TOP, ... | SWP_NOACTIVATE)` 故意不带 `SWP_NOZORDER`（`:191-194` 注释说明是为了压过 Wallpaper Engine 的窗口），这是有意设计，**不是缺陷**，但意味着与其它桌面挂件共存时会争 z 序。

最小修复方向：先把 `WS_POPUP` 换成 `WS_CHILD`（`SetWindowLongPtrW` 后补一次 `SetWindowPos(..., SWP_FRAMECHANGED|SWP_NOMOVE|SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE)`），再 `SetParent`；成功判据换 `GetAncestor(hwnd, GA_PARENT) == workerw`，不满足返回 `Attach::Fallback` 并把实读状态写进日志。

### 3) `WM_CHAR` / `WM_IME_CHAR` 去重：会吞字符，两条独立路径（判定：确认）

- **keydown 侧标记跨消息存活**：`src/app.rs:765` 对所有 `input::encode` 成功的按键置 `suppress_next_char = true`，但方向键 / `Home` / `End` / `Del` / `Ins` / `PgUp` / `PgDn` / `F1-F12` / Alt 组合**永不产生 `WM_CHAR`**，标记便留到下一次真实字符，被 `src/app.rs:770-771` 吃掉。稳定复现：按 `↑` 再打 `a`，`a` 丢失。该标记本已冗余——控制字符在 `src/app.rs:778` 就被丢弃了。
- **IME 侧抑制对象常常不存在**：`src/app.rs:1178-1187` 自行 `send_char` 后置标记（`:1185`）并 `return Some(0)` 跳过 `DefWindowProc`。它要防的“随后那条 `WM_CHAR`”多数情况并不产生，标记又一次留给下一个无关字符；多字上屏只抑制一条；非 BMP 字符在 `char::from_u32` 失败处（`:1180` 附近）直接返回，整字丢弃。
- 附带：`WM_CHAR` 的 `wparam` 是 **UTF-16 code unit**，`src/app.rs:769-784` 按 `char::from_u32` 处理，代理对无法拼回（现状靠“丢弃”兜底）。

最小修复方向：去掉跨消息的 `suppress_next_char`，改为**按键作用域**去重（`WM_KEYDOWN` 里记录 `(vk, 期望字符)`，只消费紧随同一次按键后的那一 `WM_CHAR`）；`WM_IME_CHAR` 分支不置抑制位，让 `DefWindowProc` 完成合成；非 BMP 先累积代理对再发送。

### 4) 分层属性与子窗口合成：顺序不是问题，重放不对称与父窗口选择才是（判定：部分推翻原假设）

- `WS_EX_LAYERED` 子窗口在 Win8+ 受支持，`LWA_ALPHA`（`src/win/layered.rs:34-50`）与 `SetWindowRgn`（`:69-108`）都可用；子窗口不可用的是 `UpdateLayeredWindow`，当前代码未用。
- 坐标单位已核对一致：`window::client_rect` 是物理像素，`radius` 经 `dpi::scale(..., window_dpi)`（`src/app.rs:409-413`）同为物理像素，130dpi 下裁剪区域不会偏小——**这不是问题**。
- `src/app.rs:1390-1393` 的“先父子、后分层”注释记录的是一条实测约束（“反过来做窗口变成子窗口后不再参与合成，实测如此”），`run()`（`:1395-1408`）和 `reattach()`（`:636-661`）都遵守了。隐患是它只是**隐式不变量**：`CreateWindowEx` 时就带上 `WS_EX_LAYERED`（`src/win/window.rs:82-83`），任何在 `attach` 之前调用 `set_opacity` 的未来路径都会让窗口变成子窗口后**不可见**，且 `ensure_layered`（`src/win/layered.rs:22-32`）是静默 `SetWindowLongPtrW`、无 `SWP_FRAMECHANGED`、无断言。
- 真隐患：① `run()` 挂载后只重放透明度（`:1406`），未重放尺寸/形状；`reattach()` 两者都重放（`:652-660`）→ 两条路径不对称，explorer 重启后与首次启动行为可能不同。② `find_workerw()` 只按类名 + explorer 归属挑选，未校验可见性/是否覆盖本显示器/是否为“图标之后”那一个；多屏、Wallpaper Engine 自带 WorkerW 时会挂到错误的兄弟窗口（表现为窗口不可见，日志仍报 `已挂到`）。

## 三、阻塞级问题清单

| # | 位置 | 触发条件 | 最小修复方向 |
| --- | --- | --- | --- |
| **B1** 🔴 焦点链路完全缺失 | `src/app.rs:1099-1102`、`:1141-1151`、`:1129-1140`；全仓库无 `SetFocus`/`AttachThreadInput`/`SetActiveWindow`/`GetAncestor` | 任何点击：激活落在 explorer 的 `Progman` 根，本线程队列实测 `hwndActive=NULL`、`hwndFocus=NULL`，键盘输入无从投递 | 按“二.1”四步接管：`AttachThreadInput` → `SetForegroundWindow(GetAncestor(hwnd, GA_ROOT))` → `SetActiveWindow(hwnd)` + `SetFocus(Some(hwnd))` → 解绑 |
| **B2** 🔴 激活策略自相矛盾 | `src/app.rs:1099-1102`（`MA_ACTIVATE`）vs `src/win/window.rs:107-114`（`SW_SHOWNOACTIVATE`） | 每次点击先让系统激活 explorer 的根窗口（壁纸层/任务栏状态抖动），而本窗口从不主动取激活，两者互相抵消 | `WM_MOUSEACTIVATE` 改返回 `MA_NOACTIVATE`，在 `WM_LBUTTONDOWN`（`src/app.rs:1141-1151`）里显式走 B1 序列，把激活时机收回自己手里 |
| **B3** 🔴 父窗口建立不可自证，判据与要防的场景互斥 | `src/win/workerw.rs:169`、`:177-182`、`:179`、`:184-189`、`:210-220`、`:223-231` | 若首次 `SetParent` 真按注释那样设了 owner，`GetParent` 非空 → `is_none()` 为假 → 跳过样式改写 → 永久 `WS_POPUP+owner`，但日志仍打印回读并返回 `Attached`（无降级） | 先改 `WS_CHILD` 再 `SetParent`；判据改 `GetAncestor(GA_PARENT)==workerw`；不满足返回 `Fallback`；补 `SWP_FRAMECHANGED` |
| **B4** 🔴 `suppress_next_char` 吞正常字符 | `src/app.rs:765` 置位 / `:770-771` 消费 / `:778` 已有控制字符过滤 | 先按任一不产生 `WM_CHAR` 的键（↑↓←→、Home/End/Del/Ins/PgUp/PgDn、F1-F12、Alt 组合），再输入普通字符 → 该字符丢失 | 删除跨消息标记；改 `(vk, 期望字符)` 的按键作用域去重；控制字符由 `:778` 继续过滤 |
| **B5** 🔴 `WM_IME_CHAR` 抑制模型错误 + 非 BMP 丢弃 | `src/app.rs:1178-1187`（`:1185` 置位、`:1180` 附近 `from_u32` 失败即返回） | 输入法上屏后紧接的下一个字符可能被吞；多字上屏只抑制一条；emoji/生僻字整字丢失 | 不置抑制位、交回 `DefWindowProc` 完成 `WM_IME_CHAR`→`WM_CHAR` 合成；非 BMP 累积代理对后发送 |
| **B6** 🔴 `&'static mut App` 与 wndproc 重入形成双 `&mut`（UB） | `src/app.rs:1382`（`Box::leak` 得 `&'static mut`）、`:1395`/`:1406`/`:1408`（`attach`/`set_opacity`/`show` 触发同步重入）、`src/app.rs:1349-1351`（`dispatch` 内 `&mut *(ptr as *mut App)`） | 启动与重挂载时必然发生：持有 `&mut App` 的同时 wndproc 又构造一个 `&mut App`。当前未崩不代表无 UB，优化级别/内联变化后可能改变表现 | `Box::into_raw` 只传裸指针，借用收敛到 `dispatch` 内部；或把挂载/显示改为 `PostMessage` 到消息循环之后执行 |
| **B7** 🔴 焦点视觉恒为“有焦点”，掩盖 B1 | `src/app.rs:181`（`focused: true`）+ `src/render/cursor.rs:27-32`（`Blink::new` 里 `enabled: true`）→ `src/render/painter.rs:288`（实心光标）/`:345-346`（边框色） | 启动即渲染有焦点外观，使 B1 在视觉上不可见；同时违反 `docs/aegis/plans/visual-acceptance.md:32` G3 与 `:151` V-26 | 初值改 `false`、`Blink` 初始 `enabled=false`；焦点视觉唯一由 `WM_SETFOCUS`/`WM_KILLFOCUS` 驱动 |

## 四、建议项（非阻塞，但会放大问题或误导维护）

- 🟡 **S1** `SetForegroundWindow` 三处对子窗口无效：`src/app.rs:1150`、`src/app.rs:1246`（`WAKE_MESSAGE`）、`src/win/single.rs:78-79`。目标应改为 `GetAncestor(hwnd, GA_ROOT)` 并配合 `AttachThreadInput`。
- 🟡 **S2** `WS_CHILD` 下 `hit_test` 语义失效风险：`src/app.rs:962-1027` 返回 `HTCAPTION`/`HTLEFT`/`HTTOPRIGHT` 等，但子窗口不参与系统移动/缩放循环，`WM_GETMINMAXINFO` 对子窗口也被忽略。**待实测**：拖标题带看 `GetWindowRect` 是否变化（若不变，则 README 里 M2“拖动与缩放完成”与当前挂载态矛盾）。修复方向：`WM_NCLBUTTONDOWN` 自行实现拖动（`SetCapture` + `WM_MOUSEMOVE` 里 `SetWindowPos`），或按是否已 reparent 切换 hit-test 策略。
- 🟡 **S3** 挂载后重放不对称：`run()`（`src/app.rs:1395-1408`）只重放透明度，`reattach()`（`:636-661`）重放透明度+`apply_size`。建议抽成同一个 `after_attach(hwnd)` 供两处调用。
- 🟡 **S4** `find_workerw()` 选择无校验（`src/win/workerw.rs` 枚举段）：未校验 `IsWindowVisible`、是否覆盖目标显示器、是否存在多个 `WorkerW`（Wallpaper Engine 场景）。
- 🟡 **S5** IME 候选窗定位不足：`src/app.rs:597-621` 只设 `CFS_POINT`，未设 `CANDIDATEFORM`/字体；跨进程子窗口 + 130dpi 下候选窗弹 `0,0` 的风险仍在（`visual-acceptance.md:152` V-27）。
- 🟡 **S6** 退出路径不一致且可能挂死：`src/term/session.rs:224-235` 的 `Drop` 在 UI 线程（`WM_DESTROY`，`src/app.rs:1227-1236`）里无超时 `join` 读线程；而 `Event::Exit`/`ChildExit`（`src/app.rs:697-701`）只 `PostQuitMessage` 不 `DestroyWindow` → `WM_DESTROY` 不跑、`Session` 不 drop。建议统一收尾，并给 `join` 加超时/`detach` 兜底。
- 🟡 **S7** `map_vk` 缺 `VK_OEM_*`（`src/term/input.rs`，仅覆盖 `0x08/09/0d/1b/20/21-28/2d/2e/30-39/41-5A/70-7B`）：`control_code` 里 `[ \ ] ^ _ / @` 分支不可达，`Ctrl+[`、`Ctrl+/` 失效。建议这些控制码统一由 `WM_CHAR` 承载，`WM_CHAR` 只丢弃已由 keydown 处理过的 `0x03/0x08/0x09/0x0d`。
- 💭 **S8** 未处理 `WM_SYSCHAR`：`Alt+字母` 落到 `DefWindowProc`，可能触发响铃。

## 五、未验证 / 需实测项（诚实标注）

1. **拖动与缩放是否失效**（S2）：需在挂载后用鼠标拖标题带，对比 `GetWindowRect` 前后值；本轮只做只读探测，未注入任何输入。
2. **IME 候选窗实际落点**（S5）：需实际输入中文并截图，本轮未执行。
3. **`GetGUIThreadInfo` 快照是“此刻态”**：它证明“现在没有 active/focus”，并与日志“点击期间无激活事件”一致，但没有抓到点击瞬间的时序。要更强证据，可在 `WM_MOUSEACTIVATE`/`WM_ACTIVATE` 分支加一行 `diag::debug` 后复跑点击。

## 六、复核方式

```powershell
# 1) 静态：确认焦点链路 API 仍然缺席
rg -n "SetFocus|AttachThreadInput|SetActiveWindow|GetAncestor" src\

# 2) 运行期：样式 / 父子 / 前台（只读）
pwsh -NoProfile -File target\probe\probe-window.ps1

# 3) 运行期：本线程队列的 active / focus（只读）
pwsh -NoProfile -File target\probe\probe-focus.ps1

# 4) 日志
Get-Content "$env:LOCALAPPDATA\TLShell\logs\tlshell.log" -Tail 40
```
