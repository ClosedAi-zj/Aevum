# 空闲 CPU / 峰值内存 / 可交互时刻探针。
#
# 为什么要在进程外量：`--bench-startup` 报的是「进程进来 → 首帧画完」的 CPU 墙钟，
# 它量不到两件事——① 上屏之后的空闲占用（用户什么都不做时窗口还在耗多少 CPU，
# 这直接决定笔记本风扇和电量）；② 峰值工作集（一张 24MP RAW 打开后进程最高吃到
# 多少内存）。这两条只能从外面看进程。
#
# 口径：
#   - 「空闲」= 不拖滑块、不切面板、鼠标不动、没有后台导出。窗口起来后再等 3 秒
#     让字体装配、缩略图队列、首帧解码这些启动抖动走完，然后才开始计 10 秒。
#   - CPU% = 这 10 秒里进程消耗的 CPU 时间 / 10 秒。多核上单线程满载 = 100%，
#     所以这条不是「整机占用」，是「这个进程占了一个核的多少」。
#   - **量的时候不许最小化窗口、不许锁屏、不许动鼠标**：Windows 会给最小化窗口
#     降帧甚至停帧，空闲 CPU 会被低估（QA 复审 2026-11-04 指出的唯一失真方向，
#     方向是「往好了量」——正好是验收最忌讳的那种）。脚本没法替你保证这些，
#     所以只写在这里；跑之前看一眼窗口还在不在前台。
#   - 「启动到可交互」= 从进程创建到窗口句柄出现。它含 Windows 窗口动画与 DWM
#     合成，噪声 200ms 以上，**只能当人感受的参考，不能当回归判据**（回归判据用
#     `--bench-startup` 的 CPU 墙钟）。
#
# Idle CPU / peak memory / interactive-moment probe.
#
# Why measure from outside: `--bench-startup` reports process entry to first frame and
# cannot see two things -- (1) idle cost after the window is up (what drains a laptop
# battery while the user does nothing), and (2) peak working set after opening a 24MP RAW.
# Both need an outside view of the process.
#
# Basis: "idle" means no slider drag, no panel switch, no mouse movement, no background
# export; after the window appears we wait 3s for start-up jitter (fonts, thumbnail queue,
# first decode) and then count for 10s. CPU% is CPU time over those 10s, i.e. the share of
# one core, not of the machine. "Startup to interactive" runs from process creation to the
# window handle appearing; it includes Windows animation and DWM compositing (200ms+ of
# noise) and is a human-perception reference only, never a regression fence.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$Seconds = 10,
    [string]$Sample = "",
    [int]$SettleSeconds = 3
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Exe)) {
    throw "ux_probe: exe not found: $Exe"
}
$exePath = (Resolve-Path -LiteralPath $Exe).Path

$argList = @()
if ($Sample -ne "") {
    if (-not (Test-Path -LiteralPath $Sample)) {
        throw "ux_probe: sample not found: $Sample"
    }
    $argList += ('"' + (Resolve-Path -LiteralPath $Sample).Path + '"')
}

$proc = $null
try {
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    if ($argList.Count -gt 0) {
        $proc = Start-Process -FilePath $exePath -ArgumentList $argList -PassThru
    }
    else {
        $proc = Start-Process -FilePath $exePath -PassThru
    }

    # 等窗口：最多 20 秒。超时就是红，不许「大概起来了就算」。
    $deadline = (Get-Date).AddSeconds(20)
    $windowHandle = 0
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 50
        $proc.Refresh()
        if ($proc.HasExited) {
            throw "ux_probe: process exited before its window appeared (exit code $($proc.ExitCode))"
        }
        if ($proc.MainWindowHandle -ne 0) {
            $windowHandle = $proc.MainWindowHandle
            break
        }
    }
    if ($windowHandle -eq 0) {
        throw "ux_probe: no window handle within 20s; the process never became interactive"
    }
    $interactiveMs = [int][math]::Round($clock.Elapsed.TotalMilliseconds)

    Start-Sleep -Seconds $SettleSeconds

    $proc.Refresh()
    if ($proc.HasExited) {
        throw "ux_probe: process exited before the idle window started"
    }
    $cpu0 = $proc.TotalProcessorTime
    Start-Sleep -Seconds $Seconds
    $proc.Refresh()
    if ($proc.HasExited) {
        throw "ux_probe: process exited during the idle window"
    }
    $cpu1 = $proc.TotalProcessorTime

    $cpuSeconds = ($cpu1 - $cpu0).TotalSeconds
    $cpuPct = [math]::Round(($cpuSeconds / $Seconds) * 100.0, 2)
    $peakGb = [math]::Round($proc.PeakWorkingSet64 / 1GB, 2)

    # 输出**只许 ASCII**：PowerShell 5.1 往管道写中文用的是控制台的 OEM 代码页（本机
    # GBK），bash 按 UTF-8 解就成乱码，调用方连数字都匹配不到。中文措辞留给调用方
    # （`acceptance/ux_budget.sh`）去拼——这里只交键值，键值永远可解析。
    #
    # ASCII-only output: PowerShell 5.1 writes to a pipe in the console's OEM code page
    # (GBK here) while the caller reads UTF-8, which turns the line into mojibake and hides
    # even the numbers. Chinese wording belongs to the caller; this prints keys and values.
    Write-Output ("IDLE_CPU_PCT={0} PEAK_WS_GB={1} STARTUP_INTERACTIVE_MS={2} SECONDS={3}" -f `
            $cpuPct, $peakGb, $interactiveMs, $Seconds)
}
finally {
    # 无论成败都不许留僵尸窗口：基准进程挂在那儿会污染下一次测量。
    if ($null -ne $proc) {
        $proc.Refresh()
        if (-not $proc.HasExited) {
            Stop-Process -Id $proc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
        }
    }
}
