#!/usr/bin/env bash
# SqlGuard 发布包组装脚本
#
# 把已编译的各平台二进制 + 文档 + 脚本 + 规则库 + 示例组装成发布包目录，
# 生成 VERSION / SHA256SUMS，并打包为全平台合包与各平台独立包。
#
# 用法：
#   ./scripts/package-release.sh [--version <X.Y.Z>] [--skip-archives] [--dist <DIR>] [--allow-stale]
#
# 二进制来源（按优先级）：
#   1) target/<triple>/release/<bin><ext>              （cargo build / zigbuild 直出）
#   2) dist/<bin>-<triple><ext>                        （scripts/build-release.sh 的扁平产物，CI 用）
#
# 陈旧保护：若某平台二进制的 mtime 早于 src/ 下最新源码 mtime，默认跳过该平台
#           （避免把上一版本的产物打进新版本包）；确需打包时加 --allow-stale。
#
# 输出（默认 dist/）：
#   dist/sql-guard-<VER>/                                 发布包目录
#   dist/sql-guard-<VER>-all.tar.gz / .zip                全平台合包
#   dist/sql-guard-<VER>-<platform>.tar.gz / .zip         单平台包
#   dist/SHA256SUMS                                       各压缩包的校验和

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# 确保 cargo/rustc 可用（本机交互式 shell 常未把 ~/.cargo/bin 放进 PATH）
if ! command -v rustc >/dev/null 2>&1 && [ -d "${HOME}/.cargo/bin" ]; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

VERSION=""
SKIP_ARCHIVES=0
DIST_DIR="dist"
ALLOW_STALE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version)       VERSION="$2"; shift 2 ;;
    --skip-archives) SKIP_ARCHIVES=1; shift ;;
    --dist)          DIST_DIR="$2"; shift 2 ;;
    --allow-stale)   ALLOW_STALE=1; shift ;;
    -h|--help)       sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 2 ;;
  esac
done

# ---------- 版本 ----------
if [ -z "$VERSION" ]; then
  VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
fi
[ -n "$VERSION" ] || { echo "error: 无法确定版本号" >&2; exit 2; }

PKG_NAME="sql-guard-${VERSION}"
PKG_DIR="${DIST_DIR}/${PKG_NAME}"
STAGE="${DIST_DIR}/.stage"

echo "==> 版本      : ${VERSION}"
echo "==> 发布包目录: ${PKG_DIR}"

rm -rf "$PKG_DIR" "$STAGE"
mkdir -p "$PKG_DIR"

# ---------- 1. 二进制 ----------
# 平台目录 -> 候选 triple 列表（逗号分隔，按序查找）
declare -a PLATFORMS=(
  "windows-x86_64|x86_64-pc-windows-msvc,x86_64-pc-windows-gnu"
  "linux-x86_64-musl|x86_64-unknown-linux-musl"
  "linux-aarch64-musl|aarch64-unknown-linux-musl"
  "macos-x86_64|x86_64-apple-darwin"
  "macos-aarch64|aarch64-apple-darwin"
)
BINS=(sqlguard sqlguard-mine mapdiag)

# 源码最新修改时间：用于识别"上一版本的遗留产物"
SRC_NEWEST="$(find src -name '*.rs' -printf '%T@\n' 2>/dev/null | sort -n | tail -1 || echo 0)"
[ -n "$SRC_NEWEST" ] || SRC_NEWEST=0
# 本机 triple：cargo build（不带 --target）产物落在 target/release/
HOST_TRIPLE="$(rustc -vV 2>/dev/null | awk '/^host:/{print $2}' || echo '')"

found_plat=0
for entry in "${PLATFORMS[@]}"; do
  IFS='|' read -r plat triples <<< "$entry"
  plat_dir=""
  IFS=',' read -r -a triple_arr <<< "$triples"
  for t in "${triple_arr[@]}"; do
    case "$t" in *windows*) ext=".exe" ;; *) ext="" ;; esac
    # 候选来源：target/<triple>/release/ → target/release（本机 triple 直出）→ dist/<bin>-<triple>
    srcdir=""; srcmode=""
    if [ -f "target/${t}/release/sqlguard${ext}" ]; then
      srcdir="target/${t}/release"; srcmode="target"
    elif [ "$t" = "$HOST_TRIPLE" ] && [ -f "target/release/sqlguard${ext}" ]; then
      srcdir="target/release"; srcmode="target-host"
    elif [ -f "${DIST_DIR}/sqlguard-${t}${ext}" ]; then
      srcdir="${DIST_DIR}"; srcmode="dist"
    else
      continue
    fi

    # 陈旧检测
    if [ "$srcmode" = "dist" ]; then probe="${DIST_DIR}/sqlguard-${t}${ext}"; else probe="${srcdir}/sqlguard${ext}"; fi
    bin_mtime="$(stat -c '%Y' "$probe" 2>/dev/null || echo 0)"
    if [ "$ALLOW_STALE" != "1" ] && [ -n "$SRC_NEWEST" ] && [ "$(printf '%.0f' "$SRC_NEWEST")" -gt "${bin_mtime:-0}" ]; then
      echo "    skip bin/${plat}（${probe} 陈旧于源码，可能非 ${VERSION} 产物；--allow-stale 可强制打包）"
      continue 2
    fi

    copied=0
    for b in "${BINS[@]}"; do
      if [ "$srcmode" = "dist" ]; then src="${srcdir}/${b}-${t}${ext}"; else src="${srcdir}/${b}${ext}"; fi
      [ -f "$src" ] || continue
      plat_dir="${PKG_DIR}/bin/${plat}"
      mkdir -p "$plat_dir"
      cp -f "$src" "${plat_dir}/${b}${ext}"
      chmod 755 "${plat_dir}/${b}${ext}"
      copied=$((copied + 1))
    done
    if [ "$copied" -gt 0 ]; then
      echo "    bin/${plat}: ${copied} 个二进制（来源 ${srcmode}: ${t}）"
      found_plat=$((found_plat + 1))
      break
    fi
  done
done
[ "$found_plat" -gt 0 ] || { echo "error: 没有找到任何已编译二进制，请先构建（或加 --allow-stale）" >&2; exit 2; }

# ---------- 2. 文档 ----------
mkdir -p "${PKG_DIR}/docs"
if [ -d release/docs ]; then
  cp -f release/docs/*.md "${PKG_DIR}/docs/" 2>/dev/null || true
fi
for d in default-rules.md rule-scripting.md dialect-fallback.md; do
  [ -f "docs/${d}" ] && cp -f "docs/${d}" "${PKG_DIR}/docs/${d}"
done
[ -f release/README.md ] && sed "s/@VERSION@/${VERSION}/g" release/README.md > "${PKG_DIR}/README.md"
[ -f release/docs/CHANGELOG.md ] && cp -f release/docs/CHANGELOG.md "${PKG_DIR}/CHANGELOG.md"
[ -f release/docs/VERIFY.md ] && sed "s/@VERSION@/${VERSION}/g" release/docs/VERIFY.md > "${PKG_DIR}/VERIFY.md"

# ---------- 3. 脚本 ----------
mkdir -p "${PKG_DIR}/scripts"
if [ -d release/scripts ]; then
  cp -f release/scripts/* "${PKG_DIR}/scripts/" 2>/dev/null || true
fi
[ -f scripts/build-release.sh ] && cp -f scripts/build-release.sh "${PKG_DIR}/scripts/build-release.sh"
chmod +x "${PKG_DIR}/scripts/"*.sh 2>/dev/null || true

# ---------- 4. 规则库与配置模板 ----------
mkdir -p "${PKG_DIR}/config"
[ -d config/rules ] && cp -R config/rules "${PKG_DIR}/config/"
for f in sqlguard.toml.example sqlguard.rules.toml.example; do
  [ -f "$f" ] && cp -f "$f" "${PKG_DIR}/config/${f}"
done

# ---------- 5. 示例 ----------
mkdir -p "${PKG_DIR}/examples"
for d in ddl dml; do
  [ -d "examples/${d}" ] && cp -R "examples/${d}" "${PKG_DIR}/examples/"
done
[ -f examples/README.md ] && cp -f examples/README.md "${PKG_DIR}/examples/README.md"

# ---------- 6. CI 片段 ----------
if [ -d release/ci ]; then
  mkdir -p "${PKG_DIR}/ci"
  cp -f release/ci/* "${PKG_DIR}/ci/" 2>/dev/null || true
fi

# ---------- 7. LICENSE / Docker ----------
if [ -f release/LICENSE ]; then
  cp -f release/LICENSE "${PKG_DIR}/LICENSE"
elif [ -f LICENSE ]; then
  cp -f LICENSE "${PKG_DIR}/LICENSE"
else
  echo "warn: 未找到 LICENSE 文件" >&2
fi
[ -f release/Dockerfile ] && cp -f release/Dockerfile "${PKG_DIR}/Dockerfile"
[ -f release/Dockerfile.aarch64 ] && cp -f release/Dockerfile.aarch64 "${PKG_DIR}/Dockerfile.aarch64"

# ---------- 8. VERSION 元数据 ----------
GIT_COMMIT="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
BUILD_DATE="$(date +%Y-%m-%d)"
RUSTC_VER="$(rustc --version 2>/dev/null | awk '{print $2}' || echo unknown)"
TARGET_LIST="$(ls "${PKG_DIR}/bin" | tr '\n' ',' | sed 's/,$//')"
cat > "${PKG_DIR}/VERSION" <<EOF
name         = sqlguard
version      = ${VERSION}
git_commit   = ${GIT_COMMIT}
build_date   = ${BUILD_DATE}
rustc        = ${RUSTC_VER}
profile      = release (lto=fat, codegen-units=1, strip=true, panic=abort)
binaries     = ${BINS[*]}
platforms    = ${TARGET_LIST}
license      = MIT
EOF
echo "==> VERSION 已生成"

# ---------- 9. SHA256SUMS（包内全部文件） ----------
gen_sums() {
  local dir="$1"
  (cd "$dir" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | sort | xargs sha256sum > SHA256SUMS)
}
gen_sums "$PKG_DIR"
echo "==> SHA256SUMS 已生成（$(wc -l < "${PKG_DIR}/SHA256SUMS") 个文件）"

# ---------- 10. 打包 ----------
if [ "$SKIP_ARCHIVES" = "1" ]; then
  echo "==> 跳过打包（--skip-archives）"
  echo "==> 完成：${PKG_DIR}"
  exit 0
fi

mkdir -p "$STAGE"
rm -f "${DIST_DIR}/${PKG_NAME}"-*.tar.gz "${DIST_DIR}/${PKG_NAME}"-*.zip

make_zip() {
  local src="$1" out="$2"
  if command -v zip >/dev/null 2>&1; then
    (cd "$(dirname "$src")" && zip -q -r -X "$(basename "$out")" "$(basename "$src")" >/dev/null) \
      && { echo "    -> ${out}"; return 0; }
  fi
  local py
  py="$(command -v python || command -v python3 || echo "C:/Users/win11/.workbuddy/binaries/python/versions/3.13.12/python.exe")"
  "$py" - "$src" "$out" <<'PYEOF'
import os, sys, zipfile
src, out = sys.argv[1], sys.argv[2]
if os.path.exists(out):
    os.remove(out)
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
    for root, dirs, files in os.walk(src):
        dirs.sort()
        for f in sorted(files):
            full = os.path.join(root, f)
            arc = os.path.relpath(full, os.path.dirname(src))
            z.write(full, arc.replace(os.sep, '/'))
print('    ->', out)
PYEOF
}

# 全平台合包
tar -czf "${DIST_DIR}/${PKG_NAME}-all.tar.gz" -C "$DIST_DIR" "$PKG_NAME"
echo "    -> ${DIST_DIR}/${PKG_NAME}-all.tar.gz"
make_zip "$PKG_DIR" "${DIST_DIR}/${PKG_NAME}-all.zip"

# 各平台独立包
for plat in $(ls "${PKG_DIR}/bin"); do
  stage_pkg="${STAGE}/${plat}/${PKG_NAME}"
  rm -rf "${STAGE}/${plat}"
  mkdir -p "${STAGE}/${plat}"
  cp -R "$PKG_DIR" "$stage_pkg"
  for other in $(ls "${stage_pkg}/bin"); do
    [ "$other" = "$plat" ] || rm -rf "${stage_pkg}/bin/${other}"
  done
  gen_sums "$stage_pkg"
  case "$plat" in
    windows*) ARCH="${DIST_DIR}/${PKG_NAME}-${plat}.zip"; make_zip "$stage_pkg" "$ARCH" ;;
    *)        ARCH="${DIST_DIR}/${PKG_NAME}-${plat}.tar.gz"; tar -czf "$ARCH" -C "${STAGE}/${plat}" "$PKG_NAME" ;;
  esac
  echo "    -> ${ARCH}"
done

# 压缩包自身校验和
(cd "$DIST_DIR" && sha256sum ${PKG_NAME}-*.tar.gz ${PKG_NAME}-*.zip > SHA256SUMS 2>/dev/null || true)
echo "==> 压缩包校验和：${DIST_DIR}/SHA256SUMS"

rm -rf "$STAGE"
echo "==> 全部完成"
ls -lh "${DIST_DIR}" | sed 's/^/    /'
