# SqlGuard 默认规则手册

SqlGuard 内置 16 条默认规则，分为 **P0（8 条，默认启用）** 和 **P1（8 条，默认禁用）** 两档。

## 规则总览

| 编号 | 名称 | 分组 | 严重度 | P0/P1 | 对标来源 |
|------|------|------|--------|-------|----------|
| DDL001 | `no_drop_table` | ddl-safety | error | P0 | — |
| DDL002 | `primary_key_required` | ddl-safety | warning | P0 | — |
| DML001 | `no_select_all` | dml-safety | error | P0 | SQLFluff / GoSQLX |
| DML002 | `no_delete_update_without_where` | dml-safety | error | P0 | GoSQLX |
| DML003 | `insert_columns_required` | dml-safety | error | P0 | SQLFluff AM07 |
| DML004 | `subquery_alias_required` | dml-style | error | P0 | SQLFluff AL10 |
| DML005 | `column_references_qualified` | dml-style | warning | P0 | SQLFluff RF02 |
| DML006 | `no_join_without_condition` | dml-safety | error | P0 | SQLFluff AM05 |
| DML101 | `no_unused_join` | dml-performance | warning | P1 | SQLFluff ST11 |
| DML102 | `no_unused_cte` | dml-performance | warning | P1 | SQLFluff ST03 |
| DML103 | `use_is_null` | dml-convention | error | P1 | SQLFluff CV05 |
| DML104 | `use_coalesce` | dml-convention | warning | P1 | SQLFluff CV02 |
| DML105 | `no_order_by_in_subquery` | dml-performance | warning | P1 | SQLFluff AM03 |
| DML106 | `union_all_preferred` | dml-performance | warning | P1 | SQLFluff AM02 |
| DML107 | `no_nested_case` | dml-convention | warning | P1 | SQLFluff ST04 |
| DML108 | `no_constant_where` | dml-convention | warning | P1 | SQLFluff ST10 |

---

## P0 规则（默认启用）

### DDL001 — `no_drop_table`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/ddl/no_drop_table.rhai` |
| 分组 | `ddl-safety` |
| 严重度 | `error` |
| 检测方式 | AST（精确） |

**校验原因**：DDL 脚本中不应包含 `DROP TABLE`，防止误删生产表。删除操作应通过专用迁移脚本执行，且需经过 review。

**反面案例**：
```sql
DROP TABLE users;
DROP TABLE IF EXISTS orders;
```

**正面案例**：
```sql
-- 无 DROP TABLE 语句；如需删除，使用迁移工具
CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100));
```

---

### DDL002 — `primary_key_required`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/ddl/primary_key_required.rhai` |
| 分组 | `ddl-safety` |
| 严重度 | `warning` |
| 检测方式 | AST（精确） |

**校验原因**：每张表都应有主键，确保行级别唯一标识、支持外键引用、优化查询性能。

**反面案例**：
```sql
CREATE TABLE users (
    id INT,
    name VARCHAR(100)
);
```

**正面案例**：
```sql
-- 列级主键
CREATE TABLE users (
    id INT PRIMARY KEY,
    name VARCHAR(100)
);

-- 表级主键
CREATE TABLE order_items (
    order_id INT,
    item_id INT,
    PRIMARY KEY (order_id, item_id)
);

-- 主键也可在 CREATE TABLE 之外、由后续 ALTER TABLE 补建（规则跨语句识别，不误报）
CREATE TABLE sessions (
    token      VARCHAR(255),
    user_id    INT,
    expires_at TIMESTAMP
);
ALTER TABLE sessions ADD PRIMARY KEY (token);

-- 带约束名的 ADD CONSTRAINT 同样识别
ALTER TABLE sessions ADD CONSTRAINT pk_sessions PRIMARY KEY (token);
```

**注意**：规则基于整个 SQL 文件的最终状态判断。
- `CREATE TABLE` 无内联主键、但同文件内有 `ALTER TABLE ... ADD PRIMARY KEY` 补建 → 视为合规（修复了旧版本把这种情况误报的漏洞）。
- `CREATE TABLE` 有内联主键、但同文件内 `ALTER TABLE ... DROP PRIMARY KEY` 移除 → 仍会报违规（最终无主键）。

---

### DML001 — `no_select_all`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_select_all.rhai` |
| 分组 | `dml-safety` |
| 严重度 | `error` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff / GoSQLX |

**校验原因**：`SELECT *` 会返回所有列，导致不必要的数据传输、隐式依赖表结构、破坏视图/物化视图的兼容性。应显式列出所需列。

**反面案例**：
```sql
SELECT * FROM users;
SELECT u.* FROM users u JOIN orders o ON u.id = o.user_id;
```

**正面案例**：
```sql
SELECT id, name, email FROM users;
SELECT u.id, u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id;
```

---

### DML002 — `no_delete_update_without_where`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_delete_update_without_where.rhai` |
| 分组 | `dml-safety` |
| 严重度 | `error` |
| 检测方式 | AST（精确） |
| 对标 | GoSQLX Safety |

**校验原因**：不带 WHERE 的 DELETE/UPDATE 会操作全表，通常是编码失误。如果确实需要清表，应使用 TRUNCATE（DELETE）或显式确认。

**反面案例**：
```sql
DELETE FROM users;
UPDATE users SET status = 'inactive';
```

**正面案例**：
```sql
DELETE FROM users WHERE id = 100;
DELETE FROM users WHERE created_at < '2024-01-01';
UPDATE users SET status = 'inactive' WHERE last_login IS NULL;
```

---

### DML003 — `insert_columns_required`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/insert_columns_required.rhai` |
| 分组 | `dml-safety` |
| 严重度 | `error` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff AM07 |

**校验原因**：`INSERT INTO t VALUES(...)` 隐式依赖表的列顺序，表结构变更（增删列）会导致静默错误。显式指定列名使代码自文档化且对表结构变更更鲁棒。

**反面案例**：
```sql
INSERT INTO users VALUES (1, 'Alice', 'alice@example.com');
```

**正面案例**：
```sql
INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'alice@example.com');
```

---

### DML004 — `subquery_alias_required`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/subquery_alias_required.rhai` |
| 分组 | `dml-style` |
| 严重度 | `error` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff AL10 |

**校验原因**：FROM 子句中的派生表（子查询）必须有别名，否则所有数据库都会报语法错误。

**反面案例**：
```sql
SELECT * FROM (SELECT id, name FROM users);
```

**正面案例**：
```sql
SELECT * FROM (SELECT id, name FROM users) AS active_users;
SELECT u.id, u.name FROM (SELECT id, name FROM users WHERE status = 'active') AS u;
```

---

### DML005 — `column_references_qualified`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/column_references_qualified.rhai` |
| 分组 | `dml-style` |
| 严重度 | `warning` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff RF02 |

**校验原因**：多表查询中未限定的列名可能导致歧义（同名列）或意外绑定。用 `table.column` 语法明确指定来源，提升可读性和可维护性。

**反面案例**：
```sql
SELECT id, name FROM users u JOIN orders o ON u.id = o.user_id;
```

**正面案例**：
```sql
SELECT u.id, u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id;
```

---

### DML006 — `no_join_without_condition`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_join_without_condition.rhai` |
| 分组 | `dml-safety` |
| 严重度 | `error` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff AM05 |

**校验原因**：无条件的 JOIN（CROSS JOIN 除外）会产生笛卡尔积，返回 M×N 行，通常是编码错误，会导致严重性能问题。

**反面案例**：
```sql
SELECT * FROM users JOIN orders;
```

**正面案例**：
```sql
SELECT * FROM users u JOIN orders o ON u.id = o.user_id;
SELECT * FROM users CROSS JOIN orders;  -- 显式 CROSS JOIN 豁免
```

---

## P1 规则（默认禁用，建议评估后启用）

### DML101 — `no_unused_join`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_unused_join.rhai` |
| 分组 | `dml-performance` |
| 严重度 | `warning` |
| 检测方式 | AST（精确，别名优先） |
| 对标 | SQLFluff ST11 |

**校验原因**：未使用的 JOIN 表不必要地消耗数据库资源，可能是重构遗留。

**检测逻辑**：基于 `JoinInfo.alias()` 与 `JoinInfo.table_name()` 两个 needle，检查是否在 `SelectInfo.projection()` 或 `SelectInfo.where_clause()` 中被引用。**别名优先**——`JOIN orders o` 时优先匹配 `o`，无别名才匹配 `orders`，修复了旧版"投影 `o.id` 但用 `orders` 匹配失败"的误报。

**注意**：仍是启发式，可能漏报仅用于 HAVING / GROUP BY 的 JOIN；JOIN 仅在 WHERE 过滤的情况已通过把 `where_clause()` 一并加入可见文本来缓解。

**反面案例**：
```sql
SELECT u.id, u.name FROM users u JOIN unused_table t ON u.id = t.user_id;
```

**正面案例**：
```sql
SELECT u.id, u.name FROM users u;
SELECT o.id FROM orders o JOIN users u ON o.user_id = u.id;  -- u 通过 ON 引用
```

---

### DML102 — `no_unused_cte`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_unused_cte.rhai` |
| 分组 | `dml-performance` |
| 严重度 | `warning` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff ST03 |

**校验原因**：定义了但未引用的 CTE 是无用代码，应移除。

**检测逻辑**：基于 `SelectInfo.ctes()` 提取所有 CTE 名称，再检查名称是否在主查询的 `projection()` / `where_clause()` / `from_table()` / `from_table_alias()` / `joins()` 以及递归子查询 `subqueries()` 中被引用。相比早期字符串 `split(" AS (")` 启发式，AST 方案能精确处理多 CTE、递归 CTE、列别名等场景。

**反面案例**：
```sql
WITH unused_cte AS (
    SELECT * FROM orders WHERE total > 100
)
SELECT id, name FROM users;
```

**正面案例**：
```sql
WITH active_users AS (
    SELECT id, name FROM users WHERE status = 'active'
)
SELECT * FROM active_users;
```

---

### DML103 — `use_is_null`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/use_is_null.rhai` |
| 分组 | `dml-convention` |
| 严重度 | `error` |
| 检测方式 | 字符串匹配 |
| 对标 | SQLFluff CV05 |

**校验原因**：`WHERE name = NULL` 不会返回任何行，因为 SQL 中 `NULL = NULL` 结果是 NULL（不是 TRUE）。应使用 `IS NULL` 或 `IS NOT NULL`。

**反面案例**：
```sql
SELECT * FROM users WHERE name = NULL;
SELECT * FROM users WHERE name <> NULL;
```

**正面案例**：
```sql
SELECT * FROM users WHERE name IS NULL;
SELECT * FROM users WHERE name IS NOT NULL;
```

---

### DML104 — `use_coalesce`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/use_coalesce.rhai` |
| 分组 | `dml-convention` |
| 严重度 | `warning` |
| 检测方式 | 字符串匹配 |
| 对标 | SQLFluff CV02 |

**校验原因**：`COALESCE` 是 SQL 标准函数，跨数据库兼容（PostgreSQL、MySQL、SQLite、DB2、Oracle 等均支持）。`NVL`（Oracle）和 `ISNULL`（SQL Server）是专有函数，降低可移植性。

**反面案例**：
```sql
SELECT NVL(name, 'N/A') FROM users;
SELECT ISNULL(name, 'N/A') FROM users;
```

**正面案例**：
```sql
SELECT COALESCE(name, 'N/A') FROM users;
```

---

### DML105 — `no_order_by_in_subquery`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_order_by_in_subquery.rhai` |
| 分组 | `dml-performance` |
| 严重度 | `warning` |
| 检测方式 | AST（精确，递归子查询） |
| 对标 | SQLFluff AM03 |

**校验原因**：SQL 标准中，子查询是逻辑无序的，内层 ORDER BY 通常被优化器忽略（仅配合 LIMIT/OFFSET/FETCH 时有效）。

**检测逻辑**：基于 `SelectInfo.subqueries()` 递归遍历子查询，对每个子查询检查 `has_order_by() && !has_limit() && !has_offset() && !has_fetch()`。修复了旧版字符串扫描的两个误报：
1. **窗口函数误报**：`OVER(ORDER BY ...)` 中的 ORDER BY 属于窗口函数语法，AST 中不在子查询层，不会触发；
2. **最外层误报**：`) ORDER BY` 在括号外（最外层查询，本就允许），AST 中顶层 SELECT 的 ORDER BY 不算子查询。

**反面案例**：
```sql
SELECT * FROM (SELECT * FROM users ORDER BY name) AS u;
```

**正面案例**：
```sql
SELECT * FROM (SELECT * FROM users ORDER BY name LIMIT 10) AS u;
-- 或把 ORDER BY 移到外层
SELECT * FROM (SELECT * FROM users) AS u ORDER BY name;
-- 窗口函数中的 ORDER BY 不算误报
SELECT id, ROW_NUMBER() OVER (ORDER BY created_at) AS rn FROM users;
```

---

### DML106 — `union_all_preferred`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/union_all_preferred.rhai` |
| 分组 | `dml-performance` |
| 严重度 | `warning` |
| 检测方式 | AST（精确） |
| 对标 | SQLFluff AM02 |

**校验原因**：`UNION` 会执行隐式 `DISTINCT`（排序去重），性能低于 `UNION ALL`。如果业务不需要去重，`UNION ALL` 更快且语义更清晰。

**反面案例**：
```sql
SELECT id FROM users UNION SELECT user_id FROM orders;
```

**正面案例**：
```sql
SELECT id FROM users UNION ALL SELECT user_id FROM orders;
-- 或明确语义
SELECT id FROM users UNION DISTINCT SELECT user_id FROM orders;
```

---

### DML107 — `no_nested_case`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_nested_case.rhai` |
| 分组 | `dml-convention` |
| 严重度 | `warning` |
| 检测方式 | 字符串匹配 |
| 对标 | SQLFluff ST04 |

**校验原因**：嵌套 CASE（CASE 内嵌 CASE）可读性差且易出错，应使用简单 CASE 或提取为子查询。

**反面案例**：
```sql
SELECT CASE WHEN status = 1 THEN
    CASE WHEN type = 'A' THEN 'Type A Active' ELSE 'Active' END
ELSE 'Inactive' END FROM orders;
```

**正面案例**：
```sql
SELECT CASE status
    WHEN 1 THEN 'Active'
    WHEN 0 THEN 'Inactive'
    ELSE 'Unknown'
END FROM orders;
```

---

### DML108 — `no_constant_where`

| 字段 | 值 |
|------|-----|
| 文件 | `config/rules/dml/no_constant_where.rhai` |
| 分组 | `dml-convention` |
| 严重度 | `warning` |
| 检测方式 | 字符串匹配 |
| 对标 | SQLFluff ST10 |

**校验原因**：`WHERE 1=1` 通常是动态 SQL 拼接的产物或用调试遗留代码。静态 SQL 中应移除。`WHERE 1=0` / `WHERE FALSE` 可能是调试代码。

**反面案例**：
```sql
SELECT * FROM users WHERE 1=1;
SELECT * FROM users WHERE true;
SELECT * FROM users WHERE 1=0;
```

**正面案例**：
```sql
SELECT * FROM users WHERE status = 'active';
SELECT * FROM users WHERE 1=1 AND status = 'active';  -- 动态 SQL 拼接时"无害"但建议重构
```

---

## 启用/禁用规则

### 在配置文件中

`[[rules]]` 写在规则配置文件 `sqlguard.rules.toml` 中（由主配置 `sqlguard.toml` 的 `rules_file` 引用，或自动同目录发现）。将对应规则的 `enabled` 改为 `true` 即可启用：

```toml
# sqlguard.rules.toml
[[rules]]
id = "DML103"
enabled = true  # 从 false 改为 true 即可启用
```

### 在命令行

```bash
# 启用时只运行指定规则
sqlguard check ./sql --rules DML101,DML102

# 按分组启用
sqlguard check ./sql --groups dml-performance

# 排除规则
sqlguard check ./sql --exclude-rules DML005

# 前缀通配
sqlguard check ./sql --rules DML*
```

---

## 分级策略说明

| 级别 | 含义 | 使用建议 |
|------|------|----------|
| P0 | CI 必须阻断的**安全与质量红线** | 所有项目默认启用，建议在 CI 中保持开启 |
| P1 | 建议团队评估后采用的**编码规范** | 团队达成一致后启用；可能存在少量误报 |

分级原则：P0 规则 **零误报**（或极低），检测到的问题一定需要修复；P1 规则可能存在启发式误报，需团队 review。
