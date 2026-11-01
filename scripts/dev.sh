#!/usr/bin/env bash
# 启动 modern-sshdeck 开发模式（Windows / MSVC）。
#
# 为什么需要这个脚本：
#   本机 MSVC 在写入调试信息（-Z7）时失败，导致默认的 debug profile 无法编译
#   第三方 C 依赖（ring 等）。因此这里强制使用 release profile。
#   详见 docs/BUILD-ENV.md。
#
# 用法：
#   bash scripts/dev.sh            # tauri dev（release）
#   bash scripts/dev.sh run        # 直接跑已构建的二进制（跳过 Vite）

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VS_PATH="${VS_PATH:-F:/vsiual studio}"
MSVC_VER="${MSVC_VER:-14.51.36231}"
MODE="${1:-dev}"

VCVARS="$(cygpath -w "$VS_PATH/VC/Auxiliary/Build/vcvars64.bat" 2>/dev/null || echo "$VS_PATH/VC/Auxiliary/Build/vcvars64.bat")"
MSVC_BIN="$(cygpath -w "$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostx64/x64" 2>/dev/null || echo "$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostx64/x64")"
PROJ_WIN="$(cygpath -w "$PROJECT_DIR" 2>/dev/null || echo "$PROJECT_DIR")"

BAT_DIR="$(cmd //c 'echo %TEMP%' | tr -d '\r')"
BAT="${BAT_DIR}\\sshdeck-dev-$$.bat"

if [ "$MODE" = "run" ]; then
  TARGET_CMD="start \"\" \"%PROJ%\\src-tauri\\target\\release\\modern-sshdeck.exe\""
else
  TARGET_CMD="pnpm tauri dev --release"
fi

{
  printf '@echo off\r\n'
  printf 'call "%s" >nul\r\n' "$VCVARS"
  printf 'set "PATH=%s;%%PATH%%"\r\n' "$MSVC_BIN"
  printf 'cd /d "%s"\r\n' "$PROJ_WIN"
  printf 'set "PROJ=%s"\r\n' "$PROJ_WIN"
  printf '%s\r\n' "$TARGET_CMD"
  printf 'exit /b %%ERRORLEVEL%%\r\n'
} > "$BAT"

WIN_BAT="$(cygpath -w "$BAT" 2>/dev/null || echo "$BAT")"
cmd //c "$WIN_BAT"
STATUS=$?
rm -f "$BAT"
exit $STATUS
