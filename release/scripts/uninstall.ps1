#requires -Version 5.1
<#
.SYNOPSIS
    SqlGuard Windows 卸载脚本。

.PARAMETER InstallDir
    安装根目录，默认 %LOCALAPPDATA%\SqlGuard。

.PARAMETER Purge
    同时删除 <InstallDir>\share（规则库与配置模板）。

.PARAMETER RemovePath
    从用户 PATH 中移除 <InstallDir>\bin。

.PARAMETER WhatIfMode
    只打印动作。

.EXAMPLE
    .\uninstall.ps1 -Purge -RemovePath
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'SqlGuard'),
    [switch]$Purge,
    [switch]$RemovePath,
    [switch]$WhatIfMode
)

$ErrorActionPreference = 'Stop'
$binDst = Join-Path $InstallDir 'bin'
$share  = Join-Path $InstallDir 'share'

Write-Host '==> SqlGuard 卸载 (Windows)' -ForegroundColor Cyan
Write-Host "    安装目录 : $InstallDir"

foreach ($b in @('sqlguard.exe', 'sqlguard-mine.exe', 'mapdiag.exe')) {
    $p = Join-Path $binDst $b
    if (Test-Path $p) {
        if ($WhatIfMode) { Write-Host "    [dry] remove $p" } else { Remove-Item $p -Force; Write-Host "    删除 $p" }
    } else {
        Write-Host "    不存在 $p"
    }
}

if ($Purge -and (Test-Path $share)) {
    if ($WhatIfMode) { Write-Host "    [dry] remove $share" } else { Remove-Item $share -Recurse -Force; Write-Host "    删除 $share" }
}

if ($RemovePath) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not [string]::IsNullOrEmpty($userPath)) {
        $parts = $userPath -split ';' | Where-Object { $_ -ne '' -and $_ -ne $binDst }
        $new = $parts -join ';'
        if ($WhatIfMode) {
            Write-Host "    [dry] PATH 移除 $binDst"
        } else {
            [Environment]::SetEnvironmentVariable('Path', $new, 'User')
            Write-Host "    已从用户 PATH 移除 $binDst"
        }
    }
}

# 目录若已空则删除
if ((Test-Path $binDst) -and ((Get-ChildItem $binDst -Force | Measure-Object).Count -eq 0)) {
    if (-not $WhatIfMode) { Remove-Item $binDst -Force }
    Write-Host "    删除空目录 $binDst"
}

Write-Host '==> 完成'
Write-Host '提示：项目内的 sqlguard.toml / sqlguard.rules.toml / config/rules 属于你的仓库，未做任何改动。'
