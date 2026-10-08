# 变更记录（CHANGELOG）

本项目遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)，`0.x` 阶段配置项可能随版本演进。

---

## 0.2.8（2026-10）

- 版本号升级至 0.2.8，同步更新 `Cargo.toml`、`Cargo.lock` 与发布文档中的版本标识。

提交：`chore(version): 更新 sqlguard 版本至 0.2.8`

---

## 0.2.7（2026-09）

- 版本号升级至 0.2.7，同步更新 `Cargo.toml`、`Cargo.lock` 与发布文档中的版本标识。

提交：`chore(version): 更新 sqlguard 版本至 0.2.7`

---

## 0.2.6（2026-09）

发布包首次提供**完整分发形态**：各平台二进制 + 安装/卸载/校验/构建脚本 + 安装指南与用户手册 + 内置规则库 + 示例 + SHA-256 校验清单。
多平台交叉编译统一交给 CNB 流水线（`scripts/build-release.sh` → `scripts/package-release.sh`），推送 `v*` tag 即产出全平台发布包与各平台独立包。

功能与修复：

- `init` 改为**幂等**：默认只补缺失文件，已存在的 `sqlguard.toml` / `sqlguard.rules.toml` / 规则脚本原样保留并提示 `skipped`；需整体重写时加 `--force`。避免覆盖工具链按模板渲染过的定制配置。
- DML 嵌套 `CASE` 检测改为基于 AST + 语句文本分析，降低误报。
- 版本号升级至 0.2.6。

提交：`a6acd57` bump(version)、`2a742a1` feat(dml)、`f7dfa8e` feat(init)

---

## 0.2.5

- 规则引擎**资源限制与入参校验加固**（安全修复）：约束脚本执行资源与输入，避免异常输入导致引擎失控。
- 版本号升级至 0.2.5。

提交：`2126917` bump(version)、`ae2dc68` fix(security)

---

## 0.2.4

- 新增 **DML111**：WHERE 子句禁用 OR 连接条件。
- `no_unused_join` 改进：按限定符（qualifier）精确匹配，准确识别未被使用的 JOIN，减少误报。
- 版本号升级至 0.2.4。

提交：`7f01026` chore、`c8273cf` feat(dml)、`e60fcff` feat(rules)

---

## 0.2.3

- 新增 JOIN 相关规则：显式 JOIN 类型要求、最大 JOIN 表数限制。
- 版本号升级至 0.2.3。

提交：`4cfe38a` bump(version)、`51cee58` feat(rules)

---

## 0.2.2

- 版本号调整与代码格式化（`cargo fmt`）。
- `check-diff` 支持未提交代码的增量检查。

提交：`1a89fa1` build、`40270be` style、`a504740` feat(check-diff)

---

## 0.2.1 及更早

- SARIF 报告、备份回滚生成（`gen-rollback`）、重放清单导出（`replay-export`）、
  MyBatis Mapper 模式、文件级缓存、方言逐语句回退链等核心能力在此阶段陆续落地。

> 完整提交历史见仓库 `git log`；各版本能力的设计文档见 `docs/` 目录。
