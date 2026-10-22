# TLShell 提交前质量门。用法：powershell -NoProfile -File scripts\gate.ps1
# 严格顺序：fmt -> clippy -> test -> release build。任一失败即停。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Step($name, [scriptblock]$body) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $body
    if ($LASTEXITCODE -ne 0) { throw "$name failed (exit $LASTEXITCODE)" }
}

Step 'cargo fmt --all --check'          { cargo fmt --all --check }
Step 'cargo clippy -D warnings'         { cargo clippy --all-targets -- -D warnings }
Step 'cargo test'                       { cargo test }
Step 'cargo build --release'            { cargo build --release }

Write-Host 'GATE PASS' -ForegroundColor Green
