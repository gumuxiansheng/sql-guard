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

### 4.2 `StmtInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `kind()` | String | 语句类型，见下表 |
| `line()` | INT | 起始行号 |
| `column()` | INT | 起始列号 |
| `has_create_table()` | bool | 是否为 CREATE TABLE |
| `has_drop_object()` | bool | 是否为 DROP |
| `has_select()` | bool | 是否为 SELECT |
| `create_table()` | CreateInfo | CREATE TABLE 详情（非 CREATE TABLE 时返回默认空对象） |
| `drop_object()` | DropInfo | DROP 详情 |
| `select()` | SelectInfo | SELECT 详情 |

**支持的 `kind` 值**：

| kind | SQL 语句 |
|------|----------|
| `CREATE_TABLE` | `CREATE TABLE` |
| `DROP_TABLE` | `DROP TABLE` |
| `DROP_VIEW` | `DROP VIEW` |
| `DROP_INDEX` | `DROP INDEX` |
| 其他 `DROP_<TYPE>` | 其他 DROP 对象类型 |
| `SELECT` | `SELECT` / `Query` |
| `INSERT` | `INSERT` |
| `UPDATE` | `UPDATE` |
| `DELETE` | `DELETE` |
| `ALTER_TABLE` | `ALTER TABLE` |
| `OTHER` | 未识别的语句 |

### 4.3 `CreateInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `table_name()` | String | 表名 |
| `columns()` | Array<ColumnInfo> | 列定义列表 |
| `has_primary_key()` | bool | 是否含主键（列级或表级约束） |
| `if_not_exists()` | bool | 是否带 `IF NOT EXISTS` |
| `column_names()` | Array<String> | 所有列名 |

### 4.4 `ColumnInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `name()` | String | 列名 |
| `data_type()` | String | 数据类型 |
| `is_primary_key()` | bool | 是否为列级 PRIMARY KEY |
| `is_not_null()` | bool | 是否 NOT NULL |
| `is_unique()` | bool | 是否 UNIQUE |

### 4.5 `DropInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `object_type()` | String | 对象类型（`TABLE`、`VIEW` 等） |
| `name()` | String | 对象名（多个时以 `, ` 连接） |
| `if_exists()` | bool | 是否带 `IF EXISTS` |

### 4.6 `SelectInfo`

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `has_wildcard()` | bool | 投影列表是否含 `*` 或 `table.*` |
| `projection()` | Array<String> | 投影项文本表示 |
| `has_from_table()` | bool | 是否含 FROM 表 |
| `from_table()` | String | 第一张 FROM 表名（无则空串） |

## 5. Rhai 语法要点

- **变量声明**：`let x = ...;`
- **对象 Map 字面量**：`#{ "key": value, "key2": value2 }`（`#` 是 Rhai 的对象字面量前缀）
- **数组**：`[1, 2, 3]`，方法 `push()`、`len()`、索引访问 `arr[0]`
- **循环**：`for x in arr { ... }`
- **字符串拼接**：`"a" + "b"`
- **Map 访问**：`map["key"]` 或 `map.key`
- **注释**：`//` 单行
- **条件**：`if cond { ... } else if cond2 { ... } else { ... }`

## 6. 规则脚本模板

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

## 7. 在 `sqlguard.toml` 中注册规则

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

## 8. 在命令行筛选规则

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

## 9. 完整示例

### 9.1 禁止 DROP TABLE（DDL）

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

### 9.2 CREATE TABLE 必须有主键（DDL）

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

### 9.3 禁止 SELECT *（DML）

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

### 9.4 基于字符串匹配的简单规则（不推荐，但可用）

当 AST 无法覆盖时使用：

```rhai
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("DROP TABLE") {
    violations.push("DROP TABLE not allowed");
}
```

注意：字符串匹配会误判注释、字符串字面量中的内容，应优先使用 AST。

## 10. 调试与最佳实践

1. **始终先检查 `has_parse_error()`**：解析失败时 AST 为空，规则不会触发；建议上报以免漏检。
2. **优先用 AST 而非字符串匹配**：AST 能区分注释、字面量与真实语句。
3. **行列号尽量从 `StmtInfo` 取**：`s.line()` / `s.column()` 让报告更易定位。
4. **`severity` 选择**：
   - `error` —— 严重违规，会让 `sqlguard check` 退出码非 0，阻断 CI
   - `warning` —— 仅提示，不阻断
5. **`applies_to` 精确限定**：DDL 规则只对 `ddl` 类型生效，避免误报。
6. **脚本错误处理**：脚本运行时抛错会被引擎捕获并上报为该规则的一条违规，但语法错误会导致规则整体失败。
7. **脚本路径**：`script_path` 相对 `sqlguard.toml` 所在目录解析，也支持绝对路径。

## 11. 限制与注意事项

- AST 包装类型当前只暴露上述方法；若需要检查 ALTER TABLE 细节、JOIN 结构、子查询等，需在 [`src/rule/engine.rs`](../src/rule/engine.rs) 中扩展 `StmtInfo` / 新增字段并注册方法。
- Rhai 中整数默认为 `i64`，`line_count` 等字段按 `i64` 注入。
- Rhai 不支持闭包捕获外部可变引用，但可以通过 `for` 循环遍历并直接调用 `violations.push()`。
- 同一文件的 AST 只解析一次，所有规则共享。
- 每条规则每次执行都会重新构建 Rhai 引擎并注册所有方法，规则较多时会有性能开销。
