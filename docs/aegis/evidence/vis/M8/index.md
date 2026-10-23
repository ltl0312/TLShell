# M8 证据：标签栏、清屏与自启动

本轮的三个用户可见变化都落在「同一个窗口」里，没有引入第二个进程或第二个窗口：
标签是同一窗口里的多条 shell 会话，清屏是往当前会话发 VT 序列，自启动是 HKCU 下的一个值。

## 1. 标签栏

- 几何在 `src/render/layout.rs`：`tab_bar_height(cell_h) = cell_h + 6`、
  `tab_slots()` / `new_button_center()` / `clear_button_center()` / `hit_tab_bar()`。
  标签宽度 = 标题字符数 × 单元格宽 + 16，叉号中心在标签右端内缩 8。
- 绘制在 `src/render/painter.rs::draw_tab_bar`：条底用选区色、当前标签用背景色 + 两像素下沿、
  标题/`×`/`+`/`清屏` 都按单元格步进画字，最后补一次 `draw_border`（否则圆角顶部缺一块）。
- 判据来自日志（`scripts/verify-tabs.ps1`，6 项全 PASS）：

  ```
  点击标签栏 @(107,15) 栏高=30 单元格=11: New       → 新建标签: shell 2
  点击标签栏 @(16,15)  栏高=30 单元格=11: Select(0) → 切到标签: shell 1
  点击标签栏 @(234,15) 栏高=30 单元格=11: Clear     → 清屏
  点击标签栏 @(184,15) 栏高=30 单元格=11: Close(1)  → 关闭标签: shell 2
  关闭后新建复用序号（shell 2 而不是 shell 3）
  关到空自动补一个（没有终端了 → 立刻起会话）
  ```

- 截图：`M8-01-tab-bar.png`（900×420 首帧落盘转 PNG）。像素抽样：
  标签栏底色 `(108,111,129)`、当前标签标题 `(205,214,244)`、网格区 `(57,57,71)`。

## 2. 两个真实缺陷

### 2.1 点 `×` 之后整个窗口冻结

`close_tab` 里 `self.tabs.remove(index)` 会 `Drop` 掉该标签的 `Session`，
而 `Session::drop` 要 `child.kill()` + `wait()` + `join()` 读线程，**那个 `join` 没有超时**。
读线程一旦卡在 ConPTY 的 `ReadFile` 上，UI 线程就永远回不来：点击全部无响应、日志也停。

修法：把会话从 `Tab` 里 `take()` 出来，丢给后台线程收尾；退出路径（`WM_DESTROY`）同样处理。

```
关闭标签: shell 2            ← 日志先出
（后台线程负责 kill + join，UI 线程继续处理点击）
点击标签栏 @(121,15) … New  ← 之后的消息照常到达
```

这条是「点不可用」类问题里最容易被误判成「输入法/焦点」的那种：窗口还在、画面正常，
但消息不再被处理。判据只能是「点完 × 之后还能不能点下一个」。

### 2.2 外部 `PostMessage` 的坐标会被 DPI 换算

验收脚本在 130 DPI 下按客户区坐标 `(11,10)` 投递，程序收到的是 `(16,14)`（×1.354 = 130/96）：

```
posting packed=0xF0005 → 收到 WM_LBUTTONDOWN lparam=0x150007 -> (7, 21)
```

原因是**投递进程**（pwsh）默认 DPI 不感知，Windows 按两个进程的 DPI 上下文换算鼠标消息坐标。
修法：脚本开头 `SetProcessDpiAwarenessContext(-4)`，之后坐标 1:1：

```
[debug] 收到 WM_LBUTTONDOWN lparam=0xa000b -> (11, 10)
```

不修这一条，脚本会「点歪 39 像素却仍落在某个控件上」，表现为随机命中错误的按钮——
最容易写成「功能坏了」。产品侧不受影响：真实鼠标走系统输入路径，不经过这种换算。

## 3. 自启动开关

- `src/win/autostart.rs` 只写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，
  值名 `TLShell`，内容是可执行文件绝对路径；`reg.exe` 带 `CREATE_NO_WINDOW` 调用。
- 设置面板第 10 行「随 Windows 启动」不在 `config.toml` 里（注册表就是唯一真相），
  保存时按面板值登记/取消，失败会明说「自启动没有改成功（现在：开/关）」。
- 真机：

  ```
  > tlshell.exe --autostart status
  随 Windows 启动: 未开启
  > tlshell.exe --autostart on
  随 Windows 启动: 已开启（"…\target\debug\tlshell.exe"）
  > tlshell.exe --autostart bogus
  --autostart 失败: --autostart 只接受 on|off|status，收到 bogus
  ```

  `bogus` 走 `eprintln!` + 退出码 1，不弹 MessageBox——无人值守调用不该被模态框挂死。

## 4. 诚实边界

- 快捷键 `Ctrl+T` / `Ctrl+W` / `Ctrl+L` / `Ctrl+Tab` 与三个按钮调用的是同一组函数，
  但真机验证走的是**按钮**：`Ctrl` 状态由 `GetKeyState` 读真实键盘，投递的合成消息带不上修饰键。
  键位本身只有编译期与代码审阅保证。
- 标签栏只有一条，标签多了会一直往右排、超出宽度后被裁掉（没有折行、没有滚动、没有拖拽排序）。
- 关闭标签后「补一个 shell」用的是新进程，历史不会带过来。
- 自启动只覆盖当前用户；`HKLM` 那条（所有用户）没有做，也不需要管理员。
