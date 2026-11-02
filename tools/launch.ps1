# Launch KCD2 through Steam and wait until the game is actually in gameplay.
#
# Synthetic keyboard/mouse input is swallowed by the game, so menu driving is not an
# option. Instead we arm `wh_sys_AutoLoadLastSave = 1` in
# the game's user.cfg as a marked block and launch the app with no URL arguments,
# which means Steam applies its own launch options and shows no confirmation
# modal.
#
# Usage:
#   pwsh -NoProfile -File tools\launch.ps1
#   pwsh -NoProfile -File tools\launch.ps1 -RestoreOnly     # strip the autoload block
#
# Exit codes: 0 = gameplay marker seen; 1 = launch/game missing; 2 = launched but
# no gameplay marker within -TimeoutSeconds.

[CmdletBinding()]
param(
    [string]$AppId = '1771300',
    [string]$SteamExe = $(if ($env:STEAM_EXE) { $env:STEAM_EXE } else { 'C:\Program Files (x86)\Steam\Steam.exe' }),
    [string]$GameRoot = $(if ($env:KCD2_ROOT) { $env:KCD2_ROOT } else { 'C:\Program Files (x86)\Steam\steamapps\common\KingdomComeDeliverance2' }),
    [int]$TimeoutSeconds = 240,
    [switch]$RestoreOnly
)

$ErrorActionPreference = 'Stop'
$cfgPath = Join-Path $GameRoot 'user.cfg'
$kcdLog = Join-Path $GameRoot 'kcd.log'
$markerBegin = '; ConsoleSilence autoload begin'
$markerEnd = '; ConsoleSilence autoload end'
$latin1 = [System.Text.Encoding]::GetEncoding(28591)

function Read-Cfg {
    if (-not (Test-Path -LiteralPath $cfgPath)) { return $null }
    return $latin1.GetString([System.IO.File]::ReadAllBytes($cfgPath))
}

function Remove-Autoload {
    $text = Read-Cfg
    if ($null -eq $text) { return $false }
    $pattern = '(?s)\r?\n?; ConsoleSilence autoload begin.*?; ConsoleSilence autoload end\r?\n?'
    $updated = [regex]::Replace($text, $pattern, '')
    if ($updated -eq $text) { return $false }
    [System.IO.File]::WriteAllBytes($cfgPath, $latin1.GetBytes($updated))
    return $true
}

function Add-Autoload {
    $text = Read-Cfg
    if ($null -eq $text) { return $false }
    if ($text.Contains($markerBegin)) { return $true }
    if ($text -match 'wh_sys_AutoLoadLastSave') { return $true }  # user's own flag; leave it
    $append = "`r`n$markerBegin`r`nwh_sys_AutoLoadLastSave = 1`r`n$markerEnd`r`n"
    [System.IO.File]::AppendAllText($cfgPath, $append, [System.Text.Encoding]::ASCII)
    return $true
}

if ($RestoreOnly) {
    if (Remove-Autoload) { Write-Host "autoload block removed from $cfgPath" }
    else { Write-Host 'no autoload block found' }
    return
}

if (-not (Test-Path -LiteralPath $SteamExe)) { throw "Steam not found: $SteamExe" }
if (Add-Autoload) { Write-Host "autoload armed in $cfgPath" }

$logStart = 0
if (Test-Path -LiteralPath $kcdLog) { $logStart = (Get-Item -LiteralPath $kcdLog).Length }
$existing = @(Get-Process KingdomCome -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)

Write-Host "issuing steam://run/$AppId"
Start-Process -FilePath $SteamExe -ArgumentList ('"steam://run/' + $AppId + '"')

$watch = [System.Diagnostics.Stopwatch]::StartNew()
$pid0 = 0
while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    $game = @(Get-Process KingdomCome -ErrorAction SilentlyContinue |
        Where-Object { $existing -notcontains $_.Id } | Select-Object -First 1)
    if ($game.Count -gt 0) { $pid0 = $game[0].Id; break }
    Start-Sleep -Milliseconds 250
}
if ($pid0 -eq 0) {
    Write-Host "timeout: no game process within $TimeoutSeconds s"
    exit 1
}
Write-Host ("game process up: pid={0} at {1:n1}s" -f $pid0, $watch.Elapsed.TotalSeconds)

# 就绪判据用日志里的读档标记，而不是"进程存在"——后者在加载画面里就已经成立。
$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
while ((Get-Date) -lt $deadline) {
    if (-not (Get-Process -Id $pid0 -ErrorAction SilentlyContinue)) {
        Write-Host 'game exited while waiting for gameplay'
        exit 1
    }
    if (Test-Path -LiteralPath $kcdLog) {
        $length = (Get-Item -LiteralPath $kcdLog).Length
        if ($length -lt $logStart) { $logStart = 0 }
        $stream = [System.IO.File]::Open($kcdLog, 'Open', 'Read', 'ReadWrite')
        try {
            [void]$stream.Seek($logStart, 'Begin')
            $buffer = New-Object byte[] ($length - $logStart)
            if ($buffer.Length -gt 0) { [void]$stream.Read($buffer, 0, $buffer.Length) }
            $delta = $latin1.GetString($buffer)
            $logStart = $length
        }
        finally { $stream.Dispose() }
        if ($delta -match 'Loading saved game|OnGameplayStarted|Gameplay started') {
            Write-Host ("gameplay ready: pid={0} after {1:n1}s" -f $pid0, $watch.Elapsed.TotalSeconds)
            exit 0
        }
    }
    Start-Sleep -Milliseconds 1000
}
Write-Host "launched but no gameplay marker within $TimeoutSeconds s"
exit 2
