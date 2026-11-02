# Dump a qword table (vtable) from a PE file at a given RVA, resolving each entry
# back to an RVA so entries can be compared with RE notes.
#
# Usage:
#   pwsh -NoProfile -File tools\pe-vtable.ps1 -Path <dll> -Rva 0x3DCF850 -Count 8
#   pwsh -NoProfile -File tools\pe-vtable.ps1 -Path <dll> -Rva 0x3DCF850 -Auto

param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Rva,
    [int]$Count = 8,
    [switch]$Auto
)

$ErrorActionPreference = 'Stop'
$bytes = [System.IO.File]::ReadAllBytes($Path)

function Read-U16([int]$o) { [BitConverter]::ToUInt16($bytes, $o) }
function Read-U32([int]$o) { [BitConverter]::ToUInt32($bytes, $o) }
function Read-U64([int]$o) { [BitConverter]::ToUInt64($bytes, $o) }

$peOff = Read-U32 0x3C
if ($bytes[$peOff] -ne 0x50 -or $bytes[$peOff + 1] -ne 0x45) { throw 'not a PE' }
$numSections = Read-U16 ($peOff + 6)
$optSize = Read-U16 ($peOff + 20)
$optOff = $peOff + 24
$magic = Read-U16 $optOff
$imageBase = if ($magic -eq 0x20b) { Read-U64 ($optOff + 24) } else { [uint64](Read-U32 ($optOff + 28)) }
$sectionOff = $optOff + $optSize

$sections = @()
for ($i = 0; $i -lt $numSections; $i++) {
    $o = $sectionOff + $i * 40
    $nameBytes = $bytes[$o..($o + 7)]
    $name = ([System.Text.Encoding]::ASCII.GetString($nameBytes)).TrimEnd([char]0)
    $sections += [pscustomobject]@{
        Name    = $name
        Virtual = [uint64](Read-U32 ($o + 12))
        VSize   = [uint64](Read-U32 ($o + 8))
        Raw     = [uint64](Read-U32 ($o + 20))
        RawSize = [uint64](Read-U32 ($o + 16))
        Flags   = Read-U32 ($o + 36)
    }
}

function Convert-RvaToOffset([uint64]$rva) {
    foreach ($s in $sections) {
        if ($rva -ge $s.Virtual -and $rva -lt ($s.Virtual + [Math]::Max($s.VSize, $s.RawSize))) {
            return [int]($s.Raw + ($rva - $s.Virtual))
        }
    }
    return -1
}

function Get-SectionOf([uint64]$rva) {
    foreach ($s in $sections) {
        if ($rva -ge $s.Virtual -and $rva -lt ($s.Virtual + [Math]::Max($s.VSize, $s.RawSize))) { return $s }
    }
    return $null
}

$startRva = [Convert]::ToUInt64($Rva, 16)
$off = Convert-RvaToOffset $startRva
if ($off -lt 0) { throw ("RVA 0x{0:X} not inside any section" -f $startRva) }

# Walk backwards to the first entry if -Auto: entries before the vtable are
# usually other .rdata (strings / RTTI), so just start here.
$limit = if ($Auto) { 200 } else { $Count }
$lastGood = 0
$rows = @()
for ($i = 0; $i -lt $limit; $i++) {
    $q = Read-U64 ($off + $i * 8)
    $entryRva = $q - $imageBase
    $sec = if ($q -ge $imageBase) { Get-SectionOf $entryRva } else { $null }
    $inText = $sec -and (($sec.Flags -band 0x20000000) -ne 0) -and (($sec.Flags -band 0x00000020) -ne 0)
    $rows += [pscustomobject]@{
        Index    = $i
        VtableAt = ('0x{0:X}' -f ($startRva + $i * 8))
        EntryVA  = ('0x{0:X}' -f $q)
        EntryRva = ('0x{0:X}' -f $entryRva)
        Section  = if ($sec) { $sec.Name } else { '-' }
        Code     = [bool]$inText
    }
    if ($inText) { $lastGood = $i }
}
if ($Auto) {
    $rows = $rows[0..$lastGood] + $rows[$lastGood]
    $rows = $rows[0..($lastGood)]
}
$rows | Format-Table -AutoSize
"imageBase=0x{0:X}  tableRva=0x{1:X}  entries(code)=$(if($Auto){$lastGood+1}else{'n/a'})" -f $imageBase, $startRva
