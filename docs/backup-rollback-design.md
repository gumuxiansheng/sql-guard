# DDL/DML 备份与回滚自动生成 — 需求说明与技术方案

> 状态：**已实现（M1-M5 全部完成）**
> 日期：2026-07-26
> 作者：SqlGuard Team
> 关联模块：`src/rollback/`、`src/rule/engine/`、`src/mapper/`、`src/cli.rs`、`src/config.rs`
> 支持方言：MySQL 8.0+ / PostgreSQL 12+

---

## 实现状态

| 里程碑 | 内容 | 状态 | 提交 |
|--------|------|------|------|
| **M1** | 骨架 + 方言层：`mod/generator/dialect/naming/manifest/pk/render` 模块骨架，`DialectRenderer` trait 封装 MySQL/PG 差异，`NamingAllocator` 生成 `bks_<table>_<YYYYMMDD>_<NNNN>`，`Manifest` 序列化 | ✅ 完成 | `ce9546e` |
| **M2** | DML 核心：`dml.rs` 实现 INSERT/UPDATE/DELETE/TRUNCATE/REPLACE，增量备份 + JOIN 回滚 + PK 定位 | ✅ 完成 | `ce9546e` |
| **M3** | DDL 核心：`ddl.rs` 补全 ALTER 反向操作（RENAME COLUMN/TABLE、ADD_CONSTRAINT 按类型分支） | ✅ 完成 | `2feb132` |
| **M4** | ddl_like 推断：`expected_schema.columns` / `has_auto_increment_or_serial` / partition check warning | ✅ 完成 | `9d7101d` |
| **M5** | 校验与防护：`render_backup/rollback/cleanup` 完整实现 + coalesce + 长事务预检查 + R2 预检 + 端到端集成测试 | ✅ 完成 | `b10807e` |

**测试覆盖**：125 个 `rollback` 单元测试全过（含 2 个端到端集成测试，覆盖 `generate → render` 完整生命周期、双方言、LIFO、coalesce、manifest）。

**关键契约落地**：F9 幂等 / F12 原子 RENAME / F13 schema 校验 / F14 段内锁 / F15 分区检测 / F16 binlog 控制 / F18 长事务预检查（含 processlist，N10）/ D1 lock_scope auto 决策 / D6 coalesce / R2 table 锁风险确认。

---

## 一、背景与目标

SqlGuard 当前定位是 SQL **静态检查**工具：解析 DDL/DML → 应用 Rhai 规则 → 输出违规报告。在 CI/CD 流水线中，检查通过后变更仍会直接执行，缺少配套的**回滚脚本**。一旦变更引发线上故障，DBA 需要手工编写回滚 SQL，既慢又易错。

**本次需求**：在静态检查基础上，复用已有的 SQL 解析与 AST 包装能力，对每条 DDL/DML 语句自动生成：

1. **备份语句（backup）**：变更前执行的 SQL，把将要被修改/删除的**数据**或**结构定义**保存到备份表。
2. **回滚语句（rollback）**：变更失败时执行的 SQL，把数据库恢复到变更前状态。

最终交付一份与原变更脚本**一一对应、按依赖顺序排列**的 `backup.sql` + `rollback.sql`，可直接交给 DBA / 发布平台使用。**支持 MySQL 与 PostgreSQL 两种方言**，通过配置或 CLI 参数切换，生成对应方言的可执行 SQL。

### 与现有能力的关系

| 现有能力 | 复用方式 |
|----------|----------|
| `parse_sql_to_ast` 多语句切分 | 用于按语句粒度生成 backup/rollback |
| `StmtInfo.kind`（CREATE_TABLE / ALTER_TABLE / DROP_TABLE / INSERT / UPDATE / DELETE / TRUNCATE …） | 决定走哪个生成分支 |
| `CreateInfo`（含 columns、primary_key_columns、foreign_keys、indexes、uniques、checks） | 反向生成 DROP TABLE / 还原 CREATE TABLE |
| `AlterTableInfo.operations`（ADD_COLUMN / DROP_COLUMN / RENAME / ADD_CONSTRAINT …） | 反向生成 ALTER 操作 |
| `InsertInfo` / `UpdateInfo` / `DeleteInfo`（含 where_clause） | 反向生成 DELETE / UPDATE / INSERT |
| `mapper::extract_sql_from_xml` | Mapper 模式同样支持生成 |
| `bks_` 备份表前缀约定（DDL004） | 复用命名规范，避免与既有规则冲突 |
| `Violation` 报告通道 | 生成过程中遇到不可逆/不支持场景，通过 warning 上报 |

---

## 二、术语

| 术语 | 含义 |
|------|------|
| **变更语句** | 用户输入的 DDL/DML，对应一个 `StmtInfo` |
| **备份语句** | 变更**前**执行的 SQL，保存即将变更的数据或结构 |
| **回滚语句** | 变更**失败后**执行的 SQL，撤销变更并恢复数据 |
| **可逆变更** | 存在确定性回滚路径的变更（如 INSERT → DELETE） |
| **不可逆变更** | 无足够信息生成回滚的变更（如 DROP TABLE 无原表定义） |
| **bks_ 表** | 备份用的物理表，命名 `bks_<原表>_<序号>`，与 DDL004 一致 |
| **主键元组** | 用于行级定位的主键列集合，回滚时按主键匹配 |

---

## 三、需求说明

### 3.1 用户故事

> 作为 DBA，在执行 DDL/DML 发布脚本前，我希望工具能自动产出与之一一对应的 backup.sql 和 rollback.sql，让我在故障时能快速回滚，而不需要手写。

> 作为 SRE，我希望 CI 流水线在合入 SQL 变更时同时产出 backup/rollback，归档到发布单上，作为发布附件。

### 3.2 功能需求

#### F1：输入与输出

- **输入**：与 `check` 一致的目标目录 + 配置文件，复用 `[scan]` / `[mapper]` / `[classification]` 配置。
- **输出**：在指定 `output_dir` 下生成三份文件：
  - `backup.sql` — 备份语句集合（变更前执行）
  - `rollback.sql` — 回滚语句集合（变更失败后执行，顺序与 backup 相反）
  - `rollback-manifest.json` — 元数据清单，记录每条变更语句与对应 backup/rollback 的映射，供发布平台索引

#### F2：覆盖范围

按 SQL 语句类型分如下矩阵（★=完全支持，✗=不支持，输出 warning）：

> **核心策略**：
> - **DML 默认增量备份**：UPDATE/DELETE 按 WHERE 子句只备份受影响行，避免大表全表备份开销。
> - **ALTER DROP/MODIFY COLUMN 默认全表备份**：纯静态下 AST 不含被改列的旧类型（语句本身只有新类型或列名），无法可靠生成增量回滚；仅当脚本内同表 CREATE TABLE 上下文可推断列类型且主键已知时，才升级为增量模式（见 F11）。
> - **DDL 全表备份兜底**：DROP TABLE/TRUNCATE/DROP INDEX 等场景使用 `CREATE TABLE bks_xxx LIKE original` 克隆完整表结构 + 全表数据，回滚时按 F12 原子 RENAME 切换（MySQL）/ 事务内 DROP+CREATE LIKE+INSERT（PG）。
> - **无需连接数据库、无需用户手工补原表定义**。★ C3 修正：F13/F15 会生成 `information_schema` 只读 SELECT 校验语句用于执行期 schema 漂移检测，由发布平台在执行期解析结果后决策；这些校验语句不参与 backup/rollback 主体生成逻辑，主体仍是 `CREATE TABLE LIKE` + `INSERT SELECT`，不依赖 information_schema 拼接。

| 语句 | 备份 | 回滚 | 说明 |
|------|------|------|------|
| **图例**：★=可生成完整 backup+rollback（具体策略见"备份/回滚"列与"说明"列，含全表 LIKE 兜底）；✗=不支持，输出 warning；—=无需备份 | | | |
| `CREATE TABLE` | — | `DROP TABLE IF EXISTS t` | 无数据需备份 |
| `CREATE TABLE AS SELECT` | — | `DROP TABLE IF EXISTS t` | 同上 |
| `DROP TABLE` | ★全表 | ★ | `CREATE TABLE bks_t LIKE t` + 全表 `INSERT SELECT`；回滚 `CREATE TABLE t LIKE bks_t` + `INSERT SELECT`。★ 原表已不存在，两种方言均走 `RebuildFromBackup` 策略（非原子，CREATE 失败则原表无法恢复，manifest 标 `irreversible_if_backup_missing: true`）；PG 在外层事务内执行，MySQL 直接执行 |
| `ALTER TABLE ADD COLUMN` | — | `ALTER TABLE t DROP COLUMN c` | 直接反向；**注意：列约束（NOT NULL/DEFAULT/COMMENT/列顺序）丢失**，复杂场景走全表 LIKE 路径（见 P1-10 修正） |
| `ALTER TABLE DROP COLUMN` | ★全表（默认） | ★ | 默认 `CREATE TABLE bks_t LIKE t` + 全表数据，回滚走 F12 原子切换；仅当脚本内 CREATE TABLE 上下文可推断列类型 + 主键已知时升级增量 |
| `ALTER TABLE MODIFY COLUMN` | ★全表（默认） | ★ | 同 DROP COLUMN 路径，旧类型静态不可知，默认全表 LIKE + 原子切换 |
| `ALTER TABLE RENAME COLUMN` | — | `ALTER TABLE t RENAME COLUMN new TO old` | 纯元数据 |
| `ALTER TABLE RENAME TABLE` | — | `RENAME TABLE new TO old` | 纯元数据 |
| `ALTER TABLE ADD INDEX/CONSTRAINT` | — | `DROP INDEX/CONSTRAINT` | 纯元数据 |
| `ALTER TABLE DROP INDEX/CONSTRAINT` | ★全表 | ★ | `CREATE TABLE bks_t LIKE t`（LIKE 保留索引定义）+ 全表数据；回滚 F12 原子切换 |
| `ALTER TABLE ADD PRIMARY KEY` | — | `ALTER TABLE t DROP PRIMARY KEY` | |
| `ALTER TABLE DROP PRIMARY KEY` | ★全表 | ★ | 同 DROP INDEX 路径，用 LIKE 保留原 PK 定义 |
| `TRUNCATE TABLE` | ★全表 | ★ | `CREATE TABLE bks_t LIKE t` + `INSERT SELECT *`；回滚 `TRUNCATE t` + `INSERT SELECT *`（整表清空，无法增量） |
| `CREATE INDEX` | — | `DROP INDEX IF EXISTS i ON t` | |
| `DROP INDEX` | ★全表 | ★ | `CREATE TABLE bks_t LIKE t`（含原索引）+ 全表数据；回滚 F12 原子切换 |
| `CREATE VIEW` | — | `DROP VIEW IF EXISTS v` | |
| `DROP VIEW` | ★ | ★ | `CREATE VIEW bks_v AS SELECT * FROM v`；回滚 `CREATE VIEW v AS SELECT * FROM bks_v`（视图无行级概念） |
| `RENAME TABLE` | — | `RENAME TABLE new TO old` | |
| `INSERT` | — | `DELETE FROM t WHERE pk = (...)` | 用主键定位新增行 |
| `UPDATE` | ★增量 | ★ | 按 WHERE 备份受影响行的旧值（含主键），回滚 UPDATE 反向或 DELETE+INSERT |
| `DELETE` | ★增量 | ★ | 按 WHERE 把待删行 INSERT 到 bks_ 表，回滚 INSERT SELECT |
| `REPLACE INTO` | ★增量（partial） | ★partial | REPLACE = 先 DELETE 冲突行后 INSERT 新行。静态可备份被覆盖旧行（按 VALUES 的 pk 列表 `WHERE pk IN (...)`）；**新增行 pk 运行时才知，无法静态生成 DELETE 新行回滚** → manifest 标 `partial: true` + warning "REPLACE rollback incomplete: cannot DELETE newly inserted rows statically"。v2 `--connect` 执行期捕获新行 pk 后完整回滚 |
| `MERGE` | ✗ | ✗ | 输出 warning，建议拆为 INSERT+UPDATE 后再生成 |
| 事务控制（COMMIT/ROLLBACK/SET/USE） | — | — | 跳过 |

#### F3：备份表命名

- 命名规则：`bks_<原表去 schema 后的名字>_<8位日期>_<4位序号>`，例如 `bks_users_20260731_0001`。
- **8 位日期**：当前日期 `YYYYMMDD`，便于按日归档与清理；同一天内可生成多次，序号保证唯一性。
- **4 位序号**：在同一份 backup.sql 内**全局递增**（1 起步，前补零），最大 9999；超过则报错退出。
- 与 DDL004（`backup_table_naming`）规则**兼容**：所有自动生成的备份表均以 `bks_` 开头，不会触发规则违规。

#### F4：主键识别

回滚 DML 必须按主键定位行，否则会误改其他行：

1. **优先级 1**：从 `StmtInfo.create_table` 的 `primary_key_columns` 读取（仅 CREATE TABLE 之后紧跟的 DML 适用，少见）。
2. **优先级 2**：通过配置项 `[[rollback.primary_keys]]` 显式声明（推荐生产用法）。
3. **优先级 3**：DDL 场景由于 `CREATE TABLE bks_t LIKE t` 已保留主键定义，备份表自带主键，DML 回滚可通过 `JOIN bks_t ON pk` 自动定位（无需显式声明）。
4. **无主键**：DML 回滚降级为基于 `WHERE <原 WHERE 子句>` 的批量操作，并在 manifest 中标记 `unreliable: true`，输出 warning。

#### F5：排序与依赖

- **backup.sql** 顺序与原变更脚本一致（前向）。
- **rollback.sql** 顺序与原变更脚本**相反**（后向，LIFO），保证回滚不破坏依赖。
- 每条 backup 与对应 rollback 通过 `-- @@SEQ: 00000001 @@` 注释配对。
- **事务包裹策略按方言分化**（★ MySQL DDL 隐式提交关键修正）：
  - **PostgreSQL**：DDL 可事务化，`backup.sql` / `rollback.sql` 头尾包裹 `BEGIN; … COMMIT;`，失败可整体回滚。
  - **MySQL**：`CREATE/DROP/ALTER/TRUNCATE TABLE` 触发**隐式提交**，外层事务对 DDL 无约束力。**MySQL 方言下不包裹事务**，DDL 回滚改用**原子 RENAME 切换**模式（见 F12），避免"DROP 原表后 CREATE 失败导致原表永久丢失"。DML 语句仍可包裹事务（DML 不隐式提交）。
  - 混合脚本（DDL+DML）：MySQL 下按语句类型决定是否在语句级加事务，整体不包外层事务；PG 下统一包裹。

#### F6：Mapper 模式

- 对 `*Mapper.xml` 中的 `<insert>` / `<update>` / `<delete>` 同样生成 backup/rollback。
- `<select>` 语句默认跳过（只读）；可配置 `rollback.include_select = true` 时为 SELECT FOR UPDATE 类语句生成解锁回滚（少见，默认关闭）。
- 动态 SQL（`<if>` / `<foreach>`）展开后的**每个变体**单独生成一份 backup/rollback，与 `replay-export` 一致。
- 行号通过 `raw_xml_line` 映射回原 XML。

#### F7：增量模式（gen-rollback-diff）

- 提供 `gen-rollback-diff --base origin/main` 子命令，仅对 `git diff` 改动行范围内的语句生成 backup/rollback，复用 `git_diff.rs` 已有逻辑。

#### F8：配置项

```toml
[rollback]
# 总开关
enabled = true

# 方言：mysql / postgresql（影响标识符引用、CREATE TABLE LIKE 语法、
# DROP INDEX 语法、RENAME TABLE 语法、事务开启语句等）
# 默认 mysql
dialect = "mysql"

# 备份模式：auto（默认，DML 增量 + ALTER 按上下文判断）/ full（强制全表）/ incremental（强制增量，ALTER 缺上下文报错）
# - auto: DML 走增量；ALTER DROP/MODIFY COLUMN 仅当脚本内含该表 CREATE TABLE 上下文时升级增量，否则全表
# - full: 所有场景全表备份（兼容旧方案，大表慎用）
# - incremental: 强制增量，ALTER 缺 CREATE TABLE 上下文时报错退出
backup_mode = "auto"

# 输出文件名模板（可包含 {timestamp} 占位符）
backup_file = "backup.sql"
rollback_file = "rollback.sql"
manifest_file = "rollback-manifest.json"
cleanup_file = "cleanup.sql"  # 独立清理脚本，DBA 确认回滚成功后手工执行

# 是否在生成的脚本头尾自动包裹事务（★ 按方言分化，见 F5）
# - mysql: DDL 不包事务（隐式提交无效），DML 包语句级事务；整体不包外层事务
# - postgresql: 头尾包裹 BEGIN/COMMIT，DDL 可事务化
# 配置项仅对 PG 生效，MySQL 方言忽略此配置
wrap_transaction = true

# 备份表前缀（与 DDL004 保持一致，不建议修改）
backup_table_prefix = "bks_"

# 备份表日期段配置（默认 8 位日期 YYYYMMDD，与命名规则 bks_xxx_YYYYMMDD_NNNN 对齐）
backup_table_with_date = true
backup_table_date_format = "%Y%m%d"

# 是否在 rollback.sql 末尾自动 DROP 备份表（★ P1-11 默认改为 false）
# - false（默认）: 保留备份表与 _old_NNNN 影子表，便于事后审计；DBA 确认回滚成功后手工执行 cleanup.sql
# - true: rollback.sql 末尾追加 DROP 语句（高风险，回滚失败时无据可查）
cleanup_backup_tables_after_rollback = false

# 是否在 backup.sql 段头加锁（旧配置，保留兼容；新配置用 lock_scope）
lock_tables_during_backup = true

# ★ D1 新增：锁策略，见 F14
# auto（默认）：脚本含 DDL → global，纯 DML → snapshot
# global / table / snapshot / none
lock_scope = "auto"

# ★ D1 新增：FTWRL 等待超时（秒），0 表示无限等待，见 F18
lock_timeout = 30

# ★ D1 新增：长事务预检查策略与阈值，见 F18
on_long_transaction = "abort"
long_transaction_threshold = 5

# ★ D6 新增：是否合并连续同表 backup 段的锁区间，见 render.rs coalesce_locks
coalesce_locks = true

# 是否在 backup.sql 头部加 SET SESSION sql_log_bin=0（MySQL 专用，旧配置，保留兼容）
# 优先使用 binlog_strategy
disable_binlog_for_bks = true

# ★ D2 新增：sql_log_bin 策略：auto / always / never，见 F16
# auto（默认）：v2 --connect 检测 GTID 模式后不设；v1 静态模式按 always 处理但 warning
# always：始终设 sql_log_bin=0（GTID 模式有风险，见 D2）
# never：不设，bks_ 表数据进 binlog
binlog_strategy = "auto"

# 执行期 schema 漂移校验策略（见 F13），由发布平台在执行期解析校验语句后决定
# - abort（默认）: 校验失败停止执行
# - warn: 仅警告继续执行
# - ignore: 不校验
assert_on_schema_mismatch = "abort"

# 分区表处理策略（见 F15）
# - abort（默认）: 检测到分区表停止执行
# - warn: 仅警告
# - fallback: 降级为外部导出（需配合 --export-mode external）
on_partitioned_table = "abort"

# ★ D5 新增：备份表保留天数（默认 7 天），见 F19
backup_table_retention_days = 7

# Mapper 模式下是否对 SELECT 语句生成回滚（默认关闭）
include_select = false

# 显式主键声明：未声明时按 StmtInfo.create_table / bks_ 表 JOIN 推断
# [[rollback.primary_keys]]
# table = "users"
# columns = ["id"]
```

#### F9：元数据自动捕获（CREATE TABLE LIKE 机制，双方言）

所有需要"原表定义"才能回滚的场景（DROP / ALTER DROP/MODIFY COLUMN / DROP INDEX / DROP PRIMARY KEY / DROP VIEW）统一采用 LIKE 克隆模式，**完全自动、无需手工补全**。两种方言对应不同语法：

| 方言 | 备份表创建语法 | 复制内容 | 不复制内容 |
|------|---------------|---------|-----------|
| **MySQL** | `CREATE TABLE bks_t LIKE t` | 列/类型/默认值/NOT NULL/**AUTO_INCREMENT 属性（但当前值不保留，见下）**/主键/UNIQUE/普通索引/字符集/列注释 | 外键、CHECK、触发器、表注释、**AUTO_INCREMENT 当前计数器值** |
| **PostgreSQL** | `CREATE TABLE bks_t (LIKE t INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED)` | 列/类型/默认值/NOT NULL/CHECK 约束/主键/UNIQUE/普通索引/GENERATED 列/列注释 | 外键（REFERENCES）、触发器、表级权限、SEQUENCE（SERIAL 列会断开）、表注释、**sequence 当前值** |

**★ AUTO_INCREMENT / SEQUENCE 计数器限制**（P0 修正）：
- MySQL `CREATE TABLE LIKE` **不保留 AUTO_INCREMENT 当前值**，新表计数器重置为 1。回滚后若立即 INSERT，可能复用已删除行的自增值，导致主键冲突或业务数据错乱。
- PostgreSQL `LIKE` 不复制 SEQUENCE，SERIAL 列回滚后 sequence 仍指向"删除前"的位置，新 INSERT 会跳过被删行用的值（相对安全，但仍可能不一致）。
- **静态生成无法还原计数器**（需 `SHOW TABLE STATUS` / `SELECT last_value FROM sequence` 查询当前值）。
- **本期处理**：所有 AUTO_INCREMENT/SERIAL 列的回滚在 manifest 标记 `counter_unrestored: true` + warning，DBA 需在回滚后手工执行 `ALTER TABLE t AUTO_INCREMENT = <expected>` 或 `SELECT setval('t_id_seq', <expected>)`。v2 `--connect` 模式自动生成计数器还原语句。
- **验收标准不再声称"计数器完全恢复"**，改为"标记警告 + 文档提示 DBA 手工还原"。

> PostgreSQL 不支持 `CREATE TABLE new LIKE old` 的 MySQL 简写形式；必须用括号语法 + `INCLUDING` 子句指定要复制的部分。PostgreSQL 12+ 才支持 `INCLUDING GENERATED` 和 `INCLUDING COMMENTS`，工具按 12+ 假设。

**统一备份模式**（以 DROP COLUMN 为例，MySQL 方言，★ 幂等 + FLUSH TABLES WITH READ LOCK）：
```sql
-- backup.sql（幂等：DROP+CREATE 保证重跑不污染；FLUSH TABLES WITH READ LOCK 保证并发一致）
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_users_20260731_0001`;
CREATE TABLE `bks_users_20260731_0001` LIKE `users`;
INSERT INTO `bks_users_20260731_0001` SELECT * FROM `users`;
UNLOCK TABLES;
```

> ★ P0-5 幂等修正：原 `CREATE TABLE IF NOT EXISTS ... INSERT SELECT` 在重跑时 `IF NOT EXISTS` 跳过建表但 `INSERT` 仍执行，导致备份表数据翻倍。改为 `DROP TABLE IF EXISTS + CREATE TABLE` 保证每次重跑都从干净状态开始。
>
> ★ 乙-2 / N4 锁修正：必须用**全局** `FLUSH TABLES WITH READ LOCK`（无表名）而非 `LOCK TABLES t READ` 或 per-table 形式。MySQL 的 CREATE/DROP TABLE 触发隐式提交：
> - `LOCK TABLES t READ` 持有的锁会被释放（空锁）
> - per-table 形式 `FLUSH TABLES t WITH READ LOCK`（MySQL 8.0+）语义更接近 LOCK TABLES READ，同样可能被隐式提交释放
> - 只有**全局形式** `FLUSH TABLES WITH READ LOCK`（无表名）是真正的全局读锁，不被隐式提交释放
>
> 代价：持锁期间全库只读，需在维护窗口执行（见 P1-8）。PG 用 `LOCK TABLE t IN ACCESS SHARE MODE`（事务内有效，无隐式提交问题）。

**统一回滚模式**（MySQL 方言走 F12 原子 RENAME 切换，避免隐式提交导致原表丢失，含 FK 拓扑处理）：
```sql
-- rollback.sql（原子切换：原表保留为 _old，影子表 RENAME 为原表名）
SET FOREIGN_KEY_CHECKS=0;                                  -- ★ 乙-3：避免 RENAME 破坏 FK 拓扑
-- ★ N5 修正：RENAME 前先 DROP 已存在的 _old 守卫，避免重跑时 RENAME 撞名失败
DROP TABLE IF EXISTS `users_old_0001`;
DROP TABLE IF EXISTS `_rb_0001_users`;
CREATE TABLE `_rb_0001_users` LIKE `bks_users_20260731_0001`;
INSERT INTO `_rb_0001_users` SELECT * FROM `bks_users_20260731_0001`;
RENAME TABLE `users` TO `users_old_0001`, `_rb_0001_users` TO `users`;
SET FOREIGN_KEY_CHECKS=1;
-- 校验通过后由 cleanup.sql 删除：DROP TABLE `users_old_0001`;
```

**PostgreSQL 方言等价写法**（★ 甲-4：外层事务由 render.rs 统一包裹，段内不发 BEGIN，避免嵌套）：
```sql
-- backup.sql（外层 BEGIN/COMMIT 由 render.rs 统一发出，段内只发 LOCK TABLE）
-- BEGIN;  ← 由 render.rs 发出
-- SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;  ← 由 render.rs 发出
LOCK TABLE "users" IN ACCESS SHARE MODE;
DROP TABLE IF EXISTS "bks_users_20260731_0001";
CREATE TABLE "bks_users_20260731_0001"
  (LIKE "users" INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED);
INSERT INTO "bks_users_20260731_0001" SELECT * FROM "users";
-- COMMIT;  ← 由 render.rs 发出

-- rollback.sql（事务内 DROP+CREATE LIKE+INSERT，失败 ROLLBACK；外层 BEGIN/COMMIT 由 render.rs 发出）
-- BEGIN;
DROP TABLE IF EXISTS "users";
CREATE TABLE "users"
  (LIKE "bks_users_20260731_0001" INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED);
INSERT INTO "users" SELECT * FROM "bks_users_20260731_0001";
-- COMMIT;
```

**优势**：
- 纯静态生成，工具不连接数据库
- 不依赖 `information_schema` 查询（避免方言差异与复杂 SQL 拼接）
- 不需要用户手工补 CREATE TABLE 定义
- 备份表自带主键，DML 类回滚可直接 JOIN 定位行
- ★ 备份脚本幂等可重跑
- ★ MySQL DDL 回滚原子切换，原表不会因中间失败而丢失
- ★ 备份段加锁/快照，并发一致性保证

**已知限制（双方言通用，部分限制由 manifest 标记 + CI 阻断，见风险表）**：
- 外键约束在两种方言的 LIKE 中都不保留（MySQL 完全不复制；PG 不复制 REFERENCES 子句）→ manifest 标记 `partial: true`，**默认 CI 阻断**（见风险表 P2-3），v2 通过 `SHOW CREATE TABLE` / `pg_get_viewdef` 补全
- CHECK 约束：MySQL 不复制（MySQL 8.0.16+ 才强制 CHECK），PG 通过 `INCLUDING CONSTRAINTS` 复制 → PG 在这点上比 MySQL 更完整
- 触发器、表注释、表级权限：双方言均不复制
- **AUTO_INCREMENT / SEQUENCE 当前值不保留**（见 F9 计数器限制段落），manifest 标 `counter_unrestored: true`
- PostgreSQL SERIAL/BIGSERIAL 列：LIKE 复制列定义但不复制底层 SEQUENCE，备份表的 SERIAL 列会指向新的独立 sequence；若回滚后依赖 sequence 当前值，需要 DBA 手工 `setval`，manifest 标 `partial: true`
- **分区表**：MySQL `CREATE TABLE LIKE` 对分区表生成非分区普通表，工具生成 `information_schema.partitions` 校验语句，分区表默认 abort（见 F15）

#### F10：备份表清理策略（★ P1-11 默认值修正）

`cleanup_backup_tables_after_rollback = false`（**默认改为 false**）时，rollback.sql 执行后保留所有 `bks_` 表与 `_old_NNNN` 影子表，便于事后审计、复核、二次回滚。

显式设为 `true` 时，rollback.sql 末尾追加：
```sql
-- 清理本脚本生成的备份表（仅在 DBA 显式确认回滚成功后执行）
DROP TABLE IF EXISTS `users_old_0001`;
DROP TABLE IF EXISTS `bks_users_20260731_0001`;
DROP TABLE IF EXISTS `bks_orders_20260731_0002`;
-- ...
```

> ★ P1-11 修正理由：原默认 `true` 会在回滚一跑完就 DROP 备份表，若回滚失败或需复核即无据可查；partial（外键/CHECK 缺失）回滚后还销毁证据。改为 `false`，由 DBA 确认回滚成功后手工执行清理脚本（工具额外生成 `cleanup.sql`）。

**额外输出 `cleanup.sql`**：无论配置如何，工具总在输出目录生成 `cleanup.sql`，包含所有 `bks_` 与 `_old_NNNN` 表的 DROP 语句，DBA 在确认回滚成功后手工执行。

#### F17：ADD COLUMN 回滚的约束丢失限制（★ P1-10 修正）

**问题背景**：`ALTER TABLE users ADD COLUMN phone VARCHAR(32)` 的反向回滚是 `ALTER TABLE users DROP COLUMN phone`，但 ADD COLUMN 路径**只在脚本内能找到 ADD 语句本身**，无法知道原列的：
- `NOT NULL` / `NULL` 约束
- `DEFAULT` 值
- `COMMENT`
- 列顺序（MySQL ADD COLUMN 默认追加到末尾，原列可能不是末尾）
- `ON UPDATE CURRENT_TIMESTAMP` 等列级属性

**修正方案**：
1. **简单场景保留反向 DROP**：若 ADD COLUMN 语句本身不含 NOT NULL/DEFAULT/COMMENT 等子句（纯加列），反向 `DROP COLUMN` 可行，manifest 标 `reliable: true`。
2. **复杂场景降级全表 LIKE**：若 ADD COLUMN 语句含 NOT NULL/DEFAULT/COMMENT 等子句，说明该列有约束，DROP 后再 ADD 回去无法还原约束 → 降级为全表 LIKE + F12 原子切换路径，manifest 标 `backup_mode: "full"` + warning "ADD COLUMN with constraints, fallback to full backup to preserve column definition"。
3. **manifest 标记**：所有 ADD COLUMN 回滚 item 标 `column_constraints_check: "passed" | "fallback_to_full"`，便于审计。

#### F11：备份模式策略（增量 vs 全表，★ P0 修正）

**默认策略调整**：
- **DML（UPDATE/DELETE/REPLACE）默认增量**：按 WHERE 子句只备份受影响行，DML 语句本身携带 WHERE，可静态获取。
- **ALTER DROP/MODIFY COLUMN 默认全表**：纯静态下 AST **不含被改列的旧类型**（`ALTER TABLE t DROP COLUMN phone` 语句本身没有 `VARCHAR(32)`；`ALTER TABLE t MODIFY c NEW_TYPE` 只有新类型），无法可靠生成增量回滚。
- **升级增量的唯一条件**：脚本内同一文件或同一次扫描中存在该表的 `CREATE TABLE` 语句，且 CREATE TABLE 的 AST 含完整列定义（含被改列类型）+ 主键声明。满足时自动升级增量，manifest 标 `backup_mode: "incremental"` + `incremental_source: "create_table_context"`。

| 场景 | 默认模式 | 升级增量条件 | 增量备份内容 |
|------|---------|------------|-------------|
| `UPDATE` | 增量 | — | 按 WHERE 备份受影响行所有列旧值（含主键） |
| `DELETE` | 增量 | — | 按 WHERE 备份待删行所有列（含主键） |
| `REPLACE INTO` | 增量（partial） | — | 按 VALUES 的 pk 列表备份被覆盖旧行；新行 pk 运行时才知，无法生成 DELETE 新行回滚，标 `partial: true` |
| `INSERT` | 不备份 | — | — |
| `ALTER DROP COLUMN` | **全表** | 脚本内 CREATE TABLE 含该列类型 + 主键已知 | 备份 `(主键, 被删列)` |
| `ALTER MODIFY COLUMN` | **全表** | 脚本内 CREATE TABLE 含该列旧类型 + 主键已知 | 备份 `(主键, 被改列)` |
| `DROP TABLE` | 全表 | 无法增量（表将删除） | — |
| `TRUNCATE TABLE` | 全表 | 无法增量（整表清空） | — |
| `DROP INDEX` / `DROP PK` | 全表 | 无法增量 | — |

**配置项**：
```toml
[rollback]
# 备份模式：auto（默认，DML 增量 + ALTER 按上下文判断）/ full（强制全表）/ incremental（强制增量，ALTER 缺上下文时报错）
backup_mode = "auto"
```

**auto 模式行为**（推荐）：
- DML 走增量
- ALTER DROP/MODIFY COLUMN：扫描同次输入的所有 SQL，找该表的 CREATE TABLE 上下文；找到且含列类型 + 主键 → 增量；否则全表 + warning "no CREATE TABLE context for table X, fallback to full backup"
- 其他 DDL 走全表

**增量模式回滚路径**（ALTER DROP COLUMN，仅在 CREATE TABLE 上下文可用时）：
```sql
-- 前置条件：脚本内含 CREATE TABLE users (id BIGINT PK, phone VARCHAR(32), ...)
-- backup.sql（只备份主键 + 被删列，幂等 DROP+CREATE + FLUSH TABLES WITH READ LOCK，见 F14）
-- ★ 乙-2：必须用 FLUSH TABLES WITH READ LOCK，LOCK TABLES READ 会被随后的 DROP/CREATE 隐式提交释放
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_users_20260731_0001`;
CREATE TABLE `bks_users_20260731_0001` (
  `id` BIGINT NOT NULL,
  `phone` VARCHAR(32)
);
INSERT INTO `bks_users_20260731_0001` (`id`, `phone`)
  SELECT `id`, `phone` FROM `users`;
UNLOCK TABLES;

-- rollback.sql（ADD COLUMN + JOIN UPDATE 恢复数据）
-- MySQL 方言：DML 包语句级事务（DML 不隐式提交）；ADD COLUMN 是 DDL 会隐式提交，事务对其无效
-- ★ 乙-3：若 users 被外键引用，ADD COLUMN 不影响 FK 拓扑，无需 SET FOREIGN_KEY_CHECKS=0
START TRANSACTION;
ALTER TABLE `users` ADD COLUMN `phone` VARCHAR(32);  -- 隐式提交，事务对此条无效
UPDATE `users` u
JOIN `bks_users_20260731_0001` b ON u.`id` = b.`id`
SET u.`phone` = b.`phone`;
COMMIT;
```

**全表模式回滚路径**（ALTER DROP COLUMN 默认，MySQL 走 F12 原子切换）：
```sql
-- backup.sql（全表 LIKE + 全表数据，FLUSH TABLES WITH READ LOCK 见 F14）
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_users_20260731_0001`;
CREATE TABLE `bks_users_20260731_0001` LIKE `users`;
INSERT INTO `bks_users_20260731_0001` SELECT * FROM `users`;
UNLOCK TABLES;

-- rollback.sql（F12 原子 RENAME 切换，MySQL，含 FK 拓扑处理）
SET FOREIGN_KEY_CHECKS=0;                                  -- ★ 乙-3：避免 RENAME 破坏 FK 拓扑
-- ★ N5 修正：RENAME 前先 DROP 已存在的 _old / _rollback 守卫，避免重跑撞名
DROP TABLE IF EXISTS `users_old_0001`;
DROP TABLE IF EXISTS `_rb_0001_users`;
CREATE TABLE `_rb_0001_users` LIKE `bks_users_20260731_0001`;
INSERT INTO `_rb_0001_users` SELECT * FROM `bks_users_20260731_0001`;
RENAME TABLE `users` TO `users_old_0001`, `_rb_0001_users` TO `users`;
SET FOREIGN_KEY_CHECKS=1;
-- 校验通过后由 cleanup.sql 删除：DROP TABLE `users_old_0001`;
```

**三种模式对比**：

| 维度 | auto（默认） | full（强制） | incremental（强制） |
|------|------------|------------|------------------|
| DML | 增量 | 全表 | 增量 |
| ALTER DROP/MODIFY | 有上下文则增量，否则全表 | 全表 | 缺上下文报错 |
| 大表友好度 | ★★★★ | ★ | ★★★★★ |
| 可靠性 | ★★★★★ | ★★★★ | ★★★（ALTER 易报错） |
| manifest 标记 | `auto` + 每 item 实际模式 | `full` | `incremental` |

#### F12：DDL 回滚的原子 RENAME 切换（★ P0 修正）

**问题背景**：MySQL 的 DDL 语句会触发**隐式提交**，外层 `START TRANSACTION` 对 DDL 无约束力。若回滚脚本采用 `DROP TABLE t; CREATE TABLE t LIKE bks_t; INSERT INTO t SELECT * FROM bks_t;` 三步，一旦 `CREATE` 或 `INSERT` 失败，**原表已 DROP 且无法回滚**，造成永久数据丢失。

**修正方案**：MySQL 方言下，所有"DROP + CREATE LIKE + INSERT"路径改为**原子 RENAME 切换**模式——先在影子表上完成结构与数据准备，最后用 `RENAME TABLE` 原子交换，原表保留为 `t_old` 兜底，校验通过后再 DROP。

**回滚路径（以 DROP COLUMN 回滚为例，MySQL 方言，含 FK 拓扑处理）**：
```sql
-- ★ 乙-3：RENAME 前关闭外键检查，避免 RENAME 失败 + 警告 FK 拓扑可能错乱
SET FOREIGN_KEY_CHECKS=0;
-- ★ N5：RENAME 前先 DROP 已存在的 _old / _rollback 守卫，避免重跑撞名
DROP TABLE IF EXISTS `users_old_0001`;
DROP TABLE IF EXISTS `_rb_0001_users`;
-- 1. 用 bks_ 表克隆出"恢复后的结构"影子表
CREATE TABLE `_rb_0001_users` LIKE `bks_users_20260731_0001`;
-- 2. 把数据灌入影子表
INSERT INTO `_rb_0001_users` SELECT * FROM `bks_users_20260731_0001`;
-- 3. ★ 原子切换：原表改名为 _old，影子表改名为原表名（同一条 RENAME 语句保证原子性）
RENAME TABLE `users` TO `users_old_0001`, `_rb_0001_users` TO `users`;
SET FOREIGN_KEY_CHECKS=1;
-- 4. 校验通过后（行数比对，见 F13），由 cleanup.sql 删除旧表
-- DROP TABLE `users_old_0001`;  -- 由 DBA 确认后执行，或由 cleanup 步骤处理
```

**关键性质**：
- `RENAME TABLE a TO b, c TO a` 在 MySQL 中是**原子操作**（单条语句，元数据级），不会出现"a 已重命名但 c 未重命名"的中间状态。
- 切换前原表数据完整保留在 `users_old_0001`，若切换后发现问题（行数不符、应用报错），可立即 `RENAME TABLE users TO users_broken, users_old_0001 TO users` 回退。
- 影子表 `_rb_<seq>_<table>`（如 `_rb_0001_users`）命名与 `bks_` 前缀解耦，便于区分"备份数据表"与"回滚影子表"。★ D7 修正：seq 嵌入前缀，避免同表多次回滚时序号仅差末尾几位导致并发/交叉执行时撞名风险（如 `_rollback_users_0001` 与 `_rollback_users_0003` 仅末尾序号不同，RENAME 多表交换时易混淆；改用 `_rb_0001_users` 后 seq 在前缀，全局唯一且排序友好）。

**★ 乙-3 FK 拓扑警告（必须 manifest 标记）**：
- RENAME 把 `users` 改名为 `users_old_0001` 后，其他表指向 `users` 的外键（InnoDB FK 按 internal id 跟踪，不按名字）**会继续指向 `users_old_0001`**，新 `users` 是新表对象，FK 不会自动迁移。
- `SET FOREIGN_KEY_CHECKS=0` 只是避免 RENAME 因 FK 检查失败，**不能修复 FK 拓扑**。回滚后业务表上的 FK 实际指向了 `users_old_0001`，需 DBA 手工 `ALTER TABLE <child> DROP FOREIGN KEY <fk>; ALTER TABLE <child> ADD CONSTRAINT <fk> FOREIGN KEY (...) REFERENCES users(...)` 重建。
- manifest 标 `partial: true` + warning "FK topology may break after RENAME, manual verify required"。v2 `--connect` 模式会先 DROP 原 FK 再 RENAME 再重建 FK。

**PostgreSQL 方言**：DDL 可事务化，无需 RNAME 切换，直接在事务内 `DROP + CREATE LIKE + INSERT`，失败 `ROLLBACK` 即可。`DialectRenderer` 提供 `atomic_ddl_rollback_strategy()` 方法返回枚举 `AtomicRename(MySQL)` / `Transactional(PG)`，由 `ddl_like.rs` 按方言选择路径。

**DROP TABLE 回滚的特例**：DROP TABLE 后原表已不存在，无法做"RENAME 原表为 old"。改用：
```sql
-- 直接从 bks_ 表 LIKE 重建，因 bks_ 表已在备份阶段完整克隆结构与数据
CREATE TABLE `users` LIKE `bks_users_20260731_0001`;
INSERT INTO `users` SELECT * FROM `bks_users_20260731_0001`;
-- 此场景仍有"CREATE 失败则原表无法恢复"的风险，manifest 标记 irreversible_if_backup_missing: true
-- 强依赖 bks_ 表存在，F13 校验步骤会先确认 bks_ 表行数与备份时一致
```

#### F13：执行前 schema 校验（★ P1 修正）

**问题背景**：离线生成 + 延后执行存在 **schema 漂移**——生成时假设的表结构，到执行时可能已被热修改变更（加列、改类型）。直接执行静态生成的 `ADD COLUMN phone VARCHAR(32)` 可能因列已存在或类型不匹配而失败，或更糟——静默错改。

**修正方案**：在 `backup.sql` 与 `rollback.sql` 的每段操作前后，自动生成**校验语句**（★ C2/C7 修正：以**只读 SELECT** 形式，**驱动友好**，由发布平台执行后解析结果集决策；不使用 `DELIMITER`/存储过程/`DO $$` 等 CLI 专有语法，确保 JDBC / Python / Go 等驱动可直接执行）。

**校验内容**：
1. **备份阶段校验**（写入 backup.sql 每段开头，SELECT 形式）：
   - `SELECT IF(EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema=DATABASE() AND table_name='users'), 1, 0) AS sqlguard_check_table_exists;` —— 确认原表存在（期望 1）
   - `SELECT COUNT(*) AS sqlguard_check_row_count FROM users;` —— 记录备份前行数，发布平台写入 manifest.expected_schema.row_count
   - 对 ALTER DROP/MODIFY COLUMN：`SELECT data_type AS sqlguard_check_col_phone FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name='users' AND column_name='phone';` —— 确认被改列存在且类型与生成期一致
2. **回滚阶段校验**（写入 rollback.sql 原子切换前，SELECT 形式）：
   - `SELECT COUNT(*) AS sqlguard_check_bks_row_count FROM bks_users_20260731_0001;` —— 发布平台比对 backup 段记录的 row_count
   - 对 RENAME 切换：`SELECT COUNT(*) AS sqlguard_check_shadow_row_count FROM _rb_<seq>_<table>;` 与 bks_ 行数比对
3. **断言机制**：发布平台可配置 `assert_on_schema_mismatch = "abort" | "warn" | "ignore"`（默认 abort），解析 SELECT 结果集后按配置决策。**手动 `mysql <` 执行时 SELECT 输出可见但不阻断**，DBA 需自行核对（文档明示此限制，见乙-1）。

**manifest 记录**：每个 item 增加 `expected_schema: { table_exists: bool, row_count: i64, columns: [{name, type}] }` 字段，供发布平台比对。

#### F14：并发一致性（★ P1 修正，★ N4/D1 多模式锁策略）

**问题背景**：`CREATE TABLE bks_t LIKE t` 与 `INSERT INTO bks_t SELECT * FROM t` 是两条独立语句，对活表并发写入时会漏数据，备份本身不一致。

**修正方案**：backup.sql 在每段备份操作前加锁/快照语句。**★ D1 修正：单一 FTWRL 在生产环境不可接受（全库只读在业务高峰期是灾难），改为多模式锁策略，DBA 按场景选择**：

| `lock_scope` | 适用场景 | MySQL 实现 | PG 实现 | 风险 |
|-------------|---------|-----------|---------|------|
| `global`（**v1 默认**） | 含 DDL 的脚本，或 backup 自身含 `CREATE TABLE LIKE`（v1 所有场景） | `FLUSH TABLES WITH READ LOCK;`（全局读锁，不被隐式提交释放） | 外层 `BEGIN + REPEATABLE READ` + 段内 `LOCK TABLE t IN ACCESS SHARE MODE` | 全库只读，仅维护窗口可接受 |
| `table`（★ R2 修正：需 `--accept-table-lock-risk`） | DBA 显式确认无并发，接受隐式提交释放风险 | `LOCK TABLES t READ;`（per-table，CREATE/DROP 会立即释放） | 同上 PG | **名不副实**：第一条 LOCK 后所有 DDL/INSERT 全部无锁；manifest 强制 `partial: true` + `lock_type: "TABLE_UNSAFE"`；CLI 必须带 `--accept-table-lock-risk` 否则报错退出 |
| `snapshot`（v2 演进，v1 降级为 global） | 纯 DML 且 backup 不含 DDL（v1 不支持，v2 用临时表代替 CREATE TABLE LIKE） | DDL 外提到事务前 + `START TRANSACTION WITH CONSISTENT SNAPSHOT; INSERT...SELECT; COMMIT` | 同 PG 默认路径 | 建表到快照间无保护窗口期，manifest 标 `snapshot_window_unprotected: true` |
| `none` | 静态/读多写少表，DBA 显式接受风险 | 不加锁 | 不加锁 | 备份可能不一致，仅适用于冷表 |

**默认选择逻辑**（`lock_scope = "auto"`，新默认，★ R5 修正：同时检查"变更脚本成分"与"backup 自身是否含 DDL"）：
- 变更脚本含 DDL（CREATE/DROP/ALTER TABLE/TRUNCATE 等）→ `global`（FTWRL）
- 变更脚本纯 DML，但 backup 段含 `CREATE TABLE bks_xxx LIKE t`（DDL，所有全表备份与 DML 增量备份都需要）→ **仍走 `global`**（backup 自身的 DDL 会破坏 snapshot 快照）
- 变更脚本纯 DML，且 DBA 显式配置"不建 bks_ 表，DML 增量直接 INSERT 到临时表"（v2 规划，v1 不支持）→ `snapshot`
- **v1 实际结果**：由于 v1 所有 backup 段都需要 `CREATE TABLE bks_xxx LIKE t`（DDL），`auto` 默认对所有场景走 `global`；`snapshot` 作为 v2 演进选项保留，v1 显式指定 `snapshot` 时输出 warning "snapshot mode requires DDL-free backup, v1 always emits CREATE TABLE LIKE, falling back to global"
- DBA 显式指定 `lock_scope = "table" | "none"` → 按指定值（table 模式需 `--accept-table-lock-risk`，见下）

**示例（MySQL，ALTER DROP COLUMN 备份段，`lock_scope = "global"`）**：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_users_20260731_0001`;
CREATE TABLE `bks_users_20260731_0001` LIKE `users`;
INSERT INTO `bks_users_20260731_0001` SELECT * FROM `users`;
UNLOCK TABLES;
```

**示例（MySQL，纯 UPDATE 备份段，`lock_scope = "snapshot"`，★ N11/R1 修正：DDL 外提到事务前）**：
```sql
-- ★ N11/R1 修正：DDL（CREATE TABLE LIKE）必须外提到事务前，避免隐式提交破坏 RR 快照
-- 代价：建表到快照之间存在无保护窗口期，其他 session 可能变更表结构
-- manifest 标记 snapshot_window_unprotected: true，DBA 需评估风险
DROP TABLE IF EXISTS `bks_orders_20260731_0001`;
CREATE TABLE `bks_orders_20260731_0001` LIKE `orders`;
START TRANSACTION WITH CONSISTENT SNAPSHOT;
SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;
INSERT INTO `bks_orders_20260731_0001` SELECT * FROM `orders` WHERE status = 'pending';
COMMIT;
```

> ★ 乙-2/N4 修正：MySQL `lock_scope = "global"` 必须用全局 `FLUSH TABLES WITH READ LOCK`（无表名），per-table 形式会被隐式提交释放。`lock_scope = "table"` 是显式接受该风险的逃生通道（需 `--accept-table-lock-risk`，见下）。
>
> ★ D1/R5 修正：`lock_scope = "auto"` 同时检查变更脚本成分与 backup 自身是否含 DDL。由于 v1 所有 backup 段都含 `CREATE TABLE bks_xxx LIKE t`（DDL），`auto` 默认对所有场景走 `global`；`snapshot` 作为 v2 演进选项保留。
>
> ★ N11/R1 修正：`snapshot` 模式必须将 DDL（建 bks_ 表）外提到事务前，避免隐式提交破坏 RR 快照。代价是建表到快照之间存在无保护窗口期，manifest 标 `snapshot_window_unprotected: true`。
>
> manifest 标记 `requires_lock: true` + `lock_type: "FTWRL" | "SNAPSHOT" | "TABLE_UNSAFE" | "NONE"`，发布平台按 lock_type 提示业务方预期影响。

#### F18：FTWRL 生产防护（★ D1 新增，★ R3/R8/N12 修正）

`lock_scope = "global"` 在生产环境有 4 类风险，必须配套防护：

1. **长事务阻塞 FTWRL**：FTWRL 会等待所有活跃事务结束，若 processlist 有长事务，FTWRL 长时间挂起，等同于全库 hang。
   - **防护**：backup.sql 头部生成**长事务预检查 SELECT**（由发布平台执行后判断，★ R8 用 TIMESTAMPDIFF 更简洁，标注权限）：
     ```sql
     -- sqlguard pre-check: abort if long transactions (> 5s) exist
     -- 注意：information_schema.innodb_trx 需 PROCESS 权限
     SELECT COUNT(*) AS sqlguard_check_long_tx
     FROM information_schema.innodb_trx
     WHERE TIMESTAMPDIFF(SECOND, trx_started, NOW()) > 5;
     ```
   - 发布平台执行后，若 `sqlguard_check_long_tx > 0`，按 `on_long_transaction = "abort" | "warn"`（默认 abort）决策。

2. **★ R3 修正：长查询（非事务）也会阻塞 FTWRL**：FTWRL 等待所有活跃语句完成，包括不在事务内的长 SELECT。一个跑 30 分钟的 SELECT 即使 autocommit 模式也会让 FTWRL 挂起。
   - **防护**：补充 processlist 长查询预检查：
     ```sql
     -- sqlguard pre-check: abort if long queries (> 5s, non-Sleep) exist
     SELECT COUNT(*) AS sqlguard_check_long_queries
     FROM information_schema.processlist
     WHERE TIME > 5 AND COMMAND != 'Sleep';
     ```
   - 发布平台按 `on_long_transaction` 同策略决策（共用配置项）。
   - PG 侧 `pg_stat_activity` 查询已覆盖 `state = 'active'`，无需补充：
     ```sql
     SELECT COUNT(*) AS sqlguard_check_long_tx
     FROM pg_stat_activity
     WHERE state = 'active' AND now() - query_start > interval '5 seconds';
     ```

3. **FTWRL 等待超时**（★ N12/R7 修正：`lock_wait_timeout` 约束不完全）：
   - **问题**：`SET SESSION lock_wait_timeout = <seconds>` 控制的是**表锁/MDL 等待**超时，FTWRL 获取的是全局 MDL 读锁，受 `lock_wait_timeout` 影响但行为不完全一致（部分 MySQL 版本下 FTWRL 等待不受 `lock_wait_timeout` 严格管制）。
   - **v1 防护**：仍发 `SET SESSION lock_wait_timeout = <seconds>`（默认 30）作为尽力而为的兜底，并在 manifest 标 `lock_timeout_best_effort: true`；发布平台应在执行 FTWRL 前启动独立超时监控线程，超时后 `KILL <connection_id>` 中止 FTWRL。
   - **v2 `--connect` 模式**：发布平台用 `SELECT GET_LOCK('sqlguard_ftwrl_guard', <timeout>)` 配合 FTWRL，超时返回 0 即中止。
   - PG 用 `SET LOCAL statement_timeout = '<seconds>s';`（PG 的 statement_timeout 严格有效）。

4. **纯 DML 脚本无需 FTWRL**：`lock_scope = "auto"` 默认对纯 DML 且 backup 不含 DDL 的脚本走 snapshot 模式（见 F14，v1 实际仍走 global，因 v1 backup 总含 CREATE TABLE LIKE）。

**新增配置项**：
```toml
[rollback]
# 锁策略：auto / global / table / snapshot / none，见 F14
# auto（默认，★ R5 修正）：脚本含 DDL 或 backup 含 DDL → global；纯 DML 且 backup 无 DDL → snapshot（v1 仍降级 global）
lock_scope = "auto"

# FTWRL 等待超时（秒），0 表示无限等待，见 F18
# ★ N12/R7：v1 仅作 best-effort 兜底（lock_wait_timeout 对 FTWRL 行为不完全一致），发布平台需独立超时监控
lock_timeout = 30

# 长事务/长查询预检查策略：abort / warn / ignore，见 F18
# 长事务/查询定义：运行 > long_transaction_threshold 秒
on_long_transaction = "abort"
long_transaction_threshold = 5
```

#### F15：分区表与特殊表限制（★ P1 修正）

**问题背景**：MySQL `CREATE TABLE new LIKE old` 对**分区表**生成的是**非分区普通表**，结构与数据都会错乱。生产大表几乎都分区，硬伤。

**修正方案**：
- 工具无法静态判断表是否分区（AST 中 CREATE TABLE 信息可能在另一文件或根本不在脚本内）。
- 在 backup.sql 备份段前生成**校验语句**：`SELECT partition_method FROM information_schema.partitions WHERE table_schema=DATABASE() AND table_name='users';` —— 若返回非空（即分区表），发布平台按 `on_partitioned_table = "abort" | "warn" | "fallback"` 配置处理：
  - `abort`（默认）：停止执行，提示 DBA 手工处理
  - `fallback`：降级为 `mysqldump` 外部导出（见 F16）
- manifest 标记 `partitioned: true` + `irreversible: true`（分区表回滚不可全自动）
- 其他特殊表同理处理：临时表（TEMPORARY）、带外键的表（FK 不被 LIKE 复制）、含生成列的表（PG `INCLUDING GENERATED` 保留，MySQL 不保留）。

#### F16：备份表故障域与外部导出（★ P1 修正）

**问题边界**：`bks_` 表与业务表同库同实例同磁盘，**不是真备份**——实例级误删、磁盘故障、`DROP DATABASE` 会同时带走 bks_ 表。灾难恢复价值有限。

**修正方案**：
1. **文档明确边界**：bks_ 表是"库内影子表"，定位是**快速回滚**（误操作、错误 UPDATE/DELETE），不是**灾难恢复**。灾难恢复仍依赖常规备份（mysqldump / xtrabackup / PITR）。
2. **binlog 管控**（★ D2 修正：sql_log_bin=0 破坏 GTID 一致性）：
   - **问题**：bks_ 表的创建与插入会进 binlog，同步到只读副本/备份流。原方案用 `SET SESSION sql_log_bin=0;` 前缀避免敏感数据外传，但 **GTID 模式下此操作有严重风险**：DDL 不入 binlog 但 master 上的 GTID 计数已增加，若 master crash 后 slave promoted，bks_ 表结构不存在但 GTID 已前进，造成主从不一致。
   - **修正**：新增 `binlog_strategy = "auto" | "always" | "never"`（默认 auto，★ R4 修正：v1 静态 + 含 DDL 不静默降级，直接报错）：
     - `auto` + v2 `--connect`：先 `SELECT @@gtid_mode`，若 ON 则不设 `sql_log_bin=0`，改用 `SET SESSION binlog_row_image=MINIMAL` 减少 binlog 体积
     - `auto` + v1 静态模式 + 脚本含 DDL：**直接报错退出**（exit code 2），要求 DBA 显式设 `binlog_strategy = "never"` 或 `"always"`，不留模糊空间。错误信息："binlog_strategy=auto + static mode + DDL in script is ambiguous (GTID unknown), explicitly set binlog_strategy to 'never' (safe, binlog grows) or 'always' (GTID risk) before execution"
     - `auto` + v1 静态模式 + 纯 DML：可 keep（DML 不产生表结构 DDL，sql_log_bin=0 影响较小），但输出 warning "GTID mode unknown, sql_log_bin=0 may affect row-based replication, verify before execution"
     - `always`：始终设 `sql_log_bin=0`（GTID 模式有风险，DBA 显式确认）
     - `never`：不设，bks_ 表数据进 binlog（默认行为，安全但 binlog 体积增大）
   - **权限**：`sql_log_bin=0` 在 MySQL 8.0+ 需 `SESSION_VARIABLES_ADMIN` 权限（旧版需 SUPER，8.0 已废弃）；PG 无此语法（PG 用 `REPLICA IDENTITY` / 逻辑复制槽控制，本期不处理）。
   - **默认值调整**：原 `disable_binlog_for_bks = true` 默认改为依赖 `binlog_strategy`：auto 模式下 v1 静态生成时输出 warning，建议 DBA 在 GTID 环境显式设 `binlog_strategy = "never"` 或 v2 用 `--connect`。
3. **权限管控**：bks_ 表应在生成时附加 `REVOKE ALL ON bks_xxx FROM PUBLIC; GRANT SELECT ON bks_xxx TO dba_reviewer;`（仅 DBA 可读，应用账号不可读），减少 PII 暴露面。
4. **外部导出选项**（★ C5 修正：v1 不提供，移至 v2）：
   - v1 仅支持库内 `bks_` 影子表模式（CREATE TABLE LIKE + INSERT SELECT），不提供 `--export-mode external`。
   - v2 规划：`--export-mode external` 不生成库内表，改为生成 `SELECT ... INTO OUTFILE '/var/backup/bks_users_20260731_0001.tsv'`（MySQL）或 `COPY ... TO '/var/backup/...'`（PG），把数据落到外部路径；回滚时用 `LOAD DATA INFILE` / `COPY FROM` 恢复。外部路径需 DBA 预先配置权限，工具只生成 SQL 文本。
   - 原因：外部导出涉及文件系统权限、路径校验、`secure_file_priv`（MySQL）/ `pg_write_server_files`（PG）等环境依赖，v1 不纳入里程碑（M1-M8 无对应条目），避免承诺无法兑现。

#### F19：备份表生命周期管理（★ D5 新增）

**问题背景**：`cleanup_backup_tables_after_rollback = false`（默认）保留 bks_ 与 _old_NNNN 表便于审计，但若变更成功且从未回滚，bks_ 表会无限堆积，最终撑爆磁盘。需引入保留期管理。

**修正方案**：

1. **保留期配置**：`backup_table_retention_days = 7`（默认 7 天），见 §4.7 RollbackConfig。
2. **cleanup.sql 按生成日期过滤**：cleanup.sql 不再无条件 DROP 所有 bks_ 表，而是按本脚本生成日期 `YYYYMMDD` 与当前日期比对，仅生成"已过期"的 DROP 语句：
   ```sql
   -- cleanup.sql（仅含已过期的 bks_ 表，按 backup_table_retention_days=7 过滤）
   -- 本脚本生成日期：20260731，retention_days=7，过期日期阈值：20260724
   -- 以下 bks_ 表生成日期早于阈值，可安全清理：
   DROP TABLE IF EXISTS `bks_users_20260720_0001`;  -- 生成日期 20260720，已过期
   DROP TABLE IF EXISTS `bks_users_20260722_0002`;  -- 生成日期 20260722，已过期
   -- 本脚本自身生成的 bks_ 表（生成日期 20260731）不在 cleanup.sql 中，由 DBA 在 retention_days 后用 cleanup-expired 清理
   ```
3. **`sqlguard cleanup-expired` 子命令**：
   ```
   sqlguard cleanup-expired --output-dir <dir> [--retention-days 7] [--dry-run]
   ```
   - 扫描 `output_dir` 下所有历史 manifest（`rollback-manifest.json`），收集所有 bks_ 表名 + 生成日期
   - 按当前日期 - 生成日期 > retention_days 过滤，生成"过期 DROP 脚本"
   - `--dry-run` 仅打印不生成文件
   - 输出 `cleanup-expired-YYYYMMDD.sql`，DBA 审核后执行
4. **manifest 记录**：每个 ManifestItem 记录 `bks_table_name` + `generated_date`，cleanup-expired 按此过滤。

### 3.3 非功能需求

| 维度 | 指标 |
|------|------|
| 性能 | 单文件 1000 条 DML 生成耗时 < 1s（与规则引擎同量级，纯 AST 解析 + 字符串拼接，无 DB 连接） |
| 兼容性 | 不影响现有 `check` / `check-diff` / `init` / `replay-export` 子命令，零回归 |
| 幂等性 | 相同输入重复生成，输出文件字节级一致（时间戳通过 manifest 单独存，不进入 SQL 文本） |
| 可读性 | 生成的 SQL 必须可直接 `mysql < backup.sql` 执行；含分节注释、序号配对、来源行号注释 |
| 安全性 | **备份含完整行数据（DML/DROP COLUMN 等场景为 `INSERT SELECT *`），按敏感数据处理**：bks_ 表需权限管控（仅 DBA 可读）、`sql_log_bin=0` 避免外传只读副本/备份流（见 F16）、生成的 SQL 文件本身按敏感文件存储与传输 |
| 失败处理 | 单条语句不支持时不中断，写入 warning 到 manifest 的 `warnings[]`，最终汇总到 stderr |

### 3.4 限制与边界

1. **不连接数据库**：本期纯静态生成，通过 `CREATE TABLE LIKE`（MySQL）/ `LIKE ... INCLUDING ...`（PostgreSQL）复制表结构，**主体 backup/rollback 生成不依赖 `information_schema` 拼接**（无需用户手工补原表定义）。F13/F15 会生成 `information_schema` 只读 SELECT 校验语句用于执行期 schema 漂移检测，由发布平台在执行期解析结果集后决策，不参与主体生成逻辑（见 C3）。**外键约束、触发器、表注释不复制**（双方言通用），相关场景在 manifest 标记 `partial: true`，v2 通过可选 `--connect` 模式接入 `SHOW CREATE TABLE` / `pg_get_*` 补全。
2. **支持方言**：MySQL 与 PostgreSQL（12+）。其他方言（Oracle / SQL Server / SQLite）放 v2。方言影响：标识符引用符、CREATE TABLE LIKE 语法、DROP INDEX 语法、RENAME TABLE 语法、事务开启语句（START TRANSACTION vs BEGIN）。
3. **不解析存储过程 / 触发器 / 函数体内的语句**：与现有规则一致，仅处理顶层语句。
4. **不处理跨语句事务上下文**：每条变更语句独立生成 backup/rollback，不感知业务事务边界。
5. **不支持 DDL 之外的对象**：CREATE PROCEDURE / FUNCTION / TRIGGER 不生成回滚。
6. **MERGE / UPSERT 不支持，REPLACE INTO 部分支持（标 `partial: true`）**：
   - **MERGE / UPSERT**：静态无法判断匹配分支，输出 warning 建议拆为 INSERT + UPDATE。
   - **REPLACE INTO**：语义为"先 DELETE 冲突行后 INSERT 新行"。**静态可备份被覆盖的旧行**（按主键定位 `WHERE pk IN (<VALUES 的 pk 列表>)`）；**但"新增行的 pk"是运行时数据，静态无法预知**，回滚时无法生成"DELETE 新行"语句。本期处理：REPLACE 走 `dml::gen_replace`，只生成"INSERT 旧值"回滚，manifest 标 `partial: true` + warning "REPLACE rollback incomplete: cannot DELETE newly inserted rows statically, manual cleanup required"。v2 通过 `--connect` 模式执行期捕获新行 pk 后生成完整回滚。
7. **Mapper 动态分支组合爆炸**：复用 `mapper::dynamic::DEFAULT_MAX_INDEPENDENT_IFS` 限制（默认 5），超过输出 warning。
8. **PostgreSQL SERIAL 列**：LIKE 不复制底层 SEQUENCE，备份表的 SERIAL 列指向新独立 sequence；回滚后若依赖 sequence 当前值需 DBA 手工 `setval`，manifest 标 `partial: true`。

---

## 四、技术方案

### 4.1 总体架构

新增 `src/rollback/` 模块，与 `rule/` 平级，复用 `rule::engine::parser` 与 `rule::engine::ast` 的解析结果，但不引入 Rhai 引擎（生成逻辑用 Rust 原生实现，保证性能与可测试性）。

```
┌──────────────────────────────────────────────────────────────┐
│                    sqlguard gen-rollback                       │
├──────────────────────────────────────────────────────────────┤
│  1. 加载配置（复用 load_config）                                │
│  2. 收集文件（复用 classification::collect_sql_files           │
│               + mapper::collect_mapper_files）                 │
│  3. 对每个文件：                                                │
│     ├─ SQL 脚本：parse_sql_to_ast → Vec<StmtInfo>              │
│     └─ Mapper XML：mapper::dynamic::parse_dynamic_statements   │
│                   → 展开变体 → parse_sql_to_ast                │
│  4. 逐条调用 RollbackGenerator::generate(stmt)                 │
│     → BackupRollbackPair { backup, rollback, safety, strategy, expected_schema, warnings }  │
│  5. 汇总：                                                      │
│     ├─ backup.sql    （前向排序）                               │
│     ├─ rollback.sql  （后向排序）                               │
│     └─ rollback-manifest.json                                   │
└──────────────────────────────────────────────────────────────┘
```

### 4.2 模块划分

```
src/rollback/
├── mod.rs            # 模块入口 + 公共类型（BackupRollbackPair、GenerationResult）
├── dialect.rs        # ★ 方言适配层：Dialect enum + DialectRenderer trait
│                    #   MySQL / PostgreSQL 两实现，封装所有方言差异
├── generator.rs      # RollbackGenerator 主调度，按 StmtInfo.kind 分发
├── ddl.rs            # DDL 语句的 backup/rollback 生成（CREATE/DROP/ALTER/INDEX/VIEW）
├── ddl_like.rs       # ★ CREATE TABLE LIKE 统一模式：DROP/ALTER DROP/MODIFY/DROP INDEX 等
│                    #   生成 backup = CREATE TABLE bks_t (LIKE t ...) + INSERT SELECT
│                    #   生成 rollback = DROP t + CREATE t (LIKE bks_t ...) + INSERT SELECT
│                    #   标记 partial=true（外键/CHECK/触发器未保留）
│                    #   通过 DialectRenderer 渲染 LIKE 语法差异
├── dml.rs            # DML 语句的 backup/rollback 生成（INSERT/UPDATE/DELETE）
├── naming.rs         # 备份表命名（bks_xxx_YYYYMMDD_NNNN）+ 序号分配器
├── pk.rs             # 主键解析（配置 → StmtInfo → bks_ 表 JOIN 推断）
├── render.rs         # SQL 文本渲染：分节注释、@@SEQ 配对、事务包裹、清理 DROP 段
└── manifest.rs       # rollback-manifest.json 序列化
```

### 4.3 核心数据结构

```rust
// src/rollback/mod.rs

use crate::rule::engine::ast::StmtInfo;
use serde::Serialize;

/// ★ C2 架构修正：聚合所有"安全分类"标志，避免 flag 散装。
/// 生成器填充，渲染器/manifest 序列化统一读取，CI 按 class 决策退出码（见 §4.13 决策表）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SafetyClass {
    /// 主键缺失等场景下为 false（仅 DML 生效），CI 退出码 2
    pub reliable: bool,
    /// 部分回滚（外键/CHECK/触发器未保留 / RENAME 后 FK 拓扑可能错乱 / REPLACE 新行无法回滚）
    /// CI 退出码 2
    pub partial: bool,
    /// 不可逆变更（MERGE/UPSERT 等真正不支持的语句）。注：REPLACE 不归此类，用 `partial`
    pub irreversible: bool,
    /// AUTO_INCREMENT / SEQUENCE 当前值无法静态还原，需 DBA 手工修复
    pub counter_unrestored: bool,
    /// 备份段持锁（FTWRL / ACCESS SHARE），发布平台需提示锁时长
    pub requires_lock: bool,
    /// ★ N8 修正：锁类型："FTWRL" / "SNAPSHOT" / "TABLE_UNSAFE" / "NONE" / None
    /// （不是方言名；由 resolve_lock_type 按 lock_scope 决定）
    pub lock_type: Option<String>,
    /// ★ N12/R7：lock_wait_timeout 对 FTWRL 行为不完全一致，仅 best-effort 兜底
    pub lock_timeout_best_effort: bool,
    /// ★ N11/R1：snapshot 模式下 DDL 外提到事务前，建表到快照间无保护窗口期
    pub snapshot_window_unprotected: bool,
    /// 分区表（LIKE 生成非分区表，F15 检测）
    pub partitioned: bool,
    /// DROP TABLE 回滚特例：CREATE 失败则原表无法恢复，强依赖 bks_ 表存在
    pub irreversible_if_backup_missing: bool,
}

/// 备份策略元数据（与 SafetyClass 正交，记录"怎么备份的"而非"安不安全"）
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupStrategy {
    /// incremental / full（记录实际使用的模式，便于审计降级场景）
    pub backup_mode: String,
    /// 升级增量的来源："create_table_context" / None（仅 ALTER DROP/MODIFY 在升级时填）
    pub incremental_source: Option<String>,
    /// ADD COLUMN 约束检查："passed"（无约束，走 metadata-only）/ "fallback_to_full"（有约束，降级全表）/ None（非 ADD COLUMN）
    pub column_constraints_check: Option<String>,
}

/// 执行期 schema 漂移校验的期望值（F13），发布平台在执行期比对
#[derive(Debug, Clone, Serialize)]
pub struct ExpectedSchema {
    pub table_exists: bool,
    pub row_count: i64,
    pub columns: Vec<ExpectedColumn>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpectedColumn {
    pub name: String,
    pub data_type: String,
}

/// 单条变更语句的生成结果。
pub struct BackupRollbackPair {
    /// 全局序号（1-indexed），用于 backup/rollback 配对
    pub seq: u64,
    /// 来源信息：文件路径 + 行号 + （mapper 时）statement_id
    pub source: SourceRef,
    /// 变更语句原文（去掉尾部分号）
    pub original_sql: String,
    /// 备份语句（变更前执行）。None 表示该语句无需备份。
    pub backup: Option<String>,
    /// 回滚语句（变更失败后执行）。None 表示无法生成（在 warnings 中说明）。
    pub rollback: Option<String>,
    /// ★ C2：聚合安全分类（取代散装 flag）
    pub safety: SafetyClass,
    /// 备份策略元数据
    pub strategy: BackupStrategy,
    /// 执行期 schema 漂移校验期望值（F13），None 表示该校验不适用
    pub expected_schema: Option<ExpectedSchema>,
    /// 生成过程中的提示，非致命
    pub warnings: Vec<String>,
}

pub struct SourceRef {
    pub file: String,
    pub line: i64,
    pub end_line: i64,
    pub statement_id: Option<String>, // Mapper 模式
    pub variant_label: Option<String>, // Mapper 动态分支变体
}

pub struct GenerationResult {
    pub pairs: Vec<BackupRollbackPair>,
    pub backup_sql: String,
    pub rollback_sql: String,
    pub manifest: Manifest,
}

#[derive(Serialize)]
pub struct Manifest {
    pub version: u32,
    pub generator: String,
    pub generated_at: String,
    pub dialect: String,
    pub source_count: usize,
    pub backup_count: usize,
    pub rollback_count: usize,
    /// ★ 汇总计数（供 CI 快速判断，无需遍历 items）
    pub unreliable_count: usize,
    pub irreversible_count: usize,
    pub partial_count: usize,
    pub counter_unrestored_count: usize,
    pub requires_lock_count: usize,
    pub partitioned_count: usize,
    pub items: Vec<ManifestItem>,
    pub warnings: Vec<String>,
}

/// ★ C1 修正：补齐文档通篇引用的 8 个字段，对齐发布平台契约
#[derive(Serialize)]
pub struct ManifestItem {
    pub seq: u64,
    pub source: SourceRef,  // ★ minor：SourceRef 本身已可序列化，无需独立 SourceRefSerializable
    pub original_sql: String,
    pub backup: Option<String>,
    pub rollback: Option<String>,
    /// ★ 嵌入聚合结构（取代散装 flag），序列化后字段平铺到 item 下
    #[serde(flatten)]
    pub safety: SafetyClass,
    #[serde(flatten)]
    pub strategy: BackupStrategy,
    /// F13 执行期 schema 漂移校验期望值
    pub expected_schema: Option<ExpectedSchema>,
    pub warnings: Vec<String>,
}
```

> **★ C1 修正说明**：原 `ManifestItem` 仅 10 个字段，缺 `counter_unrestored` / `requires_lock` / `lock_type` / `partitioned` / `incremental_source` / `column_constraints_check` / `irreversible_if_backup_missing` / `expected_schema`，导致发布平台无法按 manifest 决策锁提示、计数器修复、分区表阻断等。现通过 `SafetyClass` + `BackupStrategy` + `ExpectedSchema` 三个聚合结构补齐，`#[serde(flatten)]` 保证序列化后字段平铺，兼容旧发布平台按字段名读取。

### 4.4 生成器接口

```rust
// src/rollback/generator.rs

use crate::config::Config;
use crate::rule::engine::ast::StmtInfo;
use super::dialect::{Dialect, DialectRenderer};

pub struct RollbackGenerator<'a> {
    config: &'a Config,
    rollback_config: &'a RollbackConfig,
    /// 方言渲染器（MySQL / PostgreSQL）
    renderer: &'a dyn DialectRenderer,
    /// 全局序号分配器
    seq: u64,
    /// 已使用的备份表名（防重）
    used_backup_names: std::collections::HashSet<String>,
    /// 主键映射：table → Vec<column>
    primary_keys: std::collections::HashMap<String, Vec<String>>,
}

impl<'a> RollbackGenerator<'a> {
    pub fn new(config: &'a Config, rc: &'a RollbackConfig, renderer: &'a dyn DialectRenderer) -> Self { /* ... */ }

    /// 对单条语句生成 backup/rollback。stmt_kind 决定走 ddl / ddl_like / dml 分支。
    pub fn generate(&mut self, stmt: &StmtInfo, source: SourceRef, original: &str)
        -> BackupRollbackPair
    {
        self.seq += 1;
        let seq = self.seq;
        match stmt.kind.as_str() {
            // 纯反向 DDL（无需原表定义）
            "CREATE_TABLE" => ddl::gen_create_table(stmt, seq, original, self.renderer),
            "CREATE_INDEX" => ddl::gen_create_index(stmt, seq, original, self.renderer),
            "CREATE_VIEW"  => ddl::gen_create_view(stmt, seq, original, self.renderer),
            // ALTER 中只需反向元数据的子操作（ADD COLUMN 仅当无约束时走此路径，见 is_metadata_only_alter）
            "ALTER_TABLE" if is_metadata_only_alter(stmt) => ddl::gen_alter_metadata(stmt, seq, original, self.renderer),
            // ★ 需要原表定义的 DDL → 统一走 CREATE TABLE LIKE 模式（含 ADD COLUMN 带约束的降级，见 F17）
            "DROP_TABLE" | "DROP_INDEX" | "DROP_VIEW"
            | "TRUNCATE"
            | "ALTER_TABLE"  // DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT、DROP PRIMARY KEY、带约束的 ADD COLUMN 等
                => ddl_like::gen_with_full_backup(stmt, seq, original, &self.naming, self.renderer),
            // DML
            "INSERT" => dml::gen_insert(stmt, seq, original, &self.primary_keys, self.renderer),
            "UPDATE" => dml::gen_update(stmt, seq, original, &self.primary_keys, &self.config, self.renderer),
            "DELETE" => dml::gen_delete(stmt, seq, original, &self.primary_keys, self.renderer),
            // ★ REPLACE INTO 部分支持（标 partial，见 §3.4 #6）：备份被覆盖旧行，无法 DELETE 新行
            "REPLACE" => dml::gen_replace(stmt, seq, original, &self.primary_keys, self.renderer),
            // 不支持
            "MERGE" | "UPSERT" => unsupported(stmt, seq, original, "MERGE/UPSERT not supported, split into INSERT+UPDATE"),
            // 跳过
            "START_TRANSACTION" | "COMMIT" | "ROLLBACK" | "SET_VARIABLE" | "USE" => skip(),
            _ => unsupported(stmt, seq, original, &format!("Unsupported statement kind: {}", stmt.kind)),
        }
    }
}

/// 判断 ALTER TABLE 是否仅涉及纯元数据反向操作（RENAME COLUMN/TABLE、ADD INDEX/CONSTRAINT/PK、
/// **无约束的 ADD COLUMN**）。
///
/// ★ F17 修正：ADD COLUMN 仅当**不含** NOT NULL/DEFAULT/COMMENT/ON UPDATE 等列级约束时才视为
/// metadata-only（反向 DROP COLUMN 可行）；含约束时返回 false，走 ddl_like 全表 LIKE 路径，
/// 否则 DROP 后再 ADD 回去会丢失约束。
///
/// DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT/PK 等需要原表定义的子操作返回 false，走 ddl_like 路径。
fn is_metadata_only_alter(stmt: &StmtInfo) -> bool {
    if let Some(alter) = &stmt.alter_table {
        alter.operations.iter().all(|op| {
            match op.operation_type.as_str() {
                // ★ ADD COLUMN 必须检查列约束子句（F17）
                "ADD_COLUMN" => !op.has_column_constraints(),  // 即 NOT NULL/DEFAULT/COMMENT/ON UPDATE 全空
                "ADD_CONSTRAINT" | "ADD_INDEX" | "RENAME_COLUMN"
                | "RENAME_TABLE" | "ADD_PRIMARY_KEY" => true,
                _ => false,  // DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT/PK 等
            }
        })
    } else {
        false
    }
}
```

### 4.5 关键生成逻辑示例

#### 4.5.1 INSERT 回滚

**输入**：
```sql
INSERT INTO users (id, name, email) VALUES (1001, 'alice', 'a@x.com');
```

**回滚**（主键 = `id`）：
```sql
DELETE FROM `users` WHERE `id` = 1001;
```

**无主键回滚**（标记 `reliable=false`）：
```sql
-- WARN: no primary key, fallback to full-column match
DELETE FROM `users` WHERE `id` = 1001 AND `name` = 'alice' AND `email` = 'a@x.com';
```

> INSERT 语句不生成 backup（无旧数据需要保存）。

#### 4.5.2 UPDATE 备份与回滚（增量模式 + 幂等 + FLUSH TABLES，★ 乙-6 多列示例）

**输入**（多列 UPDATE，回滚必须还原每一个 SET 目标列）：
```sql
UPDATE orders
SET status = 'shipped', shipped_at = NOW(), updated_by = 'system'
WHERE status = 'pending' AND created_at < '2026-01-01';
```

**备份**（增量，假设主键 `order_id`，幂等 DROP+CREATE + FLUSH TABLES WITH READ LOCK）：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_orders_20260731_0001`;
CREATE TABLE `bks_orders_20260731_0001` LIKE `orders`;
INSERT INTO `bks_orders_20260731_0001`
  SELECT * FROM `orders`
  WHERE `status` = 'pending' AND `created_at` < '2026-01-01';
UNLOCK TABLES;
```

**回滚**（JOIN UPDATE 恢复**所有 SET 目标列**旧值，DML 包语句级事务）：
```sql
START TRANSACTION;
UPDATE `orders` o
JOIN `bks_orders_20260731_0001` b ON o.`order_id` = b.`order_id`
SET o.`status` = b.`status`,
    o.`shipped_at` = b.`shipped_at`,
    o.`updated_by` = b.`updated_by`;
COMMIT;
```

> ★ 乙-6 修正：生成器必须从 AST 的 `SET` 子句提取**所有目标列**，在回滚 UPDATE 中逐列还原 `o.<col> = b.<col>`，不能只还原单列。`dml::gen_update` 解析 `stmt.update.set_clauses: Vec<(column, expr)>` 生成完整 SET 列表。
>
> ★ 乙-6 触发器/级联警告：回滚 UPDATE 会再次触发该表的 BEFORE/AFTER UPDATE 触发器与外键级联（ON UPDATE CASCADE）。若表上有触发器或级联 FK，manifest 标 `partial: true` + warning "UPDATE rollback may fire triggers/cascades, manual verify required"。敏感场景建议在回滚前 `SET FOREIGN_KEY_CHECKS=0` + 临时 DISABLE TRIGGER（PG: `ALTER TABLE t DISABLE TRIGGER ALL`），回滚后恢复。
>
> 注：UPDATE 的增量备份通过 WHERE 子句定位改动行，备份表用 `LIKE` 克隆完整结构后只 INSERT 受影响行的所有列旧值。无 WHERE 的全表 UPDATE 会备份全表，标记 `reliable=false`。

#### 4.5.3 DELETE 备份与回滚（增量模式 + 幂等 + FLUSH TABLES）

**输入**：
```sql
DELETE FROM orders WHERE status = 'cancelled' AND created_at < '2025-01-01';
```

**备份**（增量，按 WHERE 备份待删行，幂等 + FLUSH TABLES WITH READ LOCK）：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_orders_20260731_0002`;
CREATE TABLE `bks_orders_20260731_0002` LIKE `orders`;
INSERT INTO `bks_orders_20260731_0002`
  SELECT * FROM `orders`
  WHERE `status` = 'cancelled' AND `created_at` < '2025-01-01';
UNLOCK TABLES;
```

**回滚**：
```sql
START TRANSACTION;
INSERT INTO `orders`
  SELECT * FROM `bks_orders_20260731_0002`;
COMMIT;
-- 备份表不自动清理（cleanup_backup_tables_after_rollback 默认 false）
-- DBA 确认回滚成功后手工执行 cleanup.sql
```

#### 4.5.4 ALTER TABLE DROP COLUMN（默认全表 + F12 原子切换）

**输入**：
```sql
ALTER TABLE users DROP COLUMN phone;
```

> ★ P0-3 修正：纯静态下 AST 不含被删列的旧类型（语句本身只有列名 `phone`，没有 `VARCHAR(32)`），**默认走全表 LIKE + 原子切换**。仅当脚本内含 `CREATE TABLE users` 上下文且能从 AST 提取 `phone` 列类型 + 主键时，才升级增量模式（示例见 F11）。

**备份**（全表 LIKE + 全表数据，幂等 + FLUSH TABLES WITH READ LOCK）：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_users_20260731_0003`;
CREATE TABLE `bks_users_20260731_0003` LIKE `users`;
INSERT INTO `bks_users_20260731_0003` SELECT * FROM `users`;
UNLOCK TABLES;
```

**回滚**（F12 原子 RENAME 切换，MySQL 方言，含 FK 拓扑处理）：
```sql
-- ★ 乙-3 修正：RENAME 前关闭外键检查，避免 RENAME 失败 + 警告 FK 拓扑可能错乱
SET FOREIGN_KEY_CHECKS=0;
-- 1. 用 bks_ 表克隆出"恢复后的结构"影子表（含被删的 phone 列）
CREATE TABLE `_rb_0003_users` LIKE `bks_users_20260731_0003`;
-- 2. 把数据灌入影子表
INSERT INTO `_rb_0003_users` SELECT * FROM `bks_users_20260731_0003`;
-- 3. ★ 原子切换：原表保留为 _old，影子表 RENAME 为原表名
RENAME TABLE `users` TO `users_old_0003`, `_rb_0003_users` TO `users`;
SET FOREIGN_KEY_CHECKS=1;
-- 4. 校验通过后由 cleanup.sql 删除：DROP TABLE `users_old_0003`;
```

> **关键安全性质**：RENAME 是原子操作，切换瞬间完成；若切换后发现问题，可立即 `RENAME TABLE users TO users_broken, users_old_0003 TO users` 回退。原表数据在 `users_old_0003` 完整保留，直到 DBA 确认回滚成功后才由 cleanup.sql 清理。
>
> **★ 乙-3 FK 拓扑警告**：RENAME 把 `users` 改名为 `users_old_0003`，其他表指向 `users` 的外键（InnoDB FK 按 internal id 跟踪，不按名字）会继续指向 `users_old_0003`，新 `users` 是新表对象，FK 不会自动迁移。manifest 标 `partial: true` + warning "FK topology may break after RENAME, manual verify required"。v2 `--connect` 模式会先 DROP 原 FK 再 RENAME 再重建 FK。
>
> **AUTO_INCREMENT 限制**：若 `users` 表含 AUTO_INCREMENT 列，LIKE 不保留当前计数器值，回滚后 manifest 标 `counter_unrestored: true`，DBA 需手工 `ALTER TABLE users AUTO_INCREMENT = <expected>`（见 F9）。
>
> **分区表限制**：若 `users` 是分区表，LIKE 生成非分区表，F15 校验语句会检测并按 `on_partitioned_table` 策略处理（默认 abort）。

#### 4.5.5 TRUNCATE TABLE（全表备份，无法增量）

**输入**：
```sql
TRUNCATE TABLE audit_log;
```

**备份**（整表清空，必须全表备份，幂等 + FLUSH TABLES WITH READ LOCK）：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_audit_log_20260731_0004`;
CREATE TABLE `bks_audit_log_20260731_0004` LIKE `audit_log`;
INSERT INTO `bks_audit_log_20260731_0004` SELECT * FROM `audit_log`;
UNLOCK TABLES;
```

**回滚**（TRUNCATE 后表结构还在，只需重新灌数据）：
```sql
-- MySQL：DML 包语句级事务
START TRANSACTION;
INSERT INTO `audit_log` SELECT * FROM `bks_audit_log_20260731_0004`;
COMMIT;
```

> ★ 乙-5 修正：TRUNCATE 在 MySQL 是 DDL（隐式提交），无法事务回滚；本工具的"回滚"是指用 bks_ 表数据重新填充。**PostgreSQL 的 TRUNCATE 是事务性的**，可在事务内执行且失败可 ROLLBACK，PG 方言下 TRUNCATE 走事务内路径，无需 bks_ 重灌（但本工具为统一逻辑仍生成 bks_ 备份，作为冗余兜底）。AUTO_INCREMENT 计数器在 MySQL 会被 TRUNCATE 重置，回滚后需手工还原（manifest 标 `counter_unrestored: true`）。

#### 4.5.6 DROP TABLE（全表备份，F12 原子切换特例）

**输入**：
```sql
DROP TABLE legacy_orders;
```

**备份**（`CREATE TABLE LIKE` 复制完整结构 + 全表数据，幂等 + FLUSH TABLES WITH READ LOCK）：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_legacy_orders_20260731_0005`;
CREATE TABLE `bks_legacy_orders_20260731_0005` LIKE `legacy_orders`;
INSERT INTO `bks_legacy_orders_20260731_0005` SELECT * FROM `legacy_orders`;
UNLOCK TABLES;
```

**回滚**（DROP TABLE 后原表已不存在，无法走 RENAME 切换；直接从 bks_ 表 LIKE 重建）：
```sql
-- ★ 风险提示：此场景 CREATE 失败则原表无法恢复，强依赖 bks_ 表存在
-- F13 校验步骤会先确认 bks_ 表存在且行数与备份时一致
-- ★ 乙-3：DROP TABLE 已无原表，无 FK 拓扑问题；但若其他表 FK 指向本表，需先 SET FOREIGN_KEY_CHECKS=0
SET FOREIGN_KEY_CHECKS=0;
CREATE TABLE `legacy_orders` LIKE `bks_legacy_orders_20260731_0005`;
INSERT INTO `legacy_orders` SELECT * FROM `bks_legacy_orders_20260731_0005`;
SET FOREIGN_KEY_CHECKS=1;
```

> manifest 标记 `irreversible_if_backup_missing: true`，提醒 DBA 此回滚强依赖 bks_ 表。
> 外键/CHECK 约束、触发器不复制 → manifest 标记 `partial: true`。仅当 `SHOW CREATE TABLE` 模式（v2）启用时，外键约束才会补全。

#### 4.5.7 DROP INDEX（全表备份，F12 原子切换）

**输入**：
```sql
DROP INDEX idx_users_email ON users;
```

**备份**（`CREATE TABLE LIKE` 保留索引定义 + 全表数据，幂等 + FLUSH TABLES WITH READ LOCK）：
```sql
FLUSH TABLES WITH READ LOCK;
DROP TABLE IF EXISTS `bks_users_20260731_0006`;
CREATE TABLE `bks_users_20260731_0006` LIKE `users`;
INSERT INTO `bks_users_20260731_0006` SELECT * FROM `users`;
UNLOCK TABLES;
```

**回滚**（F12 原子 RENAME 切换，索引随 LIKE 自动恢复，含 FK 拓扑处理）：
```sql
SET FOREIGN_KEY_CHECKS=0;
CREATE TABLE `_rb_0006_users` LIKE `bks_users_20260731_0006`;
INSERT INTO `_rb_0006_users` SELECT * FROM `bks_users_20260731_0006`;
RENAME TABLE `users` TO `users_old_0006`, `_rb_0006_users` TO `users`;
SET FOREIGN_KEY_CHECKS=1;
-- 校验通过后由 cleanup.sql 删除：DROP TABLE `users_old_0006`;
```

#### 4.5.8 DROP VIEW（自动回滚）

**输入**：
```sql
DROP VIEW active_users;
```

**备份**（视图不能用 `LIKE`，用 `CREATE VIEW bks_v AS SELECT *` 保留查询定义）：
```sql
CREATE VIEW `bks_active_users_20260731_0007` AS SELECT * FROM `active_users`;
```

**回滚**：
```sql
CREATE VIEW `active_users` AS SELECT * FROM `bks_active_users_20260731_0007`;
DROP VIEW IF EXISTS `bks_active_users_20260731_0007`;
```

### 4.6 配置加载

在 `src/config.rs` 中新增：

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct RollbackConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 方言：mysql / postgresql。CLI --dialect 覆盖此值。
    #[serde(default = "default_dialect")]
    pub dialect: String,
    /// 备份模式：auto（默认）/ full / incremental
    #[serde(default = "default_backup_mode")]
    pub backup_mode: String,
    #[serde(default = "default_backup_file")]
    pub backup_file: String,
    #[serde(default = "default_rollback_file")]
    pub rollback_file: String,
    #[serde(default = "default_manifest_file")]
    pub manifest_file: String,
    #[serde(default = "default_cleanup_file")]
    pub cleanup_file: String,
    /// 仅 PG 生效，MySQL 方言忽略（DDL 隐式提交无效）
    #[serde(default = "default_true")]
    pub wrap_transaction: bool,
    #[serde(default = "default_bks_prefix")]
    pub backup_table_prefix: String,
    /// 备份表名是否包含 8 位日期段（bks_xxx_YYYYMMDD_NNNN）
    #[serde(default = "default_true")]
    pub backup_table_with_date: bool,
    /// 日期段格式，默认 "%Y%m%d"，必须产出 8 位数字以保证表名只含 [a-zA-Z0-9_]
    #[serde(default = "default_date_fmt")]
    pub backup_table_date_format: String,
    /// ★ P1-11 默认改为 false（保留备份表便于审计）
    #[serde(default = "default_false")]
    pub cleanup_backup_tables_after_rollback: bool,
    /// 备份段是否加锁（旧配置，保留兼容；新配置用 lock_scope）
    /// ★ 乙-2/N4：MySQL 必须用全局 FLUSH TABLES WITH READ LOCK（无表名），per-table 形式会被隐式提交释放
    #[serde(default = "default_true")]
    pub lock_tables_during_backup: bool,
    /// ★ D1 新增：锁策略，见 F14
    /// auto（默认）：脚本含 DDL → global，纯 DML → snapshot
    /// global / table / snapshot / none
    #[serde(default = "default_lock_scope")]
    pub lock_scope: String,
    /// ★ D1 新增：FTWRL 等待超时（秒），0 表示无限等待，见 F18
    #[serde(default = "default_lock_timeout")]
    pub lock_timeout: u64,
    /// ★ D1 新增：长事务预检查策略：abort / warn / ignore，见 F18
    #[serde(default = "default_long_tx_strategy")]
    pub on_long_transaction: String,
    /// ★ D1 新增：长事务阈值（秒），见 F18
    #[serde(default = "default_long_tx_threshold")]
    pub long_transaction_threshold: u64,
    /// 是否在 backup.sql 头部加 SET SESSION sql_log_bin=0（MySQL 专用），见 F16
    /// ★ D2 修正：auto 模式下若检测到 GTID 模式（v2 --connect），不设 sql_log_bin=0
    #[serde(default = "default_true")]
    pub disable_binlog_for_bks: bool,
    /// ★ D2 新增：sql_log_bin 策略：auto / always / never，见 F16
    /// auto（默认）：v2 --connect 检测 GTID 模式后不设；v1 静态模式按 always 处理但 warning
    /// always：始终设 sql_log_bin=0（GTID 模式有风险，见 D2）
    /// never：不设，bks_ 表数据进 binlog
    #[serde(default = "default_binlog_strategy")]
    pub binlog_strategy: String,
    /// schema 漂移校验策略：abort / warn / ignore，见 F13
    #[serde(default = "default_assert_strategy")]
    pub assert_on_schema_mismatch: String,
    /// 分区表处理策略：abort / warn / fallback，见 F15
    #[serde(default = "default_partitioned_strategy")]
    pub on_partitioned_table: String,
    #[serde(default)]
    pub include_select: bool,
    #[serde(default)]
    pub primary_keys: Vec<PrimaryKeyDecl>,
    /// ★ D5 新增：备份表保留天数（默认 7 天），见 F19
    /// cleanup.sql 与 cleanup-expired 子命令按此过滤
    #[serde(default = "default_retention_days")]
    pub backup_table_retention_days: u64,
    /// ★ D6 新增：是否合并连续同表 backup 段的锁区间，见 render.rs coalesce_locks
    #[serde(default = "default_true")]
    pub coalesce_locks: bool,
    /// ★ R6 新增：coalesce 策略：conservative / aggressive
    /// conservative（默认）：仅 DDL 全表 LIKE（INSERT SELECT * FROM t 无 WHERE）合并；DML 增量不合并
    /// aggressive：所有同表段都尝试合并（DBA 显式启用，需自负 WHERE 子句含 JOIN/子查询的跨表依赖风险）
    #[serde(default = "default_coalesce_mode")]
    pub coalesce_locks_mode: String,
}

fn default_dialect() -> String { "mysql".to_string() }
fn default_backup_mode() -> String { "auto".to_string() }
fn default_date_fmt() -> String { "%Y%m%d".to_string() }
fn default_cleanup_file() -> String { "cleanup.sql".to_string() }
fn default_assert_strategy() -> String { "abort".to_string() }
fn default_partitioned_strategy() -> String { "abort".to_string() }
fn default_lock_scope() -> String { "auto".to_string() }
fn default_lock_timeout() -> u64 { 30 }
fn default_long_tx_strategy() -> String { "abort".to_string() }
fn default_long_tx_threshold() -> u64 { 5 }
fn default_binlog_strategy() -> String { "auto".to_string() }
fn default_retention_days() -> u64 { 7 }
fn default_coalesce_mode() -> String { "conservative".to_string() }
fn default_false() -> bool { false }

#[derive(Debug, Deserialize, Clone)]
pub struct PrimaryKeyDecl {
    pub table: String,
    pub columns: Vec<String>,
}
```

`Config` 增加字段 `pub rollback: RollbackConfig`，`#[serde(default)]` 保证旧配置兼容。

### 4.6.1 `ddl_like` 模块 — CREATE TABLE LIKE 统一模式（双方言，★ 甲-2 重写）

这是本次方案的核心模块。所有"需要原表定义"的 DDL 场景统一通过该模块生成，**主体 backup/rollback 不依赖 information_schema 拼接**（无需用户手工补原表定义）；F13/F15 生成的 information_schema SELECT 仅作执行期校验，不参与主体生成。**所有输出 SQL 严格遵循 F9（LIKE 机制）、F12（原子 RENAME 切换）、F13（schema 校验）、F14（并发锁）、F15（分区表检测）、F17（ADD COLUMN 约束降级）**。LIKE 语法与事务策略的方言差异通过 `DialectRenderer` 封装。

```rust
// src/rollback/ddl_like.rs

use crate::rule::engine::ast::StmtInfo;
use super::{BackupRollbackPair, SourceRef, naming, dialect::{DialectRenderer, AtomicStrategy}};

/// 对需要原表定义的 DDL 语句（DROP/ALTER DROP/MODIFY/DROP INDEX/PK/TRUNCATE/DROP VIEW/
/// 带约束的 ADD COLUMN）统一采用 CREATE TABLE LIKE 模式生成 backup/rollback。
///
/// ★ 关键约束（必须落地，否则就是 P0/P1 翻车）：
/// - backup.sql 必须**幂等**（F9）：`DROP TABLE IF EXISTS bks_xxx; CREATE TABLE bks_xxx LIKE t; INSERT ...`
///   绝不能用 `CREATE TABLE IF NOT EXISTS` + `INSERT`（重跑数据翻倍，P0-5）
/// - backup.sql 每段必须**加锁**（F14）：MySQL 用**全局** `FLUSH TABLES WITH READ LOCK`（★ 乙-2/N4 修正：
///   `LOCK TABLES READ` 与 per-table 形式都会被 CREATE/DROP 隐式提交释放，是空锁；必须用无表名的全局形式）；PG 用外层 `BEGIN + REPEATABLE READ`
/// - rollback.sql 在 MySQL 方言下必须走 **F12 原子 RENAME 切换**（DROP+CREATE+INSERT 三步会因隐式提交
///   导致 DROP 后 CREATE 失败原表丢失，P0-1）；PG 方言走事务内 DROP+CREATE LIKE+INSERT
/// - 每段必须生成 **F13 schema 校验语句**（表存在/行数/列类型），以**只读 SELECT** 形式
///   （`sqlguard_check_*` 别名），由发布平台解析结果集后按 `assert_on_schema_mismatch` 决策。
///   ★ N1 修正：不使用 `SIGNAL SQLSTATE`/存储过程/`DO $$`（CLI 专有，驱动不兼容）。
///   手动 `mysql <` 执行时 SELECT 输出可见但不阻断，DBA 需自行核对（见乙-1）
/// - RENAME 切换前必须 `SET FOREIGN_KEY_CHECKS=0`（MySQL）/ `SET CONSTRAINTS ALL DEFERRED`（PG），
///   避免外键拓扑破坏（乙-3 修正）
/// - 标记 `partial = true`（外键/触发器在 LIKE 中不保留）
pub fn gen_with_full_backup(
    stmt: &StmtInfo,
    seq: u64,
    original: &str,
    naming: &naming::NamingAllocator,
    r: &dyn DialectRenderer,
) -> BackupRollbackPair {
    let table = extract_target_table(stmt);           // 从 alter_table / drop_object / truncate 等取
    let bks_name = naming.alloc(&table);              // bks_users_20260731_0001
    let shadow_name = format!("_rb_{}_{}", seq_suffix(seq), table.trim_backticks()); // _rb_0001_users（★ D7：seq 嵌入前缀）

    // ★ F13 生成 schema 校验语句（真 SQL，会 SIGNAL，非注释）
    let schema_check = build_schema_check(&table, &bks_name, r);  // 见下方 build_schema_check

    // ★ F15 分区表检测语句
    let partition_check = build_partition_check(&table, r);

    // 视图与表分支
    let is_view = matches!(stmt.kind.as_str(), "DROP_VIEW");

    let (backup, rollback) = if is_view {
        // 视图无锁、无 LIKE、无 RENAME 切换
        (
            format!("CREATE VIEW {} AS SELECT * FROM {};",
                r.quote_ident(&bks_name), r.quote_ident(&table)),
            format!("CREATE OR REPLACE VIEW {} AS SELECT * FROM {};",
                r.quote_ident(&table), r.quote_ident(&bks_name)),
        )
    } else {
        // ★ backup.sql：幂等 DROP+CREATE + FLUSH TABLES WITH READ LOCK（MySQL）/ BEGIN+REPEATABLE READ（PG）
        let backup = build_idempotent_backup(&table, &bks_name, r);
        // ★ rollback.sql：按"语句类型 × 方言"走三策略之一（见 AtomicStrategy 枚举）
        let rollback = match r.atomic_ddl_rollback_strategy(&stmt.kind) {
            AtomicStrategy::AtomicRename => build_atomic_rename_rollback(
                &table, &bks_name, &shadow_name, r),
            AtomicStrategy::Transactional => build_transactional_rollback(
                &table, &bks_name, r),
            // ★ DROP TABLE 特例：原表已不存在，无法 RENAME，直接从 bks_ 重建
            // 非原子，CREATE 失败则原表无法恢复，safety.irreversible_if_backup_missing = true
            AtomicStrategy::RebuildFromBackup => build_rebuild_from_backup_rollback(
                &table, &bks_name, r),
        };
        (backup, rollback)
    };

    // PG 在 INCLUDING CONSTRAINTS 下会保留 CHECK 约束，partial 警告文案随方言调整
    let warning = r.partial_like_warning();

    // ★ N3 修正：expected_schema 一律 Some，row_count 占位 0（生成期不可知），
    //   由发布平台在 backup 段执行 SELECT COUNT(*) 后回写 manifest.expected_schema.row_count。
    //   若回写后仍为 0 或与 rollback 段 bks_ 行数比对不一致，按 assert_on_schema_mismatch 决策。
    //   columns 从 AST 推断（CREATE TABLE 上下文）或留空 Vec（无上下文时仅做表存在/行数校验）。
    let expected_schema = ExpectedSchema {
        table_exists: true,  // 生成期假设原表存在，执行期 SELECT 校验
        row_count: 0,        // ★ 占位，发布平台回写
        columns: extract_expected_columns(stmt),  // 从 CREATE TABLE 上下文推断，无则空 Vec
    };

    BackupRollbackPair {
        seq,
        source: SourceRef::placeholder(),
        original_sql: original.to_string(),
        backup: Some(format!("{}\n{}", schema_check, backup)),
        rollback: Some(format!("{}\n{}", schema_check, rollback)),
        // ★ N2 修正：嵌套 SafetyClass / BackupStrategy（取代散装 flag）
        safety: SafetyClass {
            reliable: true,
            partial: true,           // 外键/触发器在 LIKE 中不保留
            irreversible: false,
            counter_unrestored: has_auto_increment_or_serial(stmt),
            requires_lock: true,
            // ★ N8 修正：lock_type 填实际锁型（FTWRL/SNAPSHOT/TABLE_UNSAFE/NONE），不是方言名
            //   由 resolve_lock_scope(cfg, stmt) 决定：global→FTWRL, snapshot→SNAPSHOT, table→TABLE_UNSAFE, none→NONE
            lock_type: Some(resolve_lock_type(cfg, stmt)),  // "FTWRL" / "SNAPSHOT" / "TABLE_UNSAFE" / "NONE"
            partitioned: false,      // F15 检测后回填
            irreversible_if_backup_missing: matches!(
                r.atomic_ddl_rollback_strategy(&stmt.kind),
                AtomicStrategy::RebuildFromBackup
            ),
        },
        strategy: BackupStrategy {
            backup_mode: "full".to_string(),
            incremental_source: None,
            column_constraints_check: None,
        },
        expected_schema: Some(expected_schema),
        warnings: vec![warning],
    }
}

/// ★ 甲-2 修正：幂等 backup 段（DROP+CREATE + FLUSH TABLES WITH READ LOCK）
///
/// MySQL 输出示例：
///   FLUSH TABLES WITH READ LOCK;
///   DROP TABLE IF EXISTS `bks_users_20260731_0001`;
///   CREATE TABLE `bks_users_20260731_0001` LIKE `users`;
///   INSERT INTO `bks_users_20260731_0001` SELECT * FROM `users`;
///   UNLOCK TABLES;
///
/// ★ 乙-2/N4 修正：必须用**全局** `FLUSH TABLES WITH READ LOCK`（无表名）而非 `LOCK TABLES t READ` 或 per-table 形式。
/// 原因：MySQL 的 CREATE/DROP TABLE 触发隐式提交，会释放 LOCK TABLES 持有的锁，
///      导致随后的 INSERT SELECT 在无锁状态跑，并发写入漏进备份。
///      FLUSH TABLES WITH READ LOCK 是全局读锁，不被隐式提交释放，能覆盖 CREATE/INSERT 全程。
///      代价：持锁期间全库只读，必须在维护窗口执行。
///
/// PG 输出示例（外层 BEGIN + REPEATABLE READ，不双重 BEGIN，见甲-4）：
///   -- 由 render.rs 统一在外层包裹 BEGIN; ... COMMIT;
///   -- 段内只发出 LOCK TABLE "users" IN ACCESS SHARE MODE;（PG 锁不被 DDL 释放）
///   DROP TABLE IF EXISTS "bks_users_20260731_0001";
///   CREATE TABLE "bks_users_20260731_0001" (LIKE "users" INCLUDING ...);
///   INSERT INTO "bks_users_20260731_0001" SELECT * FROM "users";
fn build_idempotent_backup(table: &str, bks: &str, r: &dyn DialectRenderer) -> String {
    r.render_idempotent_backup(table, bks)  // 方言渲染器封装锁类型与语法
}

/// ★ 甲-2 修正：F12 原子 RENAME 切换 rollback（MySQL）
///
/// 输出示例：
///   SET FOREIGN_KEY_CHECKS=0;                           -- 乙-3：避免外键拓扑破坏
///   CREATE TABLE `_rb_0001_users` LIKE `bks_users_20260731_0001`;  -- ★ D7: _rb_<seq>_<table> 命名
///   INSERT INTO `_rb_0001_users` SELECT * FROM `bks_users_20260731_0001`;
///   RENAME TABLE `users` TO `users_old_0001`, `_rb_0001_users` TO `users`;
///   SET FOREIGN_KEY_CHECKS=1;
///   -- 清理由 cleanup.sql 处理：DROP TABLE `users_old_0001`;
///
/// ★ 乙-3 修正：RENAME 前后 SET FOREIGN_KEY_CHECKS=0/1。
/// 原因：RENAME 把 `users` 改名为 `users_old_0001`，其他表指向 `users` 的外键会指向 `users_old_0001`
///      （InnoDB FK 按 id 跟踪，不按名字），新 `users` 是新表对象，FK 不会自动迁移。
///      关闭 FK 检查避免 RENAME 失败；回滚后 FK 拓扑仍可能错乱，manifest 标 `partial: true` +
///      warning "FK topology may break after RENAME, manual verify required"。
fn build_atomic_rename_rollback(
    table: &str, bks: &str, shadow: &str, r: &dyn DialectRenderer
) -> String {
    r.render_atomic_rename_rollback(table, bks, shadow)
}

/// PG 事务内 rollback（PG DDL 可事务化，无需 RENAME 切换）
///   -- 外层 BEGIN/COMMIT 由 render.rs 统一包裹
///   DROP TABLE IF EXISTS "users";
///   CREATE TABLE "users" (LIKE "bks_users_20260731_0001" INCLUDING ...);
///   INSERT INTO "users" SELECT * FROM "bks_users_20260731_0001";
fn build_transactional_rollback(table: &str, bks: &str, r: &dyn DialectRenderer) -> String {
    r.render_transactional_rollback(table, bks)
}

/// ★ RebuildFromBackup：DROP TABLE 回滚特例（两种方言均走此路径）
///
/// 原表已被 DROP，无法 RENAME 原表为 _old，只能直接从 bks_ 表 LIKE 重建。
/// 非原子：CREATE 失败则原表无法恢复，强依赖 bks_ 表存在。
///
/// MySQL 输出：
///   SET FOREIGN_KEY_CHECKS=0;                           -- 若其他表 FK 指向本表，需先关闭
///   CREATE TABLE `users` LIKE `bks_users_20260731_0001`;
///   INSERT INTO `users` SELECT * FROM `bks_users_20260731_0001`;
///   SET FOREIGN_KEY_CHECKS=1;
///
/// PG 输出（外层 BEGIN/COMMIT 由 render.rs 统一包裹，事务内重建失败 ROLLBACK）：
///   CREATE TABLE "users" (LIKE "bks_users_20260731_0001" INCLUDING ...);
///   INSERT INTO "users" SELECT * FROM "bks_users_20260731_0001";
fn build_rebuild_from_backup_rollback(table: &str, bks: &str, r: &dyn DialectRenderer) -> String {
    r.render_rebuild_from_backup_rollback(table, bks)
}

/// ★ C7 修正：F13 schema 校验语句，生成**只读 SELECT**（驱动友好），由发布平台解析结果后决策。
///
/// 原 §4.6.1 用 `DELIMITER // CREATE PROCEDURE ... CALL`（MySQL CLI 专有）+ `DO $$ ... $$`（PG），
/// 但 `DELIMITER` 是 mysql CLI 命令，JDBC / Python mysql-connector / Go mysql-driver 等驱动
/// **不支持**，发布平台用驱动执行会直接语法报错。改为生成纯 SELECT，发布平台执行后判断 check_result。
///
/// MySQL 示例（每段 backup 开头生成）：
///   -- sqlguard schema check: table exists (expected: 1)
///   SELECT IF(EXISTS(
///     SELECT 1 FROM information_schema.tables
///     WHERE table_schema=DATABASE() AND table_name='users'
///   ), 1, 0) AS `sqlguard_check_table_exists`;
///   -- sqlguard schema check: row count (compare with rollback-time value)
///   SELECT COUNT(*) AS `sqlguard_check_row_count` FROM `users`;
///   -- sqlguard schema check: column type for phone (expected: varchar)
///   SELECT data_type AS `sqlguard_check_col_phone`
///   FROM information_schema.columns
///   WHERE table_schema=DATABASE() AND table_name='users' AND column_name='phone';
///
/// PG 示例：
///   SELECT EXISTS(
///     SELECT 1 FROM information_schema.tables
///     WHERE table_schema=current_schema() AND table_name='users'
///   ) AS sqlguard_check_table_exists;
///   SELECT COUNT(*) AS sqlguard_check_row_count FROM "users";
///
/// ★ 执行期契约（C8 修正，render.rs 接入）：
/// - backup 段末尾追加 `SELECT COUNT(*) AS sqlguard_check_row_count FROM <table>`
///   发布平台执行后记录到 manifest 的 expected_schema.row_count
/// - rollback 段开头追加 `SELECT COUNT(*) AS sqlguard_check_bks_row_count FROM <bks_table>`
///   发布平台比对两值，按 assert_on_schema_mismatch 决策
/// - 手动 `mysql <` 执行时，SELECT 输出可见但不阻断；DBA 需自行核对（文档明示此限制）
/// - assert_on_schema_mismatch = "abort" 仅在发布平台层生效（解析 SELECT 结果后停止）
fn build_schema_check(table: &str, bks: &str, r: &dyn DialectRenderer) -> String {
    r.render_schema_check(table, bks)
}

/// F15 分区表检测语句（只读 SELECT，驱动友好）
///   SELECT partition_method AS sqlguard_check_partition
///   FROM information_schema.partitions
///   WHERE table_schema=DATABASE() AND table_name='users';
/// 发布平台若返回非空即分区表，按 on_partitioned_table 决策
fn build_partition_check(table: &str, r: &dyn DialectRenderer) -> String {
    r.render_partition_check(table)
}
```

**与 `ddl.rs` 的分工**：
- `ddl.rs` 处理**纯反向 DDL**（CREATE TABLE/INDEX/VIEW、ALTER ADD INDEX/PK/RENAME、**无约束的 ADD COLUMN**）— 回滚只需简单反向，不需备份
- `ddl_like.rs` 处理**需要原表定义的 DDL**（DROP、ALTER DROP/MODIFY、TRUNCATE、**带约束的 ADD COLUMN** 见 F17）— 统一走 LIKE + 原子切换

### 4.6.2 方言适配层 — `DialectRenderer` trait

封装所有方言差异，生成器只面向 trait 编程，新增方言（v2 Oracle / SQL Server）只需实现该 trait。

```rust
// src/rollback/dialect.rs

use std::str::FromStr;

/// 支持的方言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    MySql,
    PostgreSql,
}

impl FromStr for Dialect {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "mysql" | "mariadb" => Ok(Dialect::MySql),
            "postgres" | "postgresql" | "pg" => Ok(Dialect::PostgreSql),
            other => Err(format!("Unsupported dialect: {} (supported: mysql, postgresql)", other)),
        }
    }
}

/// ★ DDL 回滚策略枚举（三方言分化，取代原"两变体"枚举漏掉的 DROP TABLE 重建分支）
///
/// - `AtomicRename`：MySQL DDL 回滚主力路径。先建影子表 + 灌数据，再 `RENAME TABLE t TO t_old, shadow TO t`
///   原子切换，原表保留为 `t_old` 兜底。适用于：ALTER DROP/MODIFY COLUMN、DROP INDEX、DROP PRIMARY KEY 等
///   原表仍存在的场景。
/// - `Transactional`：PG DDL 可事务化，事务内 `DROP + CREATE LIKE + INSERT`，失败 ROLLBACK。
///   适用于 PG 所有需要原表定义的 DDL 回滚。
/// - `RebuildFromBackup`：DROP TABLE 特例。原表已被 DROP，无法 RENAME 原表为 _old，
///   只能直接 `CREATE TABLE t LIKE bks_t; INSERT INTO t SELECT * FROM bks_t;`。
///   非原子：CREATE 失败则原表无法恢复，强依赖 bks_ 表存在。manifest 标 `irreversible_if_backup_missing: true`。
///   两种方言均走此路径（PG 虽可事务化，但原表已不存在，事务内重建仍是"从 bks_ 重建"语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicStrategy {
    AtomicRename,
    Transactional,
    RebuildFromBackup,
}

/// 方言渲染器。所有 SQL 文本拼接都通过该 trait 完成，保证生成器主逻辑方言无关。
pub trait DialectRenderer: Sync {
    /// 引用标识符（MySQL: `` `name` `` / PG: `"name"`）
    fn quote_ident(&self, name: &str) -> String;

    /// CREATE TABLE 中的 LIKE 子句。
    /// - MySQL: `LIKE \`t\``
    /// - PG: `(LIKE "t" INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED)`
    fn create_table_like_clause(&self, source_table: &str) -> String;

    /// 事务开启语句（MySQL: `START TRANSACTION` / PG: `BEGIN`）
    fn begin_transaction(&self) -> &'static str;

    /// DROP INDEX 语句（MySQL: `DROP INDEX IF EXISTS i ON t` / PG: `DROP INDEX IF EXISTS i`）
    fn drop_index(&self, index_name: &str, table_name: &str) -> String;

    /// RENAME TABLE 语句（MySQL: `RENAME TABLE old TO new` / PG: `ALTER TABLE old RENAME TO new`）
    fn rename_table(&self, old: &str, new: &str) -> String;

    /// CREATE INDEX IF NOT EXISTS 语句
    /// （MySQL 8.0 不支持 IF NOT EXISTS，降级为 `CREATE INDEX i ON t (c)` + 容错；
    ///   PG 直接 `CREATE INDEX IF NOT EXISTS i ON t (c)`）
    fn create_index_if_not_exists(&self, index_name: &str, table: &str, columns: &[String]) -> String;

    /// ALTER TABLE ADD PRIMARY KEY 语句（语法双方言兼容，但 PG 要求列 NOT NULL）
    fn add_primary_key(&self, table: &str, columns: &[String]) -> String;

    /// ALTER TABLE DROP PRIMARY KEY 语句
    /// （MySQL: `ALTER TABLE t DROP PRIMARY KEY` / PG: `ALTER TABLE t DROP CONSTRAINT t_pkey`，需 PG 知道约束名）
    fn drop_primary_key(&self, table: &str, constraint_name: Option<&str>) -> String;

    /// partial LIKE 警告文案（双方言不复制内容不同）
    fn partial_like_warning(&self) -> String;

    /// 方言名称（用于 manifest 中记录）
    fn name(&self) -> &'static str;

    /// ★ DDL 回滚策略（按语句类型 + 方言决定，调用方传入语句类型辅助判断）
    /// - ALTER DROP/MODIFY COLUMN、DROP INDEX、DROP PRIMARY KEY：
    ///     MySQL → AtomicRename，PG → Transactional
    /// - DROP TABLE（原表已不存在）：两种方言均 → RebuildFromBackup
    fn atomic_ddl_rollback_strategy(&self, stmt_kind: &str) -> AtomicStrategy;
}

/// MySQL 渲染器。
pub struct MySqlRenderer;
impl DialectRenderer for MySqlRenderer {
    fn quote_ident(&self, name: &str) -> String { format!("`{}`", name.replace("`", "``")) }
    fn create_table_like_clause(&self, src: &str) -> String { format!("LIKE {}", self.quote_ident(src)) }
    fn begin_transaction(&self) -> &'static str { "START TRANSACTION" }
    fn drop_index(&self, idx: &str, tbl: &str) -> String {
        format!("DROP INDEX IF EXISTS {} ON {}", self.quote_ident(idx), self.quote_ident(tbl))
    }
    fn rename_table(&self, old: &str, new: &str) -> String {
        format!("RENAME TABLE {} TO {}", self.quote_ident(old), self.quote_ident(new))
    }
    fn create_index_if_not_exists(&self, idx: &str, tbl: &str, cols: &[String]) -> String {
        // MySQL 8.0 不支持 CREATE INDEX IF NOT EXISTS，用不带 IF NOT EXISTS 的写法
        // （运行期若已存在会报错，由 DBA 处理；生成时无法静态判断）
        let cols_str = cols.iter().map(|c| self.quote_ident(c)).collect::<Vec<_>>().join(", ");
        format!("CREATE INDEX {} ON {} ({})", self.quote_ident(idx), self.quote_ident(tbl), cols_str)
    }
    fn add_primary_key(&self, tbl: &str, cols: &[String]) -> String {
        let cols_str = cols.iter().map(|c| self.quote_ident(c)).collect::<Vec<_>>().join(", ");
        format!("ALTER TABLE {} ADD PRIMARY KEY ({})", self.quote_ident(tbl), cols_str)
    }
    fn drop_primary_key(&self, tbl: &str, _constraint_name: Option<&str>) -> String {
        format!("ALTER TABLE {} DROP PRIMARY KEY", self.quote_ident(tbl))
    }
    fn partial_like_warning(&self) -> String {
        "外键约束、CHECK 约束、触发器、表注释未在 CREATE TABLE LIKE 中保留".to_string()
    }
    fn name(&self) -> &'static str { "mysql" }
    fn atomic_ddl_rollback_strategy(&self, stmt_kind: &str) -> AtomicStrategy {
        match stmt_kind {
            // DROP TABLE 原表已不存在，无法 RENAME，走从 bks_ 重建路径
            "DROP_TABLE" => AtomicStrategy::RebuildFromBackup,
            // 其他 DDL（ALTER DROP/MODIFY COLUMN、DROP INDEX、DROP PRIMARY KEY 等）原表仍在，走原子 RENAME
            _ => AtomicStrategy::AtomicRename,
        }
    }
}

/// PostgreSQL 渲染器。
pub struct PostgreSqlRenderer;
impl DialectRenderer for PostgreSqlRenderer {
    fn quote_ident(&self, name: &str) -> String { format!("\"{}\"", name.replace("\"", "\"\"")) }
    fn create_table_like_clause(&self, src: &str) -> String {
        format!(
            "({} INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED)",
            self.quote_ident(src)
        )
    }
    fn begin_transaction(&self) -> &'static str { "BEGIN" }
    fn drop_index(&self, idx: &str, _tbl: &str) -> String {
        // PG 的 DROP INDEX 不需要 ON table
        format!("DROP INDEX IF EXISTS {}", self.quote_ident(idx))
    }
    fn rename_table(&self, old: &str, new: &str) -> String {
        format!("ALTER TABLE {} RENAME TO {}", self.quote_ident(old), self.quote_ident(new))
    }
    fn create_index_if_not_exists(&self, idx: &str, tbl: &str, cols: &[String]) -> String {
        let cols_str = cols.iter().map(|c| self.quote_ident(c)).collect::<Vec<_>>().join(", ");
        format!("CREATE INDEX IF NOT EXISTS {} ON {} ({})", self.quote_ident(idx), self.quote_ident(tbl), cols_str)
    }
    fn add_primary_key(&self, tbl: &str, cols: &[String]) -> String {
        let cols_str = cols.iter().map(|c| self.quote_ident(c)).collect::<Vec<_>>().join(", ");
        format!("ALTER TABLE {} ADD PRIMARY KEY ({})", self.quote_ident(tbl), cols_str)
    }
    fn drop_primary_key(&self, tbl: &str, constraint_name: Option<&str>) -> String {
        // PG 主键约束名默认是 <table>_pkey，可显式指定
        let name = constraint_name.unwrap_or(&format!("{}_pkey", tbl));
        format!("ALTER TABLE {} DROP CONSTRAINT {}", self.quote_ident(tbl), self.quote_ident(name))
    }
    fn partial_like_warning(&self) -> String {
        "外键约束（REFERENCES）、触发器、表级权限、SEQUENCE（SERIAL 列）、表注释未在 LIKE ... INCLUDING 中保留；CHECK 约束已通过 INCLUDING CONSTRAINTS 保留".to_string()
    }
    fn name(&self) -> &'static str { "postgresql" }
    fn atomic_ddl_rollback_strategy(&self, stmt_kind: &str) -> AtomicStrategy {
        match stmt_kind {
            // DROP TABLE 原表已不存在，PG 也只能从 bks_ 重建（事务内重建，失败 ROLLBACK）
            "DROP_TABLE" => AtomicStrategy::RebuildFromBackup,
            // 其他 DDL 原表仍在，PG 走事务内 DROP+CREATE LIKE+INSERT
            _ => AtomicStrategy::Transactional,
        }
    }
}

/// 工厂函数：根据 Dialect 枚举返回对应渲染器。
pub fn renderer_for(d: Dialect) -> Box<dyn DialectRenderer> {
    match d {
        Dialect::MySql => Box::new(MySqlRenderer),
        Dialect::PostgreSql => Box::new(PostgreSqlRenderer),
    }
}
```

**方言差异对照表**：

| 场景 | MySQL | PostgreSQL |
|------|-------|-----------|
| 标识符引用 | `` `name` `` | `"name"` |
| CREATE TABLE LIKE | `CREATE TABLE new LIKE old` | `CREATE TABLE new (LIKE old INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED)` |
| 事务开启 | `START TRANSACTION` | `BEGIN` |
| DROP INDEX | `DROP INDEX i ON t` | `DROP INDEX i` |
| CREATE INDEX IF NOT EXISTS | 不支持（降级为 CREATE INDEX） | 原生支持 |
| RENAME TABLE | `RENAME TABLE old TO new` | `ALTER TABLE old RENAME TO new` |
| DROP PRIMARY KEY | `ALTER TABLE t DROP PRIMARY KEY` | `ALTER TABLE t DROP CONSTRAINT t_pkey` |
| CREATE VIEW 备份 | `CREATE VIEW bks_v AS SELECT * FROM v` | 同上（PG 视图只读，备份视图可行） |
| TRUNCATE | `TRUNCATE TABLE t` | `TRUNCATE TABLE t`（可在事务中） |
| CHECK 约束保留 | 不复制 | 复制（INCLUDING CONSTRAINTS） |
| SERIAL/AUTO_INCREMENT | AUTO_INCREMENT 随 LIKE 复制 | SERIAL 列底层 sequence 不复制 |

### 4.7 CLI 子命令

在 `src/cli.rs` 新增：

```rust
/// Generate backup.sql and rollback.sql for DDL/DML statements.
GenRollback {
    #[clap(default_value = ".")]
    path: PathBuf,

    #[clap(short, long, default_value = "sqlguard.toml")]
    config: PathBuf,

    #[clap(short, long, default_value = ".")]
    output_dir: PathBuf,

    /// 覆盖配置文件中的 [rollback].dialect。
    /// 可选值：mysql / postgresql
    #[clap(long, value_parser = ["mysql", "postgresql"])]
    dialect: Option<String>,

    /// Only generate for statements matching these types
    /// (ddl, dml, or specific kinds like insert,update,delete,alter_table).
    #[clap(long)]
    types: Option<String>,

    /// ★ D1 新增：覆盖配置文件中的 lock_scope，见 F14
    /// 可选值：auto / global / table / snapshot / none
    #[clap(long, value_parser = ["auto", "global", "table", "snapshot", "none"])]
    lock_scope: Option<String>,

    /// ★ D1 新增：FTWRL 等待超时（秒），覆盖配置文件，见 F18
    #[clap(long)]
    lock_timeout: Option<u64>,

    /// ★ R2 新增：lock_scope=table 模式必须显式确认（接受隐式提交释放风险）
    /// 不带此 flag 且 lock_scope=table 时报错退出
    #[clap(long)]
    accept_table_lock_risk: bool,
},

/// Incremental mode: only generate for SQL changed since a git baseline.
GenRollbackDiff {
    #[clap(long)]
    base: String,

    #[clap(default_value = ".")]
    path: PathBuf,

    #[clap(short, long, default_value = "sqlguard.toml")]
    config: PathBuf,

    #[clap(short, long, default_value = ".")]
    output_dir: PathBuf,

    #[clap(long, value_parser = ["mysql", "postgresql"])]
    dialect: Option<String>,

    #[clap(long)]
    types: Option<String>,
},

/// ★ D5 新增：清理已过期的备份表，见 F19
CleanupExpired {
    #[clap(short, long, default_value = ".")]
    output_dir: PathBuf,

    /// 覆盖配置文件中的 backup_table_retention_days
    #[clap(long)]
    retention_days: Option<u64>,

    /// 仅打印将清理的表名，不生成 cleanup 脚本
    #[clap(long)]
    dry_run: bool,
},
```

`main.rs` 增加 `run_gen_rollback` 与 `run_gen_rollback_diff`，复用 `load_config` / `resolve_absolute_path`，扫描逻辑与 `check_files` 同构（抽出 `collect_files` 共享函数）。

**方言解析顺序**：CLI `--dialect` > 配置文件 `[rollback].dialect` > 默认 `"mysql"`。最终通过 `Dialect::from_str` 解析，未知值报错退出。

### 4.8 文件渲染（★ 甲-4 修正：PG 不双重 BEGIN）

`src/rollback/render.rs` 负责把 `Vec<BackupRollbackPair>` 渲染为最终文本。

**★ 事务包裹策略（必须严格按方言分化，避免 PG 嵌套 BEGIN / MySQL DDL 隐式提交）**：
- **MySQL 方言**：**不包裹外层事务**（DDL 隐式提交对外层事务无效，包了也是假象）；DML 语句由生成器在段内自行加 `START TRANSACTION; ... COMMIT;`（DML 不隐式提交，事务有效）
- **PostgreSQL 方言**：**整份脚本外层包裹 `BEGIN; ... COMMIT;`**（PG DDL 可事务化）；段内**不再发 BEGIN**，只发 `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;`（在外层 BEGIN 之后立即发一次）+ `LOCK TABLE t IN ACCESS SHARE MODE;`（每段发）
- 配置项 `wrap_transaction` 仅对 PG 生效，MySQL 方言忽略此配置（始终不包外层）

**★ D6 锁合并策略（coalesce_locks）**：
- **问题**：50 条 ALTER 同表操作会触发 50 次 FTWRL/UNLOCK 循环，RTO 不可接受。
- **修正**：render_backup 在渲染前对 `pairs` 做锁合并：
  1. 按 `lock_scope` 与目标表分组：连续的、同一张表、同一 lock_scope 的 backup 段合并为一个锁区间
  2. 合并后的锁区间只发一次 `FLUSH TABLES WITH READ LOCK;`（或对应模式），中间所有 backup 段共享该锁，末尾发一次 `UNLOCK TABLES;`
  3. **合并条件**（必须全部满足）：
     - 相邻两个 pair 的目标表相同（`extract_target_table(p1) == extract_target_table(p2)`）
     - 相邻两个 pair 的 lock_scope 相同
     - 前一个 pair 的 backup 不含跨表依赖（如 `INSERT INTO bks_t SELECT * FROM other_table`）
     - DBA 显式启用 `coalesce_locks = true`（默认 true，可关闭以保留逐段锁的隔离性）
  4. 不满足条件时回退为逐段加锁
- **示例**（50 条 ALTER 同表，coalesce 后）：
  ```sql
  FLUSH TABLES WITH READ LOCK;
  -- @@SEQ: 00000001 @@ backup for ALTER 1
  DROP TABLE IF EXISTS `bks_users_20260731_0001`; CREATE TABLE ... LIKE `users`; INSERT ...;
  -- @@SEQ: 00000002 @@ backup for ALTER 2
  DROP TABLE IF EXISTS `bks_users_20260731_0002`; CREATE TABLE ... LIKE `users`; INSERT ...;
  -- ... 50 段共享一次锁
  UNLOCK TABLES;
  ```
- **风险**：合并后单次锁持有时长延长，但避免 50 次锁循环的累计开销。manifest 标记 `coalesced_lock_count: N`，发布平台提示预期锁时长。
- **新增配置项**：`coalesce_locks = true`（默认）+ manifest 字段 `coalesced_lock_count`

```rust
pub fn render_backup(
    pairs: &[BackupRollbackPair],
    cfg: &RollbackConfig,
    r: &dyn DialectRenderer,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("-- Auto-generated by sqlguard gen-rollback (dialect: {})\n", r.name()));
    s.push_str("-- Run BEFORE applying the change scripts\n\n");

    // ★ 甲-4 修正：仅 PG 包外层事务；MySQL 不包（DDL 隐式提交无效）
    let wrap_outer = cfg.wrap_transaction && r.name() == "postgresql";
    if wrap_outer {
        s.push_str("BEGIN;\n");
        s.push_str("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;\n\n");
    }

    // ★ 乙-1 / D2 / R4 修正：MySQL backup.sql 头部按 binlog_strategy 决定是否加 SET SESSION sql_log_bin=0
    // - binlog_strategy = "always"：加 sql_log_bin=0
    // - binlog_strategy = "never"：不加
    // - binlog_strategy = "auto" + v2 --connect：检测 GTID 后决定（v2 实现）
    // - binlog_strategy = "auto" + v1 静态 + 脚本含 DDL：报错退出（在 generate 阶段已校验，render 不会到此处）
    // - binlog_strategy = "auto" + v1 静态 + 纯 DML：加 sql_log_bin=0 + warning
    let has_ddl = pairs.iter().any(|p| is_ddl_stmt(&p.original_sql));
    let should_disable_binlog = r.name() == "mysql" && match cfg.binlog_strategy.as_str() {
        "always" => true,
        "never" => false,
        "auto" => {
            if has_ddl {
                // ★ R4：generate 阶段已校验应报错退出，render 防御性检查
                panic!("binlog_strategy=auto + static mode + DDL should have been rejected in generate phase");
            }
            s.push_str("-- WARN: binlog_strategy=auto, GTID mode unknown (static mode), sql_log_bin=0 may affect row-based replication, verify before execution\n");
            true
        }
        _ => true,  // 未知值兜底为 always
    };
    if should_disable_binlog {
        s.push_str("SET SESSION sql_log_bin=0;\n\n");
    }

    // ★ N10/R3 修正：MySQL backup.sql 头部发射长事务 + 长查询预检查 SELECT（F18）
    //   发布平台执行后按 on_long_transaction 决策；PG 侧 pg_stat_activity 已覆盖
    let effective_lock_scope = resolve_lock_scope(cfg, pairs);  // auto → global/snapshot
    if r.name() == "mysql" && effective_lock_scope == "global" {
        s.push_str(&format!(
            "-- sqlguard pre-check: abort if long transactions (> {}s) exist (F18)\n",
            cfg.long_transaction_threshold
        ));
        s.push_str(&format!(
            "SELECT COUNT(*) AS {} FROM information_schema.innodb_trx WHERE TIMESTAMPDIFF(SECOND, trx_started, NOW()) > {};\n",
            r.quote_ident("sqlguard_check_long_tx"),
            cfg.long_transaction_threshold
        ));
        s.push_str(&format!(
            "-- sqlguard pre-check: abort if long queries (> {}s, non-Sleep) exist (R3)\n",
            cfg.long_transaction_threshold
        ));
        s.push_str(&format!(
            "SELECT COUNT(*) AS {} FROM information_schema.processlist WHERE TIME > {} AND COMMAND != 'Sleep';\n\n",
            r.quote_ident("sqlguard_check_long_queries"),
            cfg.long_transaction_threshold
        ));
        // ★ N12/R7：lock_wait_timeout 作 best-effort 兜底（FTWRL 行为不完全一致）
        if cfg.lock_timeout > 0 {
            s.push_str(&format!(
                "-- sqlguard: best-effort lock timeout (FTWRL behavior varies, platform should also kill on timeout)\n"
            ));
            s.push_str(&format!(
                "SET SESSION lock_wait_timeout = {};\n\n",
                cfg.lock_timeout
            ));
        }
    } else if r.name() == "postgresql" && effective_lock_scope == "global" {
        s.push_str(&format!(
            "-- sqlguard pre-check: abort if long transactions (> {}s) exist (F18)\n",
            cfg.long_transaction_threshold
        ));
        s.push_str(&format!(
            "SELECT COUNT(*) AS {} FROM pg_stat_activity WHERE state = 'active' AND now() - query_start > interval '{} seconds';\n\n",
            r.quote_ident("sqlguard_check_long_tx"),
            cfg.long_transaction_threshold
        ));
        if cfg.lock_timeout > 0 {
            s.push_str(&format!("SET LOCAL statement_timeout = '{}s';\n\n", cfg.lock_timeout));
        }
    }

    // ★ N9/R6 修正：coalesce_locks 实现（保守策略，仅 DDL 全表 LIKE 走 coalesce）
    //   - conservative（默认）：仅当 backup 段是 DDL 全表 LIKE（INSERT SELECT * FROM t 无 WHERE）时合并
    //     DML 增量备份（含 WHERE 子句，可能 JOIN/子查询）不合并
    //   - aggressive：所有同表段都尝试合并（DBA 显式启用）
    //   - 合并后：1 次 FTWRL + N 段 backup + 1 次 UNLOCK
    let coalesce_mode = cfg.coalesce_locks_mode.as_str();  // "conservative" | "aggressive"
    let groups = if cfg.coalesce_locks {
        group_for_coalesce(pairs, coalesce_mode, &effective_lock_scope)
    } else {
        pairs.iter().filter(|p| p.backup.is_some())
            .map(|p| vec![p]).collect::<Vec<_>>()  // 每段独立
    };

    for group in &groups {
        // ★ 同组共享一次锁区间（global 模式发 FTWRL/UNLOCK，snapshot 模式发 START TX/COMMIT）
        let lock_emitted = group.len() > 1 && effective_lock_scope == "global";
        if lock_emitted {
            s.push_str("-- sqlguard: coalesced lock region (shared FTWRL across same-table segments)\n");
            s.push_str("FLUSH TABLES WITH READ LOCK;\n");
        }
        for p in group {
            s.push_str(&format!("-- @@SEQ: {:08} @@\n", p.seq));
            s.push_str(&format!("-- source: {}:{}\n", p.source.file, p.source.line));
            s.push_str(&format!("-- original: {}\n", one_line(&p.original_sql)));
            if !p.warnings.is_empty() {
                for w in &p.warnings {
                    s.push_str(&format!("-- WARN: {}\n", w));
                }
            }
            // ★ coalesce 模式下，段内的 FTWRL/UNLOCK 由 render 统一发，build_idempotent_backup 输出应剥离锁
            //   非 coalesce 模式下，段内自带锁（build_idempotent_backup 内嵌）
            let backup_sql = if lock_emitted {
                strip_lock_statements(p.backup.as_ref().unwrap())  // 剥离段内 FTWRL/UNLOCK
            } else {
                p.backup.as_ref().unwrap().clone()
            };
            s.push_str(&backup_sql);
            s.push_str("\n");
            // ★ C8/N3：行数记录 SELECT 始终输出
            if let Some(es) = &p.expected_schema {
                s.push_str(&format!(
                    "-- sqlguard: record row_count for rollback comparison (placeholder: {}, platform overwrites after execution)\n",
                    es.row_count
                ));
                s.push_str(&format!(
                    "SELECT COUNT(*) AS {} FROM {};\n\n",
                    r.quote_ident("sqlguard_check_row_count"),
                    r.quote_ident(&extract_target_table(p)),
                ));
            }
        }
        if lock_emitted {
            s.push_str("UNLOCK TABLES;\n\n");
        }
    }

    if wrap_outer {
        s.push_str("COMMIT;\n");
    }
    // ★ D2：恢复 sql_log_bin=1（与头部 D2 逻辑对称）
    if should_disable_binlog {
        s.push_str("SET SESSION sql_log_bin=1;\n");
    }
    s
}

pub fn render_rollback(
    pairs: &[BackupRollbackPair],
    cfg: &RollbackConfig,
    r: &dyn DialectRenderer,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("-- Auto-generated by sqlguard gen-rollback (dialect: {})\n", r.name()));
    s.push_str("-- Run AFTER a failed change to restore the previous state\n\n");

    let wrap_outer = cfg.wrap_transaction && r.name() == "postgresql";
    if wrap_outer {
        s.push_str("BEGIN;\n\n");
    }

    // ★ 后向顺序
    for p in pairs.iter().rev().filter(|p| p.rollback.is_some()) {
        s.push_str(&format!("-- @@SEQ: {:08} @@\n", p.seq));
        s.push_str(&format!("-- source: {}:{}\n", p.source.file, p.source.line));
        s.push_str(&format!("-- original: {}\n", one_line(&p.original_sql)));
        // ★ C2：从 SafetyClass 读取分类 flag（取代散装字段）
        if p.safety.irreversible {
            s.push_str("-- !! IRREVERSIBLE: manual review required !!\n");
        }
        if p.safety.irreversible_if_backup_missing {
            s.push_str("-- !! IRREVERSIBLE_IF_BACKUP_MISSING: bks_ table must exist, manual verify !!\n");
        }
        if !p.safety.reliable {
            s.push_str("-- !! UNRELIABLE: primary key missing, verify before execution !!\n");
        }
        if p.safety.partial {
            s.push_str("-- !! PARTIAL: structure not fully preserved, review required !!\n");
        }
        if p.safety.counter_unrestored {
            s.push_str("-- !! COUNTER_UNRESTORED: AUTO_INCREMENT/SEQUENCE not restored, manual fixup required !!\n");
        }
        if p.safety.requires_lock {
            s.push_str(&format!("-- !! REQUIRES_LOCK: {} will be held, notify stakeholders !!\n", p.safety.lock_type.as_deref().unwrap_or("unknown")));
        }
        // ★ C8 修正：rollback 段开头追加 bks 行数比对 SELECT，发布平台与 backup 段记录的 row_count 比对
        if let Some(es) = &p.expected_schema {
            if let Some(bks) = extract_bks_table(p) {  // 从 source/stmt 提取本段对应 bks_ 表名
                s.push_str(&format!(
                    "-- sqlguard: verify bks_ row_count matches backup-time value (expected: {})\n",
                    es.row_count
                ));
                s.push_str(&format!(
                    "SELECT COUNT(*) AS {} FROM {};\n",
                    r.quote_ident("sqlguard_check_bks_row_count"),
                    r.quote_ident(&bks),
                ));
            }
        }
        s.push_str(p.rollback.as_ref().unwrap());
        s.push_str("\n\n");
    }

    if wrap_outer {
        s.push_str("COMMIT;\n");
    }
    s
}
```

> ★ 乙-5 修正：**PostgreSQL 的 TRUNCATE 是事务性的**（与 MySQL 不同，PG 的 TRUNCATE 可在事务内执行，失败可 ROLLBACK）。原方案"PG TRUNCATE 隐式提交、不能放在事务里"的说法是事实错误。PG 方言下 TRUNCATE 正常包裹在外层事务内，失败整体 ROLLBACK。MySQL 的 TRUNCATE 才是 DDL 隐式提交，但 MySQL 方言本就不包外层事务，TRUNCATE 由 `ddl_like` 走全表 LIKE 备份 + 数据重灌路径（不依赖 TRUNCATE 自身的事务性）。

### 4.9 Mapper 模式接入

复用 `mapper::dynamic::parse_dynamic_statements` + `expand_variants`（`replay_export.rs` 已有用法），对每个变体 `parse_sql_to_ast` 后取首条 `StmtInfo`：

```rust
for stmt in &dyn_stmts {
    let variants = mapper::dynamic::expand_variants(
        stmt,
        mapper::dynamic::DEFAULT_MAX_INDEPENDENT_IFS,
    );
    for v in variants {
        let ast = parse_sql_to_ast(&v.sql);
        if let Some(first) = ast.statements.first() {
            let source = SourceRef {
                file: display_path(file_path, target_dir),
                line: stmt.raw_xml_line as i64,
                end_line: stmt.raw_xml_line as i64,
                statement_id: Some(stmt.statement_id.clone()),
                variant_label: if v.label.is_empty() { None } else { Some(v.label.clone()) },
            };
            let pair = generator.generate(first, source, &v.sql);
            pairs.push(pair);
        }
    }
}
```

### 4.10 增量模式（gen-rollback-diff）

复用 `git_diff::get_diff`，对每个改动文件的 hunks 应用与 `check-diff` 相同的 `[line, end_line] ∩ hunk` 过滤，只对命中改动的语句生成。

### 4.11 依赖与兼容性

- **不引入新 crate**：所有逻辑基于已有的 `sqlparser`、`serde`、`serde_json`、`toml`、`quick-xml`。
- `RollbackConfig` 全字段 `#[serde(default)]`，旧 `sqlguard.toml` 无需修改。
- `Config` 增加 `pub rollback: RollbackConfig` 字段，`Default` 实现保持启用默认值（`enabled = false`，避免现有用户意外生成）。
  > 注：`enabled` 默认 `false`，仅当用户运行 `gen-rollback` 子命令时才生效，互不影响。

### 4.12 测试策略

| 层级 | 范围 | 形式 |
|------|------|------|
| 单元测试 | `naming.rs` 序号分配、`pk.rs` 主键解析、`ddl.rs` 每种 DDL、`dml.rs` 每种 DML | `tests/rollback_*.rs`，输入固定 SQL 字符串，断言生成文本 |
| 方言单元测试 | `dialect.rs` 的 MySQL / PG 两个渲染器，对照表 11 项方言差异逐一断言 | `tests/rollback_dialect_test.rs` |
| 集成测试 | 端到端：准备 sql 目录 → 运行 `gen-rollback` → 比对 `backup.sql` / `rollback.sql` / `rollback-manifest.json` | `tests/integration_test.rs` 增加 `gen_rollback_*` 用例，**MySQL 和 PG 各跑一组** |
| Mapper 测试 | XML 输入 + 动态分支变体 | 单独 `tests/rollback_mapper_test.rs` |
| 回归测试 | 在原有 `check` 用例上附加 `gen-rollback`，确认不影响 check 行为 | 现有 `integration_test.rs` 加断言 |
| 黄金文件 | 准备 `tests/fixtures/rollback/expected/mysql/*.sql` 与 `expected/postgresql/*.sql` 两套，每次跑测试与生成的输出 diff | 防止渲染格式漂移，同时锁死方言差异 |

### 4.13 里程碑划分

| 阶段 | 范围 |
|------|------|
| M1：骨架 + 方言层 | `src/rollback/` 模块、`dialect.rs`（MySQL + PG 渲染器）、`GenRollback --dialect` CLI、`RollbackConfig`（含 `backup_mode`/`lock_tables_during_backup`/`disable_binlog_for_bks`/`assert_on_schema_mismatch`/`on_partitioned_table`）、空实现 + manifest 输出 |
| M2：DML 核心（增量 + 幂等 + 加锁） | INSERT / UPDATE / DELETE / TRUNCATE / REPLACE 的 backup + rollback（双方言验证），UPDATE/DELETE/REPLACE 按 WHERE 增量，幂等 DROP+CREATE，FLUSH TABLES WITH READ LOCK（MySQL）/ ACCESS SHARE MODE（PG）加锁 |
| M3：DDL 核心 | `ddl.rs` 纯反向 DDL（CREATE TABLE/INDEX/VIEW、ALTER ADD/RENAME），ADD COLUMN 含约束时降级全表（F17），注意 RENAME TABLE / DROP INDEX / DROP PRIMARY KEY 的双方言差异 |
| M4：★ `ddl_like.rs` + F12 原子切换 | DROP TABLE / DROP INDEX / DROP VIEW / TRUNCATE / DROP PRIMARY KEY / ALTER DROP/MODIFY COLUMN 全表 LIKE 模式；MySQL 走 F12 原子 RENAME 切换，PG 走事务内 DROP+CREATE LIKE+INSERT；ALTER 仅在脚本内含 CREATE TABLE 上下文时升级增量 |
| M5：校验与防护 | F13 schema 漂移校验语句生成、F14 并发锁、F15 分区表检测、F16 binlog 管控与权限 REVOKE、cleanup.sql 独立输出 |
| M6：Mapper | 集成 mapper::dynamic，按变体生成 |
| M7：增量 diff + 影子库校验 | `gen-rollback-diff`；内置 `verify-rollback` 子命令（影子库跑 backup→变更→rollback 比对行数/checksum，见验收 #14） |
| M8：完善 | 配置项完善、PG TRUNCATE 事务性已统一在外层事务包裹路径中（无需特殊处理，见乙-5）、文档与测试补齐、CI 阻断联动（reliable=false / partial=true 阻断发布） |

### 4.14 安全分类与 CI 退出码决策表（★ C4 修正：统一 flag 散装 → 决策表）

`SafetyClass` 的 8 个 flag 按"分类 → 触发条件 → CI 退出码 → 发布平台动作"统一决策如下。生成器填充 `SafetyClass`，渲染器按 flag 输出脚本注释，CI 按退出码阻断发布。

| SafetyClass flag | 触发条件 | CI 退出码 | 脚本注释 | 发布平台动作 |
|-----------------|---------|----------|---------|------------|
| `irreversible = true` | MERGE / UPSERT 等真正不支持的语句 | 2 | `-- !! IRREVERSIBLE` | 阻断发布，强制 DBA 介入 |
| `irreversible_if_backup_missing = true` | DROP TABLE 回滚（RebuildFromBackup 策略），CREATE 失败则原表无法恢复 | 2 | `-- !! IRREVERSIBLE_IF_BACKUP_MISSING` | 阻断发布，DBA 确认 bks_ 表存在后强制执行 |
| `reliable = false` | DML 主键缺失，回滚可能误改其他行 | 2 | `-- !! UNRELIABLE` | 阻断发布，DBA 配置 `[[rollback.primary_keys]]` 后重生成 |
| `partial = true` | 外键/CHECK/触发器未保留 / RENAME 后 FK 拓扑可能错乱 / REPLACE 新行无法回滚 | 2 | `-- !! PARTIAL` | 阻断发布，DBA 评估影响后 `--allow-partial` 强制 |
| `counter_unrestored = true` | AUTO_INCREMENT / SEQUENCE 当前值无法静态还原 | 1 | `-- !! COUNTER_UNRESTORED` | 警告继续，DBA 回滚后手工 `ALTER TABLE t AUTO_INCREMENT=N` / `setval()` |
| `requires_lock = true` | backup 段持锁（FTWRL / ACCESS SHARE） | 1 | `-- !! REQUIRES_LOCK: <lock_type>` | 警告继续，提示业务方预期锁时长与影响范围 |
| `partitioned = true` | 分区表（LIKE 生成非分区表，F15 检测） | 按 `on_partitioned_table` 配置：abort→2 / warn→1 / fallback→0 | `-- !! PARTITIONED` | abort 阻断；warn 警告；fallback 降级外部导出 |
| （以上全 false） | 完全可逆，无风险 | 0 | 无 | 正常发布 |

**CI 退出码汇总**（`gen-rollback` / `gen-rollback-diff` 子命令）：

| 退出码 | 含义 | manifest 汇总字段 |
|-------|------|------------------|
| 0 | 全部 reliable，无 partial / irreversible | `unreliable_count=0 && partial_count=0 && irreversible_count=0` |
| 1 | 有 warning（counter_unrestored / requires_lock），但回滚可靠 | `counter_unrestored_count>0 \|\| requires_lock_count>0` |
| 2 | 有 reliable=false / partial=true / irreversible=true，需 CI 阻断 | `unreversible_count>0 \|\| partial_count>0 \|\| irreversible_count>0` |

**`--fail-on-warning` 模式**：退出码 1 提升为 2（counter_unrestored / requires_lock 也阻断），用于严格发布流程。
**`--allow-partial` 模式**：`partial=true` 不提升退出码（DBA 已确认知情），但仍输出脚本注释。

---

## 五、风险与缓解（★ 已对照 DBA 评审意见全面修正）

| 风险 | 缓解 |
|------|------|
| **★ P0-1 MySQL DDL 隐式提交导致事务包裹无效** | MySQL 方言下 DDL 不包事务，改用 F12 原子 RENAME 切换（`RENAME TABLE t TO t_old, t_new TO t`），原表保留为 `t_old` 兜底；PG 方言保留事务包裹 |
| **★ P0-2 备份含敏感数据（PII）外传风险** | 删除"不含敏感数据"错误声明；backup.sql 头部按 `binlog_strategy` 决定是否加 `SET SESSION sql_log_bin=0`（★ D2/R4：auto + v1 静态 + 含 DDL 报错退出；纯 DML 加 warning；never 不加）；bks_ 表附加 `REVOKE ALL FROM PUBLIC; GRANT SELECT TO dba_reviewer`；生成的 SQL 文件按敏感文件存储传输 |
| **★ P0-3 ALTER DROP/MODIFY COLUMN 增量在纯静态下不可靠** | 默认全表 LIKE；仅当脚本内含该表 CREATE TABLE 上下文（可推断列类型 + 主键）时升级增量，manifest 标 `incremental_source: "create_table_context"`；`backup_mode = "auto"` 默认值 |
| **★ P0-4 AUTO_INCREMENT / SEQUENCE 计数器不保留** | LIKE 不复制计数器当前值，manifest 标 `counter_unrestored: true` + warning；DBA 手工 `ALTER TABLE t AUTO_INCREMENT=N` / `setval()`；v2 `--connect` 模式自动生成还原语句；验收标准不再声称"计数器完全恢复" |
| **★ P0-5 backup.sql 不幂等导致重跑数据翻倍** | 改为 `DROP TABLE IF EXISTS bks_xxx; CREATE TABLE bks_xxx LIKE t; INSERT ...`，每次重跑从干净状态开始 |
| **★ P1-6 离线生成 + 延后执行 schema 漂移** | F13 在每段操作前后生成 `information_schema` 校验语句（表存在/行数/列类型），发布平台按 `assert_on_schema_mismatch = abort\|warn\|ignore` 处理，默认 abort |
| **★ P1-7 备份表与业务表同故障域** | F16 明确边界：bks_ 表是"库内影子表"，定位快速回滚而非灾难恢复；`--export-mode external` 选项移至 v2（C5 修正，v1 不提供，避免承诺无法兑现）；v1 仅支持库内 bks_ 模式 |
| **★ P1-8 并发一致性缺失**（★ 乙-2/N4/D1 修正） | F14 多模式锁策略：`lock_scope = global`（v1 默认，FTWRL 全局读锁）/ `table`（需 `--accept-table-lock-risk`，标 `TABLE_UNSAFE`）/ `snapshot`（v2 演进，DDL 外提到事务前）/ `none`；manifest 标 `lock_type: "FTWRL"\|"SNAPSHOT"\|"TABLE_UNSAFE"\|"NONE"`；F18 长事务 + 长查询预检查 + `lock_wait_timeout` best-effort 兜底 |
| **★ P1-9 分区表 LIKE 生成非分区表** | F15 生成 `information_schema.partitions` 校验语句，按 `on_partitioned_table = abort\|warn\|fallback` 处理（默认 abort），manifest 标 `partitioned: true` + `irreversible: true` |
| **★ P1-10 ADD COLUMN 回滚不还原约束** | F17 检测 ADD COLUMN 语句是否含 NOT NULL/DEFAULT/COMMENT 子句；含则降级全表 LIKE + 原子切换，manifest 标 `column_constraints_check: "fallback_to_full"` |
| **★ P1-11 cleanup 默认 true 销毁证据** | 默认改 false，保留 bks_ 与 _old_NNNN 表；额外输出独立 cleanup.sql，DBA 确认回滚成功后手工执行 |
| `CREATE TABLE LIKE` 不复制外键/CHECK/触发器 | manifest 标 `partial: true`；**CI 阻断**（见 P2-3 联动机制）；v2 通过 `--connect` + `SHOW CREATE TABLE` / `pg_get_*` 补全 |
| **★ P2-1 MERGE/REPLACE 直接判不可逆太草率** | REPLACE INTO 走 `dml::gen_replace` 部分支持：静态备份被覆盖旧行（按 VALUES 的 pk 列表 `WHERE pk IN (...)`），但新行 pk 运行时才知，无法生成 DELETE 新行回滚 → manifest 标 `partial: true` + warning "REPLACE rollback incomplete"。v2 `--connect` 执行期捕获新行 pk 后完整回滚。MERGE/UPSERT 仍 unsupported |
| **★ P2-2 无执行正确性校验** | M7 阶段实现 `verify-rollback` 子命令：在影子库跑 backup→变更→rollback，比对行数/checksum；验收 #14 强制要求 |
| **★ P2-3 FK/触发器缺失应 CI 阻断而非仅 warning** | 与现有 check 规则联动：manifest 中 `partial=true` 或 `reliable=false` 的 item 触发 `gen-rollback --fail-on-warning` 退出码非零；发布平台按退出码阻断发布；可配置 `--warning-as-error` 提升具体 warning 类型为 error |
| **★ P2-4 reliable=false 归属不清** | 工具退出码区分：0=全部 reliable，1=有 warning 但生成成功，2=有 reliable=false/partial=true 需 CI 阻断；发布平台按退出码决策 |
| **★ 乙-5 修正：PostgreSQL TRUNCATE 是事务性的**（原方案描述有误） | PG 的 TRUNCATE 可在事务内执行，失败可 ROLLBACK，无需特殊处理；PG 方言下 TRUNCATE 与其他 DDL 一样走外层 `BEGIN/COMMIT` 包裹路径。**MySQL 的 TRUNCATE 才是 DDL 隐式提交**，但 MySQL 方言本就不包外层事务，TRUNCATE 由 `ddl_like` 走全表 LIKE 备份 + 数据重灌路径（不依赖 TRUNCATE 自身的事务性）。manifest 不再因 PG TRUNCATE 标 `irreversible=true` |
| PostgreSQL SERIAL 列 sequence 丢失 | manifest 标 `partial=true` + `counter_unrestored=true`，warnings 提示 `setval`；v2 自动生成 |
| PostgreSQL DROP PRIMARY KEY 需约束名 | 默认按 `<table>_pkey` 推断；可通过 `[[rollback.primary_key_constraints]]` 显式指定 |
| Mapper 动态分支组合爆炸 | 复用 `DEFAULT_MAX_INDEPENDENT_IFS=5`，超过输出 warning |
| 主键缺失导致回滚误改 | 标 `reliable=false`，CI 阻断（退出码 2）；DDL 场景因 LIKE 保留主键，DML 回滚可 JOIN bks_ 表自动定位 |
| 生成的 SQL 注入风险 | 备份表名白名单 `[a-zA-Z0-9_]`；标识符内嵌引号转义 `` ` `` → ` `` ``、`"` → `""` |
| 与 `bks_` 命名规则冲突 | 备份表统一 `bks_` 前缀，DDL004 不误报；自动生成的脚本不被 `check` 扫描（位于 `output_dir`） |
| **UPDATE/DELETE 无 WHERE** | 全表备份，标 `reliable=false`，CI 阻断（已有 DML 规则可配合） |

---

## 六、验收标准

1. `cargo build` 与 `cargo test` 全绿，不引入新 warning。
2. 对 `tests/fixtures/rollback/sample.sql`（含 12 条 DDL/DML）分别运行 `sqlguard gen-rollback --dialect mysql` 和 `sqlguard gen-rollback --dialect postgresql`，生成的 `backup.sql` / `rollback.sql` / `rollback-manifest.json` / `cleanup.sql` 与 `tests/fixtures/rollback/expected/{mysql,postgresql}/` 黄金文件**字节级一致**。
3. 在 MySQL 8.0 与 PostgreSQL 14+ 各起一个测试库，把生成的 `backup.sql` 与原变更脚本顺序执行，再执行 `rollback.sql`，数据库状态恢复到变更前（至少覆盖 INSERT / UPDATE / DELETE / **DROP COLUMN** / **DROP TABLE** / **DROP INDEX** / TRUNCATE / REPLACE 八种场景，**两种方言各跑一遍**）。
4. **★ P0-4 修正验收**：DROP TABLE 与 ALTER DROP COLUMN 场景下，回滚后表结构（含主键、索引、UNIQUE、字符集/encoding）完全恢复，**无需用户手工补全任何 SQL**。**AUTO_INCREMENT / SEQUENCE 当前值不在验收范围**，manifest 标 `counter_unrestored: true` 提示 DBA 手工还原（MySQL 与 PG 均需通过结构恢复）。
5. manifest 中 `partial_count > 0` 时，对应 item 的 `warnings` 包含外键/CHECK 未保留提示；`irreversible_count` 仅在 MERGE / UPSERT 等真正不支持的场景 > 0（★ 乙-5 修正：PG TRUNCATE 是事务性的，不再标 `irreversible=true`）。
6. 现有 `sqlguard check ./sql`、`sqlguard check-diff --base origin/main`、`sqlguard replay-export` 行为不变（现有集成测试零修改通过）。
7. Mapper 模式下的 `<update>` / `<delete>` 标签生成 backup/rollback，动态分支变体数量与 `replay-export` 一致。
8. `sqlguard.toml` 不写 `[rollback]` 段时，`gen-rollback` 子命令仍可工作（默认 mysql 方言）；其他子命令完全不受影响。
9. `--dialect` 参数接受 `mysql` / `mariadb` / `postgres` / `postgresql` / `pg` 大小写不敏感，其他值报错退出。
10. PostgreSQL 方言下生成的 SQL 不含反引号；MySQL 方言下生成的 SQL 不含双引号包裹的标识符（字符串字面量除外）。
11. **★ P0-3 修正验收**：默认 `backup_mode = "auto"` 下，UPDATE/DELETE/REPLACE 按 WHERE 备份受影响行（增量）；ALTER DROP/MODIFY COLUMN **默认全表**，manifest 该 item 的 `backup_mode` 为 `"full"`。仅当脚本内含该表 CREATE TABLE 上下文时，ALTER 升级增量，manifest 标 `backup_mode: "incremental"` + `incremental_source: "create_table_context"`。
12. **★ P0-5 幂等验收**：对同一输入连续运行两次 `gen-rollback`，第二次执行 `backup.sql` 后 bks_ 表行数与第一次一致（不翻倍）；backup.sql 每段含 `DROP TABLE IF EXISTS bks_xxx` 前缀。
13. **★ P0-1 原子切换验收**：MySQL 方言下，DROP COLUMN / DROP INDEX / DROP PRIMARY KEY 的 rollback.sql 含 `RENAME TABLE t TO t_old_NNNN, _rb_NNNN_t TO t` 原子切换语句（★ D7：影子表命名 `_rb_<seq>_<table>`），原表数据保留在 `t_old_NNNN`；PG 方言下 rollback.sql 头尾含 `BEGIN/COMMIT`。
14. **★ P2-2 影子库校验验收**：`sqlguard verify-rollback --shadow-dsn <dsn>` 子命令在影子库执行 backup→变更→rollback 全流程，比对每张表行数与 checksum（`CHECKSUM TABLE` / `pg_checksums`），全部一致才退出码 0；至少覆盖 DROP COLUMN / DROP TABLE / UPDATE / DELETE 四种场景。
15. **★ P1-6 schema 漂移验收**：backup.sql 与 rollback.sql 每段含 `information_schema` 校验语句（表存在/行数/列类型）；模拟 schema 漂移（手工 ALTER 加列后执行 rollback）时，发布平台按 `assert_on_schema_mismatch = abort` 停止执行。
16. **★ P1-8 并发验收**（★ 乙-2/N4/D1 修正，按 lock_scope 区分）：
    - `lock_scope = "global"`（v1 默认）：backup.sql 含**全局** `FLUSH TABLES WITH READ LOCK`（MySQL，无表名）/ `BEGIN + REPEATABLE READ` + 段内 `LOCK TABLE t IN ACCESS SHARE MODE`（PG），manifest 标 `lock_type: "FTWRL"`
    - `lock_scope = "snapshot"`：backup.sql 含 `START TRANSACTION WITH CONSISTENT SNAPSHOT`（DDL 外提到事务前），manifest 标 `lock_type: "SNAPSHOT"` + `snapshot_window_unprotected: true`
    - `lock_scope = "table"`：需 `--accept-table-lock-risk`，manifest 强制 `partial: true` + `lock_type: "TABLE_UNSAFE"`
    - `lock_scope = "none"`：manifest 标 `lock_type: "NONE"` + `partial: true`
    - 所有 global 模式：backup.sql 头部含长事务预检查 SELECT（`sqlguard_check_long_tx` + `sqlguard_check_long_queries`）+ `SET SESSION lock_wait_timeout`
17. **★ P1-9 分区表验收**：对分区表生成 rollback 时，backup.sql 含 `information_schema.partitions` 校验语句；`on_partitioned_table = abort` 时工具退出码非零 + warning。
18. **★ P1-11 cleanup 验收**：默认 `cleanup_backup_tables_after_rollback = false`，rollback.sql 不含 DROP bks_ 语句；工具额外生成 `cleanup.sql` 仅含**已过期** bks_ 表的 DROP（按 `backup_table_retention_days` 过滤），不含本脚本自身生成的未过期表。
19. **★ P2-3/P2-4 CI 联动验收**：manifest 含 `reliable=false` 或 `partial=true` 的 item 时，`gen-rollback` 退出码为 2；`--fail-on-warning` 模式下 warning 也提升为退出码 2；发布平台按退出码阻断发布。
20. **★ D1/R5 锁策略验收**：`lock_scope = "auto"` 下（v1），由于 backup 总含 `CREATE TABLE LIKE`（DDL），auto 对所有场景走 `global`（含 FTWRL）；v2 snapshot 模式（backup 无 DDL）走 `START TRANSACTION WITH CONSISTENT SNAPSHOT`；backup.sql 头部含长事务预检查 SELECT（`sqlguard_check_long_tx` + `sqlguard_check_long_queries`）+ `SET SESSION lock_wait_timeout`；`lock_scope = "table"` 缺 `--accept-table-lock-risk` 时报错退出。
21. **★ D2/R4 GTID 验收**：`binlog_strategy = "auto"` + v1 静态 + 脚本含 DDL → 直接报错退出（exit code 2），错误信息提示显式设 `never` 或 `always`；`binlog_strategy = "auto"` + v1 静态 + 纯 DML → backup.sql 含 `SET SESSION sql_log_bin=0` + warning；`binlog_strategy = "never"` 时 backup.sql 不含 `SET SESSION sql_log_bin=0`。
22. **★ D5 生命周期验收**：`sqlguard cleanup-expired --output-dir <dir> --dry-run` 扫描 manifest 后打印出生成日期早于 `retention_days` 的 bks_ 表名；非 dry-run 模式生成 `cleanup-expired-YYYYMMDD.sql`。
23. **★ D6 锁合并验收**：`coalesce_locks = true` 下，对同表 50 条 ALTER 生成的 backup.sql 只含 1 次 `FLUSH TABLES WITH READ LOCK` 与 1 次 `UNLOCK TABLES`，中间 50 段共享锁；manifest 含 `coalesced_lock_count: 50`。
24. **★ D7 命名验收**：rollback.sql 中影子表命名为 `_rb_<seq>_<table>`（如 `_rb_0001_users`），seq 在前缀；同表多次回滚的影子表名按 seq 全局唯一可排序。

---

## 七、未来演进（不在本期）

- v2：`--connect <dsn>` 选项，运行时调用 `SHOW CREATE TABLE`（MySQL）/ `pg_get_tabledef` / `pg_get_viewdef`（PG）拿到完整 DDL（含外键、CHECK、触发器、SEQUENCE、AUTO_INCREMENT 当前值），用于补全 `CREATE TABLE LIKE` 模式遗漏的部分，让 `partial=true` 升级为 `partial=false`、`counter_unrestored=true` 升级为 `false`。自动生成 PG 的 `setval` 与 MySQL 的 `ALTER TABLE t AUTO_INCREMENT=N` 语句恢复计数器。
- v2：扩展方言支持 — Oracle / SQL Server / SQLite，复用 `DialectRenderer` trait。
- v2：大表部分备份增强 — 自定义 `--where "create_at > '2025-01-01'"` / `--limit 10000` 选项，允许用户对 DROP TABLE 等全表场景也指定过滤条件。
- v2：与发布平台集成，直接上传 `rollback-manifest.json` 到发布单，按 seq 索引回滚脚本；CI 按退出码阻断发布。
- v3：执行计划比对 — 在镜像库上跑原 SQL 与 rollback SQL，验证回滚后表数据一致（接入 `replay` 模块，扩展为 `verify-rollback` 子命令的强化版）。
- v3：跨语句事务感知，识别 `BEGIN ... COMMIT` 块，按事务粒度而非语句粒度生成回滚。
