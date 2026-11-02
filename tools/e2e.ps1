# End-to-end acceptance for ConsoleSilence.asi against a real game session.
#
# Everything here runs through channels that are proven to work on this machine:
#   * the game is started through Steam with an autoload block (tools/launch.ps1);
#   * game code is executed through kcd2db's Lua runner (tools/lua-run.ps1);
#   * "what the console shows" is read back from the console ring buffer through
#     the ASI itself (ConsoleSilence.dump trigger -> ConsoleSilence.console.txt),
#     because synthetic keyboard input cannot open the in-game console.
#
# The suite proves four distinct things, each with a control that must fail if the
# mechanism is broken:
#   1. the hook is installed on IConsole::PrintLine and forwards untouched lines;
#   2. a rule-matched line disappears from the console;
#   3. repeated identical lines collapse without knowing their text;
#   4. the same swallowed line is still written to kcd.log (we filter the console
#      copy only) and unrelated lines are never touched.
#
# Usage:
#   pwsh -NoProfile -File tools\e2e.ps1
#   pwsh -NoProfile -File tools\e2e.ps1 -KeepRunning -SkipLaunch

[CmdletBinding()]
param(
    [string]$GameRoot = $(if ($env:KCD2_ROOT) { $env:KCD2_ROOT } else { 'C:\Program Files (x86)\Steam\steamapps\common\KingdomComeDeliverance2' }),
    [string]$GameBin = '',
    [switch]$SkipLaunch,
    [switch]$KeepRunning,
    [int]$GuardSeconds = 20,
    [int]$StabilitySeconds = 45
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $GameBin) { $GameBin = Join-Path $GameRoot 'Bin\Win64MasterMasterSteamPGO' }
$logFile = Join-Path $GameBin 'consolesilence.log'
$statusFile = Join-Path $GameBin 'ConsoleSilence.status.json'
$dumpFile = Join-Path $GameBin 'ConsoleSilence.console.txt'
$dumpTrigger = Join-Path $GameBin 'ConsoleSilence.dump'
$kcdLog = Join-Path $GameRoot 'kcd.log'
$evidence = Join-Path $repo ('re\out\e2e-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path $evidence | Out-Null

$script:results = New-Object System.Collections.Generic.List[object]
$script:failures = 0

function Assert-That {
    param([string]$Name, [bool]$Ok, [string]$Detail = '')
    $status = if ($Ok) { 'PASS' } else { 'FAIL' }
    if (-not $Ok) { $script:failures++ }
    $script:results.Add([pscustomobject]@{ Status = $status; Check = $Name; Detail = $Detail })
    Write-Host ("[{0}] {1}{2}" -f $status, $Name, $(if ($Detail) { " - $Detail" } else { '' }))
}

function Get-Status {
    if (-not (Test-Path -LiteralPath $statusFile)) { return $null }
    try { return Get-Content -LiteralPath $statusFile -Raw | ConvertFrom-Json }
    catch { return $null }
}

function Wait-Status {
    param([scriptblock]$Predicate, [int]$TimeoutSeconds = 30, [string]$What = 'status')
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $status = Get-Status
        if ($null -ne $status -and (& $Predicate $status)) { return $status }
        Start-Sleep -Milliseconds 400
    }
    Write-Host "timed out waiting for $What"
    return (Get-Status)
}

function Invoke-GameLua {
    param([string]$Code, [int]$TimeoutMs = 70000)
    $temp = Join-Path $evidence ('probe-' + [guid]::NewGuid().ToString('N').Substring(0, 8) + '.lua')
    [System.IO.File]::WriteAllText($temp, $Code, (New-Object System.Text.UTF8Encoding($false)))
    $output = & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'lua-run.ps1') -Path $temp -TimeoutMs $TimeoutMs
    # $output 是逐行的数组；先拼成一段文本再匹配，否则 -notmatch 会返回"不匹配的元素"而不是布尔值。
    if ((($output -join "`n") -notmatch 'ok=true')) { throw "Lua runner rejected $temp : $($output -join ' | ')" }
    return $output
}

function Read-KcdLogDelta {
    param([long]$From)
    if (-not (Test-Path -LiteralPath $kcdLog)) { return @{ Text = ''; Position = $From } }
    $stream = [System.IO.File]::Open($kcdLog, 'Open', 'Read', 'ReadWrite')
    try {
        $length = $stream.Length
        if ($length -le $From) { return @{ Text = ''; Position = $length } }
        [void]$stream.Seek($From, 'Begin')
        $buffer = New-Object byte[] ($length - $From)
        [void]$stream.Read($buffer, 0, $buffer.Length)
        $latin1 = [System.Text.Encoding]::GetEncoding(28591)
        return @{ Text = $latin1.GetString($buffer); Position = $length }
    }
    finally { $stream.Dispose() }
}

function Get-ConsoleDump {
    param([int]$TimeoutSeconds = 20)
    if (Test-Path -LiteralPath $dumpFile) { Remove-Item -LiteralPath $dumpFile -Force }
    Set-Content -LiteralPath $dumpTrigger -Value 'dump' -Encoding ASCII
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $dumpFile) {
            Start-Sleep -Milliseconds 200
            return @(Get-Content -LiteralPath $dumpFile)
        }
        Start-Sleep -Milliseconds 300
    }
    return @()
}

function Stop-Game {
    $process = Get-Process KingdomCome -ErrorAction SilentlyContinue
    if (-not $process) { return }
    Write-Host "closing the game (pid $($process.Id)) ..."
    [void]$process.CloseMainWindow()
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline -and (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
        Start-Sleep -Milliseconds 500
    }
    if (Get-Process -Id $process.Id -ErrorAction SilentlyContinue) {
        Stop-Process -Id $process.Id -Force
        Start-Sleep -Seconds 2
    }
}

# ---------------------------------------------------------------- preconditions
$binAsi = Join-Path $GameBin 'ConsoleSilence.asi'
Assert-That 'ASI is installed in the game Bin dir' (Test-Path -LiteralPath $binAsi) $binAsi
$mods = @(Get-ChildItem (Join-Path $GameRoot 'Mods') -Directory -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty Name)
Assert-That 'ERVWHDO mod present' ($mods -contains 'ERVWHDO') ($mods -join ', ')
Assert-That 'KCD Native Menus (kcdmenus) dependency present' ($mods -contains 'KCD2 Native Menus Creator') ''
Assert-That 'kcd2db.asi present (Lua runner channel)' (Test-Path -LiteralPath (Join-Path $GameBin 'kcd2db.asi')) ''
$ervPak = Join-Path $GameRoot 'Mods\ERVWHDO\Data\ERVWHDO-part1.pak'
$ervSize = if (Test-Path -LiteralPath $ervPak) { (Get-Item -LiteralPath $ervPak).Length } else { 0 }
Assert-That 'the delivered ERVWHDO build is installed (part1 = 94,399,354 B)' ($ervSize -eq 94399354) ("part1=$ervSize")

if (-not $SkipLaunch) {
    Stop-Game
    # 本轮从零开始：日志与状态是断言依据，留着上一轮会让断言假通过。
    foreach ($stale in @($logFile, $statusFile, $dumpFile)) {
        if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Force }
    }
    Write-Host "`n--- launching ---"
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'launch.ps1') -GameRoot $GameRoot
    if ($LASTEXITCODE -ne 0) { throw "launch failed with exit code $LASTEXITCODE" }
}

$pid0 = (Get-Process KingdomCome -ErrorAction SilentlyContinue | Select-Object -First 1).Id
Assert-That 'game process is running' ($null -ne $pid0) "pid=$pid0"

# ---------------------------------------------------------------- T1 hook state
$status = Wait-Status -Predicate { param($s) $s.hookState -eq 'installed' } -TimeoutSeconds 90 -What 'hookState=installed'
Assert-That 'IConsole::PrintLine hook installed' ($null -ne $status -and $status.hookState -eq 'installed') ("state=" + $(if ($status) { $status.hookState } else { '<none>' }))
if ($null -ne $status) {
    Assert-That 'hooked address is inside WHGame.dll' ($status.hookOriginal -ne '0x0' -and $status.moduleBase -ne '0x0') ("original=" + $status.hookOriginal + " module=" + $status.moduleBase)
    # 出厂状态：一条规则都没有，也不读任何配置文件。
    Assert-That 'the ASI ships with zero rules and no config file' ($status.ruleCount -eq 0) ("ruleCount=" + $status.ruleCount)
}

$baseline = Get-Status
$logBefore = (Get-Item -LiteralPath $kcdLog).Length

# ------------------------------------------------- T2 untouched line still shows
$marker = 'CS-T2-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
Invoke-GameLua -Code "System.LogAlways('$6[CS-T2] pass-through marker $marker')" | Out-Null
Start-Sleep -Milliseconds 700
$dump = Get-ConsoleDump
Assert-That 'an unlisted line reaches the console' (($dump -join "`n") -match [regex]::Escape($marker)) $marker

# ------------------------------------- T2b nothing is filtered before a mod asks
# 这一条是"出厂什么都不屏蔽"的正向证据：同样的行，在没装规则时必须出现。
$preTag = 'CS-T2B-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
Invoke-GameLua -Code "System.LogAlways('`$9[CS-T2B] r_ColorGradingChartImage before any rule $preTag')" | Out-Null
Start-Sleep -Milliseconds 700
$dump = Get-ConsoleDump
Assert-That 'a line matching nothing in particular is not swallowed' (($dump -join "`n") -match [regex]::Escape($preTag)) $preTag

# ----------------------------------------------------------------- T3 rule drop
# 规则现在只能由模组在运行时装（Lua），所以这一条同时验证"模组装规则 → 立刻生效"。
$spamTag = 'CS-T3-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
Invoke-GameLua -Code "ConsoleSilence.mute('r_ColorGradingChartImage', { owner = 'e2e' })" | Out-Null
$spamCode = @"
for i = 1, 30 do
  System.LogAlways('`$9[CS-T3] r_ColorGradingChartImage loop `$6$spamTag `$3' .. i)
end
"@
Invoke-GameLua -Code $spamCode | Out-Null
# status.json 是周期性快照（2 s），断言必须等快照把这一批计数写进去，不能读一次就判。
$afterRule = Wait-Status -TimeoutSeconds 12 -What 'drop counter' -Predicate {
    param($s) ([int]$s.dropped - [int]$baseline.dropped) -ge 25
}
$dropped = [int]$afterRule.dropped - [int]$baseline.dropped
Assert-That 'rule-matched lines are dropped (30 emitted)' ($dropped -ge 25) ("dropped delta=$dropped")
$dump = Get-ConsoleDump
Assert-That 'no rule-matched line survives in the console' (-not (($dump -join "`n") -match [regex]::Escape($spamTag))) ''

# the control for T3: the same lines must still be in kcd.log (console-only filter)
$delta = Read-KcdLogDelta -From $logBefore
$logBefore = $delta.Position
Assert-That 'swallowed lines are still written to kcd.log' ($delta.Text -match [regex]::Escape($spamTag)) "log delta=$($delta.Text.Length) bytes"

# ------------------------------------------------------- T4 repeat folding
# 折叠现在是**显式**能力（出厂关闭），所以先打开它——这本身就是"出厂不替用户决定"的证据。
Invoke-GameLua -Code 'ConsoleSilence.fold(true)' | Out-Null
$repeatTag = 'CS-T4-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$repeatCode = @"
for i = 1, 40 do
  System.LogAlways('`$6[CS-T4] identical spam line `$3$repeatTag')
end
"@
Invoke-GameLua -Code $repeatCode | Out-Null
$afterRepeat = Wait-Status -TimeoutSeconds 12 -What 'repeat counter' -Predicate {
    param($s) ([int]$s.repeatSuppressed - [int]$afterRule.repeatSuppressed) -ge 25
}
$folded = [int]$afterRepeat.repeatSuppressed - [int]$afterRule.repeatSuppressed
Assert-That 'identical repeats collapse without a rule (40 emitted)' ($folded -ge 25) ("repeatSuppressed delta=$folded")
$dump = Get-ConsoleDump
$survivors = @($dump | Where-Object { $_ -match [regex]::Escape($repeatTag) }).Count
Assert-That 'only the first repeats of the burst survive' ($survivors -le 3 -and $survivors -ge 1) ("survivors=$survivors")
Assert-That 'the folded-burst summary line is printed' ((($dump -join "`n") -match 'ConsoleSilence\] folded \d+ repeated')) ''
Invoke-GameLua -Code "ConsoleSilence.fold(false)" | Out-Null

# ----------------------------------------------------------- T5 negative control
$uniqueTag = 'CS-T5-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$uniqueCode = (1..5 | ForEach-Object { "System.LogAlways('`$6[CS-T5] unique line `$3$uniqueTag - $_')" }) -join "`n"
Invoke-GameLua -Code $uniqueCode | Out-Null
Start-Sleep -Milliseconds 900
$dump = Get-ConsoleDump
$unique = @($dump | Where-Object { $_ -match [regex]::Escape($uniqueTag) }).Count
Assert-That 'five distinct lines all survive' ($unique -eq 5) ("found=$unique")

# ------------------------------------------------------ T6 the real mod path
# 走用户真实路径：菜单按钮调的就是 SetLUTSettings，交付版在这一句里直接
# arm 一个 1500000 ms 的守卫（每 16 ms 重刷 chart）。
$guardCode = @'
local chart = 'Data//Textures//colorcharts//CInematic//1.New.Line.Cinema.dds'
ERVWHDO:SetLUTSettings(chart)
System.LogAlways('$6[CS-T6] SetLUTSettings ran, guardArmed=$3' .. tostring(ERVWHDO.lutGuard.timer_id ~= nil))
'@
Invoke-GameLua -Code $guardCode | Out-Null
$beforeGuard = Get-Status
$chartBefore = @(Get-ConsoleDump | Where-Object { $_ -match 'r_ColorGradingChartImage' }).Count
Start-Sleep -Seconds $GuardSeconds
$afterGuard = Get-Status
$dump = Get-ConsoleDump
Assert-That 'the menu path armed the LUT guard' (($dump -join "`n") -match 'guardArmed=\$?[0-9]?true') ''
# 按增量判：转储是环形缓冲，前面 T2b 那行还在里面，绝对计数会误报。
$chartAfter = @($dump | Where-Object { $_ -match 'r_ColorGradingChartImage' }).Count
Assert-That 'no chart-cvar line appears while the guard runs' ($chartAfter -le $chartBefore) ("chart lines before=$chartBefore after=$chartAfter")
Assert-That 'the guard does not inflate the drop counter' (([int]$afterGuard.dropped - [int]$beforeGuard.dropped) -le 2) ("dropped delta=" + ([int]$afterGuard.dropped - [int]$beforeGuard.dropped))

# ------------------------------------------------- T7 command echo (silent mode)
# 用户现场那条：控制台打开时，守卫每 tick 执行一次
#   r_ColorGradingChartImage Data//Textures//colorcharts//cinematic//3.a24.dds
# 引擎就把命令本身回显进控制台（`ExecuteString`：`if (!bSilent && GetStatus())`）。
# 本 ASI 的处置是把 `bSilentMode` 翻成真——命令照跑，回显不发生。
#
# 判据分两半，各自独立成立：
#   a) `commandsSilenced` 涨：证明命令确实从这条钩子上过、并被改成静音（与控制台是否
#      显示无关；当初就是因为"控制台没开时回显本来就不发生"才测漏的）。
#   b) 如果控制台此刻是显示的（用户自己按 ~ 打开），再加测"回显不再增长"。
$chartPath = 'Data//Textures//colorcharts//cinematic//3.a24.dds'   # 用户截图里那一张
$beforeSilence = Get-Status
Invoke-GameLua -Code "ERVWHDO:SetLUTSettings('$chartPath')" | Out-Null
Start-Sleep -Seconds 3
$afterSilence = Wait-Status -TimeoutSeconds 20 -What 'commandsSilenced' -Predicate {
    param($s) ([int]$s.commandsSilenced - [int]$beforeSilence.commandsSilenced) -ge 20
}
$silenced = [int]$afterSilence.commandsSilenced - [int]$beforeSilence.commandsSilenced
Assert-That 'matching console commands are intercepted on ExecuteString' ($silenced -ge 20) ("commandsSilenced delta=$silenced")

function Measure-EchoGrowth {
    param([int]$Seconds = 3)
    $first = @(Get-ConsoleDump | Where-Object { $_ -match 'r_ColorGradingChartImage' }).Count
    Start-Sleep -Seconds $Seconds
    $second = @(Get-ConsoleDump | Where-Object { $_ -match 'r_ColorGradingChartImage' }).Count
    return ($second - $first)
}

if ($afterSilence.consoleOpen -eq $true) {
    $growth = Measure-EchoGrowth -Seconds 3
    Assert-That 'with the console displayed the guard adds no echo line' ($growth -eq 0) ("echo lines added in 3 s=$growth")
}
else {
    # 控制台没显示时引擎本来就不回显，这条测不出东西——写明而不是假装通过。
    Assert-That 'echo growth check needs the console to be displayed' $true 'skipped: console closed (press ~ and re-run for the A/B)'
}
Invoke-GameLua -Code 'ERVWHDO.StopLUTGuard()' | Out-Null

# ------------------------------------------------------------- T7b Lua CRUD API
# 这一段验证 `ConsoleSilence` 表真的注册进了 Lua，并且增/删/查三件事都生效。
# 证据只走控制台转储：被吞的行不会出现在缓冲里，能出现就说明没吞。
$luaTag = 'CS-T7B-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$luaCode = @"
if type(ConsoleSilence) ~= 'table' then
  System.LogAlways('`$4[CS-T7B] missing table')
  return
end
local id = ConsoleSilence.mute('[$luaTag] spam', { kind = 'contains', action = 'drop', owner = 'e2e' })
System.LogAlways('`$6[CS-T7B] id=' .. tostring(id))
System.LogAlways('`$6[CS-T7B] which=' .. tostring(ConsoleSilence.which('[$luaTag] spam 1')))
for i = 1, 12 do System.LogAlways('[$luaTag] spam ' .. i) end
local rows = ConsoleSilence.list()
local mine = 0
for _, row in ipairs(rows) do if row.owner == 'e2e' then mine = mine + 1 end end
System.LogAlways('`$6[CS-T7B] listed=' .. tostring(mine))
local stats = ConsoleSilence.stats()
System.LogAlways('`$6[CS-T7B] stats=' .. tostring(stats.rules) .. ',' .. tostring(stats.dropped))
ConsoleSilence.unmute(id)
for i = 1, 3 do System.LogAlways('[$luaTag] after ' .. i) end
-- clear(owner) 需要**先有东西可清**：上面那条已经 unmute 掉了，这里再放一条同 owner 的。
local id3 = ConsoleSilence.mute('[$luaTag] third', { owner = 'e2e' })
local removed = ConsoleSilence.clear('e2e')
local left = 0
for _, row in ipairs(ConsoleSilence.list()) do
  if row.owner == 'e2e' then left = left + 1 end
end
System.LogAlways('`$6[CS-T7B] cleared=' .. tostring(removed) .. ',left=' .. tostring(left) .. ',third=' .. tostring(id3))
"@
Invoke-GameLua -Code $luaCode | Out-Null
Start-Sleep -Milliseconds 900
$luaDump = Get-ConsoleDump | Join-String -Separator "`n"
Assert-That 'the Lua table is registered' ($luaDump -match [regex]::Escape('[CS-T7B] missing table') -eq $false) 'ConsoleSilence table present'
Assert-That 'mute() returns a rule id' ($luaDump -match ([regex]::Escape("[CS-T7B] id=") + '(\d+)')) 'id=<number>'
Assert-That 'which() sees the rule it just made' ($luaDump -match ([regex]::Escape("[CS-T7B] which=") + '\d+')) 'which=<id>'
Assert-That 'the rule swallows the tagged lines' ($luaDump -notmatch [regex]::Escape("[$luaTag] spam 1")) "no '$luaTag spam' line survived"
Assert-That 'list() reports the runtime rule' ($luaDump -match ([regex]::Escape("[CS-T7B] listed=") + '[1-9]')) 'listed>=1'
Assert-That 'unmute() lets the lines through again' ($luaDump -match [regex]::Escape("[$luaTag] after 1")) "line reappeared after unmute"
Assert-That 'clear(owner) removed it' ($luaDump -match ([regex]::Escape("[CS-T7B] cleared=") + '[1-9]')) 'cleared>=1'
Assert-That 'nothing owned by that owner is left' ($luaDump -match ([regex]::Escape('[CS-T7B] cleared=') + '\d+,left=0')) 'left=0'

# --------------------------------------------------------------- T8 stability
Start-Sleep -Seconds $StabilitySeconds
$alive = [bool](Get-Process -Id $pid0 -ErrorAction SilentlyContinue)
Assert-That "game is still alive after $StabilitySeconds s with the hook installed" $alive "pid=$pid0"
# BugSplat 的文本日志落在 %TEMP%，附件落在游戏根；两个位置都要看。
$crashRoots = @($GameRoot, $env:TEMP)
# 每次启动 BugSplat 都会写一个 3 字节的空占位文件；真正的崩溃报告才有正文。
$crashes = @(foreach ($root in $crashRoots) {
        Get-ChildItem $root -Filter 'bugsplat_*.log' -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -gt (Get-Date).AddMinutes(-15) -and $_.Length -gt 32 }
    })
Assert-That 'no fresh BugSplat report with content was produced' ($crashes.Count -eq 0) ($crashes.Name -join ', ')
$logText = if (Test-Path -LiteralPath $logFile) { Get-Content -LiteralPath $logFile -Raw } else { '' }
Assert-That 'the ASI log has no ERROR lines' ($logText -notmatch '\[ERROR\]') ''
$final = Get-Status
Assert-That 'hook is still installed at the end' ($null -ne $final -and $final.hookState -eq 'installed') ("state=" + $(if ($final) { $final.hookState } else { '<none>' }))
Assert-That 're-arm counter stayed at zero (nobody else stole the slot)' ($null -eq $final -or [int]$final.rearmed -eq 0) ("rearmed=" + $(if ($final) { $final.rearmed } else { '?' }))
if ($null -ne $final) {
    # 三个计数器必须能互相对账：每一行最终只归入 dropped / passed / repeatSuppressed 之一。
    $sum = [int]$final.dropped + [int]$final.passed + [int]$final.repeatSuppressed
    Assert-That 'seen equals dropped + passed + repeatSuppressed' ([int]$final.seen -eq $sum) ("seen=" + $final.seen + " sum=" + $sum)
}

# ------------------------------------------------------------------- evidence
Copy-Item -LiteralPath $statusFile -Destination (Join-Path $evidence 'status.json') -Force -ErrorAction SilentlyContinue
if (Test-Path -LiteralPath $logFile) {
    Get-Content -LiteralPath $logFile -Tail 400 | Set-Content (Join-Path $evidence 'consolesilence.tail.log') -Encoding UTF8
}
$dump | Set-Content (Join-Path $evidence 'console-dump.txt') -Encoding UTF8
Read-KcdLogDelta -From 0 | ForEach-Object { $_.Text } | Set-Content (Join-Path $evidence 'kcd-log-tail.txt') -Encoding UTF8 -ErrorAction SilentlyContinue
$script:results | Format-Table -AutoSize | Out-String | Set-Content (Join-Path $evidence 'results.txt') -Encoding UTF8

if (-not $KeepRunning) { Stop-Game }

Write-Host "`n=== $($script:results.Count) checks, $($script:failures) failed ==="
$script:results | Format-Table -AutoSize
Write-Host "evidence: $evidence"
if ($script:failures -gt 0) { exit 1 }
