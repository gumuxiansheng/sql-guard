# GaussDB 开发规范 → SqlGuard 规则映射与 Rhai 能力缺口分析

> 输入：`docs/GaussDB开发技术实施策略（试用）.doc`（GaussDB 开发规范）
> 目的：把文档中的 SQL 开发规范归纳为可执行规则类别，并评估**当前** Rhai 规则引擎能否承载
> 结论口径：所有"引擎能力"结论均以 `src/rule/engine/{ast,analyzer,parser,runner,idents,scanner}.rs` 的实际注册代码为准
> 关联文档：[`rule-scripting.md`](rule-scripting.md)、[`default-rules.md`](default-rules.md)、[`dialect-fallback.md`](dialect-fallback.md)
>
> **本次更新（2026-09-28）**：
> 1. 全文按引擎当前能力**重新判定**了一遍，并做了**真机探针验证**（§8）。旧版"✅ 9 / 🟡 57"的判定已作废。
> 2. **A 类 22 条已落地**：新增 18 个规则脚本（§9 清单），另 4 条由既有的
>    DDL003/004/005/007、DDL006、DML110、DML111 覆盖（其中 G-DML-01 通过给 DML110
>    加 `params.max_join_tables` 支持 GaussDB 更严阈值实现）。
> 3. **B 类 39 条本轮未实现**——文本兜底类误报面较大，建议按项目优先级分批推进，
>    并优先补 C1/C3/C7 把它们升级为 A 类后再写。

### 0.1b 实现状态一览

| 分级 | 条目 | 已实现脚本 | 说明 |
|---|---|---|---|
| **A** | 22 | **18** 新增 + 4 复用既有规则 | 见 §9 逐条清单；已注册到 `sqlguard.rules.toml.example`，默认 `enabled = false`，GaussDB 项目按需启用 |
| **B** | 39 | 0 | 文本兜底 / 仅同文件 / 启发式；本轮不实现 |
| **C** | 5 | 0 | 需 C3 / C6 / C7 / C9 |
| **D** | 19 | — | 不适合静态规则 |

---

## 0. 结论摘要

### 0.1 M1 落地后的可实现性

| 结论 | 条目数 | 占比 |
|---|---|---|
| **A. 现在就能用 rhai 脚本实现（AST 精确）** | **22** | 25.9% |
| **B. 现在就能用 rhai 脚本实现（受限：文本兜底 / 仅同文件 / 启发式）** | **39** | 45.9% |
| **C. 仍需扩展引擎能力（🟡）** | **5** | 5.9% |
| **D. 不适合静态规则（❌）** | **19** | 22.3% |
| **合计** | **85** | 100% |

**能立刻动手写脚本的是 A + B = 61 条（71.8%）**，比 M1 之前的"9 条"多出 **52 条**。

M1 的贡献可以拆成三块：

| 能力 | 直接解锁 | 说明 |
|---|---|---|
| **C2 子句修饰符与字段补齐** | 6 条走精确 AST | `G-DML-08/09/10/17`、`G-IDX-01`、`G-OBJ-02` 从"不可判"变成"要么精确 AST、要么原文兜底" |
| **C4 语句原文切片** | 39 条中的主体 | 这是本轮**最大**的解锁源：GaussDB 专有子句（`DISTRIBUTE BY`、`ON COMMIT DELETE ROWS`、`UNLOGGED`、`LOCK TABLE`、`EXPLAIN ANALYZE`、`PARTITION … MAXVALUE`）sqlparser 全部不建模或静默丢弃，但有了 `ast.stmt_text(s)` 就能在脚本里读原文判定 |
| **C5 标识符与字节工具 + C8 规则参数** | 3 条 + 6 条阈值外部化 | 命名四联（字符集/引号/预留前缀/字节长度）与"阈值不硬编码在脚本里" |

### 0.2 一条贯穿全文的关键机制（务必理解）

sqlparser 0.60 对 GaussDB 专有语法有两种处理方式，**两种都要求规则同时读原文**：

1. **静默丢弃**：`CREATE TABLE t (...) DISTRIBUTE BY HASH(a,b,c,d)` 会被解析成 `CREATE_TABLE`，
   表名/列都在，**但 `DISTRIBUTE BY` 子句完全不在 AST 里**（实测见 §8）。
   `UPDATE t SET a=1 ORDER BY b` 同理——靠方言回退链拿到 `UPDATE` 节点，`ORDER BY` 被丢掉。
2. **整体解析失败**：`CREATE UNLOGGED TABLE ...`、`LOCK TABLE ...` 直接是 `PARSE_ERROR`。

因此本轮的判定规则是：**能用 AST 字段就别读原文；AST 没建模的一律走 `ast.stmt_text(s)` 文本兜底**。
好消息是 `PARSE_ERROR` 语句**同样保留原文**，所以文本兜底规则在第 2 种情况下依然生效（§8 已验证）。

### 0.3 还差什么

| 缺口 | 阻塞条目 | 性质 |
|---|---|---|
| **C1 表达式递归遍历** | 0 条（原先 7 条已被 C4 文本兜底接管） | 不再是"能不能做"，而是"做得准不准"——目前 B 类里相当一部分（`concat`/`now`/`LIKE`/`IN` 计数）是文本级判定，**天然会受字符串与注释干扰**，需要靠行内豁免兜底 |
| **C3 GaussDB 专有语法 AST** | 1 条（`G-TBL-01`） | 其余 17 条已改由文本兜底承担；只有"混用存储引擎"这类既需要建表选项又需要跨文件聚合的还卡着 |
| **C6 类型系统映射** | 1 条（`G-TYP-01`） | 值域映射（TINYINT/SMALLINT/…→范围）无脚本内等价物 |
| **C7 元数据 / 表结构查询** | 2 条（`G-DML-15`、部分 B 类的跨文件部分） | "大表"判定、跨文件的分布键/索引清单 |
| **C9 跨文件 / 项目级聚合** | 2 条（`G-DB-02`、`G-TBL-01`） | 实例内 DATABASE 数、存储引擎混用 |

**架构级结论不变**：引擎仍是"单文件、单规则、无外部上下文"。**B 类里所有"仅同文件"的判定**
（视图嵌套、对视图 DML、分布键比对、索引数量、分区 DDL 后重建索引）在真实工程里 DDL 往往拆在多个文件，
所以这 11 条的实际可用性取决于 **C7/C9 是否落地**——这一点已逐条标注。

**19 条 D 类**仍是运行期/并发/计划级信息，不应由静态规则引擎承担。

---

## 1. 源文档解析说明

### 1.1 文档结构

| 章节 | 内容 | 规则密度 |
|---|---|---|
| 引言 / 范围 / 术语 | 术语定义（模式、AStore、USTORE、分区表、分布表…） | 0 |
| 4.1 数据库设计 | 实例自带库、库数量、库隔离、表空间、public schema | 5 |
| 4.2 数据库对象设计 | 表设计/命名/字段类型/其它对象 | 25 |
| 4.3 数据库权限设计 | 最小权限、OWNER、权限分离 | 4 |
| 4.4 数据库事务设计 | 事务大小、长事务、子事务 | 3 |
| 4.5 JAVA 连接与编码 | schema 限定访问 | 1 |
| 4.6 SQL 编写 | DCL / DDL / DML（INSERT/UPDATE/DELETE/WHERE/LIKE/ORDER BY） | 30 |
| 4.7 性能优化 | 执行计划、统计信息 | 5 |
| 4.8 死元组（仅 AStore） | 手动 Vacuum、AutoVacuum | 6 |
| 5 分布式数据库 | 分布键、索引约束、DDL/DML 分布式要求 | 8 |
| 附录 | 推荐数据类型、整数范围、11 条最佳实践阈值 | 3（含 11 个阈值） |

### 1.2 提取口径与置信度

- **正文段落**：完整读取（UTF-16 坐标按段落遍历），`【禁止】/【强制】/【建议】` 标记为权威级别来源。
- **表格**：正文遍历不返回单元格文本，已用 `doc_get_table_info` 逐一补齐 10 张表格（分布策略对比、UPDATE 语法白名单、推荐数据类型、最佳实践阈值、创建库语句示例、倾斜检查 SQL 等）。
- **截断**：单段预览上限 200 字符，个别 `说明` 段尾部被截断；已核对——截断部分均为**解释性文字**，规则条文本身完整。
- **⚠️ 源文档自身冲突**：4.1 正文说"实例内应用自定义 DATABASE 禁止超过 10 个"，附录最佳实践表说"≤3"。规则化时必须配置化（见 §6.3），不能硬编码。
- **⚠️ 源文档笔误**：4.6.3.1 出现"【禁止】TDSQL 禁止使用 ROWID"——TDSQL 应为 GaussDB 的笔误，规则化时需与规范 owner 确认。

---

## 2. 规则分类与条目清单（M1 后重新判定）

**适配度图例（本版新增分级）**

| 标记 | 含义 | 判定手段 |
|---|---|---|
| **A** | 现在就能实现，**AST 精确** | 全部依据结构化字段，不受字符串/注释/括号干扰 |
| **B** | 现在就能实现，**受限** | 三种受限之一：① 文本兜底（读 `ast.stmt_text(s)`，可能受字符串/注释干扰）；② 仅同文件（需同文件内其它语句的信息）；③ 启发式（阈值/比例需人工定夺）。**建议一律先给 `warning`** |
| **C** | 仍需引擎扩展 🟡 | 现有 AST 与原文都拿不到可靠结论 |
| **D** | 不适合静态规则 ❌ | 需要运行期/并发/计划级信息 |

### 2.1 分类总览

| 类别码 | 类别 | 条目 | A | B | C | D |
|---|---|---|---|---|---|---|
| G-DB | 数据库级设计 | 5 | 0 | 3 | 1 | 1 |
| G-NAM | 对象命名 | 5 | 5 | 0 | 0 | 0 |
| G-TYP | 字段与类型 | 4 | 3 | 0 | 1 | 0 |
| G-TBL | 表结构 / 存储引擎 / 分区 | 7 | 0 | 4 | 1 | 2 |
| G-OBJ | 对象类型禁用 | 9 | 3 | 6 | 0 | 0 |
| G-IDX | 索引与约束 | 5 | 2 | 3 | 0 | 0 |
| G-DCL | DCL 与权限 | 4 | 1 | 1 | 1 | 1 |
| G-DDL | DDL 通用 | 6 | 1 | 3 | 0 | 2 |
| G-DML | DML 通用 | 20 | 6 | 11 | 1 | 2 |
| G-TXN | 事务 | 3 | 0 | 0 | 0 | 3 |
| G-PERF | 性能与运维 | 9 | 1 | 2 | 0 | 6 |
| G-DIST | 分布式 | 8 | 0 | 6 | 0 | 2 |
| **合计** | | **85** | **22** | **39** | **5** | **19** |

### 2.2 G-DB 数据库级设计

| 编号 | 条目（原文摘要） | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-DB-01 | 禁止使用 postgres 数据库，必须为业务创建 DATABASE | 禁止 | **B** | 文本兜底：`USE` / `CREATE DATABASE` 语句原文匹配 `POSTGRES`。局限：连接串（JDBC URL）不在 SQL 文件里，管不到 |
| G-DB-02 | 单实例应用自定义 DATABASE 禁止超过 10 个 | 禁止 | **C** | 需 **C9 跨文件聚合**；且源文档与附录阈值冲突（10 vs 3），必须走 params 配置 |
| G-DB-03 | 必须使用默认表空间（仅 3 种例外场景） | 强制 | **B** | 文本兜底：原文出现 `CREATE TABLESPACE` 即报（3 种例外无法自动识别 → 靠行内豁免） |
| G-DB-04 | 禁止使用 public schema，必须为业务创建 schema | 禁止 | **B** | 文本兜底：`public.` 限定名 / `SET search_path` / `CREATE SCHEMA public` |
| G-DB-05 | 实例多库时应使用 Database 隔离 | 建议 | **D** | 架构决策，非 SQL 属性 |

### 2.3 G-NAM 对象命名（全部 A，M1 已完全解锁）

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-NAM-01 | 对象名只能使用字母、数字、下划线 | 强制 | **A** | `is_valid_ident(strip_quotes(name))`。**实测提醒**：未加引号的非法名字 sqlparser 直接解析失败（连 AST 都没有），所以本规则的真实价值集中在"带引号名字去引号后再校验"与元数据/参数化名字上，与 G-NAM-02 高度重叠 |
| G-NAM-02 | 禁止用双引号字符串定义对象名 | 禁止 | **A** | `is_quoted_ident(ct.table_name())`。**已验证**：`CREATE TABLE "Quoted"` 的 `table_name()` 返回带引号的 `"Quoted"`，判定可靠 |
| G-NAM-03 | 禁止 pg_/gs_/adm_/my_/db_ 前缀 | 禁止 | **A** | `has_reserved_prefix(name)`（对叶子段判定，`ofsm.pg_class` 也能命中）。**已验证**：`pg_legacy` 命中 |
| G-NAM-04 | 对象名长度禁止超过 63 **字节** | 禁止 | **A** | `len_bytes(name) > params["name_max_bytes"]`（Rhai 的 `len()` 是字符数，必须用内置 `len_bytes`）。**已验证**：74 字节名字命中 |
| G-NAM-05 | 表名 / 索引名 / 备份表命名约定 | 建议 | **A** | 已有规则 DDL004 / DDL005 / DDL007。另可用 `is_reserved_word()` 覆盖关键字命名（`CREATE TABLE "order"` → 命中，已验证） |

### 2.4 G-TYP 字段与类型

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-TYP-01 | 按取值范围选整数类型；超 BIGINT 用 NUMERIC/DECIMAL | 强制 | **C** | 需 **C6**："类型 → 值域"映射（TINYINT/SMALLINT/INTEGER/BIGINT/NUMERIC）无脚本内等价物。注：**能判"用的是不是整数类型"**，判不了"范围选得对不对" |
| G-TYP-02 | 单表大字段（`VARCHAR(1000)` 等）宜不超过 8 个 | 建议 | **A** | `columns()` 拿 `data_type()` 原文，脚本内维护"大字段"判定规则（`VARCHAR/CHAR(n>2000)`、`TEXT`、`BYTEA`、`CLOB`…）并计数。类型别名想全覆盖才需要 C6 |
| G-TYP-03 | 禁止 CTID/CID/OID/XID/TID/XMIN/CMIN/XMAX/CMAX 作业务字段 | 强制 | **A** | `columns()` + `name()` 逐列比对（内置常量表即可） |
| G-TYP-04 | 优先使用附录推荐基础类型 | 建议 | **A** | 脚本内维护附录推荐类型白名单数组，对 `data_type()` 做前缀匹配 |

### 2.5 G-TBL 表结构 / 存储引擎 / 分区

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-TBL-01 | 禁止混用存储引擎（AStore / UStore） | 禁止 | **C** | 双重阻塞：`STORAGE_TYPE` 建表选项无 AST（**C3**）+ 需要跨文件统计（**C9**）。文本兜底只能覆盖"同一文件内混用" |
| G-TBL-02 | 按业务场景选择存储引擎（更新频率判据） | 强制 | **D** | 需要运行期更新频率 |
| G-TBL-03 | 集中式非自扩展分区必须定义上边界 `MAXVALUE` | 强制 | **B** | 文本兜底：`CREATE TABLE` 原文含 `PARTITION BY` 且不含 `MAXVALUE` → 报。局限：无法逐分区校验"每个分区都有上边界"，属近似判定 |
| G-TBL-04 | 单表分区/子分区个数原则上禁止超过 1000 | 禁止 | **B** | 文本兜底：统计原文中 `PARTITION ` 出现次数与阈值比对（阈值走 params）。同 C3 未落地前的近似手段 |
| G-TBL-05 | 字符集必须统一为 UTF8（UTF8MB4） | 强制 | **B** | 文本兜底：`CREATE DATABASE` 原文校验 `ENCODING` 取值 |
| G-TBL-06 | 禁止使用事务级全局临时表（`ON COMMIT DELETE ROWS`） | 禁止 | **B** | 文本兜底：原文含 `ON COMMIT DELETE ROWS`。**已验证命中** |
| G-TBL-07 | 会话级 GTT 提交前显式 delete；AStore 事务后手动 vacuum | 强制 | **D** | 运行期会话/事务行为 |

### 2.6 G-OBJ 对象类型禁用

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-OBJ-01 | 禁止使用物化视图 | 禁止 | **A** | `ViewInfo.materialized()` |
| G-OBJ-02 | 禁止在视图中进行排序操作 | 禁止 | **A** | `v.definition().has_order_by()`。**已验证命中**（这是 C2 新增 `ViewInfo.definition` 的直接成果） |
| G-OBJ-03 | 禁止在视图中嵌套视图 | 禁止 | **B** | `v.definition().from_table()` + 同文件内视图名集合比对。**已验证命中**（`v_outer` FROM `v_sorted`）。局限：**仅同文件**；跨文件需 C7 |
| G-OBJ-04 | 禁止对视图执行 SELECT 以外的 DML | 禁止 | **B** | 同文件内先收集 `CREATE VIEW` 名称，再比对 `UPDATE/DELETE/INSERT` 的 `table_name()`。局限：**仅同文件** |
| G-OBJ-05 | 联机交易禁止使用存储过程 / 自定义函数 | 禁止 | **B** | 文本兜底：原文含 `CREATE PROCEDURE` / `CREATE FUNCTION`（这两类 sqlparser 归入 `OTHER`，无结构化信息）。"联机 vs 批量"靠 `script_type` / params 区分 |
| G-OBJ-06 | 存储过程/函数禁止使用 FENCED / NOT FENCED 参数 | 禁止 | **B** | 文本兜底：原文含 `FENCED`（含 `NOT FENCED`）。局限：过程体内的同名标识符会误报，需豁免 |
| G-OBJ-07 | 禁止触发器 / 事件 / SEQUENCE / 外键 | 禁止 | **B** | 外键走 **A**（`CreateInfo.has_foreign_key()` / `foreign_keys()`）；触发器/事件/SEQUENCE 无 kind，走文本兜底（`CREATE TRIGGER` / `CREATE EVENT` / `CREATE SEQUENCE`） |
| G-OBJ-08 | 分布式禁止 `UNLOGGED TABLE` | 禁止 | **B** | 文本兜底：原文含 `UNLOGGED`。**已验证命中**——而且该语句是 `PARSE_ERROR`，证明**解析失败时文本兜底依然生效** |
| G-OBJ-09 | 不建议使用视图 / 不建议嵌套视图 | 建议 | **A** | `StmtInfo.has_create_view()`（提示级） |

### 2.7 G-IDX 索引与约束

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-IDX-01 | 有联机事务时创建索引必须加 `CONCURRENTLY` | 强制 | **A** | `CreateIndexInfo.concurrently()`。**已验证命中**（`CREATE INDEX idx_t_a` 无 CONCURRENTLY）。注意副作用：`using_method()` 在本例为空串，取值可靠但可能为空 |
| G-IDX-02 | 不建议使用全局二级索引 | 建议 | **B** | 文本兜底：原文含 `GLOBAL INDEX`（GaussDB 专有语法，无 AST）。局限：需与规范 owner 确认实际写法 |
| G-IDX-03 | 主键与唯一索引必须包含分布键（分布式） | 禁止 | **B** | 同文件内：从 `CREATE TABLE` 原文提取 `DISTRIBUTE BY HASH(a,b,c)` 的键列（**已验证可提取**，见 §8 `dist_key_count=4`），再与 `primary_key_columns()` 比对。局限：**仅同文件**；跨文件需 C7 |
| G-IDX-04 | 单表索引 <5、复合索引 <3、复合索引列 <5、索引字段总长 ≤50 字节 | 建议 | **B** | 复合索引列数精确（`ci.columns().len()`）；索引个数与索引字段总长需**同文件内**统计 + 从类型文本估宽。阈值走 params |
| G-IDX-05 | 冗余索引检测 | — | **A** | 已有规则 DDL006 |

### 2.8 G-DCL DCL 与权限

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-DCL-01 | 禁止使用 `LOCK TABLE` 语句加锁 | 禁止 | **B** | 文本兜底：原文含 `LOCK TABLE`。**已验证命中**（该语句为 `PARSE_ERROR`，仍命中） |
| G-DCL-02 | DDL 脚本字段名禁止使用引号 / 反引号 | 禁止 | **A** | `ast.stmt_text(s)` 中含 `"` 或反引号即报（AST 丢引号，必须读原文）——C4 的直接成果 |
| G-DCL-03 | 最小权限授权 / OWNER 权限谨慎 / 权限分离 | 建议 | **C** | `GRANT`/`REVOKE` 只有 kind、**无结构化字段**（grantee / privileges / 对象）。文本兜底只能做"是否出现 `GRANT ALL` / `TO PUBLIC`"这类宽泛检查，覆盖不足条文；完整实现需引擎补齐 GRANT AST |
| G-DCL-04 | 数据库初始用户不允许业务直接使用 | 禁止 | **D** | 运行期连接身份 |

### 2.9 G-DDL DDL 通用

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-DDL-01 | 分区 DDL（删/切/合/清/交换）后必须 `UPDATE GLOBAL INDEX` | 强制 | **B** | 同文件内跨语句关联：发现分区 DDL（原文匹配）+ 同文件未见 `UPDATE GLOBAL INDEX` → 报。局限：**仅同文件**，跨文件需 C9 |
| G-DDL-02 | 自动提交关闭或事务中，需显式 `COMMIT` | 强制 | **A** | 脚本内遍历 `ast.statements()` 维护状态机（`START_TRANSACTION` / `COMMIT` / `ROLLBACK` kind 齐全） |
| G-DDL-03 | `CREATE DATABASE` 必须设 `ENCODING='UTF8'` + `DBCOMPATIBILITY` 兼容 Oracle | 强制 | **B** | 文本兜底：`CREATE DATABASE` 原文校验两个选项是否存在及取值 |
| G-DDL-04 | 宜指定 `LC_CTYPE` / `LC_COLLATE='c'` | 建议 | **B** | 同上 |
| G-DDL-05 | 禁止并发对全局临时表做 DDL | 禁止 | **D** | 并发时序不可判定 |
| G-DDL-06 | 禁止业务高峰期执行 DDL | 禁止 | **D** | 时间窗不可判定 |

### 2.10 G-DML DML 通用

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-DML-01 | 禁止 5 张表以上关联（建议 ≤3，批量 ≤5） | 禁止 | **A** | `SelectInfo.table_count()` + params。**已验证命中**（5 表关联 → `too_many_joins=5`） |
| G-DML-02 | 避免复杂 SQL，建议拆分为多个小 SQL | 建议 | **B** | 启发式：`subqueries().len()` + `table_count()` + `is_union()` + `len_bytes(stmt_text)`。无客观"复杂度"定义，建议 warning |
| G-DML-03 | 用连接符 `\|\|` 替换 `concat()` 函数 | 建议 | **B** | 文本兜底：原文含 `CONCAT(`。**已验证命中**。局限：字符串/注释里的 CONCAT 会误报 → 需豁免 |
| G-DML-04 | 用 `CURRENT_DATE/TIME/TIMESTAMP(n)` 代替 `now()` | 建议 | **B** | 文本兜底：原文含 `NOW(`。**已验证命中**。附注：方言归一化层会把 `SYSDATE` 重写成 `CURRENT_TIMESTAMP`，但 `stmt_text` 取的是**归一化之前**的原文，所以同一条规则也能顺带检出 `SYSDATE`——这正是"必须读原文"的价值 |
| G-DML-05 | 禁止在业务 SQL 中使用 CTID/CID/OID/TID/XMIN/CMIN/XMAX/CMAX | 禁止 | **B** | 文本兜底 + `ident_leaf` 词边界切分（避免 `pctid` 之类误匹配）。局限：命中字符串/注释会误报 |
| G-DML-06 | 禁止使用 ROWID | 禁止 | **B** | 文本兜底（同上）。风险最高的一条：业务若真有名为 `rowid` 的普通列会误报 → 必须配豁免 |
| G-DML-07 | 禁止 `INSERT ON DUPLICATE KEY UPDATE` 更新主键/唯一约束列 | 强制 | **B** | 同文件内：从原文解析 `ON DUPLICATE KEY UPDATE` 之后的 SET 列（`InsertInfo` 无该字段），与同文件 `CREATE TABLE` 的主键/唯一列比对。跨文件需 C7 |
| G-DML-08 | `UPDATE` 语句禁止 `ORDER BY` / `GROUP BY` | 禁止 | **A** | `UpdateInfo.has_order_by()` / `has_group_by()`。**已验证命中 2 条**——注意其实现是"回退链 + 原文顶层子句扫描"，脚本侧无感知 |
| G-DML-09 | `DELETE` 语句禁止 `ORDER BY` / `GROUP BY` | 禁止 | **A** | `DeleteInfo.has_order_by()`（AST）/ `has_group_by()`（文本兜底）。**已验证命中** |
| G-DML-10 | `UPDATE ... WHERE` 含子查询宜改为 JOIN | 建议 | **A** | `UpdateInfo.has_subquery()` |
| G-DML-11 | `NOT IN` 的条件为子查询时宜用 `NOT EXISTS` 替代 | 建议 | **B** | `where_expr().kind() == "IN_SUBQUERY"` 只覆盖**顶层**；`NOT IN` 在顶层是 `UNARY_OP(NOT)`，需辅以原文 `NOT IN` 判定。深层嵌套需 C1 |
| G-DML-12 | 禁止 `IN` 子查询中含有聚合函数 | 禁止 | **B** | `subqueries()` 递归返回各层 `SelectInfo`，其 `projection()` 文本以 `COUNT(`/`SUM(` 等开头即报。属启发式（函数名白名单） |
| G-DML-13 | 禁止 `IN` 子查询结果集超过 1000 行 | 禁止 | **D** | 结果集规模静态不可判定 |
| G-DML-14 | `IN` 列表字段超过 200 个时改用 `in (values(...))` | 建议 | **B** | 顶层 `ExprInfo.kind() == "IN_LIST"` 后对 `text()` 计数。局限：仅顶层；多个谓词组合（`a=1 AND b IN (...)`）时顶层是 `BINARY_OP`，取不到 → 需 C1 才能完整 |
| G-DML-15 | `exists/not exists` 子查询含大表时宜加 `/*+ no_expand*/` | 建议 | **C** | 需 **C7**："大表"只能来自元数据或命名约定；且需在原文中检出 hint |
| G-DML-16 | `LIKE` 操作中必须使用 immutable 类型函数 | 强制 | **B** | 脚本内维护已知 `stable/volatile` 函数表（`concat` / `now` / `random`…），对原文 LIKE 右侧做函数名匹配。完整函数目录需 C6/C14 |
| G-DML-17 | `ORDER BY` 必须显式指定 `ASC/DESC` 与 `NULL FIRST/LAST` | 强制 | **A** | `sel.order_by_items()` + `has_order_by_without_direction()` / `..._without_nulls_spec()`。**三态已验证**：未指定时 `direction()`/`nulls()` 返回空串，`ORDER BY id DESC NULLS LAST` 正确返回 `DESC`/`LAST` |
| G-DML-18 | 禁止在生产环境对写 query 执行 `explain analyze` | 禁止 | **B** | 文本兜底：原文含 `EXPLAIN ANALYZE`（`EXPLAIN` 被解析为 `OTHER`，无结构化信息）。**已验证命中**。生产/非生产无法从脚本区分 → 建议按目录/文件名分流 |
| G-DML-19 | WHERE/ON 含 OR 表达式须评估转 `UNION ALL` | 建议 | **A** | 已有规则 DML111 `no_or_in_where` |
| G-DML-20 | 批量插入建议使用 `executeBatch` | 建议 | **D** | 客户端行为，非 SQL 属性 |

### 2.11 G-TXN 事务

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-TXN-01 | 联机事务 ≤1000 条 / 批量 ≤10000 / 单次批量 ≤1000 | 建议 | **D** | 运行期数据量 |
| G-TXN-02 | 批量单事务必须 30 分钟内完成 | 强制 | **D** | 运行期时长 |
| G-TXN-03 | 子事务个数禁止超过 10W、宜 ≤1000 | 强制 | **D** | 运行期；静态只能数 `SAVEPOINT` 字面出现次数，与"运行期子事务数"不是一回事 |

### 2.12 G-PERF 性能与运维

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-PERF-01 | 所有 SQL 必须查看执行计划，尤其新增/索引变更后 | 强制 | **D** | 流程 + 运行期 |
| G-PERF-02 | 批量入库（变更 >10%）或迁移后必须 `analyze` | 强制 | **D** | 运行期 |
| G-PERF-03 | `analyze` 必须在业务低峰期执行 | 强制 | **D** | 运行期资源水位 |
| G-PERF-04 | 禁止多会话同时对同一张表执行 vacuum | 禁止 | **D** | 并发时序 |
| G-PERF-05 | 禁止同时执行多个 `vacuum full`、禁止高峰期 `vacuum full` | 禁止 | **B** | 同文件内可检测多条 `VACUUM FULL`（文本兜底）；"高峰期"不可判 |
| G-PERF-06 | 按需调整 `autovacuum_vacuum_cost_delay` | 建议 | **D** | 配置项 |
| G-PERF-07 | SQL 语句最佳长度 <5KB | 建议 | **A** | `len_bytes(ast.stmt_text(s)) > params["stmt_max_bytes"]`。**已验证命中**（6314 字节语句）。注意必须用 `len_bytes` 而非 `len()` |
| G-PERF-08 | 单表字段 <50 / 单行行宽 <2KB | 建议 | **B** | 列数精确（`columns().len()`）；行宽需从类型文本估算（`VARCHAR(n)` 计 n、`INT` 计 4…），属估算 |
| G-PERF-09 | 表+索引总个数 <10000 / 单分区数据 <5000w / DATABASE 数 ≤3 | 建议 | **D** | 跨文件聚合 + 运行期 |

### 2.13 G-DIST 分布式

| 编号 | 条目 | 级别 | 适配 | 判定手段 / 备注 |
|---|---|---|---|---|
| G-DIST-01 | 必须指定表分布 `DISTRIBUTE BY` | 强制 | **B** | 文本兜底：`CREATE TABLE` 原文不含 `DISTRIBUTE BY` → 报。**已验证**：该语句被解析成 `CREATE_TABLE` 但子句被静默丢弃，所以**只能读原文** |
| G-DIST-02 | 系统配置表/字典表等小表采用 `REPLICATION` 分布 | 强制 | **B** | 文本兜底（`DISTRIBUTE BY REPLICATION`）；"是否小表"需元数据，只能按表名前缀/params 白名单近似 |
| G-DIST-03 | 分布键不建议超过 3 列 | 建议 | **B** | 从原文提取 `DISTRIBUTE BY HASH(...)` 括号内列数。**已验证**：`HASH(a, b, c, d)` → `dist_key_count=4` |
| G-DIST-04 | 分布键使用的列总长度不超过 128 | 建议 | **B** | 需从原文取分布键列 + 同文件 `CREATE TABLE` 的类型文本估宽 |
| G-DIST-05 | 分布键值禁止更新（UPDATE） | 禁止 | **B** | 同文件内：分布键列集合 + `UpdateInfo.sets_column(col)`。**已验证**：`sets_column("id")` 命中 `UPDATE t SET id = 9`。跨文件需 C7 |
| G-DIST-06 | 查询 WHERE 应包含所有分布键等值查询条件 | 建议 | **B** | 同文件内分布键 + `where_clause()` 文本包含判定。启发式：静态只能证明"WHERE 出现了该列"，证不了走的是单 DN 计划 |
| G-DIST-07 | 减少跨节点执行 / 数据重分布 / 算子下推 | 建议 | **D** | 执行计划级语义 |
| G-DIST-08 | Hash 分布做数据倾斜检查（5% 视为倾斜 / 10% 必须调整） | 建议 | **D** | 运行期 SQL 查询（`xc_node_id` 统计） |

---

## 3. 当前 Rhai 引擎能力盘点（M1 落地后）

### 3.1 已具备（机械统计自 `runner.rs::build_engine`）

| 维度 | 现状 |
|---|---|
| 注入变量 | `context`（`sql_content` / `file_path` / `file_name` / `script_type` / `line_count` / `ast` / **`params`**）、`violations` |
| 包装类型 | **25 个**：`SqlAst`、`StmtInfo`、`CreateInfo`、`ColumnInfo`、`DropInfo`、`SelectInfo`、`InsertInfo`、`UpdateInfo`、`DeleteInfo`、`JoinInfo`、`AlterTableInfo`、`AlterOpInfo`、`TruncateInfo`、`ViewInfo`、`CreateIndexInfo`、`TransactionInfo`、`CteInfo`、`WindowFuncInfo`、**`OrderByItemInfo`**、`ExprInfo`、`ForeignKeyInfo`、`CheckInfo`、`IndexInfo`、`UniqueInfo`、`CommentInfo` |
| 注册函数 | **245 条**（M1 前为 199；按类型分布：`SelectInfo` 48、`StmtInfo` 27、`SqlAst` 17、`CreateInfo` 16、`ColumnInfo` 15、`JoinInfo`/`ExprInfo` 11、`UpdateInfo` 10、`OrderByItemInfo` 9、`ViewInfo`/`CreateIndexInfo` 8、全局函数 8…） |
| 语句 kind | `CREATE_TABLE`、`DROP_<TYPE>`（动态拼装）、`SELECT`、`INSERT`、`UPDATE`、`DELETE`、`ALTER_TABLE`、`TRUNCATE`、`CREATE_VIEW`、`CREATE_INDEX`、`START_TRANSACTION`、`COMMIT`、`ROLLBACK`、`GRANT`、`REVOKE`、`MERGE`、`SET_VARIABLE`、`USE`、`OTHER`、`PARSE_ERROR`。**无** `EXPLAIN` / `LOCK` / `CREATE_SEQUENCE` / `CREATE_TRIGGER` / `CREATE_PROCEDURE` / `CREATE_DATABASE`（一律 `OTHER` 或 `PARSE_ERROR`） |
| 全局函数（C5） | `len_bytes`、`is_valid_ident`、`is_quoted_ident`、`strip_quotes`、`ident_leaf`、`normalize_ident`、`is_reserved_word`（117 项）、`has_reserved_prefix` |
| Helper | `config/rules/lib/helpers.rhai` prepend 注入 6 个函数（`guard_parse_error` / `parse_error_violation` / `violation` / `violation_line` / `violation_msg` / `join_strs`） |
| 沙箱 | `max_operations=1_000_000`、`max_modules=0`（禁用 import）、表达式深度 64 |
| 已有规则 | **25 条**：`DDL001–DDL007`、`DML001–DML007`、`DML101–DML111`（与 `config/rules/` 下 25 个 `.rhai` 脚本一一对应） |
| 其它 | 行内豁免（`sqlguard-disable-next-line` / `-line`）、规则按 id/group 筛选、逐语句方言回退链、`sqlguard explain` AST 自省 |

**关键能力一句话**：现在规则同时握有 **结构化 AST** 与 **原始语句文本**（`SqlAst.source` / `stmt_text()`），
且原文在方言归一化（`gaussdb_rewrite`）**之前**保存，因此"引号写法""原始长度""GaussDB 专有子句"
三类信息都保得住——这正是 B 类 39 条得以成立的基础。

### 3.2 支撑 B 类的三个机制（都是本轮新增）

1. **`ast.stmt_text(stmt)`** —— 按语句行区间取原文并 trim。已声明局限：同一行写两条语句（`A; B;`）时会连带取到同行后续内容；需要精确边界时用 `ast.slice(l1,c1,l2,c2)`。
2. **`PARSE_ERROR` 语句依然可读原文** —— 这使文本兜底规则在"整体解析失败"的 GaussDB 语句上照样生效（§8 用 `UNLOGGED`、`LOCK TABLE` 验证）。
3. **`has_top_level_clause` 语义已内建到 AST 字段** —— `UpdateInfo.has_order_by/has_group_by`、`DeleteInfo.has_group_by` 在引擎内部用"括号深度感知 + 跳过字符串/注释"的顶层子句扫描实现，脚本无需自己处理该复杂度。

---

## 4. 能力缺口清单（M1 后）

### ✅ 已落地（M1，2026-09-28）

| 缺口 | 状态 | 落地内容 |
|---|---|---|
| C2 子句修饰符与字段补齐 | ✅ | `OrderByItemInfo`（ASC/DESC/NULLS 三态）；`UpdateInfo` 补 `set_columns`/`sets_column`/`set_clause_text`/`has_subquery`/`has_limit`/`has_order_by`/`has_group_by`；`DeleteInfo` 补同类；`CreateIndexInfo` 补 `concurrently`/`if_not_exists`/`using_method`/`include_columns`；`ViewInfo` 补 `definition`/`has_definition`/`is_temporary`/`if_not_exists` |
| C4 语句原文切片 | ✅ | `SqlAst.source`（原始文本）+ `source`/`slice`/`stmt_text`/`stmt_text_at`/`stmt_text_at_line`/`line_range_text` |
| C5 标识符与字节工具 | ✅ | 8 个全局函数（见 §3.1） |
| C8 规则参数注入 | ✅ | `RuleConfig.params` → `context["params"]`，支持 `[rules.params]` 与内联两种写法 |

### C1 表达式递归遍历 —— **P1（性质已改变）**

- **现状**：`ExprInfo` 仍只有顶层判定，无 `children()` / 函数调用清单；`where_expr()` 只给顶层表达式。
- **阻塞条目：0 条**（原先 7 条已被 C4 文本兜底接管）。
- **但它决定了 B 类的"精度"**：`G-DML-03/04/05/11/12/14/16` 目前靠原文匹配，无法区分
  "真在 SQL 结构里出现"与"只出现在字符串/注释里"，也无法穿透 `a=1 AND b IN (...)` 这类组合谓词。
- **建议**：`SelectInfo.function_calls() -> Array<FuncCallInfo{name, args_text, line, column}>`；
  `ExprInfo.children()`；`SelectInfo.find_exprs(kind)`；给 `ExprInfo.line/column` 回填真实位置。
- **收益**：把 B 类里 7 条升级为 A 类（消除误报面），并让规则能报出精确列号。

### C3 GaussDB 专有语法 AST —— **P1（阻塞面已从 18 条降到 1 条）**

- **现状**：`grep create_database` / 表级 `PartitionBy` / `DistributeBy` → 0 命中。
  实测确认两种失败模式：`DISTRIBUTE BY` 被**静默丢弃**（语句仍是 `CREATE_TABLE`）；`UNLOGGED` / `LOCK TABLE` **整体解析失败**。
- **仍被硬阻塞**：`G-TBL-01`（混用存储引擎）——文本兜底 + 同文件只能覆盖部分场景。
- **建议**（沿用已验证的 `gaussdb_rewrite.rs` 词法归一化 + 新增 kind 思路，按 kind 分批交付）：
  1. `CreateInfo` 补 `distribute_by() -> {strategy, columns}`、`partition_by_text()`、`partition_count()`、`has_maxvalue()`、`is_temporary()`、`on_commit_delete_rows()`、`is_unlogged()`、`storage_type()`
  2. 新增 `CreateDatabaseInfo{encoding, dbcompatibility, lc_ctype, lc_collate, template}`
  3. 新增 kind：`LOCK_TABLE`、`CREATE_SEQUENCE`、`CREATE_TRIGGER`、`CREATE_PROCEDURE`
- **收益**：把 `G-DIST-01/03/04`、`G-TBL-03/04/05/06`、`G-DDL-03/04`、`G-DCL-01`、`G-OBJ-08` 等 11 条从 B 升到 A，**显著降低误报**（尤其 `G-TBL-04` 的分区计数、`G-TBL-03` 的逐分区上边界）。

### C6 类型系统内置映射 —— **P1**

- **现状**：`ColumnInfo.data_type()` 只返回原始文本。
- **阻塞条目**：`G-TYP-01`（唯一硬阻塞）；另有 `G-TYP-02/04`、`G-IDX-04`、`G-PERF-08`、`G-DIST-04` 的**精度**受它影响。
- **建议**：`is_integer_type(t)`、`integer_type_range(t) -> (min,max)`、`is_large_field_type(t, n)`、`type_byte_width(t)`、`is_recommended_type(t)`；数据源做成 **可版本化数据文件**（内置 TOML + 允许用户覆盖），而非硬编码。
- **收益**：5 条从 B 升 A（行宽/键长从"估算"变"精确"），1 条 C 转 A。

### C7 元数据 / 表结构查询 —— **P1（影响面最大）**

- **现状**：`grep metadata|schema_info|catalog` → 0 命中，规则只见单文件 AST。
- **阻塞条目**：`G-DML-15`（唯一硬阻塞）。
- **但它决定 11 条 B 类"仅同文件"规则能否在真实工程可用**：`G-OBJ-03`、`G-OBJ-04`、`G-DML-07`、
  `G-IDX-03`、`G-IDX-04`、`G-DIST-04`、`G-DIST-05`、`G-DIST-06`、`G-TBL-01`、`G-DDL-01`、`G-PERF-08`。
  真实项目里 DDL 常拆在多文件，`CREATE TABLE` 与 `CREATE INDEX`/`UPDATE` 往往不同文件。
- **建议**：`sqlguard.toml` 新增 `[metadata]`（来源三选一 `ddl_dirs` / `catalog_json` / `dsn`）；
  内置只读函数 `table_exists` / `table_columns` / `table_primary_key` / `table_unique_keys` /
  `table_indexes` / `table_is_partitioned` / `table_distribution` / `table_row_width_bytes`。
- **建议起点**：先做 `ddl_dirs`（复用现有 parser 扫 DDL 目录建索引）。

### C9 跨文件 / 项目级聚合上下文 —— **P1（架构级）**

- **现状**：`run_rules_for_file` 逐文件执行，无项目级视图。
- **阻塞条目**：`G-DB-02`、`G-TBL-01`（2 条硬阻塞）；另有 `G-PERF-09`（本就在 D 类）。
- **建议**：`run_scope = "file" | "project"`；`project` 模式先聚合各文件轻量摘要
  （对象名、建表选项、分布策略、语句计数）供 `context["project"]` 查询，再执行项目级规则；
  与 `cache::compute_run_signature` 联动，须在文件集合变化时整体失效。

### C10 规则组合与短路逻辑 —— **P2（优先级上升）**

- **为什么优先级上升**：现在 A+B 有 61 条待写。若一条 `.rhai` 只能对应一个规则 id，
  就会出现 61 个脚本 + 61 段 `[[rules]]`；而 GaussDB 命名四联、`UPDATE`/`DELETE` 子句禁用等
  天然属于"一类规则多条检查"。
- **现状问题**：
  - 一条 `.rhai` ↔ 一条 `[[rules]]`，`severity`/`rule_name` 由 TOML 固定 —— **脚本无法上报多条不同 id/severity 的 violation**
  - `set_max_modules(0)` **完全禁用 import** —— 公共库只能靠单文件 `helpers.rhai` prepend
  - 无规则依赖/优先级；"解析失败短路"要靠每条脚本自己写 `guard_parse_error(context)`
  - 豁免仅行级，无语句级/文件级/baseline
- **建议**：① violation Map 支持可选 `rule_id` / `severity` 覆盖；② 允许白名单目录 import；
  ③ 规则声明 `depends_on` / `run_if`，引擎在 AST 解析失败时自动短路非 parse 类规则；
  ④ 豁免粒度扩展 `sqlguard-disable-file` 与 `--baseline <file>`。

### C11 AST 遍历基础设施 —— **P2**

- **建议**：`SqlAst.walk_selects() -> Array<SelectInfo>`（扁平化全部嵌套层级）、`SelectInfo.depth()` /
  `parent_id()`、`JoinInfo.line/column` 与 `ColumnInfo.line/column` 位置回填（当前恒 0）、
  `StmtInfo.kind` 补齐 `EXPLAIN` / `LOCK` / `CREATE_SEQUENCE`。配合 `sqlguard explain` 同步暴露新字段。
- **收益**：降低规则脚本里重复写递归遍历的成本（`no_order_by_in_subquery` 已经自己写了一个）。

### C12 运行动态信息 —— **P3 / 明确不做**

见 §6.4。

---

## 5. 剩余扩展优先级

| 优先级 | 缺口 | 现在能解锁/改善的条目 | 工作量 | 依赖 |
|---|---|---|---|---|
| **P1-1** | C7 元数据/表结构查询 | 1 条 C→A + **11 条 B 从"仅同文件"变成真可用** | 大 | 建议先做 `ddl_dirs` |
| **P1-2** | C3 GaussDB 专有语法 AST（分批） | 1 条 C→A + 11 条 B→A（降误报） | 大 | 复用 `gaussdb_rewrite` |
| **P1-3** | C6 类型系统映射 | 1 条 C→A + 5 条 B→A（精度） | 小~中 | — |
| **P1-4** | C9 项目级聚合上下文 | 2 条 C→A | 大 | C7（复用元数据构建） |
| **P2-1** | C10 规则组合与短路 | 显著降低 61 条规则的**组织成本** | 中 | — |
| **P2-2** | C11 AST 遍历基础设施 | 提升开发效率与定位精度 | 中 | — |
| **P2-3** | C1 表达式递归 | `G-DML-03/04/05/11/12/14/16`（7 条）B→A —— 核心收益是**降误报**（区分"真在结构里"与"只在字符串/注释里"），而非解锁新规则 | 中 | — |

**建议里程碑**

| 里程碑 | 内容 | 交付效果 |
|---|---|---|
| M1 ✅ **已完成** | C2 + C4 + C5 + C8 | 立即可写的规则从 9 条 → **61 条** |
| **M2 ✅ 已完成** | 写 A 类规则 | 新增 18 个规则脚本 + 4 条复用既有规则，A 类全部落地（§9） |
| **M2.5（下一步，建议）** | C1 表达式递归 + C10 规则组合 | 7 条 B→A（降误报）+ 降低后续 39 条 B 类规则的组织成本 |
| M3 | C1 + C10 | B 类 7 条升 A（降误报）+ 规则组织成本下降 |
| M4 | C3（分批） + C6 | 1 条解锁 + 16 条降误报 |
| M5 | C7 + C9 | 11 条"仅同文件"规则在拆文件的项目里真正可用；2 条解锁 |

> **优先级排序上的主张**：M1 之后不建议立刻再动引擎，**建议先写 A 类 22 条规则**。
> 理由：A 类零误报风险、不受后续引擎改动影响、且能立刻拿真实 DDL 跑出反馈；
> 反过来，C10（规则组合）如果等 61 个脚本都写完再改，返工量最大 ——
> 所以**写 A 类规则之前先评估 C10 是否要做**，这是唯一有顺序依赖的决策。

---

## 6. Rhai 可实现性判定（M1 后）

### 6.1 A 类：现在就能实现，AST 精确（22 条，✅ 2026-09-28 已全部落地）

`G-NAM-01/02/03/04/05`、`G-TYP-02/03/04`、`G-OBJ-01/02/09`、`G-IDX-01/05`、`G-DCL-02`、
`G-DDL-02`、`G-DML-01/08/09/10/17/19`、`G-PERF-07`

**共同特征**：判定所需的全部信息都已是结构化字段（或脚本内自带常量表即可判定），
不受字符串/注释/括号干扰。**建议全部先给 `error`（G-NAM-05、G-TYP-02/04、G-PERF-07、G-IDX-01 可考虑 `warning`）**。

> **实现状态标注（2026-09-28）**：A 类 22 条已全部落地。其中 18 条为新增脚本、4 条复用既有规则。
> 以下为「规范条目 → 规则 ID → 严重度 → 落地方式」映射；脚本与声明明细见 §9。

| 规范条目 | 规则 ID | 严重度 | 落地方式 |
|---|---|---|---|
| G-NAM-01 | `GNAM001` | error | 新增脚本 |
| G-NAM-02 | `GNAM002` | error | 新增脚本 |
| G-NAM-03 | `GNAM003` | error | 新增脚本 |
| G-NAM-04 | `GNAM004` | error | 新增脚本（`params.name_max_bytes=63`） |
| G-NAM-05 | `DDL003`/`DDL004`/`DDL005`/`DDL007` | error | 复用既有规则（命名规范） |
| G-TYP-02 | `GTYP001` | warning | 新增脚本（`params.max_large_fields=8`/`large_field_bytes=2000`） |
| G-TYP-03 | `GTYP002` | error | 新增脚本 |
| G-TYP-04 | `GTYP003` | warning | 新增脚本 |
| G-OBJ-01 | `GOBJ001` | error | 新增脚本 |
| G-OBJ-02 | `GOBJ002` | error | 新增脚本 |
| G-OBJ-09 | `GOBJ003` | warning | 新增脚本 |
| G-IDX-01 | `GIDX001` | error | 新增脚本 |
| G-IDX-05 | `DDL006` | error | 复用既有规则（冗余索引） |
| G-DCL-02 | `GDCL001` | error | 新增脚本 |
| G-DDL-02 | `GDDL001` | error | 新增脚本 |
| G-DML-01 | `DML110` | warning | 复用并改造（读 `params.max_join_tables`，行号修正） |
| G-DML-08 | `GDML002` | error | 新增脚本 |
| G-DML-09 | `GDML003` | error | 新增脚本 |
| G-DML-10 | `GDML004` | warning | 新增脚本 |
| G-DML-17 | `GDML005` | error | 新增脚本 |
| G-DML-19 | `DML111` | warning | 复用既有规则（OR 改写） |
| G-PERF-07 | `GPERF001` | warning | 新增脚本（`params.stmt_max_bytes=5120`） |

> 全部新增脚本已在 `sqlguard.rules.toml.example` 声明、**默认 `enabled = false`**；非 GaussDB 项目不受影响。
> `G-DML-01` 的 `DML110` 本轮改为读 `params.max_join_tables`（GaussDB 联机 3 / 批量 5），默认仍为 5，行为不变。

### 6.2 B 类：现在就能实现，但有明确局限（39 条）

三种受限形态，写脚本时必须**在规则文档里写明局限并给出豁免写法**：

| 受限形态 | 条目 | 局限 | 缓解 |
|---|---|---|---|
| **文本兜底**（读 `ast.stmt_text`） | `G-DB-01/03/04`、`G-TBL-03/04/05/06`、`G-OBJ-05/06/07/08`、`G-IDX-02`、`G-DCL-01`、`G-DDL-03/04`、`G-DML-02/03/04/05/06/11/12/14/16/18`、`G-PERF-05`、`G-DIST-01/02/03/04/06` | 字符串字面量 / 注释中的同名片段会误报 | 行内豁免 `sqlguard-disable-next-line`；阈值走 params；**统一建议 `warning`** |
| **仅同文件**（需同文件其它语句） | `G-OBJ-03/04`、`G-DML-07`、`G-IDX-03/04`、`G-DDL-01`、`G-DIST-05`、`G-PERF-08` | 拆文件项目里拿不到建表信息 → **漏报** | 等 C7；或要求 DDL 集中存放（写入项目规范） |
| **启发式**（需人定阈值/比例） | `G-DML-02`（复杂度）、`G-DML-12`（聚合函数名表）、`G-DML-16`（函数 volatility 表）、`G-TBL-03`（逐分区上边界） | 与条文语义有差距 | 阈值走 params；在 `docs/sql-guidelines/` 写明判据 |

### 6.3 阈值一律走 `context["params"]`（C8）

源文档自身就存在冲突（DATABASE 数量 10 vs 3），且附录最佳实践阈值本身就是"建议值"。
因此这类规则**不要硬编码**：

```toml
[[rules]]
id = "GNAM004"
name = "object_name_max_bytes"
# ...
[rules.params]
name_max_bytes = 63
```
```rhai
let p = context["params"];
let limit = 63;                                  // 脚本内默认值，配置缺失时生效
if "name_max_bytes" in p { limit = p["name_max_bytes"]; }
```

适用：`name_max_bytes`（G-NAM-04）、`max_join_tables` / `max_join_tables_batch`（G-DML-01）、
`max_partitions`（G-TBL-04）、`max_in_values`（G-DML-14）、`stmt_max_bytes`（G-PERF-07）、
`max_columns` / `max_row_bytes`（G-PERF-08）、`max_indexes` / `max_dist_keys`（G-IDX-04 / G-DIST-03）。

### 6.4 D 类：不适合用 Rhai 实现（19 条）—— 五类根因

| 根因 | 条目 | 替代方案 |
|---|---|---|
| **R1 运行期数据量/时长** | G-TXN-01/02/03、G-DML-13、G-PERF-02/03 | DBA 巡检 SQL（`pg_stat_*` / `pg_stat_activity` / 慢日志），或接入运行期监控 |
| **R2 并发与时序** | G-PERF-04、G-DDL-05/06、G-DCL-04 | 变更窗口流程 + 操作审计 |
| **R3 执行计划语义** | G-PERF-01、G-DIST-07/08 | 独立 EXPLAIN 分析器；文档已给出倾斜检查 SQL，适合做成"巡检 SQL 包" |
| **R4 客户端/框架行为** | G-DML-20、G-TBL-07 | 框架层（JDBC 封装）审查清单；Mapper 模式下可做"是否存在循环单条 INSERT"的弱提示 |
| **R5 架构决策/运行期配置** | G-DB-05、G-TBL-02、G-PERF-06、G-PERF-09 | 设计评审 checklist |

> 注：`src/explain.rs` 是 **AST 自省**（打印 `context` 可用字段），**不是** SQL `EXPLAIN` 分析器，两者不要混淆。

---

## 7. 落地建议（配置与 ID 方案）

建议为 GaussDB 规范单开规则命名空间，与现有 `DDL*/DML*` 隔离，避免 ID 冲突：

```toml
# sqlguard.rules.toml
[[rules]]
id = "GNAM001"
name = "object_name_charset"
group = "gaussdb-naming"
description = "对象名只能使用字母、数字和下划线"
script_path = "config/rules/gaussdb/naming/object_name_charset.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "GNAM004"
name = "object_name_max_bytes"
group = "gaussdb-naming"
description = "对象名（含 schema 叶子段）不得超过 63 字节"
script_path = "config/rules/gaussdb/naming/object_name_max_bytes.rhai"
applies_to = ["ddl"]
severity = "error"

[rules.params]
name_max_bytes = 63
```

- 目录按类别分：`config/rules/gaussdb/{naming,type,table,obj,index,dcl,ddl,dml,perf,dist}/`
- 文件命名与现有规则一致（snake_case = `name` 字段）
- **A 类规则用 `error`，B 类规则统一先用 `warning`**（避免文本兜底的误报阻断 CI）
- 借鉴 `docs/sql-guidelines/` 模式，为每条 B 类规则配一份 `docs/sql-guidelines/<rule>.md`，
  写明**判据 / 已知局限 / 豁免写法**；C3 落地后再把对应 B 类升级为精确判定
- 依赖 C3 的规则，建议在 `gaussdb_rewrite.rs` 的 `G_ORACLE_*` 规则表旁并列新增 `G_GAUSS_*` 归一化项，保持"可审计"特性

---

## 8. 真机探针验证记录（2026-09-28）

§2 的 A/B 类判定不是纸面推断，而是用一次性探针在真实引擎上跑出来的。
探针位于临时目录（不入库），构造如下：

```
C:/tmp/ga-probe/
├── sqlguard.toml            # dialect=gaussdb, dialect_fallback=oracle；strict=false
├── sqlguard.rules.toml      # 1 条规则 PROBE，params: name_max_bytes=63, max_join_tables=3
├── config/rules/probe.rhai  # 单脚本内做 ~25 项检查，逐项 push "PROBE:<检查名>" violation
└── sql/{ddl,dml}/*.sql      # 11 个夹具文件
```

运行方式与结果：

```bash
cd C:/tmp/ga-probe
sqlguard check . --format json      # 报告落在 ./sqlguard-report.json
# → total_files=11, total_violations=119, warnings=119, errors=0, directory_issues=0
# → stderr: Dialect fallback chain: gaussdb → oracle → generic
```

### 8.1 验证通过的 A 类检查（全部命中）

| 检查 | 结果 | 对应规则 |
|---|---|---|
| `quoted_ident="Quoted"` | ✅ | G-NAM-02 |
| `stripped_ident_ok=true` | ✅ | G-NAM-01 |
| `reserved_prefix=pg_legacy` | ✅ | G-NAM-03 |
| `name_too_long=74` | ✅ | G-NAM-04（`len_bytes` 返回字节数） |
| `reserved_word="order"` | ✅ | G-NAM-05 类 |
| `index_without_concurrently=idx_t_a` | ✅ | G-IDX-01 |
| `order_item=id\|\|`（未指定方向/NULLS） | ✅ | G-DML-17 三态 |
| `order_item=id\|DESC\|LAST` | ✅ | G-DML-17 取值正确 |
| `order_by_no_direction` / `order_by_no_nulls` | ✅ | G-DML-17 |
| `pagination_without_order_by` | ✅ | G-DML-17（LIMIT 无 ORDER BY） |
| `too_many_joins=5` | ✅ | G-DML-01 |
| `order_by_text` / `order_by_items` 可迭代 | ✅ | G-DML-17 |
| `update_set_columns=a,b` / `update_set_columns=id` | ✅ | G-DIST-05 前置 |
| `update_sets_id`（`sets_column("id")`） | ✅ | G-DIST-05 |
| `update_forbidden_clause` ×2 | ✅ | **G-DML-08**（`UPDATE ... ORDER BY` 与 `... GROUP BY`） |
| `delete_forbidden_clause` / `delete_where_subquery` | ✅ | G-DML-09 / G-DML-11 |
| `view_definition_order_by` | ✅ | G-OBJ-02 |
| `view_def_from=v_sorted` + `view_nested` | ✅ | G-OBJ-03（同文件视图集合） |
| `params_name_max_bytes=63` / `params_max_join_tables=3` / 不存在的键 → `false` | ✅ | C8 |
| `source_len=<各文件字节数>` | ✅ | C4 |

### 8.2 验证通过的 B 类检查（全部命中）

| 检查 | 结果 | 关键结论 |
|---|---|---|
| `text_on_commit_delete_rows` | ✅ | G-TBL-06 |
| `text_unlogged` | ✅ | G-OBJ-08；**该语句是 `PARSE_ERROR`，文本兜底依然命中** |
| `text_lock_table` | ✅ | G-DCL-01；同上，`PARSE_ERROR` 也命中 |
| `text_explain_analyze` | ✅ | G-DML-18；该语句 kind = `OTHER` |
| `text_concat` / `text_now` | ✅ | G-DML-03 / G-DML-04 |
| `text_distribute_by` | ✅ | G-DIST-01 |
| `dist_key_count=4`（从原文提取 `HASH(a, b, c, d)`） | ✅ | G-DIST-03；**证明结构化文本提取可行**（脚本内用 `index_of` + `sub_string` + `split`） |
| `text_too_long`（6314 字节语句） | ✅ | G-PERF-07；必须用 `len_bytes` 而非 `len()` |

### 8.3 本轮发现的两个"反直觉"事实（写规则时必须知道）

1. **`DISTRIBUTE BY` 被静默丢弃，不是解析失败。**
   `sqlguard explain sql/ddl/d3_dist.sql` 显示该语句是 `CREATE_TABLE`，表名/列都在，
   **但 `DISTRIBUTE BY HASH(a,b,c,d)` 完全不在 AST 里**。原因是"主方言未干净解析 → 回退链
   `parse_one_with` 接受残句"。**结论：AST 有 ≠ 子句在**。凡涉及 GaussDB 专有子句的规则，
   必须同时读 `stmt_text`，否则会静默漏报。
2. **`sqlguard.toml` 的 `dialect` 必须是根级键且位于任何 `[section]` 之前**，
   否则会被并入子表而**静默失效**（本次探针第一轮就踩了：`dialect="gaussdb"` 写在 `[structure]`
   下，结果 stderr 显示回退链是 `generic → generic`）。这条值得写进 `sqlguard.toml.example` 的醒目位置
   —— 现已在该文件顶部有说明，但探针实测说明"很容易踩"。

### 8.4 写 A 类规则时又踩到的四个 Rhai 坑（已全部修掉，写新规则时务必避开）

| 坑 | 现象 | 规避 |
|---|---|---|
| **`fn` 内访问外层 scope 变量** | `Script error: Variable not found: limit` / `Variable not found: violations` | Rhai 的 `fn` **不是闭包**。要么把值作为参数传入（见 `max_join_tables.rhai` 的 `exceeds_table_limit(sel, limit)`），要么让 `fn` 只做收集、上报统一放顶层（见 `gaussdb_order_by_explicit_sort.rhai` 的 `collect_selects`） |
| **本构建里 `String.trim()` 返回 unit** | `Function not found: len (())` | 不要用 `.trim()`，改用 `index_of` / `sub_string` 自行处理（见 `gaussdb_max_large_fields.rhai` 注释） |
| **内建 `parse_int` 遇非数字会抛错** | `Error parsing integer number 'abc': invalid digit found in string` —— 会打断整条规则 | 用 `helpers.rhai` 的 `parse_int_safe`，或先自行校验纯数字 |
| **Rhai 没有 `to_int()`** | `Function not found: to_int (&str \| ImmutableString \| String)` | 同上，用 `parse_int_safe` |

> 集成测试 `test_check_gaussdb_a_class_rules_hit_expected_ids` 里有一条断言专门拦这类问题：
> 任何 violation 的 message 含 `Rule execution error` / `Script error` 即测试失败。

---

## 9. A 类规则实现清单（2026-09-28 已落地）

### 9.1 新增脚本（18 条）

| 规范条目 | 规则 ID | 脚本 | 严重度 | applies_to | 可配参数 |
|---|---|---|---|---|---|
| G-NAM-01 | `GNAM001` | `config/rules/ddl/gaussdb_object_name_charset.rhai` | error | ddl | — |
| G-NAM-02 | `GNAM002` | `config/rules/ddl/gaussdb_no_quoted_object_name.rhai` | error | ddl | — |
| G-NAM-03 | `GNAM003` | `config/rules/ddl/gaussdb_no_reserved_prefix.rhai` | error | ddl | — |
| G-NAM-04 | `GNAM004` | `config/rules/ddl/gaussdb_object_name_max_bytes.rhai` | error | ddl | `name_max_bytes = 63` |
| G-TYP-02 | `GTYP001` | `config/rules/ddl/gaussdb_max_large_fields.rhai` | warning | ddl | `max_large_fields = 8`、`large_field_bytes = 2000` |
| G-TYP-03 | `GTYP002` | `config/rules/ddl/gaussdb_no_system_column.rhai` | error | ddl | — |
| G-TYP-04 | `GTYP003` | `config/rules/ddl/gaussdb_recommended_data_types.rhai` | warning | ddl | — |
| G-OBJ-01 | `GOBJ001` | `config/rules/ddl/gaussdb_no_materialized_view.rhai` | error | ddl | — |
| G-OBJ-02 | `GOBJ002` | `config/rules/ddl/gaussdb_no_order_by_in_view.rhai` | error | ddl | — |
| G-OBJ-09 | `GOBJ003` | `config/rules/ddl/gaussdb_view_usage_warning.rhai` | warning | ddl | — |
| G-IDX-01 | `GIDX001` | `config/rules/ddl/gaussdb_create_index_concurrently.rhai` | error | ddl | — |
| G-DCL-02 | `GDCL001` | `config/rules/ddl/gaussdb_no_quoted_column_in_ddl.rhai` | error | ddl | — |
| G-DDL-02 | `GDDL001` | `config/rules/ddl/gaussdb_require_commit_in_transaction.rhai` | error | ddl | — |
| G-DML-08 | `GDML002` | `config/rules/dml/gaussdb_no_order_by_group_by_in_update.rhai` | error | dml | — |
| G-DML-09 | `GDML003` | `config/rules/dml/gaussdb_no_order_by_group_by_in_delete.rhai` | error | dml | — |
| G-DML-10 | `GDML004` | `config/rules/dml/gaussdb_update_subquery_to_join.rhai` | warning | dml | — |
| G-DML-17 | `GDML005` | `config/rules/dml/gaussdb_order_by_explicit_sort.rhai` | error | dml | — |
| G-PERF-07 | `GPERF001` | `config/rules/dml/gaussdb_statement_max_bytes.rhai` | warning | ddl, dml | `stmt_max_bytes = 5120` |

### 9.2 复用既有规则（4 条，不重复造轮子）

| 规范条目 | 规则 ID | 既有脚本 | 本轮改动 |
|---|---|---|---|
| G-NAM-05 | `DDL003` / `DDL004` / `DDL005` / `DDL007` | `no_reserved_keyword_naming` / `backup_table_naming` / `index_naming_convention` / `table_name_naming` | 无 |
| G-IDX-05 | `DDL006` | `no_redundant_index.rhai` | 无 |
| G-DML-19 | `DML111` | `no_or_in_where.rhai` | 无 |
| G-DML-01 | `DML110` | `max_join_tables.rhai` | **改为读 `params.max_join_tables`**（默认仍为 5，行为不变）；GaussDB 项目可配 3（联机）/ 5（批量）。顺带把行号从硬编码 1 改为真实语句行号 |

### 9.3 注册与启用

- 18 条已在 `sqlguard.rules.toml.example` 中声明，**默认 `enabled = false`**（GaussDB 项目按需把对应条目改为 `true`）；非 GaussDB 项目不受影响。
- 对应脚本已加入 `src/main.rs` 的 `INIT_RULE_SCRIPTS`，因此 `sqlguard init` 会一并落盘（守护用例 `init_rule_scripts_cover_example_declarations` 要求 example 声明的每个 `script_path` 都能被 init 写出）。
- 脚本目录固定为 `config/rules/{ddl,dml}/`（`run_init` 按 `rule_type` 生成路径，暂不支持 `config/rules/gaussdb/` 子目录）。**§7 里建议的分组目录需要改 `run_init` 才能生效，本轮未改引擎。**
- 按类别筛选：`sqlguard check ./sql --groups gaussdb-*`；按单条：`--rules GNAM00*,GIDX001`。

### 9.4 验证

- `tests/integration_test.rs::test_check_gaussdb_a_class_rules_hit_expected_ids`：
  构造 ddl/dml 违规夹具各 1 份、合规夹具各 1 份，启用全部 18 条规则跑 `sqlguard check`，断言：
  ① 无任何 `Rule execution error` / `Script error`（脚本运行时错误护栏）；
  ② 18 个规则 ID **全部命中**；
  ③ 合规夹具 **零违规**（防误报）。
- 全量：`cargo test` → lib **638 passed / 0 failed**、integration **33 passed / 0 failed**、doc **10 passed / 0 failed**。

---

*本文档为规范 → 引擎能力的映射分析，可作为后续规则批量实现与引擎扩展排期的输入。
所有当前能力判断均已对照源码核验，A/B 类关键路径另行真机探针验证（§8）。*

---

## 附录 A. 全部规范条目实现状态清单（2026-09-28）

> 本附录汇总《GaussDB 开发技术实施策略》中可规则化的 **85 条**条目及其在 sql-guard 中的实现状态。
> 适配口径：**A** 现在可实现且 AST 精确、**B** 现在可实现但有局限（文本兜底/仅同文件/启发式）、
> **C** 仍需引擎扩展、**D** 不适合静态规则。A 类 22 条已于 2026-09-28 全部落地（§9）。

| 编号 | 类别 | 条目摘要 | 适配 | 规则 ID / 阻塞缺口 | 实现状态 |
|---|---|---|---|---|---|
| G-DB-01 | G-DB | 禁止 postgres 数据库，必须建业务 DATABASE | B | — | ⏳ 未实现（B 类） |
| G-DB-02 | G-DB | 单实例自定义 DATABASE ≤10 | C | C9 跨文件聚合 + 阈值冲突 | ⏳ 未实现（需引擎扩展） |
| G-DB-03 | G-DB | 必须使用默认表空间（仅 3 种例外） | B | 文本兜底局限：3 种例外无法自动识别 | ⏳ 未实现（B 类） |
| G-DB-04 | G-DB | 禁止使用 public schema | B | 文本兜底：public./search_path/CREATE SCHEMA public | ⏳ 未实现（B 类） |
| G-DB-05 | G-DB | 实例多库应使用 Database 隔离 | D | 架构决策，非 SQL 属性 | ❌ 不适合静态规则 |
| G-NAM-01 | G-NAM | 对象名仅字母/数字/下划线 | A | GNAM001 | ✅ 已落地 |
| G-NAM-02 | G-NAM | 禁止双引号字符串定义对象名 | A | GNAM002 | ✅ 已落地 |
| G-NAM-03 | G-NAM | 禁止 pg_/gs_/adm_/my_/db_ 前缀 | A | GNAM003 | ✅ 已落地 |
| G-NAM-04 | G-NAM | 对象名长度 ≤63 字节 | A | GNAM004 | ✅ 已落地 |
| G-NAM-05 | G-NAM | 表/索引/备份表命名约定 | A | DDL003/004/005/007 | ✅ 已落地 |
| G-TYP-01 | G-TYP | 按取值范围选整数类型 | C | C6 类型→值域映射 | ⏳ 未实现（需引擎扩展） |
| G-TYP-02 | G-TYP | 单表大字段（如 VARCHAR(1000)）宜 ≤8 个 | A | GTYP001 | ✅ 已落地 |
| G-TYP-03 | G-TYP | 禁止 CTID/CID/OID/XID/TID/XMIN/CMIN/XMAX/CMAX 作业务字段 | A | GTYP002 | ✅ 已落地 |
| G-TYP-04 | G-TYP | 优先使用附录推荐基础类型 | A | GTYP003 | ✅ 已落地 |
| G-TBL-01 | G-TBL | 禁止混用存储引擎（AStore/UStore） | C | C3 专有语法 AST + C9 跨文件 | ⏳ 未实现（需引擎扩展） |
| G-TBL-02 | G-TBL | 按业务场景选择存储引擎 | D | 需运行期更新频率 | ❌ 不适合静态规则 |
| G-TBL-03 | G-TBL | 集中式非自扩展分区须定义上边界 MAXVALUE | B | 文本兜底：PARTITION BY 缺 MAXVALUE | ⏳ 未实现（B 类） |
| G-TBL-04 | G-TBL | 单表分区/子分区个数禁止超过 1000 | B | 文本兜底：统计 PARTITION 次数（阈值走 params） | ⏳ 未实现（B 类） |
| G-TBL-05 | G-TBL | 字符集统一 UTF8/UTF8MB4 | B | 文本兜底：CREATE DATABASE ENCODING | ⏳ 未实现（B 类） |
| G-TBL-06 | G-TBL | 禁止事务级全局临时表 ON COMMIT DELETE ROWS | B | 文本兜底：原文含 ON COMMIT DELETE ROWS | ⏳ 未实现（B 类） |
| G-TBL-07 | G-TBL | 会话级 GTT 提交前显式 delete；AStore 事务后手动 vacuum | D | 运行期会话/事务行为 | ❌ 不适合静态规则 |
| G-OBJ-01 | G-OBJ | 禁止使用物化视图 | A | GOBJ001 | ✅ 已落地 |
| G-OBJ-02 | G-OBJ | 禁止在视图中进行排序操作 | A | GOBJ002 | ✅ 已落地 |
| G-OBJ-03 | G-OBJ | 禁止在视图中嵌套视图 | B | 仅同文件：视图名集合比对（跨文件需 C7） | ⏳ 未实现（B 类） |
| G-OBJ-04 | G-OBJ | 禁止对视图执行 SELECT 以外的 DML | B | 仅同文件：CREATE VIEW 名 + UPDATE/DELETE/INSERT 比对 | ⏳ 未实现（B 类） |
| G-OBJ-05 | G-OBJ | 联机交易禁止存储过程/自定义函数 | B | 文本兜底：CREATE PROCEDURE/FUNCTION（kind=OTHER） | ⏳ 未实现（B 类） |
| G-OBJ-06 | G-OBJ | 禁止 FENCED / NOT FENCED 参数 | B | 文本兜底：原文含 FENCED（需豁免） | ⏳ 未实现（B 类） |
| G-OBJ-07 | G-OBJ | 禁止触发器/事件/SEQUENCE/外键 | B | 外键走 A；触发器/事件/SEQUENCE 文本兜底 | ⏳ 未实现（B 类） |
| G-OBJ-08 | G-OBJ | 分布式禁止 UNLOGGED TABLE | B | 文本兜底：原文含 UNLOGGED（PARSE_ERROR 仍可读） | ⏳ 未实现（B 类） |
| G-OBJ-09 | G-OBJ | 不建议使用视图/嵌套视图 | A | GOBJ003 | ✅ 已落地 |
| G-IDX-01 | G-IDX | 有联机事务时建索引必须加 CONCURRENTLY | A | GIDX001 | ✅ 已落地 |
| G-IDX-02 | G-IDX | 不建议使用全局二级索引 | B | 文本兜底：GLOBAL INDEX（GaussDB 专有，无 AST） | ⏳ 未实现（B 类） |
| G-IDX-03 | G-IDX | 主键/唯一索引须包含分布键（分布式） | B | 仅同文件：提取 DISTRIBUTE BY 键列比对（跨文件需 C7） | ⏳ 未实现（B 类） |
| G-IDX-04 | G-IDX | 单表索引<5/复合索引<3/复合列<5/索引字段总长≤50 字节 | B | 复合列数精确；索引数/总长需同文件统计+估宽 | ⏳ 未实现（B 类） |
| G-IDX-05 | G-IDX | 冗余索引检测 | A | DDL006 | ✅ 已落地 |
| G-DCL-01 | G-DCL | 禁止使用 LOCK TABLE 加锁 | B | 文本兜底：原文含 LOCK TABLE（PARSE_ERROR 仍可读） | ⏳ 未实现（B 类） |
| G-DCL-02 | G-DCL | DDL 脚本字段名禁止引号/反引号 | A | GDCL001 | ✅ 已落地 |
| G-DCL-03 | G-DCL | 最小权限授权/OWNER 谨慎/权限分离 | C | C3 需 GRANT AST（grantee/privileges/对象） | ⏳ 未实现（需引擎扩展） |
| G-DCL-04 | G-DCL | 数据库初始用户不允许业务直接使用 | D | 运行期连接身份 | ❌ 不适合静态规则 |
| G-DDL-01 | G-DDL | 分区 DDL 后必须 UPDATE GLOBAL INDEX | B | 仅同文件：分区 DDL + 未见 UPDATE GLOBAL INDEX（跨文件需 C9） | ⏳ 未实现（B 类） |
| G-DDL-02 | G-DDL | 事务中需显式 COMMIT/ROLLBACK | A | GDDL001 | ✅ 已落地 |
| G-DDL-03 | G-DDL | CREATE DATABASE 须设 ENCODING='UTF8'+DBCOMPATIBILITY | B | 文本兜底：校验两选项 | ⏳ 未实现（B 类） |
| G-DDL-04 | G-DDL | 宜指定 LC_CTYPE/LC_COLLATE='c' | B | 文本兜底（同上） | ⏳ 未实现（B 类） |
| G-DDL-05 | G-DDL | 禁止并发对全局临时表做 DDL | D | 并发时序不可判定 | ❌ 不适合静态规则 |
| G-DDL-06 | G-DDL | 禁止业务高峰期执行 DDL | D | 时间窗不可判定 | ❌ 不适合静态规则 |
| G-DML-01 | G-DML | 关联表数禁止 >5（建议 ≤3 联机 / ≤5 批量） | A | DML110（读 params.max_join_tables） | ✅ 已落地 |
| G-DML-02 | G-DML | 避免复杂 SQL，建议拆小 SQL | B | 启发式：子查询数+表数+UNION+语句长度（建议 warning） | ⏳ 未实现（B 类） |
| G-DML-03 | G-DML | 用连接符 || 替换 concat() | B | 文本兜底：原文含 CONCAT(（需豁免） | ⏳ 未实现（B 类） |
| G-DML-04 | G-DML | 用 CURRENT_DATE/TIME/TIMESTAMP 代替 now() | B | 文本兜底：原文含 NOW(（同规则顺带检出 SYSDATE） | ⏳ 未实现（B 类） |
| G-DML-05 | G-DML | 禁止业务 SQL 用系统列 CTID/CID/OID… | B | 文本兜底+ident_leaf 词边界（需豁免） | ⏳ 未实现（B 类） |
| G-DML-06 | G-DML | 禁止使用 ROWID | B | 文本兜底（误报风险最高，必须配豁免） | ⏳ 未实现（B 类） |
| G-DML-07 | G-DML | INSERT ON DUPLICATE KEY UPDATE 禁改主键/唯一列 | B | 仅同文件：解析 SET 列比对主键/唯一列（跨文件需 C7） | ⏳ 未实现（B 类） |
| G-DML-08 | G-DML | UPDATE 禁止 ORDER BY / GROUP BY | A | GDML002 | ✅ 已落地 |
| G-DML-09 | G-DML | DELETE 禁止 ORDER BY / GROUP BY | A | GDML003 | ✅ 已落地 |
| G-DML-10 | G-DML | UPDATE...WHERE 含子查询宜改 JOIN | A | GDML004 | ✅ 已落地 |
| G-DML-11 | G-DML | NOT IN 子查询宜用 NOT EXISTS | B | 顶层 IN_SUBQUERY 仅覆盖顶层；深层需 C1 | ⏳ 未实现（B 类） |
| G-DML-12 | G-DML | 禁止 IN 子查询含聚合函数 | B | 启发式：子查询 projection 以 COUNT(/SUM( 开头 | ⏳ 未实现（B 类） |
| G-DML-13 | G-DML | 禁止 IN 子查询结果集 >1000 行 | D | 结果集规模静态不可判定 | ❌ 不适合静态规则 |
| G-DML-14 | G-DML | IN 列表 >200 改 in(values(...)) | B | 顶层 IN_LIST 后计数；组合谓词需 C1 | ⏳ 未实现（B 类） |
| G-DML-15 | G-DML | exists 子查询大表宜加 /*+ no_expand*/ | C | C7 大表元数据 + 原文检出 hint | ⏳ 未实现（需引擎扩展） |
| G-DML-16 | G-DML | LIKE 须用 immutable 函数 | B | 启发式：维护 stable/volatile 函数表匹配 | ⏳ 未实现（B 类） |
| G-DML-17 | G-DML | ORDER BY 须显式 ASC/DESC 与 NULLS FIRST/LAST | A | GDML005 | ✅ 已落地 |
| G-DML-18 | G-DML | 禁止生产环境对写 query 执行 explain analyze | B | 文本兜底：原文含 EXPLAIN ANALYZE（生产/非生产需分流） | ⏳ 未实现（B 类） |
| G-DML-19 | G-DML | WHERE/ON 含 OR 表达式须评估转 UNION ALL | A | DML111 | ✅ 已落地 |
| G-DML-20 | G-DML | 批量插入建议 executeBatch | D | 客户端行为，非 SQL 属性 | ❌ 不适合静态规则 |
| G-TXN-01 | G-TXN | 联机事务 ≤1000/批量 ≤10000/单次 ≤1000 | D | 运行期数据量 | ❌ 不适合静态规则 |
| G-TXN-02 | G-TXN | 批量单事务须 30 分钟内完成 | D | 运行期时长 | ❌ 不适合静态规则 |
| G-TXN-03 | G-TXN | 子事务禁止 >10W、宜 ≤1000 | D | 运行期；静态只数 SAVEPOINT 字面 | ❌ 不适合静态规则 |
| G-PERF-01 | G-PERF | 所有 SQL 须查执行计划（尤其变更后） | D | 流程 + 运行期 | ❌ 不适合静态规则 |
| G-PERF-02 | G-PERF | 变更>10% 或迁移后须 analyze | D | 运行期 | ❌ 不适合静态规则 |
| G-PERF-03 | G-PERF | analyze 须业务低峰期执行 | D | 运行期资源水位 | ❌ 不适合静态规则 |
| G-PERF-04 | G-PERF | 禁止多会话同时 vacuum 同表 | D | 并发时序 | ❌ 不适合静态规则 |
| G-PERF-05 | G-PERF | 禁止同时多个/高峰期 vacuum full | B | 同文件检测多条 VACUUM FULL（高峰期不可判） | ⏳ 未实现（B 类） |
| G-PERF-06 | G-PERF | 按需调整 autovacuum_vacuum_cost_delay | D | 配置项 | ❌ 不适合静态规则 |
| G-PERF-07 | G-PERF | SQL 语句最佳长度 <5KB | A | GPERF001 | ✅ 已落地 |
| G-PERF-08 | G-PERF | 单表字段<50/单行行宽<2KB | B | 列数精确；行宽需类型文本估宽 | ⏳ 未实现（B 类） |
| G-PERF-09 | G-PERF | 表+索引<10000/分区<5000w/DATABASE≤3 | D | 跨文件聚合 + 运行期 | ❌ 不适合静态规则 |
| G-DIST-01 | G-DIST | 必须指定表分布 DISTRIBUTE BY | B | 文本兜底：CREATE TABLE 缺 DISTRIBUTE BY（须读原文） | ⏳ 未实现（B 类） |
| G-DIST-02 | G-DIST | 小表采用 REPLICATION 分布 | B | 文本兜底：DISTRIBUTE BY REPLICATION（小表需元数据近似） | ⏳ 未实现（B 类） |
| G-DIST-03 | G-DIST | 分布键不建议超过 3 列 | B | 原文提取 DISTRIBUTE BY HASH(...) 列数 | ⏳ 未实现（B 类） |
| G-DIST-04 | G-DIST | 分布键列总长度不超过 128 | B | 需分布键列 + 同文件类型文本估宽 | ⏳ 未实现（B 类） |
| G-DIST-05 | G-DIST | 分布键值禁止更新（UPDATE） | B | 仅同文件：分布键列 + sets_column（跨文件需 C7） | ⏳ 未实现（B 类） |
| G-DIST-06 | G-DIST | WHERE 应包含所有分布键等值条件 | B | 启发式：WHERE 出现该列 ≠ 单 DN 计划 | ⏳ 未实现（B 类） |
| G-DIST-07 | G-DIST | 减少跨节点执行/数据重分布/算子下推 | D | 执行计划级语义 | ❌ 不适合静态规则 |
| G-DIST-08 | G-DIST | Hash 分布做数据倾斜检查（5%/10%） | D | 运行期 SQL 查询（xc_node_id 统计） | ❌ 不适合静态规则 |

### A.1 实现状态汇总

| 适配 | 条目数 | 实现状态 |
|---|---|---|
| A | 22 | ✅ 已全部落地（18 新增脚本 + 4 复用既有规则，详见 §9） |
| B | 39 | ⏳ 未实现：文本兜底/仅同文件/启发式，待 M3+ 引擎扩展（C1/C3/C7）后写脚本 |
| C | 5 | ⏳ 未实现：需引擎扩展（C3/C6/C7/C9） |
| D | 19 | ❌ 不适合静态规则：运行期/架构/并发/计划级信息 |
| **合计** | **85** | A 类 **22** 条已落地，其余 **63** 条待后续里程碑 |

### A.2 A 类 22 条落地明细（规则 ID ↔ 规范条目）

| 规范条目 | 规则 ID | 严重度 | 落地方式 |
|---|---|---|---|
| G-NAM-01 | `GNAM001` | error | 新增脚本 |
| G-NAM-02 | `GNAM002` | error | 新增脚本 |
| G-NAM-03 | `GNAM003` | error | 新增脚本 |
| G-NAM-04 | `GNAM004` | error | 新增脚本（params.name_max_bytes=63） |
| G-NAM-05 | `DDL003/004/005/007` | error | 复用既有规则（命名规范） |
| G-TYP-02 | `GTYP001` | warning | 新增脚本（params.max_large_fields=8/large_field_bytes=2000） |
| G-TYP-03 | `GTYP002` | error | 新增脚本 |
| G-TYP-04 | `GTYP003` | warning | 新增脚本 |
| G-OBJ-01 | `GOBJ001` | error | 新增脚本 |
| G-OBJ-02 | `GOBJ002` | error | 新增脚本 |
| G-OBJ-09 | `GOBJ003` | warning | 新增脚本 |
| G-IDX-01 | `GIDX001` | error | 新增脚本 |
| G-IDX-05 | `DDL006` | error | 复用既有规则（冗余索引） |
| G-DCL-02 | `GDCL001` | error | 新增脚本 |
| G-DDL-02 | `GDDL001` | error | 新增脚本 |
| G-DML-01 | `DML110` | warning | 复用并改造（读 params.max_join_tables） |
| G-DML-08 | `GDML002` | error | 新增脚本 |
| G-DML-09 | `GDML003` | error | 新增脚本 |
| G-DML-10 | `GDML004` | warning | 新增脚本 |
| G-DML-17 | `GDML005` | error | 新增脚本 |
| G-DML-19 | `DML111` | warning | 复用既有规则（OR 改写） |
| G-PERF-07 | `GPERF001` | warning | 新增脚本（params.stmt_max_bytes=5120） |

> 全部新增脚本已在 `sqlguard.rules.toml.example` 声明、**默认 `enabled = false`**；非 GaussDB 项目不受影响。
> 启用方式：`sqlguard check ./sql --groups gaussdb-*`（按类别）或 `--rules GNAM00*,GIDX001`（按单条）。
