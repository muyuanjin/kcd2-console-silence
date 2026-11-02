# Smoke-test a kcd2db.asi build as the mod dependency (no Lua runner needed).
#
# The published `kcd2db 0.1.12` package ships a build that does NOT
# contain the TCP Lua runner this repo's e2e suite uses as its test channel, so it
# cannot be the harness build. It is still the dependency the mod environment is
# supposed to contain, so this script proves it loads and serves ERVWHDO's DB calls:
#
#   * kcd2db's own log shows its startup/DB diagnostics;
#   * ERVWHDO logs "Restored from DB: ..." (its SVO/rain/wind modules read their
#     state back on load) — that only happens when DB.Create/Get answer;
#   * ConsoleSilence still installs its hook (coexistence with that build).
#
# Usage:
#   pwsh -NoProfile -File tools\dependency-smoke.ps1 -Asi `<path to kcd2db.asi>`
#   pwsh -NoProfile -File tools\dependency-smoke.ps1 -Restore
#
# The harness build is parked as `kcd2db.asi.harness-build` and restored by
# -Restore (or by re-running without -Asi... use -Restore explicitly).

[CmdletBinding()]
param(
    [string]$GameRoot = $(if ($env:KCD2_ROOT) { $env:KCD2_ROOT } else { 'C:\Program Files (x86)\Steam\steamapps\common\KingdomComeDeliverance2' }),
    [string]$GameBin = '',
    [string]$Asi = '',
    [switch]$Restore,
    [int]$SettleSeconds = 25
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $GameBin) { $GameBin = Join-Path $GameRoot 'Bin\Win64MasterMasterSteamPGO' }
$target = Join-Path $GameBin 'kcd2db.asi'
$parked = Join-Path $GameBin 'kcd2db.asi.harness-build'
$consoleLog = Join-Path $GameBin 'consolesilence.log'

function Stop-Game {
    $process = Get-Process KingdomCome -ErrorAction SilentlyContinue
    if (-not $process) { return }
    [void]$process.CloseMainWindow()
    $deadline = (Get-Date).AddSeconds(12)
    while ((Get-Date) -lt $deadline -and (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
        Start-Sleep -Milliseconds 500
    }
    if (Get-Process -Id $process.Id -ErrorAction SilentlyContinue) { Stop-Process -Id $process.Id -Force; Start-Sleep -Seconds 2 }
}

if (Get-Process KingdomCome -ErrorAction SilentlyContinue) { throw 'close the game first' }

if ($Restore) {
    if (Test-Path -LiteralPath $parked) {
        Copy-Item -LiteralPath $parked -Destination $target -Force
        Remove-Item -LiteralPath $parked -Force
        Write-Host 'restored the harness kcd2db build'
    }
    else {
        Write-Host 'nothing parked; target left as is'
    }
    return
}

if (-not $Asi) { throw 'pass -Asi <path to the kcd2db.asi under test> (or -Restore)' }
if (-not (Test-Path -LiteralPath $Asi)) { throw "not found: $Asi" }

# 换入被测构建；harness 构建留在旁边（不叫 *.asi，加载器不会碰它）。
if (-not (Test-Path -LiteralPath $parked)) {
    Copy-Item -LiteralPath $target -Destination $parked -Force
    Write-Host "parked harness build -> $parked"
}
Copy-Item -LiteralPath $Asi -Destination $target -Force
$underTest = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "under test: $target ($((Get-Item -LiteralPath $target).Length) bytes, sha256=$underTest)"

$kcdLog = Join-Path $GameRoot 'kcd.log'
$dbLog = Join-Path $GameBin 'kcd2db.log'
$rootDbLog = Join-Path $GameRoot 'kcd2db.log'
foreach ($stale in @($kcdLog, $dbLog, $rootDbLog, $consoleLog)) {
    if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Force -ErrorAction SilentlyContinue }
}

Write-Host "`n--- launching ---"
& pwsh -NoProfile -File (Join-Path $PSScriptRoot 'launch.ps1') -GameRoot $GameRoot
if ($LASTEXITCODE -ne 0) { throw "launch failed ($LASTEXITCODE)" }
Start-Sleep -Seconds $SettleSeconds

$latin1 = [System.Text.Encoding]::GetEncoding(28591)
function Read-Log([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    $stream = [System.IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
    try {
        $buffer = New-Object byte[] $stream.Length
        [void]$stream.Read($buffer, 0, $buffer.Length)
        return $latin1.GetString($buffer)
    }
    finally { $stream.Dispose() }
}

$kcd = Read-Log $kcdLog
$db = Read-Log $dbLog
$rootDb = Read-Log $rootDbLog
$asi = Read-Log $consoleLog

$checks = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $status = if ($ok) { 'PASS' } else { 'FAIL' }
    $checks.Add([pscustomobject]@{ Status = $status; Check = $name; Detail = $detail })
    Write-Host ("[{0}] {1}{2}" -f $status, $name, $(if ($detail) { " - $detail" } else { '' }))
}

Check 'kcd2db 0.1.12 loaded (its own log exists)' (($db.Length + $rootDb.Length) -gt 0) ("bin=$($db.Length)B root=$($rootDb.Length)B")
# DB 的真实证据在 kcd2db 自己的日志里（它把每次打开/初始化/注册都写下来），
# 以及 ERVWHDO 启动后"从 DB 取回了设置"的那几行。
$dbAll = $db + $rootDb
Check 'its SQLite backend opened' ($dbAll -match 'SQLite database opened') ''
Check 'Global table was read back' ($dbAll -match 'Loaded \d+ entries from \[Global\]') ''
Check 'its Lua DB API was registered' ($dbAll -match 'Registered LuaDB method') ''
Check 'ERVWHDO restored its settings through the DB' ($kcd -match '\[ERVWHDO\].*Restored') ''
Check 'no Lua/DB error in the game log' ($kcd -notmatch 'attempt to index.*DB|DB is nil') ''
Check 'ConsoleSilence coexists with this build' ($asi -match 'PrintLine hooked') ''

$evidence = Join-Path $repo ('re\out\dependency-smoke-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path $evidence | Out-Null
$checks | Format-Table -AutoSize | Out-String | Set-Content (Join-Path $evidence 'results.txt') -Encoding UTF8
$kcd | Set-Content (Join-Path $evidence 'kcd.log.snapshot') -Encoding UTF8
($db + "`n--- root ---`n" + $rootDb) | Set-Content (Join-Path $evidence 'kcd2db.log.snapshot') -Encoding UTF8
$asi | Set-Content (Join-Path $evidence 'consolesilence.log.snapshot') -Encoding UTF8
"underTest=$target`nsha256=$underTest`n" | Set-Content (Join-Path $evidence 'under-test.txt') -Encoding UTF8

Stop-Game
$failures = @($checks | Where-Object { $_.Status -eq 'FAIL' }).Count
Write-Host "`n=== $($checks.Count) checks, $failures failed ==="
Write-Host "evidence: $evidence"
Write-Host 'the harness build is parked; run with -Restore to put it back'
if ($failures -gt 0) { exit 1 }
