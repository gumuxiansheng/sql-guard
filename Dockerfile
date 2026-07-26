# SqlGuard 容器镜像。
#
# 多阶段构建：构建阶段用 rust:alpine，运行阶段用 alpine（约 10MB）。
# 最终镜像仅含 sqlguard 二进制 + musl 运行时，无编译器、无源码。
#
# 用法：
#   docker build -t sqlguard .
#   docker run --rm -v "$PWD:/work" sqlguard check /work/sql

# ---------- 构建阶段 ----------
FROM rust:1.80-alpine AS builder

# musl 静态链接所需工具
RUN apk add --no-cache musl-dev

WORKDIR /build
COPY . .

# --locked 保证使用 Cargo.lock 锁定的依赖版本（可复现构建）
RUN cargo build --release --locked && \
    strip target/release/sqlguard

# ---------- 运行阶段 ----------
FROM alpine:3.20

# sqlguard 不需要额外运行时依赖（musl 静态链接）。
# 安装 ca-certificates 以支持 https 调用（如未来扩展网络功能）。
RUN apk add --no-cache ca-certificates

COPY --from=builder /build/target/release/sqlguard /usr/local/bin/sqlguard

WORKDIR /work
ENTRYPOINT ["sqlguard"]
CMD ["--help"]
