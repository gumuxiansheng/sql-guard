#!/usr/bin/env bash
# SqlGuard 安装脚本（Linux / macOS）
#
# 用法：
#   ./scripts/install.sh [选项]
#
# 选项：
#   --prefix <DIR>        安装前缀，默认 /usr/local（二进制落到 $PREFIX/bin）
#   --bin-dir <DIR>       直接指定二进制目录（优先级高于 --prefix）
#   --config-dir <DIR>    规则库与配置模板目录，默认 $PREFIX/share/sqlguard
#   --user                等价于 --prefix "$HOME/.local"（无需 root）
#   --no-config           只装二进制，不复制规则库与配置模板
#   --with-diag           额外安装 mapdiag 诊断工具
#   --verify              安装前按 SHA256SUMS 校验包内文件
#   --force               覆盖已存在的同名文件
#   --dry-run             只打印将要执行的动作
#   -h, --help            显示帮助
#
# 环境变量：
#   SQLGUARD_PLATFORM_DIR   强制指定 bin/ 下的平台目录名（自动识别失败时使用）

set -euo pipefail

# ---------- 默认值 ----------
PREFIX="/usr/local"
BIN_DIR=""
CONFIG_DIR=""
USER_MODE=0
INSTALL_CONFIG=1
WITH_DIAG=0
DO_VERIFY=0
FORCE=0
DRY_RUN=0

# ---------- 参数解析 ----------
usage() { sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)     PREFIX="$2"; shift 2 ;;
    --bin-dir)    BIN_DIR="$2"; shift 2 ;;
    --config-dir) CONFIG_DIR="$2"; shift 2 ;;
    --user)       USER_MODE=1; shift ;;
    --no-config)  INSTALL_CONFIG=0; shift ;;
    --with-diag)  WITH_DIAG=1; shift ;;
    --verify)     DO_VERIFY=1; shift ;;
    --force)      FORCE=1; shift ;;
    --dry-run)    DRY_RUN=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "error: 未知参数 $1（用 --help 查看用法）" >&2; exit 2 ;;
  esac
done

if [ "$USER_MODE" = "1" ]; then
  PREFIX="${HOME}/.local"
fi
[ -n "$BIN_DIR" ]    || BIN_DIR="${PREFIX}/bin"
[ -n "$CONFIG_DIR" ] || CONFIG_DIR="${PREFIX}/share/sqlguard"

# ---------- 定位发布包根目录 ----------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

log()  { echo "==> $*"; }
warn() { echo "warn: $*" >&2; }
die()  { echo "error: $*" >&2; exit 1; }

run() {
  if [ "$DRY_RUN" = "1" ]; then
    echo "    [dry-run] $*"
  else
    "$@"
  fi
}

# ---------- 平台识别 ----------
detect_platform_dir() {
  if [ -n "${SQLGUARD_PLATFORM_DIR:-}" ]; then
    echo "$SQLGUARD_PLATFORM_DIR"; return
  fi
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64)   echo "linux-x86_64-musl" ;;
        aarch64|arm64)  echo "linux-aarch64-musl" ;;
        *) die "不支持的 CPU 架构：$arch（可用 SQLGUARD_PLATFORM_DIR 手动指定）" ;;
      esac ;;
    Darwin)
      die "本发布包不含 macOS 二进制。请从 Release 附件下载 sqlguard-{x86_64,aarch64}-apple-darwin，或用 scripts/build-from-source.sh 本地构建。"
      ;;
    *)
      die "不支持的操作系统：$os（Windows 请用 scripts\\install.ps1）"
      ;;
  esac
}

PLATFORM_DIR="$(detect_platform_dir)"
BIN_SRC="${PKG_ROOT}/bin/${PLATFORM_DIR}"
[ -d "$BIN_SRC" ] || die "找不到平台目录：${BIN_SRC}（包内可用目录：$(ls "${PKG_ROOT}/bin" 2>/dev/null | tr '\n' ' ')）"

log "SqlGuard 安装"
echo "    包根目录   : ${PKG_ROOT}"
echo "    平台目录   : ${PLATFORM_DIR}"
echo "    二进制目录 : ${BIN_DIR}"
[ "$INSTALL_CONFIG" = "1" ] && echo "    配置目录   : ${CONFIG_DIR}"
[ "$DRY_RUN" = "1" ] && echo "    （dry-run 模式，不会实际写入）"

# ---------- 校验 ----------
if [ "$DO_VERIFY" = "1" ]; then
  log "校验 SHA256SUMS"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$PKG_ROOT" && sha256sum -c SHA256SUMS) || die "校验失败，请重新下载发布包"
  elif command -v shasum >/dev/null 2>&1; then
    # macOS：SHA256SUMS 格式为 "<hash>  <path>"，shasum -c 同样支持
    (cd "$PKG_ROOT" && shasum -c SHA256SUMS) || die "校验失败，请重新下载发布包"
  else
    warn "未找到 sha256sum / shasum，跳过校验"
  fi
fi

# ---------- 安装二进制 ----------
BINS=(sqlguard sqlguard-mine)
[ "$WITH_DIAG" = "1" ] && BINS+=(mapdiag)

run mkdir -p "$BIN_DIR"
for b in "${BINS[@]}"; do
  src="${BIN_SRC}/${b}"
  dst="${BIN_DIR}/${b}"
  if [ ! -f "$src" ]; then
    warn "缺少二进制 ${src}，跳过"
    continue
  fi
  if [ -f "$dst" ] && [ "$FORCE" != "1" ]; then
    echo "    skip  ${dst}（已存在，用 --force 覆盖）"
    continue
  fi
  run cp -f "$src" "$dst"
  run chmod 755 "$dst"
  echo "    ->    ${dst}"
done

# ---------- 安装规则库与配置模板 ----------
if [ "$INSTALL_CONFIG" = "1" ]; then
  run mkdir -p "$CONFIG_DIR"
  # 默认规则包（基础类 rules-core + 定制类 rules-gaussdb），安装为
  # $CONFIG_DIR/rules/<pack>/，消费侧将其父目录配到 [rule_packs].search_paths。
  for pack in rules-core rules-gaussdb; do
    if [ -d "${PKG_ROOT}/config/${pack}" ]; then
      run mkdir -p "${CONFIG_DIR}/rules"
      run cp -R "${PKG_ROOT}/config/${pack}" "${CONFIG_DIR}/rules/${pack}"
      echo "    ->    ${CONFIG_DIR}/rules/${pack}"
    fi
  done
  for f in sqlguard.toml.example sqlguard.rules.toml.example; do
    if [ -f "${PKG_ROOT}/config/${f}" ]; then
      run cp -f "${PKG_ROOT}/config/${f}" "${CONFIG_DIR}/${f}"
      echo "    ->    ${CONFIG_DIR}/${f}"
    fi
  done
fi

# ---------- 收尾提示 ----------
log "完成"
case ":${PATH}:" in
  *":${BIN_DIR}:"*) ;;
  *) echo "提示：${BIN_DIR} 不在 PATH 中，请追加：" && echo "    export PATH=\"${BIN_DIR}:\$PATH\"" ;;
esac
echo "验证：sqlguard --version"
