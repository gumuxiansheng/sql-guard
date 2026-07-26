# SqlGuard

一个面向 CI/CD 流水线的 SQL 脚本检查工具，支持**传统 SQL 脚本**和 **MyBatis Mapper XML** 两种模式，使用 [Rhai](https://rhai.rs/) 脚本编写自定义规则，提供目录结构校验、文件分类、多格式报告输出、**语句级增量校验**、**备份回滚脚本自动生成**。

## 两种检查模式

### 模式一：SQL 脚本

扫描 `.sql` / `.ddl` / `.dml` 文件，按目录结构或 glob 规则自动分类（DDL / DML / other），逐文件应用规则脚本。

### 模式二：MyBatis Mapper

启用 `[mapper]` 配置后，扫描 `*Mapper.xml` 文件，自动提取 `<select>` / `<insert>` / `<update>` / `<delete>` 标签中的 SQL 语句，逐条应用规则脚本。支持：

- 动态标签剥离（`<if>` / `<where>` / `<foreach>` 等）
- `<include refid="..."/>` 引用解析：同文件短 id（`refid="cols"`）与跨 namespace（`refid="com.example.UserMapper.cols"`）均支持
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
- **备份回滚生成**：对每条 DDL/DML 自动生成 `backup.sql` / `rollback.sql` / `rollback-manifest.json` / `cleanup.sql`，支持 MySQL / PostgreSQL 双方言、多模式锁策略、长事务预检查、binlog 控制、锁合并、schema 漂移校验、分区表检测，配合发布平台在变更失败时回滚（详见 [docs/backup-rollback-design.md](docs/backup-rollback-design.md)）
- **动态重放清单导出**：`replay-export` 子命令把 SQL 脚本与 Mapper 语句导出为 `sql-manifest.json`，供分离的 Java 工程 [`replay/`](replay/) 在镜像库上做 EXPLAIN 重放、识别慢 SQL。**注意：SqlGuard 二进制本身只导出清单，不执行重放**；重放由 `sqlguard-replay`（Java/Maven，仅面向 GaussDB/openGauss）完成，需自备 JDBC 驱动
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
│     ├── html.rs     深色主题网页报告                            │
│     └── sarif.rs    SARIF v2.1.0（GitHub/Azure/GitLab 扫描）   │
└─────────────────────────────────────────────────────────────┘
```

## 快速开始

### 安装

四种方式任选其一：

#### 方式 1：`cargo install`（推荐，跨平台）

```bash
cargo install sqlguard
```

Rust 工具链会自动编译并安装到 `~/.cargo/bin/sqlguard`，加入 `PATH` 即可使用。

#### 方式 2：预编译二进制

从 [GitHub Releases](../../releases) 下载对应平台的二进制：

| 平台 | 文件 |
|------|------|
| Linux x86_64 (musl, 静态) | `sqlguard-x86_64-unknown-linux-musl` |
| Linux aarch64 (musl, 静态) | `sqlguard-aarch64-unknown-linux-musl` |
| macOS x86_64 (Intel) | `sqlguard-x86_64-apple-darwin` |
| macOS aarch64 (Apple Silicon) | `sqlguard-aarch64-apple-darwin` |
| Windows x86_64 | `sqlguard-x86_64-pc-windows-gnu.exe` |

```bash
# Linux / macOS
chmod +x sqlguard-* && mv sqlguard-* /usr/local/bin/sqlguard

# Windows
# 重命名为 sqlguard.exe 并加入 PATH
```

二进制由 [Release 工作流](.github/workflows/release.yml) 在打 tag 时自动构建发布。

#### 方式 3：Docker

```bash
docker run --rm -v "$PWD:/work" ghcr.io/sqlguard/sqlguard:latest check /work/sql
```

镜像见 [Dockerfile](Dockerfile)。

#### 方式 4：从源码构建

```bash
git clone https://github.com/sqlguard/sqlguard.git
cd sqlguard
cargo build --release
# 产物在 target/release/sqlguard
```

### 初始化项目

```bash
sqlguard init .
```

生成 `sqlguard.toml`（主配置）+ `sqlguard.rules.toml`（规则配置）+ 21 条内置规则脚本（6 DDL + 15 DML）+ 示例 SQL 目录结构。规则配置单独拆分到 `sqlguard.rules.toml`，避免主配置文件随规则增多而过长。

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

### 生成备份回滚脚本

```bash
sqlguard gen-rollback ./sql -o rollback_out/
```

为每条 DDL/DML 生成 `backup.sql` / `rollback.sql` / `rollback-manifest.json` / `cleanup.sql`，配合发布平台在变更失败时回滚。退出码遵循 manifest：`0`=全部可靠，`1`=仅 warning，`2`=error（irreversible/unreliable/partial）。

支持 CLI 覆盖配置：

```bash
sqlguard gen-rollback ./sql -o rollback_out/ \
    --dialect mysql \
    --lock-scope global \
    --lock-timeout 60 \
    --accept-table-lock-risk
```

详见 [docs/backup-rollback-design.md](docs/backup-rollback-design.md)。

### 导出动态重放清单

```bash
sqlguard replay-export ./sql -o manifest_out/
```

把扫描到的 SQL 脚本与 Mapper 语句导出为 `sql-manifest.json`，每条语句记录其类型（select/insert/update/delete/merge/ddl/other）、来源文件、行号范围，Mapper 动态分支会按 `<if>`/`<foreach>` 组合展开为多个变体。

**本子命令只导出清单，不连数据库、不执行重放。** 真正的重放由分离的 Java 工程 [`replay/`](replay/)（`sqlguard-replay`）完成：读取清单、连接镜像库、对每条 SQL 跑 `EXPLAIN`、采集执行计划与耗时、识别慢 SQL 与次优计划。

支持 CLI 参数：

```bash
sqlguard replay-export ./sql -o manifest_out/ \
    --types select,insert           # 仅导出指定类型，空 = 全部
```

**重放工程的使用前提与限制**（重要）：

| 项 | 说明 |
|------|------|
| 工程位置 | `replay/` 目录，Maven 项目（`mvn package`） |
| 目标数据库 | **仅 GaussDB / openGauss 镜像库**（不直接支持 MySQL/PG 重放） |
| JDBC 驱动 | **不在 pom.xml 声明**，运行期通过 `--driver-jar` 参数以 URLClassLoader 动态加载（避免私有 jar 进构建依赖） |
| 是否随二进制分发 | **否**，SqlGuard 发布物只含 Rust 二进制；Java 重放工程需单独 `mvn package` 构建 |
| 输入 | 上一步生成的 `sql-manifest.json` |
| 输出 | 每条语句的执行计划、耗时、慢 SQL 报告 |

详见 `replay/` 工程的 `Main.java` 与 `pom.xml`。

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
│   ├── rollback/            # 备份回滚生成（gen-rollback）
│   │   ├── mod.rs           # 模块入口、SafetyClass / BackupRollbackPair
│   │   ├── dialect.rs       # 方言适配（MySQL / PostgreSQL）
│   │   ├── naming.rs        # 备份表命名 bks_xxx_YYYYMMDD_NNNN
│   │   ├── generator.rs     # 主调度，按 StmtInfo.kind 分发
│   │   ├── ddl.rs           # DDL 反向操作（CREATE/ALTER ADD/RENAME）
│   │   ├── ddl_like.rs      # CREATE TABLE LIKE 统一模式（DROP/ALTER DROP/MODIFY）
│   │   ├── dml.rs           # DML 增量备份（INSERT/UPDATE/DELETE/TRUNCATE/REPLACE）
│   │   ├── pk.rs            # 主键解析（配置 → StmtInfo → bks_ JOIN 推断）
│   │   ├── render.rs        # SQL 文本渲染（事务包裹、锁合并、预检查）
│   │   └── manifest.rs      # rollback-manifest.json 序列化
│   ├── replay_export.rs     # replay-export 子命令：导出 sql-manifest.json（只导出不重放）
│   └── reporter/
│       ├── mod.rs
│       ├── plain.rs         # 终端彩色报告
│       ├── json.rs          # JSON 报告
│       └── html.rs          # HTML 报告
├── replay/                  # ★ 分离的 Java/Maven 工程（sqlguard-replay），不随二进制分发
│   ├── pom.xml              # 仅 GaussDB/openGauss；JDBC 驱动运行期动态加载
│   └── src/main/java/com/sqlguard/replay/
│       ├── Main.java        # 入口，读 manifest → 连镜像库 → EXPLAIN → 报告
│       ├── replay/Replayer.java
│       └── plan/            # ExplainAdapter / PlanParser / PlanCollector
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
│           ├── no_constant_where.rhai
│           └── order_by_required_for_pagination.rhai
├── tests/
│   └── integration_test.rs  # 集成测试（含 Mapper 模式）
├── sqlguard.toml.example    # 主配置示例（不含规则）
├── sqlguard.rules.toml.example  # 规则配置示例（[[rules]]）
├── docs/
│   ├── rule-scripting.md    # 规则脚本编写手册
│   ├── default-rules.md     # 默认规则手册（21 条内置规则）
│   └── backup-rollback-design.md  # 备份回滚设计文档
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

### `[rollback]` 备份回滚生成

对每条 DDL/DML 自动生成 `backup.sql` / `rollback.sql` / `rollback-manifest.json` / `cleanup.sql`，配合发布平台在变更失败时回滚。总开关默认 `false`，不启用时 `check` 流程不触发回滚生成，保持纯检查工具行为。详见 [docs/backup-rollback-design.md](docs/backup-rollback-design.md)。

```toml
[rollback]
enabled = false                       # 总开关，默认不启用
dialect = "mysql"                     # mysql / postgresql
backup_mode = "auto"                  # auto / full / incremental
backup_file = "backup.sql"
rollback_file = "rollback.sql"
manifest_file = "rollback-manifest.json"
cleanup_file = "cleanup.sql"
wrap_transaction = true               # 仅 PG 生效；MySQL DDL 隐式提交无效
backup_table_prefix = "bks_"          # 与 DDL004 对齐
backup_table_with_date = true         # bks_xxx_YYYYMMDD_NNNN

# ★ D1 锁策略（F14）
lock_scope = "auto"                   # auto / global / table / snapshot / none
lock_timeout = 30                     # FTWRL 等待超时（秒）
on_long_transaction = "abort"         # abort / warn / ignore
long_transaction_threshold = 5        # 长事务阈值（秒）

# ★ D2 binlog 控制（F16）
binlog_strategy = "auto"              # auto / always / never

# schema 漂移校验（F13）与分区表策略（F15）
assert_on_schema_mismatch = "abort"   # abort / warn / ignore
on_partitioned_table = "abort"        # abort / warn / fallback

# ★ D5 备份表保留天数（F19）
backup_table_retention_days = 7

# ★ D6 锁合并（N9）
coalesce_locks = true
coalesce_locks_mode = "conservative"  # conservative / aggressive

# ★ R2 lock_scope=table 必须显式确认
accept_table_lock_risk = false

# 显式主键声明（未声明时按 StmtInfo.create_table / bks_ JOIN 推断）
# [[rollback.primary_keys]]
# table = "users"
# columns = ["id"]
```

关键字段说明：

| 字段 | 说明 |
|------|------|
| `dialect` | 影响标识符引用、`CREATE TABLE LIKE` 语法、`RENAME` 等差异 |
| `lock_scope` | `auto`：脚本含 DDL 或 backup 含 DDL → `global`，纯 DML → `snapshot`；`table` 模式需配 `accept_table_lock_risk = true` |
| `binlog_strategy` | `auto`：含 DDL 不设 `sql_log_bin=0`，纯 DML 设；`always` 始终设；`never` 不设（避免 GTID 主从不一致） |
| `on_long_transaction` | 备份前查询 `innodb_trx` + `processlist`（MySQL）或 `pg_stat_activity`（PG），超阈值按策略处理 |
| `coalesce_locks` | 合并连续同表段的锁区间，组内统一发射锁/解锁；`conservative` 仅全表 LIKE 合并，`aggressive` 同表增量也合并 |
| `assert_on_schema_mismatch` | 执行期比对 `expected_schema` 与实际 schema，漂移时按策略处理 |
| `on_partitioned_table` | 分区表 `CREATE TABLE LIKE` 生成非分区表，按策略决定 abort / warn / fallback |

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

### `sqlguard gen-rollback`

为 DDL/DML 文件生成备份回滚脚本四件套，退出码遵循 manifest（`0`=可靠 / `1`=仅 warning / `2`=error）。

```
sqlguard gen-rollback [OPTIONS] [PATH]

参数：
  [PATH]                待扫描目录，默认当前目录

选项：
  -c, --config <FILE>            配置文件路径，默认 sqlguard.toml
  -o, --output-dir <D>           输出目录（backup.sql / rollback.sql / manifest / cleanup）
      --dialect <D>              覆盖 [rollback].dialect：mysql / postgresql
      --lock-scope <S>           覆盖 [rollback].lock_scope：auto / global / table / snapshot / none
      --lock-timeout <SEC>       覆盖 [rollback].lock_timeout（秒）
      --accept-table-lock-risk   lock_scope=table 时必填（接受隐式提交释放风险）
      --fail-on-warning          warning 也按 error 处理（退出 2 而非 1）
      --allow-partial            允许 partial 回滚计划（safety.partial=true 不退出 2）
```

### `sqlguard replay-export`

把 SQL 脚本与 Mapper 语句导出为 `sql-manifest.json`，供分离的 Java 重放工程消费。**只导出，不重放。**

```
sqlguard replay-export [OPTIONS] [PATH]

参数：
  [PATH]                待扫描目录，默认当前目录

选项：
  -c, --config <FILE>   配置文件路径，默认 sqlguard.toml
  -o, --output-dir <D>  输出目录（生成 sql-manifest.json）
      --types <T>       仅导出指定类型，逗号分隔：select,insert,update,delete,merge,ddl,other。空 = 全部
```

重放由 `replay/` Java 工程完成，详见上方「导出动态重放清单」小节。

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

## 备份回滚生成

`rollback` 模块对每条 DDL/DML 语句自动生成 backup / rollback / manifest / cleanup 四件套，配合发布平台在变更失败时回滚。完整设计见 [docs/backup-rollback-design.md](docs/backup-rollback-design.md)。

### 工作流程

```
SQL 脚本 → rollback::RollbackGenerator::generate()
        ↓
   按 StmtInfo.kind 分发
   ├── DDL（CREATE/ALTER/DROP/RENAME）→ ddl.rs / ddl_like.rs
   └── DML（INSERT/UPDATE/DELETE/TRUNCATE/REPLACE）→ dml.rs
        ↓
   生成 BackupRollbackPair（backup + rollback + SafetyClass + ExpectedSchema）
        ↓
   render.rs 渲染为 backup.sql / rollback.sql / cleanup.sql
        ↓
   manifest.rs 序列化为 rollback-manifest.json
```

### 产物说明

| 文件 | 作用 | 执行时机 |
|------|------|----------|
| `backup.sql` | 备份变更前数据/结构（建备份表、`INSERT INTO bks_ SELECT * FROM t`） | 变更前 |
| `rollback.sql` | 失败时回滚（按 LIFO 序：DELETE → 还原 → RENAME 切换 → DROP 临时对象） | 变更失败 |
| `rollback-manifest.json` | 元数据清单（语句序号、安全分类、期望 schema、警告） | 发布平台读取 |
| `cleanup.sql` | 回滚成功后清理备份表（默认不自动执行，保留便于审计） | 回滚后（可选） |

### 关键特性

- **双方言支持**：`MySqlRenderer` / `PostgreSqlRenderer` 封装标识符引用、`CREATE TABLE LIKE`、`RENAME` 等差异
- **多模式锁策略**（D1/F14）：`auto` / `global` / `snapshot` / `table` / `none`，`auto` 根据脚本成分动态决策
- **CREATE TABLE LIKE 统一模式**：DROP TABLE / ALTER DROP COLUMN / ALTER MODIFY COLUMN 等"无法静态反向"的 DDL 通过备份表 + 原子 RENAME 切换回滚（F12）
- **DML 增量备份**：INSERT 直接生成 DELETE；UPDATE/DELETE 按 WHERE 增量备份；TRUNCATE 全表备份；REPLACE 部分支持（备份被覆盖旧行）
- **长事务预检查**（F18/N10）：备份前查询 `innodb_trx` + `information_schema.processlist`（MySQL）或 `pg_stat_activity`（PG），超阈值按 `abort` / `warn` / `ignore` 策略处理
- **binlog 控制**（F16/D2）：`auto` 模式下含 DDL 不设 `sql_log_bin=0`，纯 DML 设；避免 GTID 主从数据不一致
- **锁合并**（D6/N9）：合并连续同表段的锁区间，减少锁等待；`conservative`（默认）仅全表 LIKE 合并，`aggressive` 同表增量也合并
- **schema 漂移校验**（F13）：manifest 记录 `expected_schema`，发布平台执行期比对实际 schema，漂移按 `abort` / `warn` / `ignore` 处理
- **分区表检测**（F15）：`CREATE TABLE LIKE` 生成非分区表，按 `abort` / `warn` / `fallback` 处理
- **安全分类聚合**（C2）：`SafetyClass` 聚合 `reliable` / `partial` / `irreversible` / `counter_unrestored` / `requires_lock` 等标志，CI 按分类决策退出码
- **幂等执行**（F9）：`backup.sql` 多次执行结果一致，`CREATE TABLE IF NOT EXISTS bks_` + `INSERT IGNORE`（MySQL）/ `ON CONFLICT DO NOTHING`（PG）

### 已支持语句

| 类型 | 语句 | 回滚策略 |
|------|------|----------|
| DDL | `CREATE TABLE` | `DROP TABLE IF EXISTS` |
| DDL | `CREATE INDEX` | `DROP INDEX IF EXISTS` |
| DDL | `CREATE VIEW` | `DROP VIEW IF EXISTS` |
| DDL | `DROP TABLE` | CREATE TABLE LIKE + 原子 RENAME 切换 |
| DDL | `ALTER TABLE ADD COLUMN` | metadata-only 反向 `DROP COLUMN`（无约束时）/ 全表 LIKE 降级（有约束时） |
| DDL | `ALTER TABLE DROP COLUMN` | CREATE TABLE LIKE + 原子 RENAME 切换 |
| DDL | `ALTER TABLE MODIFY COLUMN` | CREATE TABLE LIKE + 原子 RENAME 切换 |
| DDL | `ALTER TABLE RENAME COLUMN` | 反向 RENAME COLUMN |
| DDL | `ALTER TABLE RENAME TO` | 反向 RENAME TO |
| DDL | `ALTER TABLE ADD CONSTRAINT` | 按约束类型分支反向 DROP（PK / UNIQUE / FK / CHECK / DEFAULT） |
| DML | `INSERT` | 反向 `DELETE`（按 WHERE / PK 定位） |
| DML | `UPDATE` | 备份旧值 + `UPDATE ... SET ... FROM bks_ JOIN` 还原 |
| DML | `DELETE` | 备份被删行 + `INSERT SELECT` 还原 |
| DML | `TRUNCATE` | 全表备份 + `INSERT SELECT` 还原 |
| DML | `REPLACE` | 部分支持：备份被覆盖旧行，新增行无法回滚（`partial = true`） |

> 注：`AUTO_INCREMENT` / `SEQUENCE` 当前值无法静态还原，会在 `SafetyClass.counter_unrestored` 标记，需 DBA 手工修复。

## 报告格式

### plain（默认）

终端彩色输出，`ERROR` 红 / `WARNING` 黄 / `INFO` 青，含规则编号、分组、文件路径与行列号。

### json

`sqlguard-report.json`：结构化数据，适合 CI 系统解析。每条违规包含 `rule_id`、`rule`、`group`、`severity`、`file`、`line`、`end_line`、`column`、`message`。

### html

`sqlguard-report.html`：深色主题网页报告，卡片式统计数据，可排序表格，适合归档。

### sarif

`sqlguard-report.sarif`：符合 [SARIF v2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html) 规范的 JSON，可直接上传到 GitHub Code Scanning / Azure DevOps / GitLab code scanning，在 PR 里以代码注释形式展示违规。包含去重的规则元数据（`runs[].tool.driver.rules[]`）和带行列号的 `results[].locations[]`。

### 多格式输出

```bash
sqlguard check ./sql -f all -o reports/
# 或显式指定 sarif：
sqlguard check ./sql -f json,sarif -o reports/
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

### 上传 SARIF 到 GitHub Code Scanning

`-f sarif` 生成符合规范的 `sqlguard-report.sarif`，通过 `github/codeql-action/upload-sarif` 上传后，违规会以代码注释形式显示在 PR 文件视图：

```yaml
name: sqlguard-sarif
on: [pull_request]

jobs:
  sarif:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write    # ★ 上传 SARIF 必需
    steps:
      - uses: actions/checkout@v4
      - name: SqlGuard check
        run: |
          sqlguard check ./sql -f sarif -o reports/
      - name: Upload SARIF
        uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: reports/sqlguard-report.sarif
```

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