#!/usr/bin/env bash
# Rust 检查（使用 release profile，规避本机 MSVC -Z7 问题）
#
# 用法：
#   bash scripts/check.sh            # cargo check --release
#   bash scripts/check.sh build      # cargo build --release
#   bash scripts/check.sh test       # cargo test --release
#
# 环境变量：
#   VS_PATH     Visual Studio 安装目录（默认 F:\vsiual studio）
#   MSVC_VER    MSVC 工具集版本    （默认 14.51.36231）
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VS_PATH="${VS_PATH:-F:/vsiual studio}"
MSVC_VER="${MSVC_VER:-14.51.36231}"
CMD="${1:-check}"

# 统一用正斜杠，Windows cmd 能接受，也避免 bash 转义问题。
MANIFEST="$(cygpath -w "$PROJECT_DIR/src-tauri/Cargo.toml" 2>/dev/null || echo "$PROJECT_DIR/src-tauri/Cargo.toml")"
VCVARS="$(cygpath -w "$VS_PATH/VC/Auxiliary/Build/vcvars64.bat" 2>/dev/null || echo "$VS_PATH/VC/Auxiliary/Build/vcvars64.bat")"
MSVC_BIN="$(cygpath -w "$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostx64/x64" 2>/dev/null || echo "$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostx64/x64")"

BAT_DIR="$(cmd //c 'echo %TEMP%' | tr -d '\r')"
BAT="${BAT_DIR}\\sshdeck-check-$$.bat"

{
  printf '@echo off\r\n'
  printf 'call "%s" >nul\r\n' "$VCVARS"
  printf 'set "PATH=%s;%%PATH%%"\r\n' "$MSVC_BIN"
  printf 'cargo %s --release --manifest-path "%s" 2>&1\r\n' "$CMD" "$MANIFEST"
  printf 'exit /b %%ERRORLEVEL%%\r\n'
} > "$BAT"

WIN_BAT="$(cygpath -w "$BAT" 2>/dev/null || echo "$BAT")"
cmd //c "$WIN_BAT"
STATUS=$?
rm -f "$BAT"
exit $STATUS
