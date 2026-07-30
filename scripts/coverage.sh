#!/usr/bin/env bash
# SqlGuard 覆盖率生成脚本（依赖 cargo-llvm-cov，需先 `cargo install cargo-llvm-cov`）。
# 用法：
#   ./scripts/coverage.sh          # 默认 html，输出到 target/llvm-cov/html
#   ./scripts/coverage.sh html     # 同上
#   ./scripts/coverage.sh lcov     # 生成 target/llvm-cov/lcov.info
#   ./scripts/coverage.sh summary  # 仅打印汇总
set -euo pipefail

FORMAT="${1:-html}"

case "$FORMAT" in
  html)
    cargo llvm-cov --html --output-dir target/llvm-cov
    ;;
  lcov)
    cargo llvm-cov --lcov --output-path target/llvm-cov/lcov.info
    ;;
  text|summary)
    cargo llvm-cov --summary-only
    ;;
  *)
    echo "Unknown format: $FORMAT" >&2
    echo "Usage: $0 [html|lcov|summary]" >&2
    exit 1
    ;;
esac
