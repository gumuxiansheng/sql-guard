#!/usr/bin/env bash
# SqlGuard —— 从源码构建并把产物写入发布包的 bin/<platform>/ 目录。
#
# 用途：当发布包缺少你所在平台的二进制时（例如 macOS），用本脚本就地补齐。
#
# 用法：
#   ./scripts/build-from-source.sh [--repo <源码目录>] [--target <triple>] [--zig] [--bins <a,b>]
#
# 选项：
#   --repo <DIR>      sql-guard 源码目录，默认自动探测（本脚本所在发布包的同级 sql-guard/，或当前目录）
#   --target <TRIPLE> Rust 目标三元组，默认本机（host）。常用：
#                     x86_64-unknown-linux-musl / aarch64-unknown-linux-musl
#                     x86_64-apple-darwin / aarch64-apple-darwin
#                     x86_64-pc-windows-msvc / x86_64-pc-windows-gnu
#   --zig             使用 cargo zigbuild（交叉编译 Linux musl / macOS 时必须；需先安装 zig）
#   --bins <LIST>     要构建的二进制，默认 sqlguard,sqlguard-mine,mapdiag
#   --out-dir <DIR>   输出目录，默认 <发布包>/bin/<platform-dir>
#   -h, --help        显示帮助
#
# 平台目录映射：
#   x86_64-unknown-linux-musl   -> linux-x86_64-musl
#   aarch64-unknown-linux-musl  -> linux-aarch64-musl
#   x86_64-unknown-linux-gnu    -> linux-x86_64-gnu
#   aarch64-unknown-linux-gnu   -> linux-aarch64-gnu
#   x86_64-apple-darwin         -> macos-x86_64
#   aarch64-apple-darwin        -> macos-aarch64
#   x86_64-pc-windows-msvc      -> windows-x86_64
#   x86_64-pc-windows-gnu       -> windows-x86_64
#
# 依赖：Rust 1.72+（推荐 1.80+）；交叉编译时需 `rustup target add <triple>`，
#       macOS 目标还需 `cargo install cargo-zigbuild` 与 zig（本脚本 --zig）。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

REPO=""
TARGET=""
USE_ZIG=0
BINS="sqlguard,sqlguard-mine,mapdiag"
OUT_DIR=""

usage() { sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)    REPO="$2"; shift 2 ;;
    --target)  TARGET="$2"; shift 2 ;;
    --zig)     USE_ZIG=1; shift ;;
    --bins)    BINS="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 2 ;;
  esac
done

# ---------- 定位源码目录 ----------
if [ -z "$REPO" ]; then
  for cand in "$PWD" "${PKG_ROOT}/../sql-guard" "${PKG_ROOT}/.."; do
    if [ -f "${cand}/Cargo.toml" ] && [ -d "${cand}/src" ]; then REPO="$cand"; break; fi
  done
fi
[ -n "$REPO" ] || { echo "error: 找不到 sql-guard 源码目录，请用 --repo 指定" >&2; exit 2; }
REPO="$(cd "$REPO" && pwd)"
[ -f "${REPO}/Cargo.toml" ] || { echo "error: ${REPO} 不是 sql-guard 源码目录（缺少 Cargo.toml）" >&2; exit 2; }

# ---------- 目标三元组 ----------
if [ -z "$TARGET" ]; then
  TARGET="$(rustc -vV 2>/dev/null | awk '/^host:/{print $2}')"
  [ -n "$TARGET" ] || { echo "error: 无法探测 host triple，请用 --target 指定" >&2; exit 2; }
fi

case "$TARGET" in
  x86_64-unknown-linux-musl)   PLAT="linux-x86_64-musl" ;;
  aarch64-unknown-linux-musl)  PLAT="linux-aarch64-musl" ;;
  x86_64-unknown-linux-gnu)    PLAT="linux-x86_64-gnu" ;;
  aarch64-unknown-linux-gnu)   PLAT="linux-aarch64-gnu" ;;
  x86_64-apple-darwin)         PLAT="macos-x86_64" ;;
  aarch64-apple-darwin)        PLAT="macos-aarch64" ;;
  x86_64-pc-windows-msvc)      PLAT="windows-x86_64" ;;
  x86_64-pc-windows-gnu)       PLAT="windows-x86_64" ;;
  *) echo "error: 未知目标 $TARGET，请同时用 --out-dir 指定输出目录" >&2; exit 2 ;;
esac

[ -n "$OUT_DIR" ] || OUT_DIR="${PKG_ROOT}/bin/${PLAT}"
case "$TARGET" in *windows*) EXT=".exe" ;; *) EXT="" ;; esac

echo "==> 源码目录 : ${REPO}"
echo "==> 目标     : ${TARGET}"
echo "==> 输出目录 : ${OUT_DIR}"

command -v cargo >/dev/null 2>&1 || { echo "error: 未找到 cargo，请先安装 Rust 工具链" >&2; exit 2; }

# ---------- 构建 ----------
BUILD_ARGS=(--release --locked)
[ -n "$TARGET" ] && BUILD_ARGS+=(--target "$TARGET")

echo "==> 构建（cargo ${USE_ZIG:+zigbuild }build）"
cd "$REPO"
if [ "$USE_ZIG" = "1" ]; then
  cargo zigbuild "${BUILD_ARGS[@]}"
else
  cargo build "${BUILD_ARGS[@]}"
fi

# ---------- 复制产物 ----------
if [ -n "$TARGET" ]; then SRC_DIR="${REPO}/target/${TARGET}/release"; else SRC_DIR="${REPO}/target/release"; fi
mkdir -p "$OUT_DIR"

IFS=',' read -r -a BIN_ARR <<< "$BINS"
for b in "${BIN_ARR[@]}"; do
  src="${SRC_DIR}/${b}${EXT}"
  if [ -f "$src" ]; then
    cp -f "$src" "${OUT_DIR}/${b}${EXT}"
    chmod 755 "${OUT_DIR}/${b}${EXT}"
    echo "    -> ${OUT_DIR}/${b}${EXT}"
  else
    echo "    warn: 未找到 ${src}，跳过"
  fi
done

echo "==> 完成。用 ${OUT_DIR}/sqlguard${EXT} --version 验证。"
echo "注意：新增二进制后，发布包根目录的 SHA256SUMS 需重新生成（scripts/package-release.sh）。"
