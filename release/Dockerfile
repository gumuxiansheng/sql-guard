# SqlGuard 容器镜像（基于发布包内已编译的 musl 静态二进制，无需源码与编译器）
#
# 默认 x86_64；ARM64 主机请改用 Dockerfile.aarch64。
#
# 构建（在发布包根目录执行）：
#   docker build -t sqlguard:0.2.8 .
#
# 运行：
#   docker run --rm -v "$PWD:/work" sqlguard:0.2.8 check /work/sql

FROM alpine:3.20

# musl 静态二进制无额外运行时依赖；ca-certificates 仅用于未来可能的 https 扩展
RUN apk add --no-cache ca-certificates

COPY bin/linux-x86_64-musl/sqlguard      /usr/local/bin/sqlguard
COPY bin/linux-x86_64-musl/sqlguard-mine /usr/local/bin/sqlguard-mine

WORKDIR /work
ENTRYPOINT ["sqlguard"]
CMD ["--help"]
