# SqlGuard 常见问题（FAQ）

> 使用问题优先查本文件；安装问题见 `docs/INSTALL.md`，命令细节见 `docs/USER-MANUAL.md`。

## 安装与运行

**Q：没有我平台的二进制怎么办？**
A：本包提供 Linux x86_64（musl 静态）、Linux aarch64（musl 静态）、Windows x86_64。
macOS 与其他平台请从 Release 附件下载对应产物，或用 `cargo install sqlguard` /
`scripts/build-from-source.sh --target <triple>` 本地构建。

**Q：musl 静态二进制有什么好处？**
A：不依赖 glibc 版本，CentOS 7 / Alpine / 各种国产发行版都能直接跑，容器里用 `scratch` 基础镜像也可以。

**Q：能不能只装 `sqlguard` 不装 `sqlguard-mine`？**
A：可以。安装脚本加 `--no-config` 只影响配置复制；要精确控制就手动从 `bin/<platform>/` 复制需要的那一个即可。

**Q：`sqlguard --version` 输出的版本和发布页不一致？**
A：说明 PATH 里存在旧版本。用 `which sqlguard`（Windows：`Get-Command sqlguard`）确认实际路径，删除旧文件后重试。

## 检查行为

**Q：为什么我的 SQL 被报 `PARSE_ERROR`？**
A：最常见是方言不匹配。依次尝试：

```bash
sqlguard check ./sql --dialect mysql
sqlguard check ./sql --dialect gaussdb --dialect-fallback oracle
sqlguard check ./sql --dialect generic
```

Mapper 场景下，动态标签（`<if>` / `<foreach>`）剥离后可能留下语法不完整的片段，属已知限制，
可用 `mapdiag <mapper-dir>` 归类确认。

**Q：规则没生效，可能是什么原因？**
A：按序排查：① `sqlguard.rules.toml` 里该规则 `enabled` 是否为 `true`；② `applies_to` 是否覆盖文件的
`script_type`（Mapper 统一映射为 `dml`）；③ 是否被 `--exclude-rules` / `--exclude-groups` 过滤；
④ `script_path` 相对路径是否正确（相对配置文件所在目录）。

**Q：`--rules` 和 `--exclude-rules` 同时给，谁优先？**
A：黑名单优先。命中 `--exclude-*` 的规则一定不执行，即使同时命中白名单。

**Q：怎么只让新增代码守规矩，存量代码先不管？**
A：用 `check-diff --base origin/main`，只检查相对基线改动的语句。

**Q：扫描把 `.git` / `node_modules` 也扫进去了？**
A：默认已有黑名单，但若自定义了 `[scan] exclude_dirs` 会覆盖默认值，请把默认项一并写上：

```toml
[scan]
exclude_dirs = [".git", "target", "node_modules", "build", "dist", "out", ".idea", ".vscode"]
```

**Q：扫得太慢？**
A：① 用 `[scan] paths` 配精准白名单；② 大仓库开缓存 `[cache] enabled = true` 或 `--cache`。

## 编码与文件格式

**Q：中文脚本报 FILE001 编码错误，但我不想改文件编码？**
A：把扫描编码设为文件真实编码即可，此时 FILE001 自动跳过：

```toml
[scan]
encoding = "gbk"
```

**Q：CRLF 换行被报 FILE002？**
A：FILE002 默认 `warning` 不阻断。若团队统一用 CRLF，可
`--exclude-rules FILE002` 或把 `line_ending_severity` 调到可接受级别 / 关闭 `check_line_ending`。

## 回滚

**Q：`gen-rollback` 返回退出码 2 是什么意思？**
A：存在 `irreversible` / `unreliable` / `partial` 的语句，即"无法安全自动回滚"。
加 `--review-report` 生成 HTML，按红/黄/绿三区定位需人工复核的语句。
确认可接受风险时用 `--allow-partial`（允许 partial）或 `--fail-on-warning` 的反向组合来控制门禁。

**Q：`lock_scope = table` 被拒绝？**
A：表级锁在 MySQL 下会被 DDL 的隐式提交提前释放，风险明确。必须显式加 `accept_table_lock_risk = true`（或 CLI `--accept-table-lock-risk`）表示知情。

## 集成

**Q：CI 里 `check-diff` 报"找不到 base"？**
A：需要完整 git 历史：`actions/checkout@v4` 配 `fetch-depth: 0`，并 `git fetch origin <base-ref>`。

**Q：怎么把违规显示在 PR 的代码行上？**
A：用 `-f sarif` 生成 SARIF 报告，再用 `github/codeql-action/upload-sarif` 上传（需 `security-events: write` 权限）。

**Q：能作为提交前钩子吗？**
A：可以，最简单是 pre-commit 里跑增量：

```bash
sqlguard check-diff --base HEAD || exit 1
```

## 其他

**Q：会连接我的数据库吗？**
A：不会。SqlGuard 全程静态分析，不连接数据库、不执行 SQL。`replay-export` 只导出清单，重放由独立的 Java 工程完成。

**Q：规则脚本能提交进我的项目仓库吗？**
A：可以，而且推荐。`config/rules/` 与 `sqlguard.rules.toml` 本就是项目级资产，`sqlguard init` 生成的就是这套。

**Q：怎么确认下载的文件没被篡改？**
A：见 `VERIFY.md`，执行 `sha256sum -c SHA256SUMS`（Windows 用 `scripts/verify.ps1`）。
