# SqlGuard

一个面向 CI/CD 流水线的 SQL 脚本检查工具，支持**传统 SQL 脚本**和 **MyBatis Mapper XML** 两种模式，使用 [Rhai](https://rhai.rs/) 脚本编写自定义规则，提供目录结构校验、文件分类、多格式报告输出、**语句级增量校验**。

## 两种检查模式

### 模式一：SQL 脚本

扫描 `.sql` / `.ddl` / `.dml` 文件，按目录结构或 glob 规则自动分类（DDL / DML / other），逐文件应用规则脚本。

### 模式二：MyBatis Mapper

启用 `[mapper]` 配置后，扫描 `*Mapper.xml` 文件，自动提取 `<select>` / `<insert>` / `<update>` / `<delete>` 标签中的 SQL 语句，逐条应用规则脚本。支持：

- 动态标签剥离（`<if>` / `<where>` / `<foreach>` 等）
- `<include refid="..."/>` 同文件引用解析
- `#{name}` → `?`（值占位符标准化）
- `${tableName}` → `_var_tableName`（标识符合成为合法标识符，保证 sqlparser 可解析）
- 违规行号映射回 XML 原始行号

## 特性

- **可编程规则**：使用 Rhai 脚本语言编写自定义检查规则，操作 sqlparser 解析后的 AST
- **SQL 解析保障**：基于 [sqlparser](https://crates.io/crates/sqlparser) 解析 SQL AST，通过简化包装类型暴露给 Rhai 脚本，避免字符串匹配误判注释和字面量
- **规则筛选**：支持按规则编号（`id`）和分组（`group`）筛选，前缀通配（`DDL*`），黑白名单组合
- **目录结构校验**：强制要求的目录结构，支持严格模式和允许列表
- **三种报告格式**：终端彩色输出（plain）、结构化 JSON、深色主题 HTML 网页报告
- **增量校验**：`check-diff` 子命令通过 `git diff` 获取改动语句，按语句级 `[line, end_line] ∩ hunk` 过滤，CI 中只校验本次提交改动的 SQL
- **CI 友好**：`error` 级违规返回非零退出码，`warning` 级仅提示不阻断

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│                        sqlguard check                        │
├─────────────────────────────────────────────────────────────┤
│  1. 目录结构校验   (checker/directory)                        │
│  2. 文件收集 ──────┬── SQL 脚本 (.sql/.ddl/.dml)              │
│                    │     → checker/classification             │
│                    │     → 按 glob 规则分类为 ddl/dml/other   │
│                    │                                          │
│                    └── Mapper XML (mapper.enabled=true)       │
│                          → mapper/mod.rs                      │
│                          → mapper/parser.rs 提取 SQL          │
│                          → mapper/placeholder.rs 标准化占位符  │
│                          → mapper/include.rs 解析 <include>   │
│                                                                 │
│  3. 规则引擎       (rule/engine.rs)                            │
│     ├── parse_sql_to_ast()    sqlparser → SqlAst 包装类型      │
│     ├── build_engine()        构建 Rhai 引擎，注册方法          │
│     ├── run_rules_for_file()  对每个 SQL 单元执行所有适用规则   │
│     └── RuleFilter            按 id/group 筛选规则              │
│                                                                 │
│  4. 报告输出       (reporter/)                                  │
│     ├── plain.rs    终端彩色输出                                │
│     ├── json.rs     结构化 JSON 报告                            │
│     └── html.rs     深色主题网页报告                            │
└─────────────────────────────────────────────────────────────┘
```

## 快速开始

### 安装

从 [deploy](deploy/) 目录选取对应平台的二进制文件，放入 `PATH`：

```bash
# Linux x86_64
cp deploy/sqlguard-x86_64-linux-musl /usr/local/bin/sqlguard
chmod +x /usr/local/bin/sqlguard

# Linux ARM64
cp deploy/sqlguard-aarch64-linux-musl /usr/local/bin/sqlguard

# macOS
cp deploy/sqlguard-x86_64-apple-darwin /usr/local/bin/sqlguard
```

### 初始化项目

```bash
sqlguard init .
```

生成 `sqlguard.toml` 配置文件 + 3 条内置规则脚本 + 示例 SQL 目录结构。

### 编写 SQL 脚本

```bash
mkdir -p sql/ddl sql/dml sql/others
```

```sql
-- sql/ddl/create_users.sql
CREATE TABLE users (
    id INT PRIMARY KEY,
    name VARCHAR(100)
);
```

```sql
-- sql/dml/query_users.sql
SELECT id, name FROM users WHERE id = 1;
```

### 执行检查

```bash
sqlguard check ./sql
```

输出示例：

```
══════════════════════════════════════════════════════════════
 SqlGuard Report
══════════════════════════════════════════════════════════════

Files checked: 2

✓ No violations found

────────────────────────────────────────
Summary: All checks passed
```

## 项目结构

```
SqlGuard/
├── src/
│   ├── main.rs              # 入口，调度 check / init 流程
│   ├── cli.rs               # CLI 命令定义（clap）
│   ├── config.rs            # 配置加载与校验（toml 反序列化）
│   ├── error.rs             # 错误类型、Violation 结构体
│   ├── checker/
│   │   ├── mod.rs
│   │   ├── directory.rs     # 目录结构校验
│   │   └── classification.rs # 文件分类（glob 匹配）
│   ├── rule/
│   │   ├── mod.rs
│   │   └── engine.rs        # 规则引擎：AST 包装、Rhai 注册、规则执行
│   ├── mapper/
│   │   ├── mod.rs           # Mapper 模式入口：文件收集、类型映射
│   │   ├── parser.rs        # XML 解析与 SQL 提取（quick-xml）
│   │   ├── include.rs       # <include refid="..."/> 引用解析
│   │   └── placeholder.rs   # #{} / ${} 占位符标准化
│   └── reporter/
│       ├── mod.rs
│       ├── plain.rs         # 终端彩色报告
│       ├── json.rs          # JSON 报告
│       └── html.rs          # HTML 报告
├── config/
│   └── rules/
│       ├── ddl/
│       │   ├── no_drop_table.rhai
│       │   └── primary_key_required.rhai
│       └── dml/
│           └── no_select_all.rhai
├── tests/
│   └── integration_test.rs  # 集成测试（含 Mapper 模式）
├── sqlguard.toml.example    # 完整配置示例
├── docs/
│   └── rule-scripting.md    # 规则脚本编写手册
├── deploy/
│   ├── sqlguard-x86_64-apple-darwin
│   ├── sqlguard-x86_64-linux-musl
│   ├── sqlguard-aarch64-linux-musl
│   └── USAGE.md             # 使用手册
├── Cargo.toml
└── README.md
```

## 配置

完整配置见 [sqlguard.toml.example](sqlguard.toml.example)。四个主要区块：

### `[structure]` 目录结构约束

```toml
[structure]
paths = ["sql/ddl", "sql/dml"]  # 必须存在的目录
strict = true                    # 严格模式：多余目录也报错
allow_extra = [".gitkeep"]       # 允许的额外条目
```

### `[[classification.rules]]` 文件分类

按 glob 规则匹配文件路径，分配 `type`（DDL / DML / SQL / other）。规则按 `priority` 降序匹配，首个命中生效。

```toml
[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10
```

### `[[rules]]` 规则定义

```toml
[[rules]]
id = "DDL001"                                    # 必填，全局唯一
name = "no_drop_table"                           # 规则名
group = "ddl-safety"                             # 可选，分组（CLI 筛选用）
description = "Disallow DROP TABLE in DDL scripts"
enabled = true
script_path = "config/rules/ddl/no_drop_table.rhai"
applies_to = ["ddl"]                             # 仅对匹配的 script_type 生效
severity = "error"                               # error / warning
```

### `[mapper]` MyBatis Mapper 模式

```toml
[mapper]
enabled = true
paths = ["src/main/resources/mapper"]
patterns = ["**/*Mapper.xml", "**/*.xml"]
```

缺省或 `enabled = false` 时仅扫描 SQL 脚本文件。

### `[output]` 输出配置

```toml
[output]
formats = ["plain", "json", "html"]
```

## CLI 命令

### `sqlguard check`

```
sqlguard check [OPTIONS] [PATH]

参数：
  [PATH]                待检查目录，默认当前目录

选项：
  -c, --config <FILE>   配置文件路径，默认 sqlguard.toml
  -f, --format <FMT>    报告格式：plain / json / html / all，默认 plain
  -o, --output-dir <D>  输出目录（json/html 必需）
      --rules <IDS>     仅执行指定 id 的规则，逗号分隔，支持前缀通配（如 DDL*）
      --groups <G>      仅执行指定分组的规则
      --exclude-rules <IDS>  排除指定 id
      --exclude-groups <G>   排除指定分组
```

### `sqlguard init`

```bash
sqlguard init [PATH]     # 生成默认配置与示例规则
```

### `sqlguard check-diff`

只校验自指定 git 基线以来改动的 SQL 语句（增量模式），适合 CI 中只检查 PR/push 的本次改动。

```
sqlguard check-diff --base <BASE> [OPTIONS] [PATH]

参数：
  [PATH]                待检查目录，默认当前目录

选项：
      --base <BASE>     ★必填★ Git 基线：commit / branch / tag，如 origin/main、HEAD~1
  -c, --config <FILE>   配置文件路径，默认 sqlguard.toml
  -f, --format <FMT>    报告格式：plain / json / html / all，默认 plain
  -o, --output-dir <D>  输出目录（json/html 必需）
      --rules <IDS>     仅执行指定 id 的规则，逗号分隔，支持前缀通配
      --groups <G>      仅执行指定分组的规则
      --exclude-rules <IDS>  排除指定 id
      --exclude-groups <G>   排除指定分组
```

工作流程：

1. 调用 `git diff --unified=0 <base>...HEAD -- *.sql *.ddl *.dml [mapper patterns]` 获取改动文件与 hunk 行范围
2. 对每个改动文件运行规则（规则看到全部语句，跨语句规则不破坏）
3. 用 `[line, end_line] ∩ hunk` 语句级交集过滤 violation——只保留与改动有交集的违规
4. 新增文件整体算改动（`is_new=true` 跳过 hunk 过滤）
5. 输出报告，`error` 级违规返回退出码 1

```bash
# PR 场景：与 origin/main 比较
sqlguard check-diff --base origin/main -f json -o reports/

# 本地开发：与上一次提交比较
sqlguard check-diff --base HEAD~1

# 指定规则筛选
sqlguard check-diff --base origin/main --groups dml-safety
```

行号精度：`end_line` 由 sqlparser 解析后取下一 token 行号 - 1 近似计算，规则脚本产出的 violation 自动从 AST 回填 `end_line`（脚本零改动）。

## 规则筛选

通过 CLI 参数按 `id` 或 `group` 精准控制规则执行：

```bash
# 只跑 DDL 相关规则
sqlguard check ./sql --groups ddl-safety

# 只跑指定编号
sqlguard check ./sql --rules DDL001,DDL002

# 前缀通配：所有 DDL 开头的规则
sqlguard check ./sql --rules DDL*

# 排除实验性规则
sqlguard check ./sql --exclude-groups experimental

# 组合使用
sqlguard check ./sql --groups ddl-safety --exclude-rules DDL003
```

筛选语义：

- **白名单**（`--rules` / `--groups`）：为空表示不限制；非空时规则需命中白名单才执行
- **黑名单**（`--exclude-rules` / `--exclude-groups`）：命中即跳过，**优先级高于白名单**
- **前缀通配**：`DDL*` 匹配所有以 `DDL` 开头的 id
- `enabled = false` 的规则永远不会执行
- 筛选仅影响规则执行，不影响目录结构检查与文件分类

## 规则脚本

规则使用 [Rhai](https://rhai.rs/) 编写，引擎为每个 SQL 单元注入两个全局变量：

- `context` — 包含 `sql_content`、`file_path`、`script_type`、`ast` 等字段
- `violations` — 空数组，脚本通过 `push()` 上报违规

### 最小示例

```rhai
let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(#{
            "message": "DROP TABLE not allowed: " + d.name(),
            "line": s.line(),
            "column": s.column()
        });
    }
}
```

### AST API 摘要

`SqlAst`：`statements()` / `has_parse_error()` / `parse_error()` / `has_create_table()` / `has_drop_table()` / `kinds()` / `create_tables()` / `drop_objects()` / `selects()`

`StmtInfo`：`kind()` / `line()` / `end_line()` / `line_range()` / `column()` / `create_table()` / `drop_object()` / `select()` / `insert()` / `update()` / `delete()`

`CreateInfo`：`table_name()` / `columns()` / `has_primary_key()` / `if_not_exists()` / `column_names()`

`ColumnInfo`：`name()` / `data_type()` / `is_primary_key()` / `is_not_null()` / `is_unique()`

`DropInfo`：`object_type()` / `name()` / `if_exists()`

`SelectInfo`：`has_wildcard()` / `projection()` / `has_from_table()` / `from_table()`

`InsertInfo`：`table_name()` / `columns()` / `has_columns()`

`UpdateInfo`：`table_name()` / `has_where()` / `where_clause()`

`DeleteInfo`：`table_name()` / `has_where()` / `where_clause()`

> 完整 API 参考与编写指南见 [docs/rule-scripting.md](docs/rule-scripting.md)。

## MyBatis Mapper 模式详解

### 启用方式

在 `sqlguard.toml` 中配置：

```toml
[mapper]
enabled = true
paths = ["src/main/resources/mapper"]
patterns = ["**/*Mapper.xml", "**/*.xml"]
```

### 处理流程

1. 收集 `paths` 下匹配 `patterns` 的 `.xml` 文件
2. 第一遍扫描：收集所有 `<sql id="...">` 片段
3. 第二遍扫描：提取 `<select>` / `<insert>` / `<update>` / `<delete>` 标签内的 SQL：
   - 剥离动态标签（`<if>` / `<where>` / `<foreach>` 等），保留其内部文本
   - 解析 `<include refid="..."/>`，替换为对应片段内容（递归，最大深度 10）
   - `#{name, jdbcType=VARCHAR}` → `?`（值占位符）
   - `${tableName}` → `_var_tableName`（标识符合成为合法 SQL 标识符）
4. 每条 SQL 独立提交给规则引擎，行号映射回 XML 原文件

### 已知限制

- `<include>` 仅支持同文件内引用（不跨 namespace）
- 动态 SQL 标签剥离后，sqlparser 可能因条件分支导致语法不完整而解析失败
- statement 类型统一映射为 `dml`（当前版本）

## 报告格式

### plain（默认）

终端彩色输出，`ERROR` 红 / `WARNING` 黄 / `INFO` 青，含规则编号、分组、文件路径与行列号。

### json

`sqlguard-report.json`：结构化数据，适合 CI 系统解析。每条违规包含 `rule_id`、`rule`、`group`、`severity`、`file`、`line`、`end_line`、`column`、`message`。

### html

`sqlguard-report.html`：深色主题网页报告，卡片式统计数据，可排序表格，适合归档。

### 多格式输出

```bash
sqlguard check ./sql -f all -o reports/
```

## CI/CD 集成

### GitHub Actions

```yaml
- name: SqlGuard Check
  run: |
    sqlguard check ./sql
```

`error` 级违规返回退出码 1，自动阻断 CI 流水线。

### 增量校验（推荐用于 PR）

只检查本次提交改动的 SQL，避免已存量代码的违规阻塞新提交。基于 `git diff` 按语句级 `[line, end_line] ∩ hunk` 交集过滤。

```yaml
name: sqlguard-incremental
on:
  pull_request:
    paths:
      - '**/*.sql'
      - '**/*.ddl'
      - '**/*.dml'
      - 'src/main/resources/mapper/**/*.xml'

jobs:
  incremental-check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0          # ★ 关键：需要完整 git 历史才能 diff

      - name: Fetch base branch
        run: git fetch origin ${{ github.base_ref }}

      - name: SqlGuard incremental check
        run: |
          sqlguard check-diff \
            --base origin/${{ github.base_ref }} \
            -f json -o reports/
```

`check-diff` 同样返回退出码 1 阻断流水线，但只上报本次改动语句的违规。新增文件整体算改动（全文件校验），修改文件按 hunk 过滤。

### 分阶段检查

```bash
# PR 只检查 DDL 安全规则
sqlguard check ./sql --groups ddl-safety

# 完整检查但排除实验性规则
sqlguard check ./sql --exclude-groups experimental
```

## 退出码

| 退出码 | 含义 |
|--------|------|
| 0 | 无 `error` 级违规，目录结构无误 |
| 1 | 存在 `error` 级违规，或目录结构问题 |
| 2 | 配置错误 / 文件读取失败等系统错误 |

## 构建

### 前置要求

- Rust 1.72+（推荐 1.80+）

### 本机构建

```bash
cargo build --release
# 产物在 target/release/sqlguard
```

### 交叉编译（其他平台）

```bash
# Linux x86_64 / aarch64（需 nightly + musl 目标）
rustup target add x86_64-unknown-linux-musl --toolchain nightly
rustup target add aarch64-unknown-linux-musl --toolchain nightly
cargo +nightly build --release --target x86_64-unknown-linux-musl
cargo +nightly build --release --target aarch64-unknown-linux-musl

# Windows x86_64（需在 Windows 上构建）
cargo build --release --target x86_64-pc-windows-gnu
```

### 运行测试

```bash
cargo test --release
```

## 技术栈

| 依赖 | 用途 |
|------|------|
| [sqlparser](https://crates.io/crates/sqlparser) | SQL 解析，生成 AST |
| [Rhai](https://rhai.rs/) | 嵌入式脚本引擎，执行自定义规则 |
| [quick-xml](https://crates.io/crates/quick-xml) | MyBatis XML 解析与 SQL 提取 |
| [clap](https://crates.io/crates/clap) | CLI 参数解析 |
| [globset](https://crates.io/crates/globset) | 文件匹配与分类 |
| [colored](https://crates.io/crates/colored) | 终端彩色输出 |
| [serde](https://crates.io/crates/serde) / [toml](https://crates.io/crates/toml) | 配置序列化 |

## License

MIT