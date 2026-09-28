#requires -Version 5.1
<#
.SYNOPSIS
    SqlGuard 发布包校验脚本（Windows）。

.DESCRIPTION
    1) 按 SHA256SUMS 校验包内每个文件的 SHA-256
    2) 对 windows-x86_64 二进制执行 --version 冒烟测试

.EXAMPLE
    .\scripts\verify.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$pkgRoot   = Split-Path -Parent $scriptDir
Set-Location $pkgRoot

$failed = 0
$total  = 0

Write-Host '==> [1/2] 校验 SHA256SUMS' -ForegroundColor Cyan
$sumFile = Join-Path $pkgRoot 'SHA256SUMS'
if (-not (Test-Path $sumFile)) {
    Write-Host '    error: 缺少 SHA256SUMS' -ForegroundColor Red
    $failed++
} else {
    foreach ($line in (Get-Content $sumFile)) {
        if ($line -match '^\s*#') { continue }
        if ($line -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') { continue }
        $expected = $Matches[1].ToLower()
        $rel      = $Matches[2].Trim()
        $total++
        $full = Join-Path $pkgRoot $rel
        if (-not (Test-Path $full)) {
            Write-Host "    MISSING  $rel" -ForegroundColor Red
            $failed++
            continue
        }
        $actual = (Get-FileHash $full -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) {
            Write-Host "    FAILED   $rel" -ForegroundColor Red
            $failed++
        }
    }
    Write-Host "    已核对 $total 个文件"
}

Write-Host '==> [2/2] 二进制冒烟 (windows-x86_64)' -ForegroundColor Cyan
$binDir = Join-Path $pkgRoot 'bin\windows-x86_64'
if (-not (Test-Path $binDir)) {
    Write-Host '    跳过：未找到 bin\windows-x86_64' -ForegroundColor Yellow
} else {
    foreach ($b in @('sqlguard.exe', 'sqlguard-mine.exe', 'mapdiag.exe')) {
        $f = Join-Path $binDir $b
        if (Test-Path $f) {
            try {
                $out = & $f '--version' 2>&1
                if ($LASTEXITCODE -eq 0) {
                    Write-Host "    OK       $b -> $out" -ForegroundColor Green
                } else {
                    Write-Host "    FAILED   $b -> $out" -ForegroundColor Red
                    $failed++
                }
            } catch {
                Write-Host "    FAILED   $b -> $_" -ForegroundColor Red
                $failed++
            }
        }
    }
}

Write-Host ''
if ($failed -eq 0) {
    Write-Host '==> 全部通过：发布包完整且二进制可运行' -ForegroundColor Green
    exit 0
} else {
    Write-Host "==> 存在 $failed 项失败，请重新下载发布包" -ForegroundColor Red
    exit 1
}
