# 初始化 MSVC 环境并执行 cargo / tauri 命令（release profile）。
#
# 用法：
#   pwsh -File scripts/build-msvc.ps1 -Command "cargo check --release"
#   pwsh -File scripts/build-msvc.ps1 -Command "pnpm tauri build"

param(
  [Parameter(Mandatory = $true)]
  [string]$Command,
  [string]$VsPath = "F:\vsiual studio"
)

$ErrorActionPreference = "Stop"

$vcvars = Join-Path $VsPath "VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path $vcvars)) {
  throw "vcvars64.bat not found at $vcvars ; set -VsPath to your Visual Studio directory."
}

$vcTools = Get-ChildItem (Join-Path $VsPath "VC\Tools\MSVC") -Directory |
  Sort-Object Name -Descending | Select-Object -First 1
if (-not $vcTools) { throw "No MSVC toolset found under $VsPath\VC\Tools\MSVC" }
$msvcBin = Join-Path $vcTools.FullName "bin\Hostx64\x64"

# 用一个临时 bat 保证 vcvars 在同一进程内生效，并把 MSVC bin 提到 PATH 最前。
$bat = Join-Path $env:TEMP "sshdeck-build-$([guid]::NewGuid().ToString('N')).bat"
$lines = @(
  "@echo off"
  "call `"$vcvars`" >nul"
  "set PATH=$msvcBin;%PATH%"
  "$Command"
)
Set-Content -Path $bat -Encoding ASCII -Value $lines
try {
  cmd /c $bat
  exit $LASTEXITCODE
} finally {
  Remove-Item $bat -ErrorAction SilentlyContinue
}
