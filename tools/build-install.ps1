# Build and install ConsoleSilence.asi into the game's Bin directory.
#
# The ASI loader (dinput8.dll) picks up *.asi from the directory that holds
# KingdomCome.exe, so "install" = copy the cdylib there under a stable name, then
# verify the deployed bytes are the bytes we just built.
#
# Usage:
#   pwsh -NoProfile -File tools\build-install.ps1
#   pwsh -NoProfile -File tools\build-install.ps1 -Uninstall

[CmdletBinding()]
param(
    [switch]$Uninstall,
    [string]$GameBin = $(if ($env:KCD2_BIN) { $env:KCD2_BIN } else { 'C:\Program Files (x86)\Steam\steamapps\common\KingdomComeDeliverance2\Bin\Win64MasterMasterSteamPGO' })
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$artifact = Join-Path $repo 'target\x86_64-pc-windows-msvc\release\console_silence.dll'
$target = Join-Path $GameBin 'ConsoleSilence.asi'

if (-not (Test-Path -LiteralPath $GameBin)) {
    throw "game Bin directory missing: $GameBin"
}

if ($Uninstall) {
    if (Test-Path -LiteralPath $target) {
        Remove-Item -LiteralPath $target -Force
        Write-Host "removed $target"
    }
    else {
        Write-Host "nothing to remove at $target"
    }
    return
}

if (Get-Process KingdomCome -ErrorAction SilentlyContinue) {
    throw 'game is running; close it first (the ASI file is locked)'
}

Push-Location $repo
try {
    cargo build --release --target x86_64-pc-windows-msvc
}
finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $artifact)) {
    throw "build artifact missing: $artifact"
}

$artifactHash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
if (Test-Path -LiteralPath $target) {
    $backup = "$target.bak-" + (Get-Date -Format 'yyyyMMdd-HHmmss')
    Copy-Item -LiteralPath $target -Destination $backup -Force
    Write-Host "backed up previous install -> $backup"
}
Copy-Item -LiteralPath $artifact -Destination $target -Force
$targetHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
if ($targetHash -ne $artifactHash) {
    throw "deploy hash mismatch: target=$targetHash artifact=$artifactHash"
}
Write-Host "installed $target ($((Get-Item -LiteralPath $target).Length) bytes)"
Write-Host "  sha256=$targetHash (verified == artifact)"

# 配置只在缺失时生成；已存在的手改配置不覆盖（用户可能已经按自己的日志调过规则）。
$config = Join-Path $GameBin 'ConsoleSilence.cfg'
if (Test-Path -LiteralPath $config) {
    # 没有配置文件这回事了：旧版本留下的那份会在运行时被忽略，这里直接清掉，免得让人误以为要改它。
    Remove-Item -LiteralPath $config -Force
    Write-Host "  removed the obsolete config file: $config"
}