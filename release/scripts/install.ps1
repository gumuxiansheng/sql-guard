#requires -Version 5.1
<#
.SYNOPSIS
    SqlGuard Windows 安装脚本。

.DESCRIPTION
    把发布包中 windows-x86_64 平台的二进制安装到指定目录，可选复制内置规则库与
    配置模板，可选把安装目录写入用户级 PATH。

.PARAMETER InstallDir
    安装根目录，默认 %LOCALAPPDATA%\SqlGuard（二进制落到 <InstallDir>\bin）。

.PARAMETER ConfigDir
    规则库与配置模板目录，默认 <InstallDir>\share。

.PARAMETER NoConfig
    只装二进制，不复制规则库与配置模板。

.PARAMETER IncludeDiagTools
    额外安装 mapdiag.exe 诊断工具。

.PARAMETER AddToPath
    把 <InstallDir>\bin 追加到「用户」PATH（重开终端生效，无需管理员权限）。

.PARAMETER Verify
    安装前按 SHA256SUMS 校验包内每个文件。

.PARAMETER Force
    覆盖已存在的同名文件。

.PARAMETER WhatIfMode
    只打印将要执行的动作，不实际写入。

.EXAMPLE
    .\install.ps1 -AddToPath -Verify

.EXAMPLE
    .\install.ps1 -InstallDir D:\tools\SqlGuard -NoConfig
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'SqlGuard'),
    [string]$ConfigDir = '',
    [switch]$NoConfig,
    [switch]$IncludeDiagTools,
    [switch]$AddToPath,
    [switch]$Verify,
    [switch]$Force,
    [switch]$WhatIfMode
)

$ErrorActionPreference = 'Stop'

$scriptDir  = Split-Path -Parent $MyInvocation.MyCommand.Path
$pkgRoot    = Split-Path -Parent $scriptDir
if ([string]::IsNullOrEmpty($ConfigDir)) { $ConfigDir = Join-Path $InstallDir 'share' }

$platformDir = 'windows-x86_64'
$binSrc      = Join-Path $pkgRoot "bin\$platformDir"
$binDst      = Join-Path $InstallDir 'bin'

function Write-Step { param([string]$Message) Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Item { param([string]$Message) Write-Host "    -> $Message" }
function Write-Skip { param([string]$Message) Write-Host "    skip $Message" }

Write-Step 'SqlGuard 安装 (Windows)'
Write-Host "    包根目录   : $pkgRoot"
Write-Host "    平台目录   : $platformDir"
Write-Host "    二进制目录 : $binDst"
if (-not $NoConfig) { Write-Host "    配置目录   : $ConfigDir" }
if ($WhatIfMode)    { Write-Host '    (WhatIf 模式，不会实际写入)' -ForegroundColor Yellow }

if (-not (Test-Path $binSrc)) {
    throw "找不到平台目录：$binSrc（包内可用：$( (Get-ChildItem (Join-Path $pkgRoot 'bin') -Directory).Name -join ', ')）"
}

# ---------- 校验 ----------
if ($Verify) {
    Write-Step '校验 SHA256SUMS'
    $sumFile = Join-Path $pkgRoot 'SHA256SUMS'
    if (-not (Test-Path $sumFile)) { throw "缺少 SHA256SUMS：$sumFile" }
    $failed = 0
    foreach ($line in (Get-Content $sumFile)) {
        if ($line -match '^\s*#') { continue }
        if ($line -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') { continue }
        $expected = $Matches[1].ToLower()
        $rel      = $Matches[2].Trim()
        $full     = Join-Path $pkgRoot $rel
        if (-not (Test-Path $full)) { Write-Host "    缺少文件：$rel" -ForegroundColor Red; $failed++; continue }
        $actual = (Get-FileHash $full -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) { Write-Host "    校验失败：$rel" -ForegroundColor Red; $failed++ }
    }
    if ($failed -gt 0) { throw "校验失败 $failed 个文件，请重新下载发布包" }
    Write-Host '    全部文件校验通过' -ForegroundColor Green
}

# ---------- 安装二进制 ----------
Write-Step '安装二进制'
if (-not $WhatIfMode) { New-Item -ItemType Directory -Force -Path $binDst | Out-Null } else { Write-Item "[dry] mkdir $binDst" }

$bins = @('sqlguard.exe', 'sqlguard-mine.exe')
if ($IncludeDiagTools) { $bins += 'mapdiag.exe' }

foreach ($b in $bins) {
    $src = Join-Path $binSrc $b
    $dst = Join-Path $binDst  $b
    if (-not (Test-Path $src)) { Write-Host "    warn 缺少 $src，跳过" -ForegroundColor Yellow; continue }
    if ((Test-Path $dst) -and -not $Force) { Write-Skip "$dst（已存在，用 -Force 覆盖）"; continue }
    if ($WhatIfMode) { Write-Item "[dry] copy $src -> $dst"; continue }
    Copy-Item $src $dst -Force
    Write-Item $dst
}

# ---------- 安装规则库与配置模板 ----------
if (-not $NoConfig) {
    Write-Step '安装规则库与配置模板'
    $rulesSrc = Join-Path $pkgRoot 'config\rules'
    if (Test-Path $rulesSrc) {
        $rulesDst = Join-Path $ConfigDir 'rules'
        if ($WhatIfMode) {
            Write-Item "[dry] copy $rulesSrc -> $rulesDst"
        } else {
            New-Item -ItemType Directory -Force -Path $rulesDst | Out-Null
            Copy-Item (Join-Path $rulesSrc '*') $rulesDst -Recurse -Force
            Write-Item $rulesDst
        }
    }
    foreach ($f in @('sqlguard.toml.example', 'sqlguard.rules.toml.example')) {
        $src = Join-Path $pkgRoot "config\$f"
        if (Test-Path $src) {
            if ($WhatIfMode) { Write-Item "[dry] copy $src -> $ConfigDir"; continue }
            New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
            Copy-Item $src $ConfigDir -Force
            Write-Item (Join-Path $ConfigDir $f)
        }
    }
}

# ---------- PATH ----------
if ($AddToPath) {
    Write-Step '追加用户 PATH'
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrEmpty($userPath)) { $userPath = '' }
    $parts = $userPath -split ';' | Where-Object { $_ -ne '' }
    if ($parts -notcontains $binDst) {
        if ($WhatIfMode) {
            Write-Item "[dry] PATH += $binDst"
        } else {
            [Environment]::SetEnvironmentVariable('Path', ($parts + $binDst) -join ';', 'User')
            Write-Item "已写入用户 PATH：$binDst"
        }
    } else {
        Write-Skip "PATH 已包含 $binDst"
    }
}

Write-Step '完成'
if (-not $AddToPath) {
    Write-Host "提示：可重开终端后运行 sqlguard --version；或把 $binDst 加入 PATH。"
}
Write-Host '验证：sqlguard --version'
