#!/usr/bin/env bash
# SqlGuard 多平台 Release 二进制构建脚本（供 CNB tag_push 流水线调用）。
#
# 在 Linux x86_64 执行机上交叉编译以下目标：
#   - x86_64-unknown-linux-musl     （静态链接，无 glibc 依赖）
#   - aarch64-unknown-linux-musl    （ARM64 Linux）
#   - x86_64-pc-windows-gnu         （Windows，自包含 mingw 导入库 + rust-lld）
#   - x86_64-apple-darwin           （Intel macOS，cargo-zigbuild 交叉编译）
#   - aarch64-apple-darwin          （Apple Silicon macOS，cargo-zigbuild 交叉编译）
#
# 复用项目 .cargo/config.toml 中 rust-lld + link-self-contained 的跨平台链接配置
# 处理 musl / windows-gnu 目标；macOS 目标由 Zig 自带链接器与 SDK 完成链接。
#
# 产物统一输出到 dist/，供 cnbcool/attachments 插件上传到 Release 附件。
set -euo pipefail

# ---- 可配置项 ----
ZIG_VERSION="0.13.0"
ZIGBUILD_VERSION="0.18.4"
BINS=(sqlguard sqlguard-mine)
TARGETS=(
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
  x86_64-pc-windows-gnu
  x86_64-apple-darwin
  aarch64-apple-darwin
)

echo "==> Rust toolchain"
rustc --version
cargo --version

# binutils 的 strip 可处理 ELF / Mach-O / PE，统一剥离调试符号；unzip 用于解压 zig wheel。
# 提前安装，供下方 Zig / cargo-zigbuild 安装使用。
# 安装 Zig（用于 macOS 目标交叉编译；Zig 自带 macOS SDK/链接器，纯 Rust crate 无需外部 SDK）
# 以下两步为 best-effort：若网络受限导致安装失败，仅跳过 macOS 目标，不影响 Linux/Windows 产物。
# 注意：Zig 官方 ziglang.org 在国内/CNB 网络下载极慢（实测约 5KB/s，曾导致任务 10 分钟
#       无任何输出被平台强制 kill），故改用清华 PyPI 镜像（实测约 6.4MB/s，12s 完成）。
echo "==> Ensuring binutils (strip) + binutils-aarch64-linux-gnu + xz-utils + unzip + grep"
# 宿主 binutils strip 是 x86_64 版，无法识别 aarch64(ARM64) ELF，
# 故额外安装 binutils-aarch64-linux-gnu 提供 aarch64-linux-gnu-strip 用于 ARM64 musl 目标。
apt-get update -qq && apt-get install -y -qq binutils binutils-aarch64-linux-gnu xz-utils unzip grep

echo "==> Installing Zig ${ZIG_VERSION} (必需：用作 psm/cc 的交叉 C 编译器 + macOS 目标链接器)"
ZIG_WHEEL="ziglang-${ZIG_VERSION}-py3-none-manylinux_2_12_x86_64.manylinux2010_x86_64.musllinux_1_1_x86_64.whl"
# 清华 PyPI simple 索引中 href 为相对路径（../../packages/...），需解析为绝对 URL
# 注意：CI 为非 tty 环境，curl 默认静默无进度输出，极易被平台 watchdog（10 分钟无输出即 kill）误杀。
#       故下载务必加 --progress-bar（-#），并收紧 --max-time，确保超时后能及时告警继续（macOS 为 best-effort）。
ZIG_WHEEL_URL=$(curl -fsSL --connect-timeout 15 --max-time 60 --retry 2 --retry-delay 2 \
  "https://pypi.tuna.tsinghua.edu.cn/simple/ziglang/" \
  | grep -oE "href=\"[^\"]*${ZIG_WHEEL}[^\"]*\"" | head -1 \
  | sed -E 's/^href="//; s/"$//; s#^\.\./\.\./#https://pypi.tuna.tsinghua.edu.cn/#') || true
echo "    wheel url: ${ZIG_WHEEL_URL:-<未解析到，跳过 macOS 目标>}"
if [ -n "${ZIG_WHEEL_URL}" ] \
   && curl -fL# --connect-timeout 15 --max-time 180 --retry 1 \
        "${ZIG_WHEEL_URL}" -o "/tmp/${ZIG_WHEEL}" \
   && unzip -oq "/tmp/${ZIG_WHEEL}" -d /tmp/ziglang-wheel \
   && ln -sf "/tmp/ziglang-wheel/ziglang/zig" /usr/local/bin/zig \
   && chmod +x /usr/local/bin/zig; then
  zig version
else
  echo "error: Zig 安装失败。Zig 现在既是 macOS 目标的链接器，也是各目标 psm/cc 汇编的 C 编译器，无法跳过。"
  exit 1
fi

echo "==> Creating Zig C-compiler wrapper scripts for psm/cc cross-assembly"
# psm（经 rhai -> stacker 引入）的 build script 用 cc crate 编译目标平台汇编。
# 若把 CC 直接设为 "zig cc -target ..."，cc crate 会走它那段有缺陷的 zig 探测路径：
# 把 cargo 的 4 段目标三元组（x86_64-unknown-linux-musl）原样丢给 zig，而 zig 0.13
# 只认 3 段形式（x86_64-linux-musl），于是报 "unable to parse target query ...:
# UnknownOperatingSystem" 导致构建失败。
# 解决：用「名称不含 zig」的普通脚本封装 zig，让 cc crate 把它当普通 GCC 处理，
# 从而绕过其 zig 探测；脚本内丢弃 cc crate 可能追加的 4 段 -target，并把正确的
# -target 放到参数末尾以覆盖之，确保 zig 始终拿到能接受的 3 段目标。
make_zig_cc() {
  local name="$1" zig_target="$2"
  cat > "/usr/local/bin/${name}" <<'ZIGWRAP'
#!/bin/sh
# 自动生成：将 cargo 4 段目标翻译为 zig 3 段目标（绕过 cc crate 有缺陷的 zig 探测）
zig_args=""
while [ $# -gt 0 ]; do
  case "$1" in
    -target|--target)     shift 2 ;;   # 丢弃 cc crate 追加的 4 段 -target
    -target=*|--target=*) shift ;;
    *) zig_args="$zig_args $1"; shift ;;
  esac
done
exec zig cc $zig_args -target ZIGTARGET
ZIGWRAP
  # heredoc 内不能安全展开变量，故用 sed 把占位符替换为真实 zig 目标
  sed -i "s/ZIGTARGET/${zig_target}/" "/usr/local/bin/${name}"
  chmod +x "/usr/local/bin/${name}"
}
make_zig_cc x86_64-linux-musl-gcc  x86_64-linux-musl
make_zig_cc aarch64-linux-musl-gcc aarch64-linux-musl
make_zig_cc x86_64-windows-gnu-cc  x86_64-windows-gnu

echo "==> Installing cargo-zigbuild ${ZIGBUILD_VERSION} (best-effort for macOS targets)"
# 优先使用 GitHub 预编译二进制（~1MB，实测秒下），避免 cargo install 长时间编译；
# 失败时静默回退到 cargo install。
CZB_URL="https://github.com/rust-cross/cargo-zigbuild/releases/download/v${ZIGBUILD_VERSION}/cargo-zigbuild-v${ZIGBUILD_VERSION}.x86_64-unknown-linux-musl.tar.gz"
# 加 --progress-bar 输出进度避免 watchdog 误杀；收紧 --max-time/retry 避免长时间卡在下载
if curl -fL# --connect-timeout 15 --max-time 120 --retry 1 "${CZB_URL}" -o "/tmp/cargo-zigbuild.tar.gz" \
   && tar -xzf "/tmp/cargo-zigbuild.tar.gz" -C /usr/local/bin \
   && chmod +x /usr/local/bin/cargo-zigbuild; then
  cargo-zigbuild --version
else
  # 兜底：cargo install（编译较慢，但仅在预编译二进制下载失败时执行）
  if ! cargo install "cargo-zigbuild" --version "${ZIGBUILD_VERSION}" --locked 2>/dev/null; then
    echo "warn: cargo-zigbuild 安装失败，macOS 目标将被跳过"
  fi
fi

# 交叉环境下 cargo 自带的 strip 找不到目标平台 strip 程序，故关闭它，
# 改为构建后由 binutils strip 统一处理（见下方循环）。
export CARGO_PROFILE_RELEASE_STRIP=false

echo "==> Adding Rust targets"
rustup target add "${TARGETS[@]}"

mkdir -p dist

for T in "${TARGETS[@]}"; do
  echo "==> Building target: ${T}"

  # psm/cc crate 用的交叉 C 编译器：指向上面的 Zig 封装脚本（名称不含 "zig"，
  # 让 cc crate 当普通 GCC 处理，绕过其有缺陷的 zig 探测；脚本内部已正确翻译目标）。
  # 仅对走 `cargo build` 的 musl / windows-gnu 目标设置；macOS 目标由
  # cargo-zigbuild 自行管理 CC，无需（也不应）在此覆盖。
  case "${T}" in
    x86_64-unknown-linux-musl)
      export CC_x86_64_unknown_linux_musl="x86_64-linux-musl-gcc"
      ;;
    aarch64-unknown-linux-musl)
      export CC_aarch64_unknown_linux_musl="aarch64-linux-musl-gcc"
      ;;
    x86_64-pc-windows-gnu)
      export CC_x86_64_pc_windows_gnu="x86_64-windows-gnu-cc"
      ;;
  esac

  case "${T}" in
    *apple-darwin)
      # macOS 目标必须用 zigbuild（提供 SDK + 链接器）。best-effort：失败则跳过该目标。
      echo "    (best-effort) macOS target via zigbuild"
      MACOSX_DEPLOYMENT_TARGET=10.12 \
        cargo zigbuild --release --target "${T}" --locked \
        || { echo "    warn: macOS target ${T} 构建失败，跳过（best-effort）"; continue; }
      ;;
    *)
      # musl / windows-gnu 走项目自带 rust-lld + link-self-contained 配置
      cargo build --release --target "${T}" --locked
      ;;
  esac

  # 决定二进制后缀
  case "${T}" in
    *windows-gnu) BINEXT=".exe" ;;
    *)            BINEXT="" ;;
  esac

  # 选择目标平台对应的 strip 工具：
  #   宿主 binutils strip(x86_64) 不认 ARM64 ELF，故 aarch64 用 aarch64-linux-gnu-strip；
  #   macOS(Mach-O) 在 Linux 下 GNU strip 无法处理，跳过（best-effort，保持未 strip）。
  case "${T}" in
    aarch64-unknown-linux-musl) STRIP="aarch64-linux-gnu-strip" ;;
    *apple-darwin)             STRIP="" ;;
    *)                         STRIP="strip" ;;
  esac

  for B in "${BINS[@]}"; do
    SRC="target/${T}/release/${B}${BINEXT}"
    DST="dist/${B}-${T}${BINEXT}"
    cp "${SRC}" "${DST}"
    # 个别目标 strip 失败不应中断整体发布（仅告警）；Mach-O 直接跳过
    if [ -z "${STRIP}" ]; then
      echo "    -> ${DST} (strip 跳过：Mach-O 需 macOS 环境)"
    elif ${STRIP} "${DST}"; then
      echo "    -> ${DST} (stripped)"
    else
      echo "    warn: strip failed for ${DST}, kept unstripped"
    fi
  done
done

echo "==> Build artifacts:"
ls -lh dist
