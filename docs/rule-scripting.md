# SqlGuard 规则脚本编写手册

## 1. 概述

SqlGuard 使用 [Rhai](https://rhai.rs/) 脚本语言编写自定义规则。每条规则是一个 `.rhai` 文件，由引擎针对每个 SQL 文件独立执行一次。规则脚本通过遍历预解析的 AST 来检测违规，并将结果写入 `violations` 数组。

规则脚本具备以下能力：
- 读取 SQL 原文、文件路径、脚本类型等上下文信息
- 访问由 sqlparser 解析后的简化 AST
- 使用字符串匹配作为兜底（不推荐，AST 更精确）
- 上报多条违规，包含行号、列号

## 2. 执行模型

参考 [`src/rule/engine/runner.rs`](../src/rule/engine/runner.rs) 中的 `run_single_rule`：

1. 引擎读取 `.rhai` 脚本内容
2. **prepend `config/rules/lib/helpers.rhai`**（编译时通过 `include_str!` 嵌入二进制），注入 5 个公共辅助函数：`guard_parse_error`、`parse_error_violation`、`violation`、`violation_line`、`violation_msg`
3. 构造一个 `Scope`，注入两个全局变量：
   - `context` —— 一个 Map，包含 SQL 与 AST 信息
   - `violations` —— 一个空 Array，用于收集违规
4. 执行拼接后的完整脚本
5. 读取 `violations` 数组，每项转换为 `Violation` 结构上报

> ⚠️ **Rhai 值语义注意**：Rhai 是值语义语言，函数内对 Array 参数的 push 不会写回调用者变量。因此所有 helper 函数只返回值，`violations.push(...)` 必须在脚本顶层执行。详见 [`helpers.rhai`](../config/rules/lib/helpers.rhai) 中的注释。

## 3. 全局变量

### 3.1 `context`（Map）

| 字段 | 类型 | 说明 |
|------|------|------|
| `sql_content` | String | SQL 原文（可用于注释检查等 AST 无法覆盖的场景） |
| `file_path` | String | 文件绝对路径 |
| `file_name` | String | 文件名（如 `create_users.sql`） |
| `script_type` | String | 分类后的脚本类型（如 `ddl`、`dml`、`other`），由 `sqlguard.toml` 中 `[[classification.rules]]` 决定 |
| `line_count` | INT | SQL 文件总行数 |
| `ast` | SqlAst | 解析后的 AST，见下文 |
| `params` | Map | 该规则的配置参数（来自 `[[rules]].params` / `[rules.params]`）；未配置时为空 Map。见 §5.10.3 |

访问方式：`context["ast"]`、`context["sql_content"]` 等。

### 3.2 `violations`（Array）

脚本通过 `violations.push(...)` 上报违规。推荐使用内置 helper 函数构建 violation：

```rhai
// 带行列号（推荐）
violations.push(violation("message", 10, 1));

// 仅带行号
violations.push(violation_line("message", 10));

// 纯消息（无行列号）
violations.push(violation_msg("some message"));

// 原始 Map 字面量（底层形式，不需 helper）
violations.push(#{
    "message": "violation message",
    "line": 10,
    "column": 1
});
```

`line`、`column` 均为整数，可省略（缺省时为 `None`）。`severity` 与 `rule_name` 来自 `sqlguard.toml`，脚本无需指定。

## 4. AST API

AST 包装类型定义于 [`src/rule/engine/ast.rs`](../src/rule/engine/ast.rs)。所有方法均以 `&mut self` 注册，调用形式为 `obj.method()`。

### 4.1 `SqlAst`（通过 `context["ast"]` 获取）

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `statements()` | Array&lt;StmtInfo&gt; | 全部语句列表 |
| `has_parse_error()` | bool | 是否解析失败 |
| `parse_error()` | String | 解析错误信息（无错时为空串） |
| `kinds()` | Array&lt;String&gt; | 所有语句的 kind 列表 |
| `create_tables()` | Array&lt;CreateInfo&gt; | 全部 CREATE TABLE 信息 |
| `drop_objects()` | Array&lt;DropInfo&gt; | 全部 DROP 信息 |
| `selects()` | Array&lt;SelectInfo&gt; | 全部 SELECT 信息 |
| `has_create_table()` | bool | 是否含 CREATE TABLE |
| `has_drop_table()` | bool | 是否含 DROP TABLE |
| `has_alter_table()` | bool | 是否含 ALTER TABLE（且解析出详情） |
| `has_comma_join_anywhere()` | bool | 整个 SQL 文本中是否检测到 `FROM a, b` 形式的隐式逗号 JOIN（文本扫描，跨语句生效） |
| `comments()` | Array&lt;CommentInfo&gt; | 源文本中的所有注释（行注释 `--` 与块注释 `/* */`，sqlparser 解析时丢弃，这里通过独立文本扫描得到） |

### 4.2 `StmtInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `kind()` | String | 语句类型，见下表 |
| `line()` | INT | 起始行号 |
| `end_line()` | INT | 语句结束行（含），用于判断语句边界 |
| `line_range()` | (INT, INT) | 返回 `(start_line, end_line)` 元组 |
| `column()` | INT | 起始列号 |
| `has_create_table()` | bool | 是否为 CREATE TABLE |
| `has_drop_object()` | bool | 是否为 DROP |
| `has_select()` | bool | 是否为 SELECT |
| `has_insert()` | bool | 是否为 INSERT |
| `has_update()` | bool | 是否为 UPDATE |
| `has_delete()` | bool | 是否为 DELETE |
| `has_alter_table()` | bool | 是否为 ALTER TABLE（且解析出详情） |
| `has_truncate()` | bool | 是否为 TRUNCATE |
| `has_create_view()` | bool | 是否为 CREATE VIEW |
| `has_create_index()` | bool | 是否为 CREATE INDEX |
| `has_transaction()` | bool | 是否为事务语句（START TRANSACTION / COMMIT / ROLLBACK） |
| `create_table()` | CreateInfo | CREATE TABLE 详情（非 CREATE TABLE 时返回默认空对象） |
| `drop_object()` | DropInfo | DROP 详情 |
| `select()` | SelectInfo | SELECT 详情 |
| `insert()` | InsertInfo | INSERT 详情 |
| `update()` | UpdateInfo | UPDATE 详情 |
| `delete()` | DeleteInfo | DELETE 详情 |
| `alter_table()` | AlterTableInfo | ALTER TABLE 详情（非 ALTER TABLE 时返回默认空对象） |
| `truncate()` | TruncateInfo | TRUNCATE 详情 |
| `create_view()` | ViewInfo | CREATE VIEW 详情 |
| `create_index()` | CreateIndexInfo | CREATE INDEX 详情 |
| `transaction()` | TransactionInfo | 事务语句详情 |

**支持的 `kind` 值**：

| kind | SQL 语句 |
|------|----------|
| `CREATE_TABLE` | `CREATE TABLE` |
| `DROP_TABLE` | `DROP TABLE` |
| `DROP_VIEW` | `DROP VIEW` |
| `DROP_INDEX` | `DROP INDEX` |
| 其他 `DROP_<TYPE>` | 其他 DROP 对象类型（SCHEMA / ROLE / SEQUENCE / STAGE） |
| `SELECT` | `SELECT` / `Query` |
| `INSERT` | `INSERT` |
| `UPDATE` | `UPDATE` |
| `DELETE` | `DELETE` |
| `ALTER_TABLE` | `ALTER TABLE` |
| `TRUNCATE` | `TRUNCATE [TABLE]` |
| `CREATE_VIEW` | `CREATE [OR REPLACE] [MATERIALIZED] VIEW` |
| `CREATE_INDEX` | `CREATE [UNIQUE] INDEX` |
| `START_TRANSACTION` | `START TRANSACTION` / `BEGIN` |
| `COMMIT` | `COMMIT` |
| `ROLLBACK` | `ROLLBACK` |
| `GRANT` | `GRANT` |
| `REVOKE` | `REVOKE` |
| `MERGE` | `MERGE`（upsert，sqlparser 解析有限） |
| `SET_VARIABLE` | `SET` |
| `USE` | `USE` |
| `OTHER` | 未识别的语句 |

### 4.3 `CreateInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `table_name()` | String | 表名 |
| `columns()` | Array&lt;ColumnInfo&gt; | 列定义列表 |
| `has_primary_key()` | bool | 是否含主键（列级或表级约束） |
| `if_not_exists()` | bool | 是否带 `IF NOT EXISTS` |
| `column_names()` | Array&lt;String&gt; | 所有列名 |
| `foreign_keys()` | Array&lt;ForeignKeyInfo&gt; | 表级 + 列级外键约束合并列表 |
| `checks()` | Array&lt;CheckInfo&gt; | 表级 + 列级 CHECK 约束合并列表 |
| `indexes()` | Array&lt;IndexInfo&gt; | 表级 INDEX / KEY 约束列表（MySQL 风格） |
| `uniques()` | Array&lt;UniqueInfo&gt; | 表级 UNIQUE 约束列表（不含主键） |
| `has_foreign_key()` | bool | 是否含任意外键 |
| `has_check()` | bool | 是否含任意 CHECK 约束 |
| `has_index()` | bool | 是否含任意 INDEX 约束 |
| `has_unique()` | bool | 是否含任意 UNIQUE 约束 |

### 4.4 `ColumnInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `name()` | String | 列名 |
| `data_type()` | String | 数据类型 |
| `is_primary_key()` | bool | 是否为列级 PRIMARY KEY |
| `is_not_null()` | bool | 是否 NOT NULL |
| `is_unique()` | bool | 是否 UNIQUE |
| `default_value()` | String | DEFAULT 表达式文本（无则空串） |
| `is_auto_increment()` | bool | 是否 AUTO_INCREMENT / AUTOINCREMENT |
| `comment()` | String | 列 COMMENT 文本（无则空串） |
| `has_check()` | bool | 是否含列级 CHECK 约束 |
| `references_table()` | String | 列级外键引用的表名（无则空串） |
| `has_foreign_key()` | bool | 是否含列级 REFERENCES 外键 |

### 4.5 `DropInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `object_type()` | String | 对象类型（`TABLE`、`VIEW` 等） |
| `name()` | String | 对象名（多个时以 `, ` 连接） |
| `if_exists()` | bool | 是否带 `IF EXISTS` |

### 4.5b `AlterTableInfo`

仅当语句为 `ALTER TABLE` 时由 `s.alter_table()` 返回非空详情，用于判断表是否通过 ALTER 增删主键，以及结构化遍历所有 ALTER 操作。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `table_name()` | String | 被修改的表名 |
| `adds_primary_key()` | bool | 是否包含增加主键的操作：`ADD PRIMARY KEY (...)`、`ADD CONSTRAINT x PRIMARY KEY (...)` 或 `ADD COLUMN col ... PRIMARY KEY` |
| `drops_primary_key()` | bool | 是否包含 `DROP PRIMARY KEY`（移除主键） |
| `operations()` | Array&lt;AlterOpInfo&gt; | 所有 ALTER 操作的结构化列表（含 ADD/DROP COLUMN、RENAME、ADD/DROP CONSTRAINT 等） |

### 4.6 `SelectInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `has_wildcard()` | bool | 是否含 `*` 或 `table.*`（递归覆盖顶层、UNION/集合运算分支、FROM/JOIN 子查询） |
| `has_bare_wildcard()` | bool | 本层是否含**裸** `SELECT *`（`t1.*` 这类限定通配符不算）。出现裸星号时无法判断各 JOIN 表是否被使用，规则应跳过 |
| `referenced_qualifiers()` | Array&lt;String&gt; | 查询体真实引用到的表限定符（大写、去重），来源：投影（含 `t.*`）、WHERE、GROUP BY、HAVING、QUALIFY、ORDER BY；**不含 JOIN 的 ON 条件** |
| `is_qualifier_referenced(name)` | bool | 某个别名/表名是否在查询体中被引用（大小写不敏感），等价于 `referenced_qualifiers()` 的包含判断 |
| `projection()` | Array&lt;String&gt; | 顶层投影项文本表示（**含 `*` 与 `t1.*` 通配符项**） |
| `has_from_table()` | bool | 是否含 FROM 表 |
| `from_table()` | String | 第一张 FROM 表名（无则空串） |
| `from_table_alias()` | String | 主表别名（`FROM users u` 中的 `u`，无则空串） |
| `has_from_table_alias()` | bool | 主表是否带别名 |
| `joins()` | Array&lt;JoinInfo&gt; | JOIN 子句列表 |
| `has_joins()` | bool | 是否有 JOIN |
| `has_cross_join()` | bool | 是否含 CROSS JOIN |
| `has_join_without_condition()` | bool | 是否存在无 ON/USING 条件的 JOIN（CROSS JOIN 除外） |
| `has_subquery_in_from()` | bool | FROM 是否含派生表（子查询） |
| `from_subquery_has_alias()` | bool | FROM 派生表是否都带别名 |
| `has_unqualified_column()` | bool | 多表查询中是否存在未限定的**裸列**引用（不含函数/字面量） |
| `is_union()` | bool | 是否为 UNION 集合运算（不含 INTERSECT/EXCEPT） |
| `is_union_all()` | bool | UNION 是否带 `ALL`（含 `ALL BY NAME`） |
| `is_intersect()` | bool | 是否为 INTERSECT 集合运算 |
| `is_except()` | bool | 是否为 EXCEPT 集合运算 |
| `has_where()` | bool | 是否含 WHERE 子句 |
| `where_clause()` | String | WHERE 表达式文本（无则空串） |
| `has_group_by()` | bool | 是否含 GROUP BY 子句 |
| `has_having()` | bool | 是否含 HAVING 子句 |
| `has_qualify()` | bool | 是否含 QUALIFY 子句（Snowflake 等方言） |
| `has_order_by()` | bool | 是否含 ORDER BY（在 Query 层，集合运算也能查到） |
| `order_by_items()` | Array&lt;OrderByItemInfo&gt; | ORDER BY 的逐项明细（含 ASC/DESC 与 NULLS FIRST/LAST 的显式性）。`ORDER BY ALL` 时为空列表 |
| `has_order_by_without_direction()` | bool | 是否存在未显式写 `ASC`/`DESC` 的排序项（无 ORDER BY 时为 false） |
| `has_order_by_without_nulls_spec()` | bool | 是否存在未显式写 `NULLS FIRST/LAST` 的排序项 |
| `order_by_text()` | String | ORDER BY 整体文本（各项以 `, ` 连接） |
| `has_limit()` | bool | 是否含 LIMIT |
| `has_offset()` | bool | 是否含 OFFSET |
| `has_fetch()` | bool | 是否含 FETCH（SQL:2008 风格分页） |
| `has_distinct()` | bool | 是否含 DISTINCT |
| `ctes()` | Array&lt;CteInfo&gt; | WITH 子句中的 CTE 列表 |
| `has_cte()` | bool | 是否含 WITH 子句 |
| `is_recursive()` | bool | WITH 是否带 RECURSIVE 关键字 |
| `subqueries()` | Array&lt;SelectInfo&gt; | 递归嵌套的子查询列表（FROM 派生表 / WHERE 标量子查询 / EXISTS / 集合运算括号分支） |
| `has_subquery()` | bool | 是否含任意子查询 |
| `has_window_function()` | bool | 投影/WHERE/HAVING 中是否含窗口函数（`OVER (...)`） |
| `window_functions()` | Array&lt;WindowFuncInfo&gt; | 窗口函数详情列表 |
| `where_expr()` | ExprInfo | WHERE 表达式顶层信息（无则返回默认空对象） |
| `has_where_expr()` | bool | WHERE 表达式是否已解析 |
| `having_expr()` | ExprInfo | HAVING 表达式顶层信息 |
| `has_having_expr()` | bool | HAVING 表达式是否已解析 |
| `projection_exprs()` | Array&lt;ExprInfo&gt; | 投影项的表达式信息列表 |
| `has_comma_join()` | bool | 该 SELECT 语句范围内是否检测到 `FROM a, b` 形式隐式逗号 JOIN |

### 4.6b `JoinInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `table_name()` | String | JOIN 的目标表名（不含别名） |
| `table_name_leaf()` | String | 去掉 schema 限定的表名（`ofsm.cdeorg` → `cdeorg`），用于匹配不带 schema 的列引用（`cdeorg.col`） |
| `join_type()` | String | `INNER` / `LEFT` / `RIGHT` / `FULL` / `CROSS` / `OTHER` |
| `has_condition()` | bool | 是否带 ON/USING 条件（CROSS JOIN 恒为 true） |
| `alias()` | String | JOIN 目标表别名（`JOIN orders o` 中的 `o`，无则空串） |
| `has_alias()` | bool | JOIN 目标表是否带别名 |
| `condition_text()` | String | ON 子句表达式文本（无则空串） |
| `has_condition_text()` | bool | 是否有 ON 子句文本 |

### 4.6c `InsertInfo` / `UpdateInfo` / `DeleteInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `InsertInfo.table_name()` | String | INSERT 目标表名 |
| `InsertInfo.columns()` | Array&lt;String&gt; | 显式指定的列名列表 |
| `InsertInfo.has_columns()` | bool | 是否显式指定列 |
| `UpdateInfo.table_name()` | String | UPDATE 目标表名 |
| `UpdateInfo.has_where()` | bool | 是否含 WHERE |
| `UpdateInfo.where_clause()` | String | WHERE 表达式文本 |
| `UpdateInfo.set_columns()` | Array&lt;String&gt; | `SET` 的目标列名（元组赋值展开为各列） |
| `UpdateInfo.sets_column(name)` | bool | 某个列是否被赋值（大小写不敏感、自动去 schema 前缀与引号） |
| `UpdateInfo.set_clause_text()` | String | `SET` 子句整体文本 |
| `UpdateInfo.has_subquery()` | bool | WHERE 中是否含子查询 |
| `UpdateInfo.has_limit()` | bool | 是否含 `LIMIT` |
| `UpdateInfo.has_order_by()` | bool | 是否含顶层 `ORDER BY`（⚠️ 原文兜底，见 §5.10.2） |
| `UpdateInfo.has_group_by()` | bool | 是否含顶层 `GROUP BY`（⚠️ 原文兜底） |
| `DeleteInfo.table_name()` | String | DELETE 目标表名 |
| `DeleteInfo.has_where()` | bool | 是否含 WHERE |
| `DeleteInfo.where_clause()` | String | WHERE 表达式文本 |
| `DeleteInfo.has_subquery()` | bool | WHERE 中是否含子查询 |
| `DeleteInfo.has_limit()` | bool | 是否含 `LIMIT` |
| `DeleteInfo.has_order_by()` | bool | 是否含 `ORDER BY`（AST 可得） |
| `DeleteInfo.has_group_by()` | bool | 是否含顶层 `GROUP BY`（⚠️ 原文兜底） |

## 5. 新增 Info 类型（P1-P3 能力扩展）

以下 Info 类型由 [`src/rule/engine/ast.rs`](../src/rule/engine/ast.rs) 和 [`analyzer.rs`](../src/rule/engine/analyzer.rs) 提供，对应 CTE / 窗口函数 / 表达式树 / 约束 / 事务 / 视图 / 索引 / 注释等 AST 能力。所有结构体均 `Clone + Default`，方法以 `&mut self` 注册到 Rhai 引擎。

### 5.1 `CteInfo`（CTE / WITH 子句）

由 `SelectInfo.ctes()` 返回。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `name()` | String | CTE 名称 |
| `column_count()` | INT | CTE 显式列数（`WITH x(a, b) AS (...)` 中的 2；未显式列出时为 0） |
| `is_recursive()` | bool | 该 CTE 是否处于 `WITH RECURSIVE` 上下文 |

### 5.2 `WindowFuncInfo`（窗口函数）

由 `SelectInfo.window_functions()` 返回。识别 `Expr::Function { over: Some(_), .. }`。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `function_name()` | String | 窗口函数名（如 `ROW_NUMBER` / `SUM` / `RANK`） |
| `has_partition_by()` | bool | 是否带 `PARTITION BY` |
| `has_order_by()` | bool | 是否带窗口内 `ORDER BY` |
| `has_window_frame()` | bool | 是否带窗口帧（`ROWS` / `RANGE` / `GROUPS BETWEEN ...`） |

### 5.3 `ExprInfo`（表达式顶层信息）

由 `SelectInfo.where_expr()` / `having_expr()` / `projection_exprs()` 返回。**不递归暴露子表达式**（避免类型爆炸），只暴露顶层判定。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `kind()` | String | 表达式类型，取值见下表 |
| `text()` | String | 表达式文本表示 |
| `function_name()` | String | 函数名（仅 `FUNCTION` 类型有效） |
| `is_literal()` | bool | 是否为字面量 |
| `is_column()` | bool | 是否为列引用（含复合 `t.c`） |
| `is_subquery()` | bool | 是否含子查询（`SUBQUERY` / `EXISTS`） |
| `operator()` | String | 二元/一元运算符（如 `=` / `AND` / `NOT`） |
| `column_name()` | String | 列名（仅 `IDENTIFIER` / `COMPOUND_IDENTIFIER` 有效） |
| `has_null_test()` | bool | 是否为 `IS NULL` / `IS NOT NULL` 判定 |

`kind` 取值：`IDENTIFIER / COMPOUND_IDENTIFIER / LITERAL / NULL / BINARY_OP / UNARY_OP / FUNCTION / CASE / SUBQUERY / EXISTS / IN_LIST / BETWEEN / CAST / IS_NULL / IS_NOT_NULL / OTHER`。

### 5.4 `ForeignKeyInfo`（外键约束）

由 `CreateInfo.foreign_keys()` 返回，合并表级 `FOREIGN KEY (...) REFERENCES ...` 与列级 `REFERENCES ...`。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `name()` | String | 约束名（`CONSTRAINT x` 中的 `x`，无则空串） |
| `columns()` | Array&lt;String&gt; | 本表外键列名 |
| `foreign_table()` | String | 引用的目标表名 |
| `referred_columns()` | Array&lt;String&gt; | 目标表的引用列名 |
| `on_delete()` | String | ON DELETE 动作（`CASCADE` / `SET NULL` / `RESTRICT` / `NO ACTION` / `SET DEFAULT`，无则空串） |
| `on_update()` | String | ON UPDATE 动作（同上） |

### 5.5 `CheckInfo`（CHECK 约束）

由 `CreateInfo.checks()` 返回，合并表级与列级。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `name()` | String | 约束名（无则空串） |
| `expr_text()` | String | CHECK 表达式文本 |

### 5.6 `IndexInfo` / `UniqueInfo`（索引与唯一约束）

`IndexInfo` 由 `CreateInfo.indexes()` 返回（MySQL 风格 `KEY/INDEX`）。`UniqueInfo` 由 `CreateInfo.uniques()` 返回（不含主键）。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `IndexInfo.name()` | String | 索引名 |
| `IndexInfo.columns()` | Array&lt;String&gt; | 索引列名 |
| `IndexInfo.is_unique()` | bool | 是否为 UNIQUE 索引 |
| `UniqueInfo.name()` | String | 约束名 |
| `UniqueInfo.columns()` | Array&lt;String&gt; | 约束列名 |

### 5.7 `AlterOpInfo`（ALTER 操作）

由 `AlterTableInfo.operations()` 返回。每个 ALTER 操作转为一条记录。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `operation_type()` | String | 操作类型，见下表 |
| `column_name()` | String | 涉及的列名（ADD/DROP/ALTER/RENAME COLUMN 时） |
| `table_name()` | String | RENAME TABLE 的新表名 |
| `constraint_name()` | String | 涉及的约束名（DROP CONSTRAINT / DROP FOREIGN KEY 时） |
| `detail()` | String | 详情文本（人类可读） |

`operation_type` 取值：`ADD_COLUMN / DROP_COLUMN / ALTER_COLUMN / RENAME_COLUMN / RENAME_TABLE / ADD_CONSTRAINT / DROP_CONSTRAINT / DROP_PRIMARY_KEY / DROP_FOREIGN_KEY / DROP_UNIQUE / OTHER`。

### 5.8 `TruncateInfo` / `ViewInfo` / `CreateIndexInfo` / `TransactionInfo`

由 `StmtInfo.truncate()` / `create_view()` / `create_index()` / `transaction()` 返回。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `TruncateInfo.table_name()` | String | TRUNCATE 的表名 |
| `TruncateInfo.has_table_keyword()` | bool | 是否显式带 `TABLE` 关键字 |
| `ViewInfo.name()` | String | 视图名 |
| `ViewInfo.materialized()` | bool | 是否为 MATERIALIZED VIEW |
| `ViewInfo.is_replace()` | bool | 是否为 CREATE OR REPLACE |
| `ViewInfo.column_count()` | INT | 显式列数 |
| `CreateIndexInfo.name()` | String | 索引名（无则空串） |
| `CreateIndexInfo.table_name()` | String | 索引所在表名 |
| `CreateIndexInfo.columns()` | Array&lt;String&gt; | 索引列名 |
| `CreateIndexInfo.is_unique()` | bool | 是否为 UNIQUE 索引 |
| `TransactionInfo.kind()` | String | `START_TRANSACTION` / `COMMIT` / `ROLLBACK` |

### 5.9 `CommentInfo`（注释）

由 `SqlAst.comments()` 返回。sqlparser 在 parse 阶段丢弃注释，这里通过独立文本扫描得到。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `text()` | String | 注释文本（不含 `--` 或 `/* */` 分隔符） |
| `line()` | INT | 注释所在行号（1-based） |
| `kind()` | String | `LINE`（行注释）或 `BLOCK`（块注释） |

### 5.10 子句修饰符 / 原文切片 / 标识符工具 / 规则参数

本节对应 GaussDB 规范落地时补齐的四项引擎能力（C2 / C4 / C5 / C8）。

#### 5.10.1 `OrderByItemInfo`（ORDER BY 逐项明细）

由 `SelectInfo.order_by_items()` 返回。Rhai 不暴露 `Option`，方向与 NULL 排序都是**三态**，
用 `has_*` 方法区分"原文未显式指定"与"显式指定为某个值"——「未指定」正是 GaussDB 规范的违规点。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `expr_text()` | String | 排序表达式文本 |
| `has_direction()` | bool | 是否显式写了 `ASC` / `DESC` |
| `direction()` | String | `ASC` / `DESC`；未指定时为空串 |
| `is_asc()` / `is_desc()` | bool | 是否为显式升序 / 降序 |
| `has_nulls_spec()` | bool | 是否显式写了 `NULLS FIRST` / `NULLS LAST` |
| `nulls()` | String | `FIRST` / `LAST`；未指定时为空串 |
| `has_nulls_first()` / `has_nulls_last()` | bool | 是否为显式 NULLS FIRST / LAST |

```rhai
// GaussDB：ORDER BY 必须显式指定 ASC/DESC 与 NULL 排序方式
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_order_by() {
            if sel.has_order_by_without_direction() {
                violations.push(violation("ORDER BY must specify ASC/DESC", s.line(), s.column()));
            }
            if sel.has_order_by_without_nulls_spec() {
                violations.push(violation("ORDER BY must specify NULLS FIRST/LAST", s.line(), s.column()));
            }
        }
    }
}
```

#### 5.10.2 语句原文切片（C4）

方言归一化层会重写文本（GaussDB 下 `` `x` `` → `"x"`、`SYSDATE` → `CURRENT_TIMESTAMP`），
sqlparser 也会丢弃引号写法。`SqlAst.source` 保存的是**原始文本**（未经重写），切片 API 全部基于它。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `source()` | String | 原始 SQL 全文 |
| `slice(start_line, start_col, end_line, end_col)` | String | 精确切片：1-based、**按字符**计数、左闭右开；`start_col<=1` 表示从行首，`end_col<=0` 表示到行尾；兼容 LF/CRLF；越界不 panic |
| `stmt_text(stmt)` | String | 某条语句的原文（按 `line..=end_line` 整行区间切片后 trim） |
| `stmt_text_at(index)` | String | 第 `index` 条语句（0-based）原文；越界返回空串 |
| `stmt_text_at_line(line)` | String | 覆盖指定行号的那条语句原文 |
| `line_range_text(l1, l2)` | String | 闭区间行范围 `[l1, l2]` 的整行文本（trim） |

> ⚠️ 语句原文取的是**整行区间**：若同一行写了两条语句（`A; B;`），会连带取到同行后续内容。
> 需要精确边界时用 `slice()` 配合语句的 `line()` / `column()`。

**依赖原文兜底的字段**：`UpdateInfo.has_order_by()` / `has_group_by()`、`DeleteInfo.has_group_by()`。
原因是 sqlparser 0.60 的 `Update` 结构没有 `order_by`/`group_by` 字段、`Delete` 没有 `group_by`。
这些标志由「语句原文的顶层子句扫描」得出：跟踪括号深度并跳过字符串/注释，因此
`OVER (ORDER BY ...)`、`WHERE x IN (SELECT ... ORDER BY ...)` 里的同名子句不会被误判为顶层。

> ⚠️ 边界情形：`UPDATE ... ORDER BY` 在 sqlparser 中是"残缺解析"，要靠**方言回退链**才能拿到
> UPDATE 节点。若方言链没有可用的回退方言（如 `dialect = "generic"` 且未配置 `dialect_fallback`），
> 这类语句会退化为 `PARSE_ERROR`（会报 PARSE 警告，不会静默），此时 `has_order_by()` 取不到值。
> 使用 `gaussdb` / `postgresql` 方言时链尾恒有 `generic` 兜底，不受影响。

```rhai
// 字段名是否被引号包裹（AST 无法表达，需看原文）
for s in ast.statements() {
    if s.has_alter_table() {
        let t = ast.stmt_text(s);
        if t.contains("\"") {
            violations.push(violation("do not quote column names in DDL", s.line(), s.column()));
        }
    }
}
```

#### 5.10.3 规则参数（C8）

`[[rules]]` 下用 `[rules.params]`（或内联 `params = {...}`）声明任意参数，注入到 `context["params"]`：

```toml
[[rules]]
id = "GDML001"
name = "max_join_tables"
script_path = "config/rules/gaussdb/dml/max_join_tables.rhai"
applies_to = ["dml"]
severity = "warning"

[rules.params]
max_join_tables = 3
max_join_tables_batch = 5
```

```rhai
let p = context["params"];
let limit = 3;                                   // 脚本内默认值：配置缺失时生效
if "max_join_tables" in p { limit = p["max_join_tables"]; }
```

- 未配置时 `context["params"]` 是**空 Map**（不是 `()`），可直接 `len()` / 用 `in` 判定
- 整个 `params` 写成非 table 形态（标量 / 数组）时会被包成 `#{ "value": ... }`，保证脚本侧恒为 Map
- TOML `Datetime` 转为 RFC3339 字符串（Rhai 没有日期类型）

#### 5.10.4 标识符与字节级内置函数（C5）

全局函数，脚本顶层直接调用，无需经 `context`：

| 函数 | 返回类型 | 说明 |
|------|----------|------|
| `len_bytes(s)` | INT | UTF-8 **字节**长度。Rhai 的 `s.len()` 是字符数；「对象名 ≤63 字节」这类规则必须用本函数 |
| `is_valid_ident(s)` | bool | 非空、仅含 `[A-Za-z0-9_]`、不以数字开头（**不**先剥引号，故带引号返回 false） |
| `is_quoted_ident(s)` | bool | 是否被 `"` / `` ` `` / `[]` 成对包裹 |
| `strip_quotes(s)` | String | 剥掉成对引号；不配对时原样返回 |
| `ident_leaf(s)` | String | 取最后一段标识符并剥引号（`ofsm.cdeorg` → `cdeorg`） |
| `normalize_ident(s)` | String | 叶子名 + PG 折叠：未加引号 → 小写，加引号 → 保留大小写 |
| `is_reserved_word(s)` | bool | 叶子名是否为 SQL 保留关键字（大小写不敏感，117 项） |
| `has_reserved_prefix(s)` | bool | 叶子名是否以 `pg_` / `gs_` / `adm_` / `my_` / `db_` 开头 |

```rhai
// GaussDB 命名四连：字符集 / 引号 / 预留前缀 / 字节长度
for s in ast.statements() {
    if s.has_create_table() {
        let t = s.create_table().table_name();
        if !is_valid_ident(t) {
            violations.push(violation("object name must use only letters, digits and underscores", s.line(), s.column()));
        }
        if is_quoted_ident(t) {
            violations.push(violation("do not quote object names", s.line(), s.column()));
        }
        if has_reserved_prefix(t) || is_reserved_word(t) {
            violations.push(violation("reserved name or prefix: " + t, s.line(), s.column()));
        }
        if len_bytes(t) > 63 {
            violations.push(violation("object name exceeds 63 bytes", s.line(), s.column()));
        }
    }
}
```

> `is_reserved_word` / `has_reserved_prefix` 作用于**叶子段**，因此 `ofsm.pg_class` 也能命中。
> 保留字表（`src/rule/engine/idents.rs::RESERVED_WORDS`）目前与 DDL003 规则脚本的内联表
> 语义一致，改一处必须同步另一处。

#### 5.10.5 新增字段速查

| 类型 | 新增方法 |
|------|----------|
| `SelectInfo` | `order_by_items()` / `has_order_by_without_direction()` / `has_order_by_without_nulls_spec()` / `order_by_text()` |
| `UpdateInfo` | `set_columns()` / `sets_column(name)` / `set_clause_text()` / `has_subquery()` / `has_limit()` / `has_order_by()` / `has_group_by()` |
| `DeleteInfo` | `has_subquery()` / `has_limit()` / `has_order_by()` / `has_group_by()` |
| `CreateIndexInfo` | `concurrently()` / `if_not_exists()` / `using_method()` / `include_columns()` |
| `ViewInfo` | `definition()`（返回 `SelectInfo`） / `has_definition()` / `is_temporary()` / `if_not_exists()` |
| `SqlAst` | `source()` / `slice(l1,c1,l2,c2)` / `stmt_text(stmt)` / `stmt_text_at(i)` / `stmt_text_at_line(l)` / `line_range_text(l1,l2)` |

```rhai
// 视图内部检查（GaussDB：禁止在视图中排序）
for s in ast.statements() {
    if s.has_create_view() {
        let v = s.create_view();
        if v.has_definition() && v.definition().has_order_by() {
            violations.push(violation("ORDER BY is not allowed inside a view", s.line(), s.column()));
        }
    }
}

// 索引并发创建（GaussDB：有联机事务时建索引必须加 CONCURRENTLY）
for s in ast.statements() {
    if s.has_create_index() {
        let ci = s.create_index();
        if !ci.concurrently() {
            violations.push(violation("CREATE INDEX should use CONCURRENTLY in online systems", s.line(), s.column()));
        }
    }
}

// UPDATE SET 是否触碰了某个列（GaussDB：分布键值禁止 UPDATE）
for s in ast.statements() {
    if s.has_update() {
        let u = s.update();
        if u.sets_column("id") {
            violations.push(violation("distribution key must not be updated", s.line(), s.column()));
        }
        if u.has_order_by() || u.has_group_by() {
            violations.push(violation("ORDER BY/GROUP BY is not allowed in UPDATE", s.line(), s.column()));
        }
    }
}
```

## 6. Rhai 语法要点
- **变量声明**：`let x = ...;`
- **对象 Map 字面量**：`#{ "key": value, "key2": value2 }`（`#` 是 Rhai 的对象字面量前缀）
- **数组**：`[1, 2, 3]`，方法 `push()`、`len()`、索引访问 `arr[0]`
- **循环**：`for x in arr { ... }`
- **字符串拼接**：`"a" + "b"`
- **Map 访问**：`map["key"]` 或 `map.key`
- **注释**：`//` 单行
- **条件**：`if cond { ... } else if cond2 { ... } else { ... }`
- **递归函数**：Rhai 支持在脚本顶层 `fn name(arg) { ... }` 定义函数并自调用（用于递归遍历子查询，见 [no_order_by_in_subquery.rhai](../config/rules/dml/no_order_by_in_subquery.rhai)）

## 7. 规则脚本模板

```rhai
// <rule_name>.rhai - <一句话描述>
// 基于 AST：<简述检测逻辑>

// 1. 解析失败时上报（推荐保留，避免漏检）
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// 2. 遍历语句，按 kind 分派
for s in ast.statements() {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        // 检测条件
        if !ct.has_primary_key() {
            violations.push(violation(
                "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                s.line(), s.column()
            ));
        }
    }
}
```

## 8. 在配置文件中注册规则

参考 [`sqlguard.toml.example`](../sqlguard.toml.example) 与 [`sqlguard.rules.toml.example`](../sqlguard.rules.toml.example)。

### 8.1 单文件 vs 双文件

随着规则增多，所有 `[[rules]]` 堆在一个文件里会让配置越来越长。SqlGuard 支持把**规则配置**和**其它配置**拆成两个文件：

- **主配置** `sqlguard.toml`：结构、分类、输出、Mapper、扫描、文件检查等（不含 `[[rules]]`）。
- **规则配置** `sqlguard.rules.toml`：只含 `[[rules]]` 数组。

在主配置中通过 `rules_file` 指向规则文件即可：

```toml
# sqlguard.toml
rules_file = "sqlguard.rules.toml"
```

也可以**不写 `rules_file`**——工具会**自动在同目录查找 `sqlguard.rules.toml`**。
两条规则来源解析优先级（高 → 低）：

1. 显式 `rules_file` → 从该文件加载（覆盖主配置内联的 `[[rules]]`）。
2. 同级 `sqlguard.rules.toml` 存在 → 从该文件加载。
3. 否则沿用主配置内联的 `[[rules]]`（向后兼容旧的单文件配置）。

### 8.2 `[[rules]]` 字段

```toml
# sqlguard.rules.toml
[[rules]]
id = "DDL001"                                    # 规则编号，必填且全局唯一
name = "no_drop_table"                            # 规则名（出现在报告中）
group = "ddl-safety"                              # 规则分组，可选
description = "Disallow DROP TABLE in DDL scripts" # 可选，描述
enabled = true                                    # 是否启用
script_path = "config/rules/ddl/no_drop_table.rhai"  # 相对规则文件所在目录
applies_to = ["ddl"]                              # 仅对这些 script_type 生效
severity = "error"                                # "error" 会让 check 返回非零；"warning" 不会
```

字段说明：

| 字段 | 必填 | 说明 |
|------|------|------|
| `id` | 是 | 规则编号，全局唯一。CLI 通过 `--rules` / `--exclude-rules` 用它引用规则 |
| `name` | 是 | 规则名，出现在报告中，便于阅读 |
| `group` | 否 | 规则分组名。CLI 通过 `--groups` / `--exclude-groups` 用它筛选 |
| `description` | 否 | 规则描述 |
| `enabled` | 否（默认 `true`） | 是否启用 |
| `script_path` | 是 | Rhai 脚本路径，相对规则文件所在目录 |
| `applies_to` | 是 | 仅对这些 `script_type` 生效（与 `[[classification.rules]]` 的 `type` 对应） |
| `severity` | 否（默认 `"error"`） | `error` 让 `check` 退出码非 0；`warning` 不阻断 |

`applies_to` 与 `[[classification.rules]]` 中的 `type` 对应：引擎在 `run_rules_for_file` 中过滤——只有 `script_type ∈ applies_to` 时才执行该规则。

加载配置时会校验 `id` 必填且唯一；缺失或重复会以 `Config error` 形式报错。

## 9. 在命令行筛选规则

`check` 子命令支持按 `id` 或 `group` 筛选要执行的规则，所有筛选参数都接受逗号分隔的列表，并支持前缀通配 `prefix*`：

```bash
# 只跑指定 id 的规则
sqlguard check ./sql --rules DDL001,DDL002

# 只跑指定分组
sqlguard check ./sql --groups ddl-safety

# 排除某些规则或分组
sqlguard check ./sql --exclude-rules DML001
sqlguard check ./sql --exclude-groups experimental

# 前缀通配：匹配所有 DDL 开头的 id
sqlguard check ./sql --rules DDL*

# 组合使用
sqlguard check ./sql --groups ddl-safety --exclude-rules DDL003
```

筛选语义：

- **白名单**（`--rules` / `--groups`）：为空时表示不限制；非空时规则需命中白名单才执行。
- **黑名单**（`--exclude-rules` / `--exclude-groups`）：命中即跳过，**优先级高于白名单**。
- **前缀通配**：`DDL*` 匹配所有以 `DDL` 开头的 id；`ddl-*` 匹配所有以 `ddl-` 开头的 group。
- **`enabled = false` 的规则永远不会执行**，无论筛选参数如何。
- 筛选参数仅影响规则执行，不影响目录结构检查与文件分类。

当任一筛选参数非空时，`sqlguard` 会在 stderr 打印一行激活的筛选条件，便于 CI 日志追溯。

### 9.1 行内豁免（inline exemption）

在 SQL 文件中加注释可豁免特定行的特定规则，避免全局禁用导致漏检。三种语法（不区分大小写）：

```sql
-- sqlguard-disable-next-line DML001
SELECT * FROM users;  -- 被豁免，不报 DML001

SELECT * FROM users; -- sqlguard-disable-line DML001  -- 同行豁免

/* sqlguard-disable DML001, DML002 */  -- 块注释豁免（单行）
```

**规则 ID 通配**：

- `*` 豁免全部规则：`-- sqlguard-disable-next-line *`
- 前缀通配：`-- sqlguard-disable-next-line DML*` 豁免所有 DML 开头的规则

**多规则**：逗号分隔，如 `-- sqlguard-disable-next-line DML001, DML002`

**行为**：

- 豁免仅过滤 violation 输出，不影响规则执行（规则仍会运行，只是结果被过滤）
- 无行号的 violation（如规则脚本错误）不会被豁免
- Mapper 模式下豁免行号自动偏移到 XML 坐标系
- 被豁免的 violation 数量会在 stderr 打印 `Info: N violation(s) exempted by inline comments in <file>`

## 10. 完整示例

### 10.1 禁止 DROP TABLE（DDL）

来自 [`config/rules/ddl/no_drop_table.rhai`](../config/rules/ddl/no_drop_table.rhai)：

```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(violation(
            "DROP TABLE is not allowed in DDL scripts: " + d.name(),
            s.line(), s.column()
        ));
    }
}
```

### 10.2 CREATE TABLE 必须有主键（DDL）

来自 [`config/rules/ddl/primary_key_required.rhai`](../config/rules/ddl/primary_key_required.rhai)：

```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        if !ct.has_primary_key() {
            violations.push(violation(
                "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                s.line(), s.column()
            ));
        }
    }
}
```

### 10.3 禁止 SELECT *（DML）

来自 [`config/rules/dml/no_select_all.rhai`](../config/rules/dml/no_select_all.rhai)：

```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_wildcard() {
            violations.push(violation(
                "SELECT * is not allowed. Specify columns explicitly.",
                s.line(), s.column()
            ));
        }
    }
}
```

### 10.4 基于 AST 的 CTE 未使用检测（DML）

来自 [`config/rules/dml/no_unused_cte.rhai`](../config/rules/dml/no_unused_cte.rhai)，演示 P0-3 的 `SelectInfo.ctes()` / `has_cte()` 能力：

```rhai
let ast = context["ast"];

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_cte() {
            // 汇总主查询与子查询可见文本
            let visible = "";
            for col in sel.projection() { visible += " " + col; }
            visible += " " + sel.where_clause();
            visible += " " + sel.from_table();
            visible += " " + sel.from_table_alias();
            for j in sel.joins() {
                visible += " " + j.table_name();
                visible += " " + j.alias();
            }
            let upper_visible = visible.to_upper();

            for cte in sel.ctes() {
                let cte_name = cte.name().to_upper();
                if cte_name != "" && !upper_visible.contains(cte_name) {
                    violations.push(violation(
                        "CTE '" + cte.name() + "' is defined but not referenced",
                        s.line(), s.column()
                    ));
                }
            }
        }
    }
}
```

### 10.5 基于 AST 递归子查询的 ORDER BY 检测（DML）

来自 [`config/rules/dml/no_order_by_in_subquery.rhai`](../config/rules/dml/no_order_by_in_subquery.rhai)，演示 P1-4 的 `SelectInfo.subqueries()` 递归能力 + P1-5 的窗口函数修复：

```rhai
let ast = context["ast"];

// 递归检查子查询：ORDER BY 必须配合 LIMIT/OFFSET/FETCH
fn check_subquery(sel) {
    for sub in sel.subqueries() {
        if sub.has_order_by() && !sub.has_limit() && !sub.has_offset() && !sub.has_fetch() {
            violations.push(violation_line(
                "ORDER BY in subquery is typically ignored unless combined with LIMIT/OFFSET",
                1
            ));
            return true;
        }
        if check_subquery(sub) { return true; }
    }
    return false;
}

for s in ast.statements() {
    if s.has_select() {
        check_subquery(s.select());
    }
}
```

### 10.6 基于字符串匹配的简单规则（不推荐，但可用）

当 AST 无法覆盖时使用：

```rhai
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("DROP TABLE") {
    violations.push(violation_msg("DROP TABLE not allowed"));
}
```

注意：字符串匹配会误判注释、字符串字面量中的内容，应优先使用 AST。

## 11. 调试与最佳实践

1. **始终先用 `guard_parse_error(context)`**：解析失败时 AST 为空，规则不会触发；建议使用内置 helper 检测并上报以免漏检：
   ```rhai
   if guard_parse_error(context) {
       violations.push(parse_error_violation(context));
       return;
   }
   ```
2. **优先用 AST 而非字符串匹配**：AST 能区分注释、字面量与真实语句。
3. **行列号尽量从 `StmtInfo` 取**：`s.line()` / `s.column()` 让报告更易定位。
4. **`severity` 选择**：
   - `error` —— 严重违规，会让 `sqlguard check` 退出码非 0，阻断 CI
   - `warning` —— 仅提示，不阻断
5. **`applies_to` 精确限定**：DDL 规则只对 `ddl` 类型生效，避免误报。
6. **脚本错误处理**：脚本运行时抛错会被引擎捕获并上报为该规则的一条违规，但语法错误会导致规则整体失败。
7. **脚本路径**：`script_path` 相对规则文件（`sqlguard.rules.toml` 或含 `[[rules]]` 的主配置）所在目录解析，也支持绝对路径。
8. **递归遍历子查询**：用顶层 `fn` 定义函数，在 `for s in sel.subqueries()` 中自调用——Rhai 不支持闭包捕获外部可变引用，但 `violations.push()` 在函数体内可正常调用。

## 12. 限制与注意事项

- AST 包装类型覆盖了 SELECT/INSERT/UPDATE/DELETE/CREATE TABLE/DROP/ALTER TABLE/TRUNCATE/CREATE VIEW/CREATE INDEX/事务语句等主要 DDL/DML 场景；存储过程体（`CREATE PROCEDURE` / `CREATE FUNCTION` 的 `BEGIN ... END` 块）sqlparser 0.60 解析能力有限，规则只能基于 kind 检测"存在 CREATE PROCEDURE"，无法检查过程体内容。
- **节点级位置信息**：sqlparser 0.60 的 AST 节点本身不携带位置，`JoinInfo` / `ColumnInfo` / `ForeignKeyInfo` / `CheckInfo` / `IndexInfo` / `UniqueInfo` / `ExprInfo` 上的 `line()` / `column()` 方法预留返回 0，需未来基于 token 流回填才能精确到 JOIN/列级。
- **逗号隐式 JOIN**：sqlparser 把 `FROM a, b` 与 `FROM a CROSS JOIN b` 都解析为 `JoinOperator::CrossJoin`，AST 上无标记区分。`SelectInfo.has_comma_join()` 与 `SqlAst.has_comma_join_anywhere()` 通过文本扫描（跟踪 paren depth 与子句终止关键字）回退实现，可靠但非 AST 原生。
- **注释**：sqlparser 在 parse 阶段丢弃注释，`SqlAst.comments()` 通过独立文本扫描得到，能识别 `--` 与 `/* */`，但不能区分注释是否在字符串字面量内（极端场景误报）。
- **表达式树深度**：`ExprInfo` 只暴露顶层判定，不递归暴露子表达式（避免类型爆炸）。若需深入子表达式，建议结合 `where_clause()` 文本做二次解析。
- Rhai 中整数默认为 `i64`，`line_count` 等字段按 `i64` 注入。
- Rhai 不支持闭包捕获外部可变引用，但可以通过 `for` 循环遍历并直接调用 `violations.push()`，或在顶层 `fn` 中调用。
- 同一文件的 AST 只解析一次，所有规则共享。
- Rhai 引擎由 `runner::build_engine()` 每次 `check` 构建一次，`run_rules_for_file` 复用同一个 `&Engine`；**不是**每条规则重建引擎，因此规则数量增长不会带来引擎构建开销。真正的每规则开销只有脚本读取 + 编译 + 执行。
