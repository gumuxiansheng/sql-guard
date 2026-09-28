#!/usr/bin/env bash
# SqlGuard 发布包校验脚本（Linux / macOS）
#
# 做两件事：
#   1) 按 SHA256SUMS 校验包内每个文件的 SHA-256
#   2) 对本机平台的二进制执行 --version 冒烟测试
#
# 用法：
#   ./scripts/verify.sh            # 校验 + 冒烟
#   ./scripts/verify.sh --sums-only # 只校验哈希
#
# 退出码：0 = 全部通过；1 = 存在失败项

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SUMS_ONLY=0
[ "${1:-}" = "--sums-only" ] && SUMS_ONLY=1

cd "$PKG_ROOT"

echo "==> [1/2] 校验 SHA256SUMS"
FAILED=0
if command -v sha256sum >/dev/null 2>&1; then
  CHECKER="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  CHECKER="shasum"
else
  echo "warn: 未找到 sha256sum / shasum，跳过哈希校验"
  CHECKER=""
fi

if [ -n "$CHECKER" ]; then
  if [ ! -f SHA256SUMS ]; then
    echo "error: 缺少 SHA256SUMS"; FAILED=1
  else
    TOTAL=0
    while IFS= read -r line; do
      [ -z "$line" ] && continue
      case "$line" in \#*) continue ;; esac
      HASH="$(echo "$line" | awk '{print $1}')"
      REL="$(echo "$line" | sed -E 's/^[0-9a-fA-F]+[[:space:]]+\*?//')"
      TOTAL=$((TOTAL + 1))
      if [ ! -f "$REL" ]; then
        echo "  MISSING  $REL"; FAILED=$((FAILED + 1)); continue
      fi
      ACTUAL="$($CHECKER "$REL" | awk '{print $1}')"
      if [ "${ACTUAL,,}" != "${HASH,,}" ]; then
        echo "  FAILED   $REL"; FAILED=$((FAILED + 1))
      fi
    done < SHA256SUMS
    echo "  已核对 $TOTAL 个文件"
  fi
fi

if [ "$SUMS_ONLY" = "1" ]; then
  [ "$FAILED" -eq 0 ] && echo "==> 校验通过" || { echo "==> 校验失败：$FAILED 项"; exit 1; }
  exit 0
fi

echo "==> [2/2] 二进制冒烟（本机平台）"
OS="$(uname -s)"; ARCH="$(uname -m)"
case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64|amd64)  PLAT="linux-x86_64-musl" ;;
      aarch64|arm64) PLAT="linux-aarch64-musl" ;;
      *) PLAT="" ;;
    esac ;;
  *) PLAT="" ;;
esac

if [ -z "$PLAT" ] || [ ! -d "bin/$PLAT" ]; then
  echo "  跳过：本机平台（$OS/$ARCH）无对应二进制目录"
else
  for b in sqlguard sqlguard-mine mapdiag; do
    f="bin/$PLAT/$b"
    if [ -f "$f" ]; then
      chmod +x "$f" 2>/dev/null || true
      if OUT="$("$f" --version 2>&1)"; then
        echo "  OK       $f -> $OUT"
      else
        echo "  FAILED   $f -> $OUT"; FAILED=$((FAILED + 1))
      fi
    fi
  done
fi

echo
if [ "$FAILED" -eq 0 ]; then
  echo "==> 全部通过：发布包完整且二进制可运行"
  exit 0
else
  echo "==> 存在 $FAILED 项失败，请重新下载发布包"
  exit 1
fi
