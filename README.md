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
│  3. 规则引擎       (rule/engine/)                              │
│     ├── mod.rs        模块入口                                │
│     ├── ast.rs        SqlAst / StmtInfo / SelectInfo 等包装类型│
│     ├── parser.rs     parse_sql_to_ast() → SqlAst             │
│     ├── analyzer.rs   analyze_expr() / collect_comments() 等   │
│     ├── scanner.rs    字符串扫描工具                          │
│     └── runner.rs     构建 Rhai 引擎，prepend helpers，执行规则│
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

生成 `sqlguard.toml`（主配置）+ `sqlguard.rules.toml`（规则配置）+ 16 条内置规则脚本（2 DDL + 14 DML）+ 示例 SQL 目录结构。规则配置单独拆分到 `sqlguard.rules.toml`，避免主配置文件随规则增多而过长。

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
│       ├── lib/
│       │   └── helpers.rhai      # 公共辅助函数（编译时嵌入，用户无需关心）
│       ├── ddl/
│       │   ├── no_drop_table.rhai
│       │   └── primary_key_required.rhai
│       └── dml/
│           ├── no_select_all.rhai
│           ├── no_delete_update_without_where.rhai
│           ├── insert_columns_required.rhai
│           ├── subquery_alias_required.rhai
│           ├── column_references_qualified.rhai
│           ├── no_join_without_condition.rhai
│           ├── no_unused_join.rhai
│           ├── no_unused_cte.rhai
│           ├── use_is_null.rhai
│           ├── use_coalesce.rhai
│           ├── no_order_by_in_subquery.rhai
│           ├── union_all_preferred.rhai
│           ├── no_nested_case.rhai
│           └── no_constant_where.rhai
├── tests/
│   └── integration_test.rs  # 集成测试（含 Mapper 模式）
├── sqlguard.toml.example    # 主配置示例（不含规则）
├── sqlguard.rules.toml.example  # 规则配置示例（[[rules]]）
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

完整配置见 [sqlguard.toml.example](sqlguard.toml.example) 与 [sqlguard.rules.toml.example](sqlguard.rules.toml.example)。主配置（结构 / 分类 / 输出 / Mapper / 扫描 / 文件检查）与规则配置（`[[rules]]`，拆到 `sqlguard.rules.toml`）分开维护；主配置通过 `rules_file = "sqlguard.rules.toml"` 引用规则文件，不写该字段时工具会自动在同目录查找 `sqlguard.rules.toml`。

### `[structure]` 目录结构约束

```toml
[structure]
paths = ["sql/ddl", "sql/dml"]  # 必须存在的目录
strict = true                    # 严格模式：多余目录也报错
allow_extra = [".gitkeep"]       # 允许的额外条目
```

### `[scan]` 文件扫描行为

控制 SQL 脚本扫描白名单与递归跳过目录，避免进入 `.git`/`target`/`node_modules` 等大目录。

```toml
[scan]
# SQL 脚本扫描白名单（相对配置文件目录或绝对路径）。
# 为空时回退到 [structure].paths，仍为空则扫描整个 target_dir（兜底）。
# 指定后只扫描这些目录下的 .sql/.ddl/.dml。
paths = []

# 递归扫描时跳过的目录名（按名称匹配，任意层级生效）。
# 适用于：SQL 脚本扫描、Mapper XML 扫描、目录结构校验三个场景。
# 未配置 [scan] 段时也按此默认黑名单生效。
exclude_dirs = [
  ".git", ".svn", ".hg", ".bzr",                    # 版本控制元数据
  "target", "node_modules", "build", "dist", "out",  # 构建产物
  ".idea", ".vscode",                                # IDE 配置
]
```

`paths` 与 `exclude_dirs` 组合语义：

| 场景 | `paths` | 行为 |
|------|---------|------|
| 默认 | `[]` | 回退到 `[structure].paths` 作为扫描白名单 |
| 全兜底 | `[]` 且 `[structure].paths` 也为空 | 扫描整个 `target_dir`（仅靠 `exclude_dirs` 过滤） |
| 精准白名单 | `["sql/ddl", "sql/dml"]` | 只扫描这些目录，白名单外的 SQL 不会被检查 |

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

### `[file_check]` 文件格式检查

对扫描到的每个文件做字节级检查（独立于 SQL 语法规则），检查与 SQL 无关的文件属性：

```toml
[file_check]
enabled = true
check_encoding = true               # UTF-8 无 BOM 检查（FILE001）
check_line_ending = true            # 换行符 LF 检查（FILE002）
encoding_severity = "error"         # 编码违规级别（必须）
line_ending_severity = "warning"    # 换行符违规级别（提示）
```

| 规则 | 检查内容 | 默认级别 | 说明 |
|------|----------|----------|------|
| `FILE001` | 编码为 UTF-8 且不带 BOM | `error`（必须） | 检测 UTF-8/UTF-16/UTF-32 BOM 及非法 UTF-8 字节 |
| `FILE002` | 换行符为 LF | `warning`（提示） | 检测 CRLF / 单独 CR，报告首处行号 |

两条检查归入 `file-format` 分组，缺省（未写 `[file_check]` 段）时按默认值启用。
可用 `--exclude-rules FILE001,FILE002` 或 `--exclude-groups file-format` 临时关闭。

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
- `violations` — 空数组，脚本通过 `violations.push(...)` 上报违规

**公共辅助函数**（`config/rules/lib/helpers.rhai`）由引擎编译时嵌入并在每条脚本执行前自动 prepend，规则脚本无需 import 即可直接调用：

| 函数 | 返回值 | 说明 |
|------|--------|------|
| `guard_parse_error(context)` | bool | AST 是否有解析错误 |
| `parse_error_violation(context)` | Map | 构建 parse error violation |
| `violation(msg, line, col)` | Map | 构建带行列号的 violation |
| `violation_line(msg, line)` | Map | 构建仅带行号的 violation |
| `violation_msg(msg)` | String | 构建纯消息 violation |

### 最小示例

```rhai
// 检查解析错误 + 遍历语句
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(violation(
            "DROP TABLE not allowed: " + d.name(),
            s.line(), s.column()
        ));
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

在 `sqlguard.toml` 中配置（规则写在 `sqlguard.rules.toml`）：

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