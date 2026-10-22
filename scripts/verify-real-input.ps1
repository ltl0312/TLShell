# 真实输入探针的一键验收：抬到最前 → 真实点击/拖选/打字 → 还原光标与前台。
#
# 它发的是系统级事件；每一步发送前都要求前台是自己的窗口，否则一个事件都不发。
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\debug\tlshell.exe'
$work = Join-Path $env:TEMP 'aegis-realprobe'
$cfg = Join-Path $work 'config.toml'
$log = Join-Path $env:LOCALAPPDATA 'TLShell\logs\tlshell.log'

New-Item -ItemType Directory -Force -Path $work | Out-Null
Get-Process tlshell -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600

$before = if (Test-Path $log) { (Get-Content $log).Count } else { 0 }
if (Test-Path $cfg) { Remove-Item $cfg -Force }

$env:TLSHELL_LOG = 'debug'
$proc = Start-Process -FilePath $exe -ArgumentList @('--config', $cfg, '--probe-real') -PassThru
Start-Sleep -Seconds 7

$tail = Get-Content $log | Select-Object -Skip $before
$proc | Stop-Process -Force

$keys = @(
    '抬到最前',
    '真实点击',
    '点击取焦点',
    '真实拖选',
    '扩展选择',
    '自检写入',
    '真实输入结算'
)
foreach ($k in $keys) {
    $hit = $tail | Where-Object { $_ -like "*$k*" } | Select-Object -Last 2
    if ($hit) { $hit } else { "MISS: $k" }
}
