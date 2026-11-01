<#
empty-bin.ps1 - actually reclaim space by emptying a volume's Recycle Bin.

Why this exists
    Since 2026-11-01 we know deletion on this machine is intercepted at the
    filesystem level: even `[System.IO.Directory]::Delete` (RemoveDirectoryW,
    which is documented never to use the Recycle Bin) routes files into
    E:\$RECYCLE.BIN. Verified with a 50 MB probe - the probe showed up as a
    recycled item with its original path intact.

    Consequence: deleting build output frees NO space until the Bin is emptied.
    A 46.7 GB `target` deletion left `df` unchanged, while the Bin grew to
    85.58 GB / 133k items. tools/hard-rm.ps1 can therefore only report what it
    *removed*, not what it *reclaimed* - that is why it now cross-checks free
    space and points here when the number does not move.

    Enumeration through Clear-RecycleBin is unreliable at this scale (it has
    stalled on 400k items before), so this walks the per-SID folders and deletes
    the $R/$I pairs directly. Items already inside the Bin are removable for
    real - the interception only applies to deletes outside it.

Usage
    powershell -NoProfile -ExecutionPolicy Bypass -File tools\empty-bin.ps1
    powershell ... -File tools\empty-bin.ps1 -Drive D:      # another volume
    powershell ... -File tools\empty-bin.ps1 -WhatIf        # size only, deletes nothing

Safety
    Only touches entries INSIDE <drive>\$Recycle.Bin\<SID>\. Never removes the
    $Recycle.Bin folder itself. Skips S-1-5-18 (SYSTEM's bin) - those need
    elevation and are not ours to clear.
    ASCII-only on purpose: PowerShell 5.1 misreads BOM-less UTF-8 scripts.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string] $Drive = 'E:',
    [switch] $KeepRepoBuildOutputOnly
)

$ErrorActionPreference = 'Continue'

if ($Drive -notmatch '^[A-Za-z]:$') { $Drive = $Drive.TrimEnd('\') }

function Get-FreeGB([string] $d) {
    $di = New-Object System.IO.DriveInfo $d
    return [pscustomobject]@{ Free = $di.AvailableFreeSpace; Total = $di.TotalSize }
}

function Clear-ReadOnly([string] $p) {
    # $R payloads keep the original attributes, and read-only files make
    # Directory.Delete fail with "access denied".
    $stack = New-Object System.Collections.Stack
    $stack.Push($p)
    while ($stack.Count -gt 0) {
        $cur = $stack.Pop()
        try {
            $di = New-Object System.IO.DirectoryInfo $cur
            foreach ($f in $di.EnumerateFiles()) { try { if ($f.IsReadOnly) { $f.IsReadOnly = $false } } catch {} }
            foreach ($d in $di.EnumerateDirectories()) { $stack.Push($d.FullName) }
        } catch {}
    }
}

function Test-Keep([string] $meta) {
    # Optional mode: spare items whose original path was inside the repo, so a
    # "clean the build output" run does not also destroy unrelated deletions.
    if (-not $KeepRepoBuildOutputOnly) { return $false }
    try {
        $b = [System.IO.File]::ReadAllBytes($meta)
        if ($b.Length -lt 28) { return $false }
        $len = [BitConverter]::ToInt32($b, 24)
        $p = [System.Text.Encoding]::Unicode.GetString($b, 28, [Math]::Min($len * 2, $b.Length - 28)).TrimEnd([char]0)
        return ($p -like '*\code\aevum-studio\*')
    } catch { return $false }
}

$binRoot = Join-Path ($Drive + '\') '$Recycle.Bin'
if (-not [System.IO.Directory]::Exists($binRoot)) {
    Write-Output "no recycle bin on $Drive, nothing to do"
    exit 0
}

$free0 = (Get-FreeGB $Drive)
$sids = Get-ChildItem $binRoot -Directory -Force -ErrorAction SilentlyContinue
$totalItems = 0
$totalBytes = [long]0
foreach ($s in $sids) { $totalItems += (Get-ChildItem $s.FullName -Force -ErrorAction SilentlyContinue).Count }
Write-Output ("bin {0}: {1:N0} items, free {2:N1} GB of {3:N0} GB" -f $Drive, $totalItems, ($free0.Free / 1GB), ($free0.Total / 1GB))

if ($WhatIf -or $PSCmdlet.ShouldProcess($binRoot, "empty recycle bin ($totalItems items)")) {
    $deleted = 0
    $failed = 0
    $kept = 0
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    foreach ($s in $sids) {
        if ($s.Name -eq 'S-1-5-18') { Write-Output "skip SYSTEM bin $($s.Name)"; continue }
        foreach ($item in (Get-ChildItem $s.FullName -Force -ErrorAction SilentlyContinue)) {
            if ($item.Name -like '$I*') {
                if (Test-Keep $item.FullName) { $kept++; continue }
            }
            try {
                if ($item.PSIsContainer) {
                    $sz = (Get-ChildItem $item.FullName -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
                    Clear-ReadOnly $item.FullName
                    [System.IO.Directory]::Delete($item.FullName, $true)
                    $totalBytes += [long]$sz
                } else {
                    $totalBytes += $item.Length
                    if ($item.IsReadOnly) { [System.IO.File]::SetAttributes($item.FullName, [System.IO.FileAttributes]::Normal) }
                    [System.IO.File]::Delete($item.FullName)
                }
                $deleted++
            } catch {
                $failed++
            }
        }
    }
    $sw.Stop()
    $free1 = (Get-FreeGB $Drive)
    Write-Output ("---- empty-bin: {0:N0} items deleted, {1:N1} GB of payload, {2} failure(s), kept {3}, {4:N1}s" -f `
            $deleted, ($totalBytes / 1GB), $failed, $kept, $sw.Elapsed.TotalSeconds)
    Write-Output ("---- free space: {0:N1} GB -> {1:N1} GB (delta {2:+0.0;-0.0;0} GB)" -f `
            ($free0.Free / 1GB), ($free1.Free / 1GB), (($free1.Free - $free0.Free) / 1GB))
    if ($failed -gt 0) { exit 1 }
}
exit 0
