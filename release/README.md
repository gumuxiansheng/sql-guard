# SqlGuard @VERSION@ 发布包

SqlGuard 是一个**静态 SQL 质量门禁**工具：不连数据库、不执行 SQL，在 SQL 脚本与 MyBatis Mapper
进入仓库或发布流水线之前完成规范检查、备份回滚脚本生成与重放清单导出。

本压缩包是 **@VERSION@** 的官方发布包，包含二进制、安装脚本、完整文档、内置规则库与示例。

---

## 1. 最快上手

```bash
# 解压后进入包目录
cd sql-guard-@VERSION@

# Linux / macOS
sudo ./scripts/install.sh --verify

# Windows（PowerShell）
.\scripts\install.ps1 -AddToPath -Verify

# 验证
sqlguard --version          # 期望输出 sqlguard @VERSION@
```

三步跑通一次真实检查：

```bash
mkdir demo && cd demo
sqlguard init .                       # 生成配置 + 内置规则 + 示例目录
sqlguard check ./sql                  # 检查
sqlguard check ./sql -f html -o r/    # 生成 HTML 报告
```

---

## 2. 目录地图

```
sql-guard-@VERSION@/
├── README.md            本文件：包总览与快速开始
├── VERSION              版本与构建元数据（commit / 构建日期 / rustc / 平台清单）
├── CHANGELOG.md         版本变更记录
├── VERIFY.md            完整性校验说明（SHA-256 核对方法）
├── LICENSE              MIT
├── SHA256SUMS           包内每个文件的 SHA-256
├── bin/                 各平台二进制（见下）
│   └── <platform>/      sqlguard · sqlguard-mine · mapdiag
├── docs/
│   ├── INSTALL.md       安装指南（Linux / macOS / Windows / Docker / 源码）
│   ├── USER-MANUAL.md   用户手册（命令、配置、规则、CI 集成、排障）
│   ├── FAQ.md           常见问题
│   ├── VERIFY.md        校验说明（同上，随 docs 一并保留）
│   ├── CHANGELOG.md     变更记录（同上）
│   ├── default-rules.md     内置规则清单
│   ├── rule-scripting.md    Rhai 规则编写指南与 AST API
│   └── dialect-fallback.md  方言与逐语句回退链
├── scripts/
│   ├── install.sh / install.ps1        安装
│   ├── uninstall.sh / uninstall.ps1    卸载
│   ├── verify.sh / verify.ps1          校验 + 冒烟
│   ├── build-from-source.sh            自行构建缺失平台
│   └── build-release.sh                CI 多平台交叉编译（供流水线调用）
├── config/              内置规则脚本 + 配置模板
│   ├── rules/ddl|dml|lib …
│   ├── sqlguard.toml.example
│   └── sqlguard.rules.toml.example
├── examples/            SQL 样例（clean / violations）
├── ci/                  可直接复制的流水线片段
├── Dockerfile           基于包内 musl 二进制构建镜像（x86_64）
└── Dockerfile.aarch64   同上，aarch64 版
```

---

## 3. 二进制与平台

每个平台目录下有三个二进制：

| 二进制 | 必装 | 用途 |
|--------|------|------|
| `sqlguard` | ✅ | 主程序：`check` / `check-diff` / `init` / `gen-rollback` / `replay-export` / `explain` |
| `sqlguard-mine` | 按需 | 从 MyBatis Mapper JOIN 条件挖掘逻辑外键，产出 `relations.json` |
| `mapdiag` | 可选 | Mapper 解析诊断（排查大量解析失败时才用） |

平台目录命名与对应关系：

| `bin/` 目录 | 适用系统 |
|-------------|---------|
| `linux-x86_64-musl` | Linux x86_64（静态链接，任意发行版 / Alpine 通用） |
| `linux-aarch64-musl` | Linux ARM64（含国产 ARM 服务器） |
| `macos-x86_64` | macOS Intel |
| `macos-aarch64` | macOS Apple Silicon |
| `windows-x86_64` | Windows 10/11 与 Windows Server x86_64（`.exe`） |

> **以 `bin/` 下实际存在的目录为准。** 若你需要的平台目录缺失（例如拿到的是单平台包），
> 有两种补齐方式：
> 1. 从 Release 附件下载对应平台的独立包（命名形如 `sql-guard-@VERSION@-<platform>.tar.gz` / `.zip`）；
> 2. 在本机用 `scripts/build-from-source.sh` 自行构建（需 Rust 1.72+）。

全平台的交叉编译由 **CNB 流水线**在推送 `v*` tag 时完成（`scripts/build-release.sh` → `scripts/package-release.sh`），
产物即本发布包与各平台独立包。

---

## 4. 完整性校验（强烈建议）

```bash
sha256sum -c SHA256SUMS                 # Linux / macOS
.\scripts\verify.ps1                    # Windows（校验 + 二进制冒烟）
```

压缩包自身也提供校验和（发布页 `SHA256SUMS`）。细节见 `VERIFY.md`。

---

## 5. 文档入口

| 我想… | 看哪里 |
|-------|--------|
| 装上并用起来 | `docs/INSTALL.md` |
| 了解全部命令与配置 | `docs/USER-MANUAL.md` |
| 查内置规则清单 | `docs/default-rules.md` |
| 写自定义规则 | `docs/rule-scripting.md` |
| 接入 CI | `docs/USER-MANUAL.md` §11 与 `ci/` 目录 |
| 排查报错 | `docs/FAQ.md` 与用户手册 §13 |

---

## 6. 常见问题速查

| 现象 | 处理 |
|------|------|
| `command not found` | 安装目录不在 PATH，见 `docs/INSTALL.md` §10 |
| 大量 `PARSE_ERROR` | 换方言：`--dialect gaussdb --dialect-fallback oracle` |
| 规则没生效 | 查 `enabled` / `applies_to` / 是否被 `--exclude-*` 过滤 |
| 想只查本次改动 | `sqlguard check-diff --base origin/main` |
| 版本对不上 | PATH 里有旧版本，清理后重装 |

---

## 7. 许可证

MIT，见 `LICENSE`。
