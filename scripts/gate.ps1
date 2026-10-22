# TLShell 提交前质量门。用法：powershell -NoProfile -ExecutionPolicy Bypass -File scripts\gate.ps1
# 严格顺序：fmt -> clippy -> test -> deny -> release build。任一失败即停。
#
# 全部 cargo 命令带 --locked：门禁必须验证 Cargo.lock 里锁定的那份依赖图，
# 否则本地通过、CI 用另一份解析结果失败（安全评审 F-12）。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Step($name, [scriptblock]$body) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $body
    if ($LASTEXITCODE -ne 0) { throw "$name failed (exit $LASTEXITCODE)" }
}

Step 'cargo fmt --all --check'          { cargo fmt --all --check }
Step 'cargo clippy --locked -D warnings' { cargo clippy --all-targets --locked -- -D warnings }
Step 'cargo test --locked'              { cargo test --locked }

# cargo-deny 需要 advisory 数据库（联网）。装不上就直接失败，不假装通过。
if (-not (Get-Command cargo-deny -ErrorAction SilentlyContinue)) {
    Write-Host 'cargo-deny 未安装：先跑 cargo install cargo-deny --locked' -ForegroundColor Red
    throw 'cargo deny missing'
}
Step 'cargo deny check'                 { cargo deny --all-features check }

Step 'cargo build --locked --release'   { cargo build --locked --release }

Write-Host 'GATE PASS' -ForegroundColor Green
