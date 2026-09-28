# 完整性校验说明

本文件说明如何确认你拿到的 SqlGuard 发布包**未被篡改、与官方构建一致**。

---

## 1. 发布包提供了什么

| 文件 | 作用 |
|------|------|
| `SHA256SUMS` | 发布包内**每个文件**的 SHA-256 摘要（路径相对包根目录） |
| `VERSION` | 版本 + 构建元数据（git commit、rustc 版本、构建时间、目标平台清单） |
| `scripts/verify.sh` | Linux/macOS：校验哈希 + 逐个二进制 `--version` 冒烟 |
| `scripts/verify.ps1` | Windows：同上 |

---

## 2. 三步校验

### 步骤 1：核对压缩包摘要

下载后先比对**压缩包自身**的 SHA-256 与发布页公布值：

```bash
sha256sum sql-guard-@VERSION@-linux-x86_64-musl.tar.gz
```

```powershell
(Get-FileHash .\sql-guard-@VERSION@-windows-x86_64.zip -Algorithm SHA256).Hash
```

压缩包清单：

| 文件 | 适用 |
|------|------|
| `sql-guard-@VERSION@-all.tar.gz` | 全平台合包（Linux/macOS 解压） |
| `sql-guard-@VERSION@-all.zip` | 全平台合包（Windows 解压） |
| `sql-guard-@VERSION@-linux-x86_64-musl.tar.gz` | 仅 Linux x86_64 |
| `sql-guard-@VERSION@-linux-aarch64-musl.tar.gz` | 仅 Linux aarch64 |
| `sql-guard-@VERSION@-windows-x86_64.zip` | 仅 Windows x86_64 |

### 步骤 2：解压后校验包内文件

```bash
cd sql-guard-@VERSION@
sha256sum -c SHA256SUMS
# 每个文件输出 "<路径>: OK" 即为通过
```

```powershell
cd sql-guard-@VERSION@
.\scripts\verify.ps1
```

### 步骤 3：运行冒烟测试

```bash
./scripts/verify.sh          # 校验 + 对本机平台的二进制执行 --version
# 或手动：
./bin/linux-x86_64-musl/sqlguard --version      # → sqlguard @VERSION@
./bin/linux-x86_64-musl/sqlguard-mine --version
```

---

## 3. 版本自证

二进制自身也会报告版本，可与 `VERSION` / 发布页三方互证：

```bash
sqlguard --version
```

同时 `VERSION` 文件记录了构建来源，可用于回溯：

```
version      = 0.2.6
git_commit   = a6acd576e5b19211f90dba4e0702dad2dfac5516
build_date   = 2026-09-18
rustc        = 1.97.1
targets      = x86_64-pc-windows-msvc, x86_64-unknown-linux-musl, aarch64-unknown-linux-musl
```

若三者不一致，说明你手上的文件并非本次发布产物，请重新下载。

---

## 4. 关于 GPG 签名

当前发布流程**未启用 GPG 签名**（CI 未配置签名密钥）。可信链依赖：

1. **只从官方 Release 页面下载**（CNB / GitHub Releases），不使用第三方镜像或转发链接；
2. 比对发布页公布的 SHA-256；
3. `sqlguard --version` 与 `VERSION` 一致。

### 如何为你的组织加上签名

在 CI 中导入 GPG 私钥后，对摘要文件签名并随包分发：

```bash
gpg --detach-sign --armor SHA256SUMS      # 生成 SHA256SUMS.asc
```

用户校验：

```bash
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum -c SHA256SUMS
```

若你的组织要求更强的供应链保证，可进一步接入 Sigstore cosign 对压缩包签名。

---

## 5. 校验失败的处置

| 现象 | 处置 |
|------|------|
| 压缩包摘要与发布页不一致 | **不要解压**，删除后重新从官方源下载 |
| `sha256sum -c` 出现 `FAILED` | 包在传输中损坏或被替换，重新下载；连续失败请在仓库提 Issue |
| 摘要一致但无法运行 | 多为平台选错（如 ARM 机器用了 x86_64 产物），用 `uname -m` 确认架构 |
| 摘要一致且可运行，但版本号不同 | PATH 中存在旧版本，清理旧二进制 |
