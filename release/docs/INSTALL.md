# SqlGuard 安装指南

本文档面向**拿到发布包的用户**，覆盖 Linux / macOS / Windows 三大系统的安装、校验、升级与卸载。

> 版本：以包内 `VERSION` 文件为准（本文档随 **0.2.7** 发布包分发）。
> 若你只需要"能跑起来"的最短路径，直接跳到 [§2 三步安装](#2-三步安装最短路径)。

---

## 目录

1. [前置要求与平台对照表](#1-前置要求与平台对照表)
2. [三步安装（最短路径）](#2-三步安装最短路径)
3. [方式 A：使用安装脚本（推荐）](#3-方式-a使用安装脚本推荐)
4. [方式 B：手动安装](#4-方式-b手动安装)
5. [方式 C：Docker](#5-方式-cdocker)
6. [方式 D：从源码构建](#6-方式-d从源码构建)
7. [完整性校验](#7-完整性校验)
8. [安装后验证](#8-安装后验证)
9. [升级与卸载](#9-升级与卸载)
10. [常见问题](#10-常见问题)

---

## 1. 前置要求与平台对照表

### 1.1 运行时依赖

| 项目 | 要求 |
|------|------|
| 操作系统 | Linux（glibc 2.17+ 或任意 musl）、macOS 11+、Windows 10+ |
| CPU 架构 | x86_64（amd64）或 aarch64（arm64） |
| 外部依赖 | **无**。Linux 产物为 musl 静态链接，Windows 产物为原生 PE，均不依赖运行时 |
| 可选依赖 | 使用 `check-diff` / `replay-export --base` / `sqlguard-mine --base` 时需要 `git` 可执行文件 |
| 磁盘 | 约 40 MB（含全部平台二进制 + 文档 + 规则库） |

### 1.2 发布包平台目录对照

| `bin/` 子目录 | 适用系统 | 二进制后缀 |
|---------------|----------|-----------|
| `linux-x86_64-musl` | Linux x86_64（**静态链接，任意发行版通用**） | 无 |
| `linux-aarch64-musl` | Linux aarch64 / ARM64（静态链接，含国产 ARM 服务器、飞腾、鲲鹏） | 无 |
| `macos-x86_64` | macOS Intel（由 CI 流水线产出） | 无 |
| `macos-aarch64` | macOS Apple Silicon（由 CI 流水线产出） | 无 |
| `windows-x86_64` | Windows 10/11 x86_64、Windows Server 2016+ | `.exe` |

> **以 `bin/` 下实际存在的目录为准**：全平台交叉编译由 CNB 流水线（推送 `v*` tag 时）执行
> `scripts/build-release.sh` → `scripts/package-release.sh` 产出。若你拿到的是**单平台包**，
> `bin/` 下就只有对应平台目录；需要其他平台时，从 Release 附件下载对应的独立包，
> 或用包内 `scripts/build-from-source.sh` 在本机自行构建。

### 1.3 三个二进制分别是什么

| 二进制 | 是否必装 | 用途 |
|--------|---------|------|
| `sqlguard` | **必装** | 主程序：SQL/Mapper 检查、增量检查、备份回滚生成、重放清单导出、AST 解释 |
| `sqlguard-mine` | 按需 | 独立工具：从 MyBatis Mapper 的 JOIN 条件挖掘「逻辑外键」，产出 `relations.json` |
| `mapdiag` | 可选 | 诊断工具：批量 dump Mapper 提取出的 SQL 及其解析结果，按失败原因归类（排查解析问题时才需要） |

安装脚本默认只装前两个，`mapdiag` 需显式加 `--with-diag` / `-IncludeDiagTools`。

---

## 2. 三步安装（最短路径）

```bash
# ① 解压
tar -xzf sql-guard-0.2.7-linux-x86_64-musl.tar.gz && cd sql-guard-0.2.7

# ② 安装（Linux/macOS）
sudo ./scripts/install.sh

# ③ 验证
sqlguard --version
```

Windows（PowerShell，管理员非必需）：

```powershell
Expand-Archive sql-guard-0.2.7-windows-x86_64.zip -DestinationPath .
cd sql-guard-0.2.7
.\scripts\install.ps1 -AddToPath
sqlguard --version
```

---

## 3. 方式 A：使用安装脚本（推荐）

### 3.1 Linux / macOS —— `scripts/install.sh`

```bash
./scripts/install.sh [选项]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--prefix <DIR>` | `/usr/local` | 安装前缀；二进制落到 `$PREFIX/bin` |
| `--bin-dir <DIR>` | `$PREFIX/bin` | 直接指定二进制目录（优先级高于 `--prefix`） |
| `--config-dir <DIR>` | `$PREFIX/share/sqlguard` | 内置规则脚本与配置模板的落盘位置 |
| `--user` | 关闭 | 等价于 `--prefix $HOME/.local`，无需 root |
| `--no-config` | 关闭 | 只装二进制，不复制规则库与配置模板 |
| `--with-diag` | 关闭 | 额外安装 `mapdiag` 诊断工具 |
| `--verify` | 关闭 | 安装前先按 `SHA256SUMS` 校验每个文件 |
| `--force` | 关闭 | 覆盖已存在的同名文件 |
| `--dry-run` | 关闭 | 只打印将要执行的动作，不落盘 |
| `-h, --help` | - | 查看帮助 |

典型用法：

```bash
# 系统级安装（需要 sudo）
sudo ./scripts/install.sh --verify

# 当前用户安装，无需 root（安装到 ~/.local/bin）
./scripts/install.sh --user

# CI 镜像里只装二进制
./scripts/install.sh --bin-dir /opt/sqlguard/bin --no-config
```

脚本行为要点：

- 自动识别 `uname -s` / `uname -m` 选择 `bin/` 下对应平台目录；识别失败时可用环境变量 `SQLGUARD_PLATFORM_DIR` 强制指定（值为 `bin/` 下的子目录名）。
- 复制后自动 `chmod 755`；已存在同名文件默认跳过并提示（除非 `--force`）。
- 若目标目录不在 `PATH` 中，结尾打印一行 `export PATH=...` 提示，需用户自行追加到 shell 配置。
- 规则库默认复制到 `/usr/local/share/sqlguard/rules`，`sqlguard init` 会优先从这里取内置规则（见用户手册 §5.4）。

### 3.2 Windows —— `scripts/install.ps1`

```powershell
.\scripts\install.ps1 [参数]
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `-InstallDir <DIR>` | `%LOCALAPPDATA%\SqlGuard` | 安装根目录；二进制落到 `<DIR>\bin` |
| `-ConfigDir <DIR>` | `<InstallDir>\share` | 规则库与配置模板落盘位置 |
| `-NoConfig` | 关闭 | 只装二进制 |
| `-IncludeDiagTools` | 关闭 | 额外安装 `mapdiag.exe` |
| `-AddToPath` | 关闭 | 把 `<InstallDir>\bin` 写入**用户级** PATH（需重开终端生效） |
| `-Verify` | 关闭 | 安装前按 `SHA256SUMS` 校验 |
| `-Force` | 关闭 | 覆盖已存在文件 |
| `-WhatIfMode` | 关闭 | 只打印动作 |

典型用法：

```powershell
# 用户级安装 + 自动写 PATH + 校验
.\scripts\install.ps1 -AddToPath -Verify

# 装到 D:\tools\SqlGuard（例如 CI 镜像）
.\scripts\install.ps1 -InstallDir D:\tools\SqlGuard -NoConfig
```

> `-AddToPath` 修改的是**当前用户**的 PATH（`[Environment]::SetEnvironmentVariable(..., 'User')`），
> 不需要管理员权限；安装到 `C:\Program Files` 这类系统目录才需要管理员。

---

## 4. 方式 B：手动安装

适用于 macOS（本包无二进制）、或不想用安装脚本的场景。

### 4.1 Linux / macOS

```bash
# 1) 选对应平台的二进制
BIN=bin/linux-x86_64-musl            # 或 bin/linux-aarch64-musl

# 2) 复制并赋可执行权限
sudo install -m 0755 "$BIN/sqlguard"       /usr/local/bin/sqlguard
sudo install -m 0755 "$BIN/sqlguard-mine"  /usr/local/bin/sqlguard-mine

# 3)（可选）规则库与配置模板
sudo mkdir -p /usr/local/share/sqlguard
sudo cp -r config/rules /usr/local/share/sqlguard/
sudo cp config/sqlguard.toml.example config/sqlguard.rules.toml.example /usr/local/share/sqlguard/
```

> macOS 请在 Release 附件下载 `sqlguard-{x86_64,aarch64}-apple-darwin`，下载后需执行一次
> `xattr -d com.apple.quarantine ./sqlguard` 以绕过 Gatekeeper 拦截。

### 4.2 Windows

```powershell
$src = "bin\windows-x86_64"
$dst = "$env:LOCALAPPDATA\SqlGuard\bin"
New-Item -ItemType Directory -Force -Path $dst | Out-Null
Copy-Item "$src\sqlguard.exe"       $dst
Copy-Item "$src\sqlguard-mine.exe"  $dst

# 加入用户 PATH（重开终端生效）
[Environment]::SetEnvironmentVariable(
  'Path',
  [Environment]::GetEnvironmentVariable('Path','User') + ";$dst",
  'User')
```

---

## 5. 方式 C：Docker

发布包内含两个 Dockerfile，直接把**包内已编译的 musl 静态二进制**打进 alpine 镜像（无需源码、无需 Rust 工具链）：

```bash
# x86_64
docker build -t sqlguard:0.2.7 .
docker run --rm -v "$PWD:/work" sqlguard:0.2.7 check /work/sql

# aarch64（ARM64 主机）
docker build -f Dockerfile.aarch64 -t sqlguard:0.2.7-aarch64 .
```

> 官方镜像 `ghcr.io/sqlguard/sqlguard:latest` 由 CI 发布；若你的环境无法拉取外网镜像，用上面的本地构建即可。

---

## 6. 方式 D：从源码构建

任意平台（含 macOS）只要有 Rust 工具链即可：

```bash
# 一键从 crates.io 安装（最简单）
cargo install sqlguard

# 或从源码
git clone https://cnb.cool/mikezhu/sql-guard.git
cd sql-guard
cargo build --release
# 产物：target/release/sqlguard、target/release/sqlguard-mine
```

发布包内也提供了 `scripts/build-from-source.sh`，它会构建并把产物直接落到本包的 `bin/<platform>/` 下，
用于"补齐本包缺失的平台"（例如 macOS）：

```bash
./scripts/build-from-source.sh --target aarch64-apple-darwin
```

| 要求 | 版本 |
|------|------|
| Rust | 1.72+（推荐 1.80+；本包由 rustc 1.97.1 构建，见 `VERSION`） |

---

## 7. 完整性校验

发布包根目录提供 `SHA256SUMS`，记录**包内每个文件**的 SHA-256。

### 7.1 校验包内文件

```bash
# Linux / macOS（在解压后的包根目录执行）
sha256sum -c SHA256SUMS
```

```powershell
# Windows（PowerShell 5.1+）
.\scripts\verify.ps1
# 或单文件核对：
(Get-FileHash .\bin\windows-x86_64\sqlguard.exe -Algorithm SHA256).Hash
```

`scripts/verify.sh` / `scripts/verify.ps1` 除了校验哈希，还会逐个执行 `sqlguard --version` 冒烟测试。

### 7.2 校验压缩包本身

压缩包清单（由 `scripts/package-release.sh` 生成，与包内 `SHA256SUMS` 一致）：

| 文件 | 适用 |
|------|------|
| `sql-guard-0.2.7-all.tar.gz` | 全平台合包（Linux/macOS 解压） |
| `sql-guard-0.2.7-all.zip` | 全平台合包（Windows 解压） |
| `sql-guard-0.2.7-linux-x86_64-musl.tar.gz` | 仅 Linux x86_64 |
| `sql-guard-0.2.7-linux-aarch64-musl.tar.gz` | 仅 Linux aarch64 |
| `sql-guard-0.2.7-windows-x86_64.zip` | 仅 Windows x86_64 |

下载后请核对压缩包自身的 SHA-256 与发布页公布值一致：

```bash
sha256sum sql-guard-0.2.7-linux-x86_64-musl.tar.gz
```

### 7.3 关于签名

当前发布流程**未提供 GPG 签名**（CI 未配置签名密钥）。校验的可信锚点是：

1. 从项目官方 Release 页面（CNB / GitHub Releases）下载，不要从第三方镜像下载；
2. 比对发布页公布的 SHA-256；
3. 通过 `sqlguard --version` 确认版本号与发布页一致。

> 若你需要签名发布，可在 CI 中导入 GPG 私钥后对 `SHA256SUMS` 执行
> `gpg --detach-sign --armor SHA256SUMS`，生成 `SHA256SUMS.asc` 随包分发。

---

## 8. 安装后验证

```bash
sqlguard --version            # 期望输出：sqlguard 0.2.7
sqlguard --help               # 列出 7 个子命令
sqlguard-mine --version       # 期望输出：sqlguard-mine 0.2.7

# 端到端冒烟：初始化 + 检查（用包内自带示例）
mkdir -p /tmp/sg-smoke && cd /tmp/sg-smoke
sqlguard init . >/dev/null
sqlguard check ./sql          # 无违规时输出 "No violations found"，退出码 0
```

若提示 `command not found`，说明安装目录不在 `PATH` 中，参见 [§10 常见问题](#10-常见问题)。

---

## 9. 升级与卸载

### 9.1 升级

覆盖安装即可，配置与规则文件不受影响：

```bash
sudo ./scripts/install.sh --force           # Linux/macOS
.\scripts\install.ps1 -Force                # Windows
sqlguard --version                          # 确认新版本
```

若用 `cargo install` 安装：`cargo install sqlguard --force`。

> 跨版本升级注意：主版本 `0.x` 阶段，配置字段可能随版本演进。升级后请先跑一次
> `sqlguard check` 确认配置未被拒绝（配置错误会返回**退出码 2**）。

### 9.2 卸载

```bash
sudo ./scripts/uninstall.sh                 # 删除 /usr/local/bin 下的二进制
sudo ./scripts/uninstall.sh --purge         # 同时删除 /usr/local/share/sqlguard
```

```powershell
.\scripts\uninstall.ps1                     # 删除 %LOCALAPPDATA%\SqlGuard
.\scripts\uninstall.ps1 -Purge -RemovePath # 同时清理 PATH 条目
```

手动安装的场景，直接删除对应文件即可（见用户手册 §12）。

---

## 10. 常见问题

| 现象 | 原因与处理 |
|------|-----------|
| `sqlguard: command not found` | 安装目录不在 `PATH`。Linux 执行 `export PATH="/usr/local/bin:$PATH"` 并写入 `~/.bashrc`；Windows 重开终端或用 `scripts/install.ps1 -AddToPath` 重装 |
| `Permission denied` | 未加可执行权限：`chmod +x /usr/local/bin/sqlguard` |
| macOS 提示"无法打开，因为来自身份不明的开发者" | `xattr -d com.apple.quarantine /usr/local/bin/sqlguard` |
| Windows 报"已阻止此应用" | 右键文件 → 属性 → 勾选"解除锁定"，或用 `Unblock-File` |
| `error: invalid value 'xxx' for '--dialect'` | 方言名拼写错误，合法值：`generic` / `mysql` / `postgresql` / `gaussdb` / `oracle` / `ansi` |
| 执行 `check-diff` 报 git 相关错误 | 需要安装 `git` 且在有 `.git` 的仓库内执行；CI 需 `fetch-depth: 0` |
| 二进制体积异常大 / 无法在本机运行 | 下载了错误平台的产物，用 `uname -m` 确认架构后重新下载 |
| 想确认文件是否被篡改 | 见 [§7 完整性校验](#7-完整性校验) |

更多使用层面的问题见 `docs/USER-MANUAL.md` 与 `docs/FAQ.md`。
