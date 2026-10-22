# 第 3 轮全量测试（2026-10-22 20:43:21 +08:00）

本轮参数：opacity=0.86 padding=12 radius=10

环境：Windows 10.0.26200.0，PowerShell 7.6.6。

| 层 | 项 | 结果 | 说明 |
|---|---|---|---|
| A | cargo fmt --check | PASS | 格式一致 |
| A | cargo clippy -D warnings | PASS | 无告警 |
| A | cargo test | PASS | 161 passed |
| B | selftest 全部通过 | PASS | 12 项 |
| B | selftest 退出码 | PASS | exit=0 |
| C | 窗口创建为顶层 | PASS | window hwnd=0x |
| C | WorkerW 挂载已删除 | PASS | window hwnd=0x |
| C | shell 启动成功 | PASS | 启动 shell: |
| C | 子进程环境白名单 | PASS | 子进程环境: \d+ 项，启动目录 |
| C | 字体度量就绪 | PASS | 字体就绪:.*单元格 \d+x\d+ |
| C | 点击命中并取焦点 | PASS | 收到 WM_LBUTTONDOWN |
| C | 获得键盘焦点 | PASS | 获得键盘焦点 |
| C | 拖选扩展 | PASS | 扩展选择 -> |
| C | ASCII 写入 PTY | PASS | 自检写入 1 字节 |
| C | CJK 三字节写入 | PASS | 自检写入 3 字节 |
| C | 代理对四字节写入 | PASS | 自检写入 4 字节 |
| C | 结算键盘就绪 | PASS | 输入自检\[结算\].*已写入 PTY 14 字节 |
| C | 前台已归还 | PASS | 前台已还给\|没有可归还的前台\|已不存在，跳过归还 |
| C | 启动即可见可点 | PASS | 取前台=False 顶层抬升=True |
| C | 首帧落盘 | PASS | aegis-frame.bmp |
| C | BMP 尺寸 | PASS | 900x420 |
| C | 通道序 R/B 未颠倒 | PASS | bg=R62 G62 B76，期望 62/62/75（opacity=0.86） |
| C | 圆角外透明(V-19) | PASS | corner=R255 G255 B255 |
| C | 圆角内不透明 | PASS | top_edge=R62 G62 B76 |
| C | 单实例判重 | PASS | 第二个实例 exit=0 |
| C | 设置面板打开 | PASS | F2 |
| C | 设置写入落盘 | PASS | Enter |
| C | 设置值真的改了两档 | PASS | opacity=0.9 期望 0.9 |
| C | 唤起热键生效 | PASS | Ctrl+Alt+T |
| D | --quit 无实例退 0 | PASS | exit=0 |
| D | --quit 真的退出实例 | PASS | aliveBefore=True closed=True |
| D | 白/黑壁纸对比度 ≥ 4.5:1 | PASS | 测得 8.23/9.09，最差 8.23 |
| D | 降权重启在非提权下退 0 | PASS | exit=0 |
| D | 未知参数被拒绝 | PASS | exit=1 |
| D | 坏配置仍能启动 | PASS | alive=True |
| D | 缺 USERPROFILE 拒绝启动 | PASS | exited=True exit=1 |
| E | 真实点击命中自己 | PASS | 真实点击@.*moved=true down=true up=true |
| E | 真实点击拿到焦点 | PASS | 点击取焦点: foreground=true focus_set=true keyboard_ready=true |
| E | 真实拖选出选区 | PASS | 扩展选择 -> line=\d+ col=\d+ 选区=Some |
| E | 真实按键写进 PTY | PASS | 真实输入结算: 点击=\d+ 按键=[1-9]\d* 写入 PTY=[1-9]\d* 字节 |
| E | 真实按键敲出 ipconfig | PASS | (?s)自检写入 1 字节: \[105\].*?\[112\].*?\[99\].*?\[111\].*?\[110\].*?\[102\].*?\[105\].*?\[103\] |
| E | 前台已归还 | PASS | 前台已还给\|没有可归还的前台 |

汇总：PASS=42

C 层只向自己的窗口 `PostMessage`，不注入系统级输入；自检结束把前台还给原窗口。
