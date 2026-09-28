#!/usr/bin/env bash
# SqlGuard 卸载脚本（Linux / macOS）
#
# 用法：
#   sudo ./scripts/uninstall.sh [选项]
#
# 选项：
#   --prefix <DIR>     安装前缀，默认 /usr/local
#   --bin-dir <DIR>    二进制目录（优先级高于 --prefix），默认 $PREFIX/bin
#   --config-dir <DIR> 规则库目录，默认 $PREFIX/share/sqlguard
#   --user             等价于 --prefix "$HOME/.local"
#   --purge            同时删除规则库与配置模板
#   --dry-run          只打印将要执行的动作
#   -h, --help         显示帮助

set -euo pipefail

PREFIX="/usr/local"
BIN_DIR=""
CONFIG_DIR=""
USER_MODE=0
PURGE=0
DRY_RUN=0

usage() { sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)     PREFIX="$2"; shift 2 ;;
    --bin-dir)    BIN_DIR="$2"; shift 2 ;;
    --config-dir) CONFIG_DIR="$2"; shift 2 ;;
    --user)       USER_MODE=1; shift ;;
    --purge)      PURGE=1; shift ;;
    --dry-run)    DRY_RUN=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 2 ;;
  esac
done

[ "$USER_MODE" = "1" ] && PREFIX="${HOME}/.local"
[ -n "$BIN_DIR" ]    || BIN_DIR="${PREFIX}/bin"
[ -n "$CONFIG_DIR" ] || CONFIG_DIR="${PREFIX}/share/sqlguard"

run() {
  if [ "$DRY_RUN" = "1" ]; then echo "    [dry-run] $*"; else "$@"; fi
}

echo "==> SqlGuard 卸载"
echo "    二进制目录 : ${BIN_DIR}"
[ "$PURGE" = "1" ] && echo "    配置目录   : ${CONFIG_DIR}"

for b in sqlguard sqlguard-mine mapdiag; do
  dst="${BIN_DIR}/${b}"
  if [ -e "$dst" ]; then
    run rm -f "$dst"
    echo "    删除 ${dst}"
  else
    echo "    不存在 ${dst}"
  fi
done

if [ "$PURGE" = "1" ]; then
  if [ -d "$CONFIG_DIR" ]; then
    run rm -rf "$CONFIG_DIR"
    echo "    删除 ${CONFIG_DIR}"
  else
    echo "    不存在 ${CONFIG_DIR}"
  fi
fi

echo "==> 完成"
echo "提示：项目内的 sqlguard.toml / sqlguard.rules.toml / config/rules 属于你的仓库，未做任何改动。"
