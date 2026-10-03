# build.ps1 — colored_echo_img.mod2 ビルド & デプロイ
param(
    [switch]$NoDeploy
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$SrcDir = $PSScriptRoot
$AviUtl2Root = "C:\ProgramData\aviutl2"
$ScriptDir = Join-Path $AviUtl2Root "Script\ColoredEchoImg"
$TargetDir = Join-Path $SrcDir "target\release"

Write-Host "`n=== ColoredEchoImg ビルド (Rust mod2) ===" -ForegroundColor Yellow

Push-Location $SrcDir
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

$Dll = Join-Path $TargetDir "colored_echo_img.dll"
if (-not (Test-Path $Dll)) { Write-Error "missing $Dll" }

$ModOut = Join-Path $SrcDir "colored_echo_img.mod2"
Copy-Item -Path $Dll -Destination $ModOut -Force
Write-Host "  完了: $ModOut" -ForegroundColor Green

if (-not $NoDeploy) {
    Write-Host "`n=== デプロイ ===" -ForegroundColor Yellow
    if (-not (Test-Path $ScriptDir)) {
        New-Item -ItemType Directory -Path $ScriptDir -Force | Out-Null
    }
    Copy-Item -Path $ModOut -Destination (Join-Path $ScriptDir "colored_echo_img.mod2") -Force
    Write-Host "  配置: $(Join-Path $ScriptDir 'colored_echo_img.mod2')" -ForegroundColor DarkGreen
}

Write-Host "`n全ビルド完了`n" -ForegroundColor Yellow
