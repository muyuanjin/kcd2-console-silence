# Read (and optionally type into) the game's Windows console window, writing the
# result to a file instead of stdout.
#
# AttachConsole() redirects this process' stdout to the target console, so any
# Write-Output here would end up in the game's window rather than the caller's
# pipeline. Everything therefore goes through the -OutFile parameter.
#
# Usage:
#   pwsh -NoProfile -File tools\console-tail.ps1 -ProcessId 1234 -Lines 60 -OutFile out.txt
#   pwsh -NoProfile -File tools\console-tail.ps1 -ProcessId 1234 -Command "#ERVWHDO.StartLUTGuard(3000)" -Lines 60 -OutFile out.txt

param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [Parameter(Mandatory = $true)][string]$OutFile,
    [string[]]$Command = @(),
    [int]$Lines = 60,
    [int]$WaitAfterMs = 1500
)

$ErrorActionPreference = 'Stop'

Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class CsConsole {
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool AttachConsole(int pid);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool WriteConsoleInputW(IntPtr h, INPUT_RECORD[] buffer, uint length, out uint written);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    public static extern bool ReadConsoleOutputCharacterW(IntPtr h, [Out] char[] buffer, uint length, COORD read, out uint readCount);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool GetConsoleScreenBufferInfo(IntPtr h, out CONSOLE_SCREEN_BUFFER_INFO info);

    [StructLayout(LayoutKind.Sequential)] public struct COORD { public short X; public short Y; }
    [StructLayout(LayoutKind.Sequential)] public struct SMALL_RECT { public short Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct CONSOLE_SCREEN_BUFFER_INFO {
        public COORD Size; public COORD CursorPosition; public ushort Attributes;
        public SMALL_RECT Window; public COORD MaximumWindowSize;
    }
    [StructLayout(LayoutKind.Explicit, CharSet = CharSet.Unicode)]
    public struct INPUT_RECORD {
        [FieldOffset(0)] public ushort EventType;
        [FieldOffset(4)] public KEY_EVENT_RECORD KeyEvent;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct KEY_EVENT_RECORD {
        public int bKeyDown;
        public ushort wRepeatCount;
        public ushort wVirtualKeyCode;
        public ushort wVirtualScanCode;
        public char UnicodeChar;
        public uint dwControlKeyState;
    }
    public const ushort KEY_EVENT = 0x0001;

    public static string ReadText(IntPtr hOut, int lines) {
        CONSOLE_SCREEN_BUFFER_INFO info;
        if (!GetConsoleScreenBufferInfo(hOut, out info)) return "<no screen buffer info>";
        int width = info.Size.X;
        int rows = (lines < 0) ? (info.CursorPosition.Y + 1) : Math.Min(lines, info.CursorPosition.Y + 1);
        var sb = new StringBuilder();
        var rowBuffer = new char[width];
        for (int row = Math.Max(0, info.CursorPosition.Y - rows + 1); row <= info.CursorPosition.Y; row++) {
            uint read;
            var coord = new COORD { X = 0, Y = (short)row };
            if (ReadConsoleOutputCharacterW(hOut, rowBuffer, (uint)width, coord, out read) && read > 0) {
                sb.Append(new string(rowBuffer, 0, (int)read).TrimEnd());
                sb.Append('\n');
            }
        }
        return sb.ToString();
    }

    public static void TypeLine(IntPtr hIn, string line) {
        var records = new INPUT_RECORD[line.Length + 1];
        for (int i = 0; i < line.Length; i++) {
            records[i].EventType = KEY_EVENT;
            records[i].KeyEvent.bKeyDown = 1;
            records[i].KeyEvent.wRepeatCount = 1;
            records[i].KeyEvent.wVirtualKeyCode = 0;
            records[i].KeyEvent.wVirtualScanCode = 0;
            records[i].KeyEvent.UnicodeChar = line[i];
            records[i].KeyEvent.dwControlKeyState = 0;
        }
        records[line.Length].EventType = KEY_EVENT;
        records[line.Length].KeyEvent.bKeyDown = 1;
        records[line.Length].KeyEvent.wRepeatCount = 1;
        records[line.Length].KeyEvent.wVirtualKeyCode = 0x0D;
        records[line.Length].KeyEvent.wVirtualScanCode = 0x1C;
        records[line.Length].KeyEvent.UnicodeChar = '\r';
        uint written;
        WriteConsoleInputW(hIn, records, (uint)records.Length, out written);
    }
}
'@

$sb = New-Object System.Text.StringBuilder
[void][CsConsole]::FreeConsole()
if (-not [CsConsole]::AttachConsole($ProcessId)) {
    [System.IO.File]::WriteAllText($OutFile, "AttachConsole failed for pid $ProcessId (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))")
    exit 1
}
try {
    $access = [Convert]::ToUInt32('C0000000', 16)
    $hIn = [CsConsole]::CreateFileW('CONIN$', $access, 3, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    $hOut = [CsConsole]::CreateFileW('CONOUT$', $access, 3, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    if ($hIn -eq [IntPtr]::Zero -or $hOut -eq [IntPtr]::Zero) { throw 'CONIN$/CONOUT$ open failed' }
    [void]$sb.AppendLine('----- console tail before -----')
    [void]$sb.AppendLine([CsConsole]::ReadText($hOut, $Lines))
    foreach ($line in $Command) {
        [CsConsole]::TypeLine($hIn, $line)
        Start-Sleep -Milliseconds 900
    }
    Start-Sleep -Milliseconds $WaitAfterMs
    [void]$sb.AppendLine('----- console tail after -----')
    [void]$sb.AppendLine([CsConsole]::ReadText($hOut, $Lines))
    [void][CsConsole]::CloseHandle($hIn)
    [void][CsConsole]::CloseHandle($hOut)
}
finally {
    [void][CsConsole]::FreeConsole()
}
[System.IO.File]::WriteAllText($OutFile, $sb.ToString())
