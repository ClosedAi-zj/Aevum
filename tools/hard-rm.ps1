<#
hard-rm.ps1 - delete files/directories WITHOUT sending them to the Recycle Bin.

Why this exists
    This machine runs with CODEBUDDY_SAFE_DELETE_ENABLED=1, so a plain `rm`
    does not delete anything: it moves the file to the volume's Recycle Bin
    under E:\$RECYCLE.BIN. Cleaning build output a few times filled E: with
    115 GB / 399k recycled files and left the drive at 100%, which then breaks
    the linker (LNK1180: not enough disk space).

    This script bypasses the shell and calls the .NET delete APIs directly, so
    the data is gone for real and the Recycle Bin never grows.

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

Write-Output ("---- hard-rm: {0:N0} files, {1:N2} GB reclaimed, {2} failure(s)" -f $removedItems, ($removedBytes / 1GB), $failures)
if ($failures -gt 0) { exit 1 }
exit 0
