# SqlGuard 可用性评审 —— 以「挑剔的团队 DBA」视角

> 评审对象：SqlGuard v0.1.0（Rust 编写的 SQL 静态检查 + 动态重放 + 备份回滚设计）  
> 评审视角：团队 DBA（生产环境守门人，保守、重视正确性、厌恶惊喜、需要可解释与可兜底）  
> 评审日期：2026-07-26  
> 方式：通读 README / 代码 / 文档 / 示例 / 竞品调研

---

## 0. 一句话结论

**工程底子不错，但「宣传的能力」和「实际能用的能力」之间存在明显落差。** 作为静态 SQL lint 工具，它在「单文件二进制 + AST 精确规则 + MyBatis Mapper 支持 + PR 增量校验」这几点是真有差异化价值的；但最被大肆宣传的「备份回滚自动生成」在当前 CLI 里**根本没有被调用（纯死代码）**，而「动态分析」其实是一个**只支持 GaussDB/openGauss 的分离 Java 工程**，对 MySQL/PG 用户并不开箱即用。再加上文档与代码严重脱节（规则数量、孤儿规则）、缺少 DBA 常用的「行内豁免 / SARIF / 方言选择」等 ergonomics，一个挑剔的 DBA 会先给一个「观望，不急着上生产」的结论。

---

## 1. 它到底是什么（基于代码的事实）

| 模块                               | 实际状态              | 证据                                                                                                                                                          |
| -------------------------------- | ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `check` 静态检查（SQL 脚本）             | ✅ 可用              | `src/main.rs::run_check` 完整实现                                                                                                                               |
| `check` 静态检查（MyBatis Mapper XML） | ✅ 可用              | `src/mapper/*`，`<include>`/`#{}`/`${}` 处理                                                                                                                   |
| `check-diff` PR 增量校验             | ✅ 可用              | `src/git_diff.rs` + `run_check_diff`                                                                                                                        |
| 三种报告（plain/json/html）            | ✅ 可用              | `src/reporter/*`                                                                                                                                            |
| `replay-export` 导出清单             | ✅ 可用              | `src/replay_export.rs`，但**只导出**，不重放                                                                                                                         |
| 备份/回滚自动生成                        | ❌ **死代码，CLI 不触发** | `rollback::` 仅在各文件 `#[cfg(test)]` 内被引用；`main.rs` 4 个子命令（Check/ReplayExport/Init/CheckDiff）**没有任何一个调用 `RollbackGenerator::generate()`**；无 `gen-rollback` 子命令 |
| 动态重放（EXPLAIN 分析慢 SQL）            | ⚠️ **分离工程，非开箱即用** | `replay/`（Java/Maven），`pom.xml` 注明 JDBC 驱动运行期动态加载、仅面向 GaussDB/openGauss；未在 `deploy/` 提供产物                                                                   |

> 结论先行：README 把「备份回滚生成」和「动态分析」写成了核心卖点，但前者在当前二进制里**完全不可达**，后者对主流 MySQL/PG 用户**需要先自建 Java 工程 + 自备 GaussDB JDBC**。这是评审中最刺眼的一条。

---

## 2. 可用性分析（按 DBA 关心的主题组织）

### 2.1 安装与分发 —— 及格，但有坑

**优点**

- 单文件静态二进制（musl，约 5MB），无运行时依赖，丢进 CI 容器极方便，这点比 SQLFluff（要 Python 环境）强。

**问题**

- `deploy/` 里直接提交二进制（linux-musl / darwin / aarch64 各约 5MB），`examples/sample_project/` 里还**又提交了一份 `sqlguard-x86_64-apple-darwin`**（约 5MB）。把二进制提交进 git 仓库是典型的反模式，会让仓库无谓膨胀、且二进制永远滞后于源码。
- **没有提供 Windows 二进制**（只有 macOS + Linux musl）。部分团队开发者用 Windows，GitHub Actions 也有 windows runner；无法覆盖。
- **没有官方分发渠道**：没有 `brew install`、没有 `cargo install`、没有 Docker 镜像（仓库里只有裸二进制）。团队规模化落地时，「从 deploy/ 目录手动 cp」的体验偏原始。

### 2.2 文档与代码严重脱节 —— 信任杀手

挑剔的 DBA 第一件事就是照文档跑一遍，脱节会直接摧毁信任。

| 文档声称                                                        | 代码实际                                                                    | 证据                                                                  |
| ----------------------------------------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------------------------- |
| README：「生成 **16 条**内置规则（**2 DDL + 14 DML**）」                | 实际 **20 条（6 DDL + 14 DML）**                                             | `config/rules/ddl/` 实际有 6 个文件；`main.rs::run_init` 注册 6 DDL + 14 DML |
| `docs/default-rules.md`：「内置 **16 条**默认规则」                   | 同上，且新增的 DDL003~DDL006 **在 default-rules.md 里完全没有文档**                    | 该 md 只写到 DML108 与 2 条 DDL                                           |
| `config/rules/dml/order_by_required_for_pagination.rhai` 存在 | **孤儿规则**：未被任何 `[[rules]]`、未被 `run_init`、未被 `generate_default_config` 引用 | 全仓 grep 仅命中自身文件                                                     |

> 作为 DBA，我会想：连规则数量都对不上，那「零误报的 P0 红线」到底以哪份为准？

### 2.3 解析失败 = 静默丢失全部检查（最危险的正确性隐患）

`docs/rule-scripting.md` 明确要求每条规则先 `guard_parse_error(context)`，一旦解析失败就 `return`，只上报一条 PARSE_ERROR。问题在于：**AST 为空时，所有其它 AST 规则自动跳过**。

- 如果团队 SQL 用了 sqlparser 不支持的方言语法（MySQL 的 `INSERT IGNORE`、`ON DUPLICATE KEY UPDATE`、Oracle 包体、PG 某些扩展……），**整个文件除了一个 parse error 之外会「零违规」通过**——给人「检查过了，没问题」的错觉。
- **`check` 流程根本没有「目标方言」配置项**（`Config` 结构里只有 `RollbackConfig.dialect`，而 `check` 用的 `Config` 没有 dialect 字段）。意味着 MySQL/PG/Oracle 语法都按同一个默认解析器走，方言适配完全缺失。
- 对 DBA 而言，这是「假阴性」风险，比「误报」更可怕。

### 2.4 缺少 DBA 日常依赖的 ergonomics

| 能力                                               | 是否有        | 影响                                                                                                                             |
| ------------------------------------------------ | ---------- | ------------------------------------------------------------------------------------------------------------------------------ |
| **行内豁免**（类似 `--noqa` / `/* sqlguard-disable */`） | ❌ 无        | 迁移脚本里偶尔「故意写 `SELECT *`」「故意不带 WHERE 的全表 UPDATE」无法逐行豁免，只能全局 `--exclude-rules` 关掉整条规则。Legacy 存量 SQL 一开 P0 就满屏红，且无粒度豁免 → 逼团队关规则或放弃 |
| **SARIF 输出**                                     | ❌ 无        | 无法接入 GitHub Advanced Security / Azure DevOps / GitLab code scanning 的代码扫描面板（现代 CI 的硬需求）                                        |
| **方言选择（针对 check）**                               | ❌ 无（见 2.3） |                                                                                                                                |
| **缓存 / 增量扫描（非 git）**                             | ❌ 无        | 每次 `check` 重解析全部文件、重建引擎；大仓库（数百 SQL）会慢。`check-diff` 即便只查改动，也从头解析                                                                |
| **Windows 支持**                                   | ❌ 无        | 见 2.1                                                                                                                          |

### 2.5 MyBatis Mapper 模式 —— 差异化亮点，但有边界

**这是 SqlGuard 相对 SQLFluff 等最实在的差异化**（SQLFluff 不解析 MyBatis XML）。处理 `#{}`→`?`、`${}` 标识化、`<include>` 同文件展开、行号回映射，做得相当细。

**但边界明显（挑剔 DBA 会踩）：**

- `mapper::map_statement_type` 把 `<select>/<insert>/<update>/<delete>` **全部硬编码成 `"dml"`**（`docs/architecture-review.md` 3.4 也点名）。意味着你无法把「查询」和「变更」用不同规则集治理（比如只允许 DML 红线，但 SELECT 走宽松），`applies_to` 的灵活性被削弱。
- `<include>` **仅支持同文件内引用**，跨 namespace 不行。
- 动态 SQL 标签剥离后，sqlparser 可能因条件分支语法不完整而解析失败 → 又回到 2.3 的静默丢失。
- `CREATE PROCEDURE` / `CREATE FUNCTION` 的 `BEGIN...END` 体，sqlparser 0.45 解析有限，**只能识别「存在」而无法检查过程体内容**（官方已知限制）。存储过程密集的团队基本用不上。

### 2.6 增量校验 `check-diff` —— 用心，但依赖脆弱

- 思路很好：用 `git diff --unified=0` 取 hunk 行号，按 `[line, end_line] ∩ hunk` 过滤，PR 只报本次改动。比「存量全量阻断」务实得多。
- 但强依赖 **`git` 二进制 + 完整历史**（`fetch-depth: 0`，见 README CI 示例）。部分 CI / 浅克隆 / monorepo 子仓会踩坑。
- 用 `base...HEAD`（三点 diff），fork PR 与 branch PR 语义不同，可能产生「漏报」或「多报」，需要 DBA 理解后才能放心。

### 2.7 备份/回滚设计（文案层面）—— 技术含量很高，但「落不了地」

`docs/backup-rollback-design.md`（145KB）的设计**非常专业**：双方言、CREATE TABLE LIKE + 原子 RENAME 切换、长事务预检查、binlog 控制、锁合并、schema 漂移校验、幂等、安全分类（reliable/partial/irreversible）。这比 goInception/Yearning 的「binlog 回放」思路更高级（不依赖 binlog、无需连库）。

**但是**（`docs/architecture-review.md` 之外的更大问题）：

- 如前 1 节所述，**这套 125 个单元测试覆盖的代码，在 CLI 里没有任何入口调用**。DBA 即便把 `[rollback].enabled = true` 打开，`check` 也不会生成 `backup.sql`/`rollback.sql`。等于「设计写了一本书，按钮没接电线」。
- 即便接上，设计本身也有大量「不可逆 / partial」边界（AUTO_INCREMENT/SEQUENCE 无法静态还原、REPLACE 新增行无法回滚、分区表 abort 等）。一个保守 DBA **绝不会**把自动生成的回滚脚本直接用于生产，必须经过人工 review——而「人工 review 自动生成脚本」的体验当前并没有配套（没有对比视图、没有 dry-run 解释）。

### 2.8 其它工程质量（摘自内部 `architecture-review.md`，DBA 视角转述）

- 核心 `engine.rs` 曾达 3000+ 行单文件（现已拆分到 `engine/` 子模块，但内部评审指出历史债）——维护性风险。
- `engine` 核心**缺乏单元测试**，只靠 `tests/integration_test.rs`（1971 行，走二进制集成测试）。规则引擎的正确性靠集成测试兜底，定位难、慢。
- 规则用 **Rhai（小众 DSL）** 编写。`docs/rule-scripting.md` 质量尚可，但 DBA 想加一条规则得先学 Rhai，且有「`violations.push` 必须在顶层、函数内 push 不回写」这类反直觉坑。对比 SQLFluff 的 `.sqlfluff` 配置 + Jinja，学习曲线更陡。
- 依赖版本未锁死（`rhai="1"`、`sqlparser="0.45"` 仅 major 锁），CI 应 `--frozen` 构建以保障可重现。

---

## 3. 竞品分析

### 3.1 格局总览

SqlGuard 想同时吃两块蛋糕：**(A) 静态 SQL lint（CI 红线）** 和 **(B) 上线审核 + 执行 + 回滚（DBA 工作流）**。这两块在国内外是完全不同的产品形态：

- 国外「(A) 类」成熟（SQLFluff、sqlcheck、SonarQube）；
- 国内「(B) 类」极其发达（Yearning、goInception、Archery、SOAR），因为国内强管控 + 审批流是刚需；
- SqlGuard 的独特定位是「**(A) 的可编程精确 lint** + 自研的「**(B) 的静态回滚生成**」，但两头都还没完全站稳。

### 3.2 对比表

| 工具              | 语言/形态                | 目标库                                         | 静态规则               | 自定义规则             | 动态/性能分析                           | 执行+回滚                        | 平台/审批流          | 一句话定位                                      |
| --------------- | -------------------- | ------------------------------------------- | ------------------ | ----------------- | --------------------------------- | ---------------------------- | --------------- | ------------------------------------------ |
| **SqlGuard**    | Rust 单二进制 + Rhai     | 通用（check 无方言适配）                             | 20 条（AST 精确）       | ✅ Rhai 脚本（需学 DSL） | ⚠️ 分离 Java 工程，仅 GaussDB/openGauss | ❌ 当前 CLI 未接入                 | ❌ 无             | 可编程精确 lint + 静态回滚设计（部分未落地）                 |
| **SQLFluff**    | Python CLI           | 20+ 方言（MySQL/PG/Oracle/T-SQL/Snowflake…）    | 极多（layout+lint）    | ✅ 配置 + 插件 + Jinja | ❌                                 | ❌                            | ❌               | 业界 de-facto 静态 lint 标准，支持 `--fix`          |
| **sqlcheck**    | Go 单二进制              | 多方言                                         | 固定反模式集（Karwin 分类）  | ❌ 不可扩展            | ❌                                 | ❌                            | ❌               | 轻量反模式扫描，规则不可改、无修复                          |
| **Yearning**    | Go + Web（MySQL only） | MySQL                                       | 内置 GUI 规则集         | ❌（GUI 开关，非脚本）     | ❌                                 | ✅ binlog 回滚 + 执行             | ✅ 多级审批工单        | 最流行的开源 MySQL 审核平台                          |
| **goInception** | Go 服务                | MySQL                                       | 内置风险规则             | 有限（变量调参）          | ❌                                 | ✅ 审核+执行+binlog 回滚+OSC/gh-ost | 作为引擎被集成         | 审核/执行/备份引擎，生产验证充分                          |
| **Archery**     | Python/Django 平台     | 多库（MySQL/PG/Oracle/MSSQL/Mongo/ClickHouse…） | 借 goInception；深度有限 | 有限                | ✅ 接 SOAR/慢日志                      | ✅                            | ✅ 细粒度 RBAC + 工单 | 一站式平台，广度之王，运维成本也高                          |
| **SOAR**        | Go 单二进制（小米）          | MySQL                                       | 启发式 + 索引建议         | ✅ 自定义改写规则         | ✅ EXPLAIN 解读/索引建议                 | ❌                            | ❌               | 优化与改写专家，最接近 SqlGuard 的「动态」抱负（但 MySQL only） |
| **DBdoctor**    | 商业（eBPF+外置优化器）       | MySQL/PG/TiDB                               | 性能导向               | —                 | ✅ 真实代价评估                          | ❌                            | ⚠️ 基础           | 性能审核新锐，突破「只做静态」                            |

### 3.3 与竞品的正面对比

- **vs SQLFluff（最直接的静态 lint 对手）**
  - SqlGuard 赢：① 单二进制零依赖、更适合 CI；② AST 精确规则（SQLFluff 也有 parser，但 SqlGuard 的 Rhai 可写任意复杂逻辑）；③ **原生 MyBatis Mapper 支持**（SQLFluff 不解析 MyBatis XML）。
  - SqlGuard 输：① 方言覆盖（SQLFluff 20+ 方言，SqlGuard 无方言选择）；② SARIF；③ 生态/社区/文档成熟度；④ 行内豁免。
- **vs Yearning / goInception（上线审核+回滚对手）**
  - SqlGuard 的**静态回滚生成**（不依赖 binlog、无需连库、能处理 CREATE TABLE LIKE 原子切换）理念上更优雅、更安全；但**完全未接入 CLI**，而 goInception/Yearning 是生产验证、开箱即用的。SqlGuard 还**缺审批流/工单/权限/执行**这一整个平台层——这是 DBA 真正 daily driver 的部分。
- **vs SOAR（动态分析对手）**
  - SOAR 的 EXPLAIN 解读/索引建议/改写是 MySQL 单库的成熟方案；SqlGuard 的「动态重放」设计更通用（多方言 + 镜像库重放 + 慢 SQL 识别），但**仅 GaussDB/openGauss 的 Java 实现**，且未发布二进制。理念领先，落地最弱。
- **vs Archery（平台对手）**
  - Archery 用 Python/Django 把查询/审核/执行/备份/优化/权限全包了，广度碾压；SqlGuard 是「单一 CLI 工具」，不试图做平台。两者不是直接替代，但 DBA 选型时往往先要「平台」再谈「引擎」。

---

## 4. 一个挑剔 DBA 的「采用决策树」

```
需要审批流 / 工单 / 权限 / 多人协作上线？   → 直接看 Yearning / Archery，SqlGuard 不提供
只是 CI 里拦 SQL 红线 + 管 MyBatis？        → SqlGuard 可试点（check + check-diff 是真能用的）
需要自动回滚落地到生产？                     → 现阶段的 SqlGuard 做不到（回滚未接入 CLI），用 goInception/Yearning
需要多方言 + 自动修复 + SARIF？             → SQLFluff 更成熟
只想要轻量反模式扫描、不想配环境？           → sqlcheck 单二进制即可
关心运行时真实性能（而非语法合规）？         → SOAR / DBdoctor
```



---

## 5. 改进优先级建议（让 DBA 愿意用）

**P0（不修就不敢用）**

1. **把 `rollback` 真正接入 CLI**：要么加 `gen-rollback` 子命令，要么在 `check` 里按 `[rollback].enabled` 真实调用 `RollbackGenerator::generate()`。当前「设计 145KB + 125 测试」却零入口，是最大的信任黑洞。
2. **修文档/代码脱节**：规则数量改对（20 条）、补 DDL003~DDL006 文档、删除或接上孤儿规则 `order_by_required_for_pagination.rhai`；把「动态分析 = 分离 Java + 仅 GaussDB」写清楚，别让 MySQL/PG 用户误以为开箱即用。
3. **给 `check` 加方言选择**（`[dialect]` 配置 + CLI），并明确「解析失败时至少应 warning 阻断而非静默跳过」——消除假阴性。

**P1（DBA 日常刚需）**  
4\. **行内豁免**（如 `-- sqlguard-disable-next-line DML001`），否则存量 SQL 无法渐进式启用。  
5\. **SARIF 输出**（接 GitHub/Azure/GitLab 代码扫描）。  
6\. **不再提交二进制到仓库**；提供 `brew`/`cargo install`/Docker 至少其一；补 Windows 二进制。  
7\. 给 Mapper 的 statement_type 做**配置化映射**（select→query 可单独治理）。

**P2（锦上添花）**  
8\. 文件级 mtime/hash 缓存（大仓库提速）。  
9\. `engine` 核心补单元测试（当前靠集成测试）。  
10\. 考虑把规则语言从 Rhai 换成更普及的表达（或至少提供大量「开箱即用规则模板」降低学习成本）。

---

## 6. 总结

SqlGuard 不是「玩具」——它的 AST 精确规则、MyBatis 支持、增量 CI 校验、以及那套**理念超前**的静态回滚设计，都显示出作者对 DBA 痛点有真实理解。但它目前处在「**设计领先于实现、文档落后于代码**」的阶段：

- 真正能用的只有 `check` / `check-diff` / 报告 / `replay-export`（且 replay 只导出不重放）；
- 最被宣传的回滚生成**未接入 CLI**；动态分析**只对 GaussDB 的分离 Java 工程**；
- 文档与代码对不上、缺方言/缺豁免/缺 SARIF/缺修复，这些恰恰是 DBA 上线前必问的「最后一公里」。

**给团队 DBA 的建议**：可以把 SqlGuard 当作 **MyBatis 项目的 CI 静态红线工具**小范围试点（这部分确实好用且差异化）；但**不要**把它当作上线审核/回滚平台来依赖，那部分当前是「PPT 级可用」。等 P0 三项（回滚接入、文档对齐、方言+解析失败处理）落地后，再重新评估。
