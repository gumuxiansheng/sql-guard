# SqlGuard 规则脚本编写手册

## 1. 概述

SqlGuard 使用 [Rhai](https://rhai.rs/) 脚本语言编写自定义规则。每条规则是一个 `.rhai` 文件，由引擎针对每个 SQL 文件独立执行一次。规则脚本通过遍历预解析的 AST 来检测违规，并将结果写入 `violations` 数组。

规则脚本具备以下能力：
- 读取 SQL 原文、文件路径、脚本类型等上下文信息
- 访问由 sqlparser 解析后的简化 AST
- 使用字符串匹配作为兜底（不推荐，AST 更精确）
- 上报多条违规，包含行号、列号

## 2. 执行模型

参考 [`src/rule/engine.rs`](../src/rule/engine.rs) 中的 `run_single_rule`：

1. 引擎读取 `.rhai` 脚本内容
2. 构造一个 `Scope`，注入两个全局变量：
   - `context` —— 一个 Map，包含 SQL 与 AST 信息
   - `violations` —— 一个空 Array，用于收集违规
3. 执行脚本
4. 读取 `violations` 数组，每项转换为 `Violation` 结构上报

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

访问方式：`context["ast"]`、`context["sql_content"]` 等。

### 3.2 `violations`（Array）

脚本通过 `violations.push(...)` 上报违规。每项支持两种形式：

**形式 A：字符串（仅消息，无行列号）**

```rhai
violations.push("some violation message");
```

**形式 B：对象 Map（推荐，含行列号）**

```rhai
violations.push(#{
    "message": "violation message",
    "line": 10,
    "column": 1
});
```

`line`、`column` 均为整数，可省略（缺省时为 `None`）。`severity` 与 `rule_name` 来自 `sqlguard.toml`，脚本无需指定。

## 4. AST API

AST 包装类型定义于 [`src/rule/engine.rs`](../src/rule/engine.rs)。所有方法均以 `&mut self` 注册，调用形式为 `obj.method()`。

### 4.1 `SqlAst`（通过 `context["ast"]` 获取）

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `statements()` | Array<StmtInfo> | 全部语句列表 |
| `has_parse_error()` | bool | 是否解析失败 |
| `parse_error()` | String | 解析错误信息（无错时为空串） |
| `kinds()` | Array<String> | 所有语句的 kind 列表 |
| `create_tables()` | Array<CreateInfo> | 全部 CREATE TABLE 信息 |
| `drop_objects()` | Array<DropInfo> | 全部 DROP 信息 |
| `selects()` | Array<SelectInfo> | 全部 SELECT 信息 |
| `has_create_table()` | bool | 是否含 CREATE TABLE |
| `has_drop_table()` | bool | 是否含 DROP TABLE |
| `has_alter_table()` | bool | 是否含 ALTER TABLE（且解析出详情） |
| `has_comma_join_anywhere()` | bool | 整个 SQL 文本中是否检测到 `FROM a, b` 形式的隐式逗号 JOIN（文本扫描，跨语句生效） |
| `comments()` | Array<CommentInfo> | 源文本中的所有注释（行注释 `--` 与块注释 `/* */`，sqlparser 解析时丢弃，这里通过独立文本扫描得到） |

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
| `columns()` | Array<ColumnInfo> | 列定义列表 |
| `has_primary_key()` | bool | 是否含主键（列级或表级约束） |
| `if_not_exists()` | bool | 是否带 `IF NOT EXISTS` |
| `column_names()` | Array<String> | 所有列名 |
| `foreign_keys()` | Array<ForeignKeyInfo> | 表级 + 列级外键约束合并列表 |
| `checks()` | Array<CheckInfo> | 表级 + 列级 CHECK 约束合并列表 |
| `indexes()` | Array<IndexInfo> | 表级 INDEX / KEY 约束列表（MySQL 风格） |
| `uniques()` | Array<UniqueInfo> | 表级 UNIQUE 约束列表（不含主键） |
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
| `operations()` | Array<AlterOpInfo> | 所有 ALTER 操作的结构化列表（含 ADD/DROP COLUMN、RENAME、ADD/DROP CONSTRAINT 等） |

### 4.6 `SelectInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `has_wildcard()` | bool | 是否含 `*` 或 `table.*`（递归覆盖顶层、UNION/集合运算分支、FROM/JOIN 子查询） |
| `projection()` | Array<String> | 顶层投影项文本表示 |
| `has_from_table()` | bool | 是否含 FROM 表 |
| `from_table()` | String | 第一张 FROM 表名（无则空串） |
| `from_table_alias()` | String | 主表别名（`FROM users u` 中的 `u`，无则空串） |
| `has_from_table_alias()` | bool | 主表是否带别名 |
| `joins()` | Array<JoinInfo> | JOIN 子句列表 |
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
| `has_limit()` | bool | 是否含 LIMIT |
| `has_offset()` | bool | 是否含 OFFSET |
| `has_fetch()` | bool | 是否含 FETCH（SQL:2008 风格分页） |
| `has_distinct()` | bool | 是否含 DISTINCT |
| `ctes()` | Array<CteInfo> | WITH 子句中的 CTE 列表 |
| `has_cte()` | bool | 是否含 WITH 子句 |
| `is_recursive()` | bool | WITH 是否带 RECURSIVE 关键字 |
| `subqueries()` | Array<SelectInfo> | 递归嵌套的子查询列表（FROM 派生表 / WHERE 标量子查询 / EXISTS / 集合运算括号分支） |
| `has_subquery()` | bool | 是否含任意子查询 |
| `has_window_function()` | bool | 投影/WHERE/HAVING 中是否含窗口函数（`OVER (...)`） |
| `window_functions()` | Array<WindowFuncInfo> | 窗口函数详情列表 |
| `where_expr()` | ExprInfo | WHERE 表达式顶层信息（无则返回默认空对象） |
| `has_where_expr()` | bool | WHERE 表达式是否已解析 |
| `having_expr()` | ExprInfo | HAVING 表达式顶层信息 |
| `has_having_expr()` | bool | HAVING 表达式是否已解析 |
| `projection_exprs()` | Array<ExprInfo> | 投影项的表达式信息列表 |
| `has_comma_join()` | bool | 该 SELECT 语句范围内是否检测到 `FROM a, b` 形式隐式逗号 JOIN |

### 4.6b `JoinInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `table_name()` | String | JOIN 的目标表名（不含别名） |
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
| `InsertInfo.columns()` | Array<String> | 显式指定的列名列表 |
| `InsertInfo.has_columns()` | bool | 是否显式指定列 |
| `UpdateInfo.table_name()` | String | UPDATE 目标表名 |
| `UpdateInfo.has_where()` | bool | 是否含 WHERE |
| `UpdateInfo.where_clause()` | String | WHERE 表达式文本 |
| `DeleteInfo.table_name()` | String | DELETE 目标表名 |
| `DeleteInfo.has_where()` | bool | 是否含 WHERE |
| `DeleteInfo.where_clause()` | String | WHERE 表达式文本 |

## 5. 新增 Info 类型（P1-P3 能力扩展）

以下 Info 类型由 [`src/rule/engine.rs`](../src/rule/engine.rs) 提供，对应 CTE / 窗口函数 / 表达式树 / 约束 / 事务 / 视图 / 索引 / 注释等 AST 能力。所有结构体均 `Clone + Default`，方法以 `&mut self` 注册到 Rhai 引擎。

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
| `columns()` | Array<String> | 本表外键列名 |
| `foreign_table()` | String | 引用的目标表名 |
| `referred_columns()` | Array<String> | 目标表的引用列名 |
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
| `IndexInfo.columns()` | Array<String> | 索引列名 |
| `IndexInfo.is_unique()` | bool | 是否为 UNIQUE 索引 |
| `UniqueInfo.name()` | String | 约束名 |
| `UniqueInfo.columns()` | Array<String> | 约束列名 |

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
| `CreateIndexInfo.columns()` | Array<String> | 索引列名 |
| `CreateIndexInfo.is_unique()` | bool | 是否为 UNIQUE 索引 |
| `TransactionInfo.kind()` | String | `START_TRANSACTION` / `COMMIT` / `ROLLBACK` |

### 5.9 `CommentInfo`（注释）

由 `SqlAst.comments()` 返回。sqlparser 在 parse 阶段丢弃注释，这里通过独立文本扫描得到。

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `text()` | String | 注释文本（不含 `--` 或 `/* */` 分隔符） |
| `line()` | INT | 注释所在行号（1-based） |
| `kind()` | String | `LINE`（行注释）或 `BLOCK`（块注释） |

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

let ast = context["ast"];

// 1. 解析失败时上报（推荐保留，避免漏检）
if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

// 2. 遍历语句，按 kind 分派
let stmts = ast.statements();
for s in stmts {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        // 检测条件
        if !ct.has_primary_key() {
            violations.push(#{
                "message": "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
```

## 8. 在 `sqlguard.toml` 中注册规则

参考 [`sqlguard.toml.example`](../sqlguard.toml.example)：

```toml
[[rules]]
id = "DDL001"                                    # 规则编号，必填且全局唯一
name = "no_drop_table"                            # 规则名（出现在报告中）
group = "ddl-safety"                              # 规则分组，可选
description = "Disallow DROP TABLE in DDL scripts" # 可选，描述
enabled = true                                    # 是否启用
script_path = "config/rules/ddl/no_drop_table.rhai"  # 相对配置文件所在目录
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
| `script_path` | 是 | Rhai 脚本路径，相对配置文件所在目录 |
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

## 10. 完整示例

### 10.1 禁止 DROP TABLE（DDL）

来自 [`config/rules/ddl/no_drop_table.rhai`](../config/rules/ddl/no_drop_table.rhai)：

```rhai
let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(#{
            "message": "DROP TABLE is not allowed in DDL scripts: " + d.name(),
            "line": s.line(),
            "column": s.column()
        });
    }
}
```

### 10.2 CREATE TABLE 必须有主键（DDL）

来自 [`config/rules/ddl/primary_key_required.rhai`](../config/rules/ddl/primary_key_required.rhai)：

```rhai
let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        if !ct.has_primary_key() {
            violations.push(#{
                "message": "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
```

### 10.3 禁止 SELECT *（DML）

来自 [`config/rules/dml/no_select_all.rhai`](../config/rules/dml/no_select_all.rhai)：

```rhai
let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "SELECT" {
        let sel = s.select();
        if sel.has_wildcard() {
            violations.push(#{
                "message": "SELECT * is not allowed. Specify columns explicitly.",
                "line": s.line(),
                "column": s.column()
            });
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
                    violations.push(#{
                        "message": "CTE '" + cte.name() + "' is defined but not referenced",
                        "line": s.line(),
                        "column": s.column()
                    });
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
            violations.push(#{
                "message": "ORDER BY in subquery is typically ignored unless combined with LIMIT/OFFSET",
                "line": 1
            });
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
    violations.push("DROP TABLE not allowed");
}
```

注意：字符串匹配会误判注释、字符串字面量中的内容，应优先使用 AST。

## 11. 调试与最佳实践

1. **始终先检查 `has_parse_error()`**：解析失败时 AST 为空，规则不会触发；建议上报以免漏检。
2. **优先用 AST 而非字符串匹配**：AST 能区分注释、字面量与真实语句。
3. **行列号尽量从 `StmtInfo` 取**：`s.line()` / `s.column()` 让报告更易定位。
4. **`severity` 选择**：
   - `error` —— 严重违规，会让 `sqlguard check` 退出码非 0，阻断 CI
   - `warning` —— 仅提示，不阻断
5. **`applies_to` 精确限定**：DDL 规则只对 `ddl` 类型生效，避免误报。
6. **脚本错误处理**：脚本运行时抛错会被引擎捕获并上报为该规则的一条违规，但语法错误会导致规则整体失败。
7. **脚本路径**：`script_path` 相对 `sqlguard.toml` 所在目录解析，也支持绝对路径。
8. **递归遍历子查询**：用顶层 `fn` 定义函数，在 `for s in sel.subqueries()` 中自调用——Rhai 不支持闭包捕获外部可变引用，但 `violations.push()` 在函数体内可正常调用。

## 12. 限制与注意事项

- AST 包装类型覆盖了 SELECT/INSERT/UPDATE/DELETE/CREATE TABLE/DROP/ALTER TABLE/TRUNCATE/CREATE VIEW/CREATE INDEX/事务语句等主要 DDL/DML 场景；存储过程体（`CREATE PROCEDURE` / `CREATE FUNCTION` 的 `BEGIN ... END` 块）sqlparser 0.45 解析能力有限，规则只能基于 kind 检测"存在 CREATE PROCEDURE"，无法检查过程体内容。
- **节点级位置信息**：sqlparser 0.45 的 AST 节点本身不携带位置，`JoinInfo` / `ColumnInfo` / `ForeignKeyInfo` / `CheckInfo` / `IndexInfo` / `UniqueInfo` / `ExprInfo` 上的 `line()` / `column()` 方法预留返回 0，需未来基于 token 流回填才能精确到 JOIN/列级。
- **逗号隐式 JOIN**：sqlparser 把 `FROM a, b` 与 `FROM a CROSS JOIN b` 都解析为 `JoinOperator::CrossJoin`，AST 上无标记区分。`SelectInfo.has_comma_join()` 与 `SqlAst.has_comma_join_anywhere()` 通过文本扫描（跟踪 paren depth 与子句终止关键字）回退实现，可靠但非 AST 原生。
- **注释**：sqlparser 在 parse 阶段丢弃注释，`SqlAst.comments()` 通过独立文本扫描得到，能识别 `--` 与 `/* */`，但不能区分注释是否在字符串字面量内（极端场景误报）。
- **表达式树深度**：`ExprInfo` 只暴露顶层判定，不递归暴露子表达式（避免类型爆炸）。若需深入子表达式，建议结合 `where_clause()` 文本做二次解析。
- Rhai 中整数默认为 `i64`，`line_count` 等字段按 `i64` 注入。
- Rhai 不支持闭包捕获外部可变引用，但可以通过 `for` 循环遍历并直接调用 `violations.push()`，或在顶层 `fn` 中调用。
- 同一文件的 AST 只解析一次，所有规则共享。
- 每条规则每次执行都会重新构建 Rhai 引擎并注册所有方法，规则较多时会有性能开销。
