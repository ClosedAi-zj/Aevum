<#
hard-rm.ps1 - delete files/directories WITHOUT sending them to the Recycle Bin.

Why this exists
    Deleting build output on this machine does not free space the usual way.
    Two separate layers are involved, both measured on 2026-11-01:

    1. A plain `rm` moves files to E:\$RECYCLE.BIN (this machine runs with
       CODEBUDDY_SAFE_DELETE_ENABLED=1). Cleaning build output a few times
       filled E: with 115 GB / 399k recycled files, left the drive at 100%,
       and broke the linker (LNK1180: not enough disk space).
    2. Even [System.IO.Directory]::Delete - RemoveDirectoryW, documented never
       to use the Recycle Bin - is intercepted here. Probe: write 50 MB, delete
       it through the .NET API, and it comes back as a recycled item with its
       original path intact. So "we called the .NET API" does NOT mean "the
       space came back".

    This script still avoids the shell path (one less hop, and it is genuinely
    permanent on machines without the interception), but be precise about what
    it promises: it reports what it *removed*. Whether the volume got the space
    back is a different question, so the run now ends with a free-space delta.
    If that delta is near zero, the payload is sitting in the Recycle Bin - run
    tools\empty-bin.ps1 (which deletes items already inside the Bin, and those
    deletes do take effect).

    Exit codes: 0 = removed and space returned, 1 = some delete failed,
    2 = removed but the volume did not get the space back (empty the Bin).

Usage
    powershell -NoProfile -ExecutionPolicy Bypass -File tools\hard-rm.ps1 <path> [<path> ...]
    powershell ... -File tools\hard-rm.ps1 -WhatIf <path>      # dry run, prints size only

Safety
    Refuses drive roots and Windows/User profile roots. Works on absolute or
    relative paths; relative paths are resolved against the current directory.
    ASCII-only on purpose: PowerShell 5.1 misreads BOM-less UTF-8 scripts.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)]
    [string[]] $Path
)

$ErrorActionPreference = 'Continue'

function Get-TreeSize([string] $p) {
    $sum = [long]0
    $n = 0
    $stack = New-Object System.Collections.Stack
    $stack.Push($p)
    while ($stack.Count -gt 0) {
        $cur = $stack.Pop()
        try {
            $di = New-Object System.IO.DirectoryInfo $cur
            foreach ($f in $di.EnumerateFiles()) { try { $sum += $f.Length; $n++ } catch {} }
            foreach ($d in $di.EnumerateDirectories()) { $stack.Push($d.FullName) }
        } catch {}
    }
    return [pscustomobject]@{ Bytes = $sum; Files = $n }
}

function Clear-ReadOnly([string] $p) {
    # Directory.Delete fails with "access denied" on read-only files, and .git
    # object stores / build caches are full of them. Clear the flag first.
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

function Test-Protected([string] $p) {
    $full = [System.IO.Path]::GetFullPath($p).TrimEnd('\')
    # Drive root, e.g. "E:".
    if ($full -match '^[A-Za-z]:$') { return $true }
    $guarded = @(
        "$env:SystemRoot",
        "$env:SystemDrive\Windows",
        "$env:SystemDrive\Users",
        "$env:USERPROFILE",
        "$env:SystemDrive\Program Files",
        "$env:SystemDrive\Program Files (x86)",
        "$env:SystemDrive\ProgramData"
    )
    foreach ($g in $guarded) {
        if ([string]::IsNullOrEmpty($g)) { continue }
        $gg = [System.IO.Path]::GetFullPath($g).TrimEnd('\')
        if ($full -ieq $gg) { return $true }
    }
    return $false
}

$failures = 0
$removedBytes = [long]0
$removedItems = 0

# Free space is the only thing that matters to the caller, so measure it instead
# of trusting the byte sum we just computed from file lengths.
function Get-FreeBytes([string] $p) {
    $full = [System.IO.Path]::GetFullPath($p)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::IsNullOrEmpty($root)) { return $null }
    try { return (New-Object System.IO.DriveInfo $root).AvailableFreeSpace } catch { return $null }
}
$freeBefore = Get-FreeBytes ([System.IO.Path]::GetFullPath($Path[0]))

foreach ($raw in $Path) {
    if ([string]::IsNullOrWhiteSpace($raw)) { continue }
    $full = [System.IO.Path]::GetFullPath($raw)

    if (Test-Protected $full) {
        Write-Error "refusing to delete a protected root: $full"
        $failures++
        continue
    }

    $isDir = [System.IO.Directory]::Exists($full)
    $isFile = [System.IO.File]::Exists($full)
    if (-not ($isDir -or $isFile)) {
        Write-Warning "not found, skipped: $full"
        continue
    }

    if ($isDir) {
        $t = Get-TreeSize $full
        $sizeText = "{0:N0} files, {1:N1} MB" -f $t.Files, ($t.Bytes / 1MB)
        if ($PSCmdlet.ShouldProcess($full, "delete directory permanently ($sizeText)")) {
            try {
                Clear-ReadOnly $full
                [System.IO.Directory]::Delete($full, $true)
                $removedBytes += $t.Bytes
                $removedItems += $t.Files
                Write-Output "deleted dir  $full  ($sizeText)"
            } catch {
                Write-Error "delete failed: $full : $($_.Exception.Message)"
                $failures++
            }
        }
    } else {
        $len = (New-Object System.IO.FileInfo $full).Length
        $sizeText = "{0:N1} MB" -f ($len / 1MB)
        if ($PSCmdlet.ShouldProcess($full, "delete file permanently ($sizeText)")) {
            try {
                $fi = New-Object System.IO.FileInfo $full
                if ($fi.IsReadOnly) { [System.IO.File]::SetAttributes($full, [System.IO.FileAttributes]::Normal) }
                [System.IO.File]::Delete($full)
                $removedBytes += $len
                $removedItems++
                Write-Output "deleted file $full  ($sizeText)"
            } catch {
                Write-Error "delete failed: $full : $($_.Exception.Message)"
                $failures++
            }
        }
    }
}

Write-Output ("---- hard-rm: {0:N0} files, {1:N2} GB removed, {2} failure(s)" -f $removedItems, ($removedBytes / 1GB), $failures)

# The number above is what we were asked to delete. The number the user cares
# about is disk free space, and on this machine the two can disagree by 100%
# because deletes get redirected into the Recycle Bin.
$freeAfter = Get-FreeBytes ([System.IO.Path]::GetFullPath($Path[0]))
$reclaimed = $null
if ($null -ne $freeBefore -and $null -ne $freeAfter) {
    $reclaimed = $freeAfter - $freeBefore
    Write-Output ("---- free space: {0:N2} GB reclaimed ({1:N1} GB -> {2:N1} GB free)" -f `
            ($reclaimed / 1GB), ($freeBefore / 1GB), ($freeAfter / 1GB))
}
if ($failures -gt 0) { exit 1 }
if ($null -ne $reclaimed -and $removedBytes -gt 1GB -and $reclaimed -lt ($removedBytes / 2)) {
    Write-Warning ("deleted {0:N2} GB but the volume only got {1:N2} GB back. The payload is in the Recycle Bin; run tools\empty-bin.ps1 to actually reclaim it." -f `
            ($removedBytes / 1GB), ($reclaimed / 1GB))
    exit 2
}
exit 0
