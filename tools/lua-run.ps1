# Execute a Lua file inside the running game through kcd2db's Lua runner.
#
# kcd2db.asi (installed in the game's Bin dir) starts a TCP server on
# 127.0.0.1:28771 when the game is launched with -kcd2dbLuaRunner. The wire
# format is a 4-byte little-endian payload length followed by the payload; a
# native payload asks for buffer mode, so the script source is read from disk and
# executed with IScriptSystem::ExecuteBuffer on the game thread.
#
# This is the harness' only way to run game code: synthetic keyboard/mouse input
# is swallowed by the game, and the
# Windows console window does not accept injected command lines either.
#
# Usage:
#   pwsh -NoProfile -File tools\lua-run.ps1 -Path .\probe.lua
#   pwsh -NoProfile -File tools\lua-run.ps1 -Code "ERVWHDO.StartLUTGuard(3000)"
#   pwsh -NoProfile -File tools\lua-run.ps1 -Ping

param(
    [string]$Path,
    [string]$Code,
    [int]$Port = 28771,
    [switch]$Ping,
    [int]$TimeoutMs = 70000
)

$ErrorActionPreference = 'Stop'

function Invoke-Runner {
    param([byte[]]$Payload, [int]$Port, [int]$TimeoutMs)

    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $client.Connect('127.0.0.1', $Port)
        $stream = $client.GetStream()
        $stream.ReadTimeout = $TimeoutMs
        $stream.WriteTimeout = $TimeoutMs
        $lengthBytes = [BitConverter]::GetBytes([uint32]$Payload.Length)
        $stream.Write($lengthBytes, 0, 4)
        $stream.Write($Payload, 0, $Payload.Length)
        $stream.Flush()
        $buffer = New-Object System.Collections.Generic.List[byte]
        $chunk = New-Object byte[] 8192
        while ($true) {
            try {
                $read = $stream.Read($chunk, 0, $chunk.Length)
            }
            catch [System.IO.IOException] {
                break
            }
            if ($read -le 0) { break }
            $buffer.AddRange([byte[]]$chunk[0..($read - 1)])
            # Native replies are short and complete; a 250 ms lull means "done".
            if ($stream.DataAvailable) { continue }
            Start-Sleep -Milliseconds 250
            if (-not $stream.DataAvailable) { break }
        }
        return [System.Text.Encoding]::UTF8.GetString($buffer.ToArray())
    }
    finally {
        $client.Close()
    }
}

if ($Ping) {
    $payload = [System.Text.Encoding]::UTF8.GetBytes("KCD2DB_LUA_RUNNER/1`ncommand=ping`n")
    Write-Output (Invoke-Runner -Payload $payload -Port $Port -TimeoutMs $TimeoutMs)
    return
}

if ($Code) {
    $temp = Join-Path $env:TEMP ("kcd2-lua-" + [guid]::NewGuid().ToString('N') + ".lua")
    [System.IO.File]::WriteAllText($temp, $Code, (New-Object System.Text.UTF8Encoding($false)))
    $Path = $temp
}
if (-not $Path) { throw 'pass -Path <file.lua> or -Code <lua>' }
if (-not (Test-Path -LiteralPath $Path)) { throw "lua file not found: $Path" }
$full = (Resolve-Path -LiteralPath $Path).Path

$payloadText = "KCD2DB_LUA_RUNNER/1`ncommand=run`nmode=buffer`npath=$full`n"
$payload = [System.Text.Encoding]::UTF8.GetBytes($payloadText)
Write-Output (Invoke-Runner -Payload $payload -Port $Port -TimeoutMs $TimeoutMs)
