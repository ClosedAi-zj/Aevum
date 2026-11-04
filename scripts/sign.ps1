<#
  恒晷 · Aevum Studio —— 代码签名接入点（可选，默认不阻断打包）

  用法：
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/sign.ps1 -Path dist/aevum/aevum.exe
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/sign.ps1 -Path dist/aevum
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/sign.ps1 -Path dist/installer/Aevum-Studio-1.9.0-setup.exe

  环境变量（三个都从环境取，**不要把口令写进脚本或仓库**）：
    AEVUM_SIGN_PFX        代码签名证书的 .pfx 路径；不设 = 不签名
    AEVUM_SIGN_PASSWORD   .pfx 的口令
    AEVUM_SIGN_TIMESTAMP  RFC3161 时间戳服务地址，缺省用 DigiCert

  为什么时间戳是必须的，不是可选项：
    代码签名证书有有效期（通常 1~3 年）。没有时间戳时，签名只在证书有效期内可信——
    证书一过期，**所有已经发出去的旧版本**都会变成「签名已过期」，Windows 会开始警告，
    而这些二进制一个字节都没变。带 RFC3161 时间戳后，时间戳服务为「这个签名在此刻
    存在」背书，验证方会拿**签名时间**去判有效期，于是证书到期后旧版本仍然有效。
    换句话说：不签时间戳 = 每次证书续期都要重新发布所有历史版本。

  为什么默认不阻断：
    签名需要一张公司证书。开发机、fork、社区 PR、离线构建都没有；如果缺证书就让打包
    失败，等于把「只有持证的人能出包」写进了构建链——而绿色包本身并不要求签名。
    所以这里的口径是：**没有证书 = 打印一行说明 + 退出码 0**，构建继续，产物照出。
    只有「配了证书但签失败」才算错误（退出码 1），因为那说明证书或时间戳服务坏了，
    静默放过会发出一批半签名的包。

  在 CI 里怎么配（GitHub Actions 示例）：
    - 把 .pfx 的 base64 存进仓库 Secret：SIGN_PFX_B64
    - 把口令存进 Secret：SIGN_PFX_PASSWORD
    - 步骤：
        - name: Sign
          shell: pwsh
          run: |
            $pfx = Join-Path $env:RUNNER_TEMP 'sign.pfx'
            [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:SIGN_PFX_B64))
            $env:AEVUM_SIGN_PFX = $pfx
            $env:AEVUM_SIGN_PASSWORD = $env:SIGN_PFX_PASSWORD
            ./scripts/sign.ps1 -Path dist/aevum
      （Secret 通过 env: 注入，不要 echo 到日志；.pfx 写完在 RUNNER_TEMP，随 runner 销毁。）

  安全提示：signtool 只接受命令行口令（/p），口令因此会出现在本机的进程列表里。这在
  一次性的构建机上可接受；在自己的工作站上，优先把证书装进用户证书存储（不带 /f /p），
  这条脚本留了 AEVUM_SIGN_PFX 为空时的跳过分支，但不会替你去用存储里的证书。

  ---

  Aevum Studio code-signing hook (optional, never blocks packaging by default).

  Usage:
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/sign.ps1 -Path dist/aevum
  Environment:
    AEVUM_SIGN_PFX        path to the .pfx; unset means "do not sign"
    AEVUM_SIGN_PASSWORD   passphrase for the .pfx
    AEVUM_SIGN_TIMESTAMP  RFC3161 timestamp URL (DigiCert by default)

  Why the timestamp is mandatory rather than optional:
    A code-signing certificate expires (usually in 1-3 years). Without a timestamp the
    signature is only trusted while the certificate is valid, so the moment it expires
    **every already-shipped build** starts reporting an expired signature even though not a
    byte changed. With an RFC3161 timestamp the service vouches that "this signature existed
    at this instant", verifiers check validity against the **signing time**, and old builds
    stay valid after expiry. Without a timestamp, every certificate renewal means
    re-releasing every historical build.

  Why it does not block by default:
    Signing needs a company certificate. Dev machines, forks, community PRs and offline
    builds have none, and failing packaging without one would bake "only certificate holders
    can build" into the chain -- while the portable zip never required signing anyway. So the
    rule is: **no certificate = one explanatory line + exit 0**, the build goes on. Only
    "configured but failed to sign" is an error (exit 1), because that means the certificate
    or the timestamp service is broken and silently shipping half-signed binaries is worse.

  Note: signtool only takes the passphrase on the command line (/p), so it is visible in the
  local process list. Acceptable on an ephemeral CI runner; on your own workstation prefer
  installing the certificate into the user store and signing without /f /p -- this script
  skips when AEVUM_SIGN_PFX is empty and will not silently fall back to a store certificate.
#>
[CmdletBinding()]
param(
    # 文件或目录。目录会递归找 *.exe / *.dll。
    # A file or a directory; a directory is scanned recursively for *.exe / *.dll.
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = 'Stop'

# 输出走 GBK 控制台时中文会乱码，所以这里统一用英文短句：脚本要在任何代码页下都能读懂。
# The console may be GBK, which mangles Chinese output, so every message here is English:
# the script has to stay readable under any code page.

function Write-Note([string]$Message) {
    Write-Host "[sign] $Message"
}

# ---- 1. 收集待签文件 -------------------------------------------------------
# 目录只挑 *.exe / *.dll：zip、md、licenses 不能签，签了也会破坏 SHA256SUMS。
# Only *.exe / *.dll from a directory: zips, docs and licences cannot be signed, and signing
# them would break the published SHA256SUMS.
if (Test-Path -LiteralPath $Path -PathType Container) {
    $targets = @(Get-ChildItem -LiteralPath $Path -Recurse -File -Include '*.exe', '*.dll' -ErrorAction SilentlyContinue)
} elseif (Test-Path -LiteralPath $Path -PathType Leaf) {
    $targets = @(Get-Item -LiteralPath $Path)
} else {
    Write-Note "path not found: $Path"
    exit 1
}

if ($targets.Count -eq 0) {
    Write-Note "no .exe or .dll under $Path, nothing to sign"
    exit 0
}

# ---- 2. 找 signtool --------------------------------------------------------
# 先看 PATH，再按版本倒序找 Windows Kits（同一台机器常装了好几个 SDK，取最新的那个）。
# PATH first, then Windows Kits newest-first (a machine often carries several SDKs).
function Find-SignTool {
    $cmd = Get-Command 'signtool.exe' -ErrorAction SilentlyContinue
    if ($null -ne $cmd) {
        return $cmd.Source
    }
    $kits = @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin",
        "$env:ProgramFiles\Windows Kits\10\bin"
    ) | Where-Object { Test-Path -LiteralPath $_ }

    foreach ($root in $kits) {
        $candidates = @(Get-ChildItem -LiteralPath $root -Recurse -Filter 'signtool.exe' -File -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\x64\\signtool\.exe$' } |
            Sort-Object -Property FullName -Descending)
        if ($candidates.Count -gt 0) {
            return $candidates[0].FullName
        }
    }
    return $null
}

$signTool = Find-SignTool
if ($null -eq $signTool) {
    # 不阻断：没装 Windows SDK 的开发机照样要能出包。
    # Do not block: a dev machine without the Windows SDK must still be able to package.
    Write-Note "signtool.exe not found (install the Windows SDK), skipping signing - build continues"
    exit 0
}

# ---- 3. 证书 ---------------------------------------------------------------
$pfx = $env:AEVUM_SIGN_PFX
$pwd = $env:AEVUM_SIGN_PASSWORD

if ([string]::IsNullOrWhiteSpace($pfx)) {
    Write-Note "AEVUM_SIGN_PFX not set, signing skipped - build continues (see scripts/sign.ps1 header for CI setup)"
    exit 0
}
if (-not (Test-Path -LiteralPath $pfx -PathType Leaf)) {
    # 配了但文件不在 = 配置错误，必须红：否则这一批包会以「以为签了其实没签」的样子发出去。
    # Configured but missing is a misconfiguration and must fail: otherwise these builds ship
    # believing they are signed when they are not.
    Write-Note "AEVUM_SIGN_PFX points to a missing file: $pfx"
    exit 1
}
if ([string]::IsNullOrEmpty($pwd)) {
    Write-Note "AEVUM_SIGN_PASSWORD is empty but AEVUM_SIGN_PFX is set; refusing to sign with an empty passphrase"
    exit 1
}

# RFC3161 时间戳服务：必须支持 /tr（RFC3161），老的 /t 是 Authenticode 时间戳，
# 证书过期后的行为不如 RFC3161 可靠。
# RFC3161 timestamp server: /tr (RFC3161) is required; the older /t (Authenticode) does not
# survive certificate expiry as reliably.
$timestamp = $env:AEVUM_SIGN_TIMESTAMP
if ([string]::IsNullOrWhiteSpace($timestamp)) {
    $timestamp = 'http://timestamp.digicert.com'
}

# ---- 4. 签 ----------------------------------------------------------------
$failed = 0
foreach ($f in $targets) {
    Write-Note "signing $($f.FullName)"
    & $signTool sign /fd sha256 /td sha256 /tr $timestamp /f $pfx /p $pwd $f.FullName
    if ($LASTEXITCODE -ne 0) {
        Write-Note "FAILED to sign $($f.FullName) (signtool exit $LASTEXITCODE)"
        $failed++
    }
}

if ($failed -gt 0) {
    Write-Note "$failed file(s) failed to sign; the certificate or the timestamp service is broken"
    exit 1
}

# ---- 5. 回验 --------------------------------------------------------------
# 签完立刻 verify /pa：signtool 返回 0 只代表「命令跑完了」，证书链不完整、时间戳没贴上
# 这些情况都要靠 verify 才能发现。
# Verify right after signing: exit 0 from `sign` only means the command ran; an incomplete
# chain or a missing countersignature only shows up in `verify`.
foreach ($f in $targets) {
    & $signTool verify /pa /q $f.FullName
    if ($LASTEXITCODE -ne 0) {
        Write-Note "signature verification failed: $($f.FullName)"
        exit 1
    }
}

Write-Note "signed and verified $($targets.Count) file(s)"
exit 0
