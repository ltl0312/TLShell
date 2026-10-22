# M6/M7 全量测试证据

执行器：[`scripts/run-all.ps1`](../../../../scripts/run-all.ps1)（四层，判定分离）。

```powershell
pwsh -NoProfile -File .\scripts\run-all.ps1 -Rounds 6
```

退出码：`0` 全通过 / `1` 有 FAIL / `2` 有 BLOCKED（环境缺失不算通过）。

## 分层

| 层 | 内容 | 项数 |
|---|---|---|
| A 静态门禁 | `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` | 3 |
| B 无界面自检 | `--selftest --report`，逐项断言 `ok`（12 项，含 IME 结果串空值），并断言退出码 | 2 |
| C 真机功能探针 | `--probe-input --dump-frame`：日志链路 14 项 + 落盘像素 5 项 + 单实例 1 项 + 设置面板 4 项 + 唤起热键 1 项 | 25 |
| D 行为负例 | `--quit` 真退出、`--restart-unelevated` 非提权退 0、未知参数拒绝、坏配置仍启动、缺 `USERPROFILE` 拒绝启动、白/黑壁纸对比度 | 6 |
| E 真实输入（可选，`-RealInput`） | `--probe-real`：真实点击命中自己、拿到焦点、拖选出选区、按键写进 PTY、**敲出整词 `ipconfig`**、归还前台 | 6 |

合计 **42 项/轮**（含 E 层时）。C 层只往自己的窗口 `PostMessage`（不碰 `SendInput`/`mouse_event`），
结束时把前台还给原窗口，因此可以在用户正常工作时不打断地跑。

## 结果

| 轮次 | 参数组合（`opacity` / `padding` / `radius` / `font`） | 结论 | 报告 |
|---|---|---|---|
| 1 | 0.82 / 8 / 6 / 13 | PASS 42 / FAIL 0 / BLOCKED 0 | [round-1.md](round-1.md) |
| 2 | 0.84 / 10 / 8 / 14 | PASS 42 / FAIL 0 / BLOCKED 0 | [round-2.md](round-2.md) |
| 3 | 0.86 / 12 / 10 / 15 | PASS 42 / FAIL 0 / BLOCKED 0 | [round-3.md](round-3.md) |
| 4 | 0.88 / 14 / 12 / 16 | PASS 42 / FAIL 0 / BLOCKED 0 | [round-4.md](round-4.md) |
| 5 | 0.90 / 16 / 14 / 17 | PASS 42 / FAIL 0 / BLOCKED 0 | [round-5.md](round-5.md) |
| 6 | 0.92 / 18 / 16 / 18 | PASS 42 / FAIL 0 / BLOCKED 0 | [round-6.md](round-6.md) |

每轮用 `%TEMP%\aegis-round-N\config.toml` 换一组参数，所以六轮不只是「同一套用例跑六次」：
它同时验证参数变化（透明度、内边距、圆角、字号）不会破坏任何链路。

每轮还配一个干净的 `%APPDATA%`（`%TEMP%\aegis-round-N\appdata`）：`state.json` 会记住上一轮的
窗口尺寸并顶掉 `config` 的 `width/height`，不隔离的话 C 层像素判据会读到上一轮的几何
（本轮就踩到 `1024x730`）。

`cargo test` 161 passed，clippy 零告警，fmt 干净。

## 关键断言（不是「跑完没报错」）

- **键盘真的能到程序**：`收到 WM_LBUTTONDOWN` → `获得键盘焦点` → `点击取焦点: … keyboard_ready=true`
  → `自检写入 1/3/4 字节`（ASCII / CJK / 代理对）→ `已写入 PTY 14 字节`。
- **启动即可见可点**：`启动取焦点: …` 且命中自检落在自己窗口上（后台启动被前台锁挡住时
  靠 `HWND_TOPMOST` 抬升保证），见 [M7 证据](../vis/M7/index.md)。
- **唤起热键生效**：投 `WM_HOTKEY` 后必须出现「收到唤起热键」，这是被遮挡时唯一的自救路径。
- **设置面板真的改配置**：F2 打开 → 两次 `→` → `Enter` → 磁盘上 `opacity` 增加 0.04。
- **WorkerW 挂载确已删除**：日志中不含 `attach 回读`，窗口 `GetParent` 为 `NULL`（顶层）。
- **颜色通道序**：落盘像素空白处按当轮 `opacity` 解析解比对（写死 0.88 的数字会误报，已改）。
- **圆角抗锯齿**：角点完全透明（合成白），上边缘不透明——硬边裁剪做不到这一点。
- **壁纸两端对比度**：白底 8.23:1、黑底 9.09:1，都过 WCAG AA。
- **自检不抢用户**：`前台已还给 <hwnd>（True）`，或明确记录「没有可归还的前台」。
- **键盘字符真的对**：真实按键敲 `ipconfig`，八个字节必须按顺序是
  `105,112,99,111,110,102,105,103`。这条是「扫描码被当成 UTF-16 码元」缺陷的端到端回归。

## 诚实范围

- 六轮是同一套用例、六种参数组合；它不是六份不同的用例集。
- **真实鼠标点击的路由**由 E 层（`-RealInput`）验证：真实 `SendInput` 走完整链路 ——
  `抬到最前 → 命中自己 → 真实点击 → 键盘就绪 → 拖选扩展 → 写入 PTY`。
  常规回归不跑它（会动真实鼠标键盘），只在需要时手动执行。
- **提权重启的提权分支**、**IME 真机上屏**、**中间灰壁纸**、**录屏类动态过渡**未验证，
  逐条列在 [M7 证据](../vis/M7/index.md#未验证项不掩饰)。
