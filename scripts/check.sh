#!/usr/bin/env bash
# Rust 检查（使用 release profile，规避本机 MSVC -Z7 问题）
#
# 用法：
#   bash scripts/check.sh            # cargo check --release
#   bash scripts/check.sh build      # cargo build --release
#   bash scripts/check.sh test       # cargo test --release
#   bash scripts/check.sh all        # i18n 校验 + cargo check（默认完整检查）
#
# 环境变量：
#   VS_PATH     Visual Studio 安装目录（默认 F:\vsiual studio）
#   MSVC_VER    MSVC 工具集版本    （默认 14.51.36231）
#   SKIP_I18N   设为 1 则跳过 i18n 校验
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VS_PATH="${VS_PATH:-F:/vsiual studio}"
MSVC_VER="${MSVC_VER:-14.51.36231}"
CMD="${1:-check}"

cd "$PROJECT_DIR" || exit 1

# ─────────────────────────── i18n 校验前置 ───────────────────────────
#
# 为什么放在 cargo 之前：
#   i18n key 缺失是**编译期抓不到**的问题（`t("host.connect")` 的入参是普通字符串，
#   key 不存在时 i18next 会把 key 名原样渲染，界面出现 `host.connect` 这种字面量）。
#   先跑这一步能在几毫秒内给出结论，不必等几分钟的 cargo 编译。
#
#   `test` / `build` 也一并校验 —— 这三个场景都需要「文案无缺失」这个前提。
if [ "${SKIP_I18N:-0}" != "1" ]; then
  echo "── i18n 校验 ──"
  if command -v python >/dev/null 2>&1; then
    python "$SCRIPT_DIR/check-i18n.py"
    I18N_STATUS=$?
  elif command -v python3 >/dev/null 2>&1; then
    python3 "$SCRIPT_DIR/check-i18n.py"
    I18N_STATUS=$?
  else
    echo "[警告] 未找到 python，跳过 i18n 校验" >&2
    I18N_STATUS=0
  fi

  if [ $I18N_STATUS -ne 0 ]; then
    echo ""
    echo "✗ i18n 校验失败：上面的 key 在资源里不存在，界面会显示 key 名而非文案。" >&2
    echo "  修好后重跑，或用 SKIP_I18N=1 临时跳过。" >&2
    exit $I18N_STATUS
  fi
  echo ""
fi

# ─────────────────────────── cargo ───────────────────────────

# 统一用正斜杠，Windows cmd 能接受，也避免 bash 转义问题。
MANIFEST="$(cygpath -w "$PROJECT_DIR/src-tauri/Cargo.toml" 2>/dev/null || echo "$PROJECT_DIR/src-tauri/Cargo.toml")"
VCVARS="$(cygpath -w "$VS_PATH/VC/Auxiliary/Build/vcvars64.bat" 2>/dev/null || echo "$VS_PATH/VC/Auxiliary/Build/vcvars64.bat")"
MSVC_BIN="$(cygpath -w "$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostx64/x64" 2>/dev/null || echo "$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostx64/x64")"

# `all` = 只做 check（i18n 已在上面跑过）
CARGO_CMD="$CMD"
[ "$CMD" = "all" ] && CARGO_CMD="check"

BAT_DIR="$(cmd //c 'echo %TEMP%' | tr -d '\r')"
BAT="${BAT_DIR}\\sshdeck-check-$$.bat"

{
  printf '@echo off\r\n'
  printf 'call "%s" >nul\r\n' "$VCVARS"
  printf 'set "PATH=%s;%%PATH%%"\r\n' "$MSVC_BIN"
  printf 'cargo %s --release --manifest-path "%s" 2>&1\r\n' "$CARGO_CMD" "$MANIFEST"
  printf 'exit /b %%ERRORLEVEL%%\r\n'
} > "$BAT"

WIN_BAT="$(cygpath -w "$BAT" 2>/dev/null || echo "$BAT")"
cmd //c "$WIN_BAT"
STATUS=$?
rm -f "$BAT"
exit $STATUS
