# SqlGuard 用户手册

> 适用版本：**0.2.7**（以包内 `VERSION` 为准）
> 安装方法见 `docs/INSTALL.md`。本手册假设 `sqlguard` 已在 `PATH` 中。

---

## 目录

1. [产品定位](#1-产品定位)
2. [30 秒快速开始](#2-30-秒快速开始)
3. [核心概念](#3-核心概念)
4. [命令参考](#4-命令参考)
5. [配置文件](#5-配置文件)
6. [规则体系](#6-规则体系)
7. [MyBatis Mapper 模式](#7-mybatis-mapper-模式)
8. [报告格式](#8-报告格式)
9. [备份与回滚生成](#9-备份与回滚生成)
10. [重放清单导出](#10-重放清单导出)
11. [CI/CD 集成](#11-cicd-集成)
12. [维护：缓存、编码、升级、卸载](#12-维护缓存编码升级卸载)
13. [退出码与故障排查](#13-退出码与故障排查)
14. [附录](#14-附录)

---

## 1. 产品定位

SqlGuard 是一个**静态 SQL 质量门禁**工具，面向 DBA 与研发团队，在 SQL 脚本、数据库变更脚本、MyBatis Mapper
进入仓库或发布流水线之前完成检查。

它有两种输入模式：

| 模式 | 输入 | 典型场景 |
|------|------|---------|
| **SQL 脚本模式** | `.sql` / `.ddl` / `.dml` 文件 | 数据库变更脚本评审、DDL/DML 规范检查 |
| **MyBatis Mapper 模式** | `*Mapper.xml` | Java 业务仓库里散落的 SQL，无需抽取即可检查 |

核心能力矩阵：

| 能力 | 子命令 | 说明 |
|------|--------|------|
| 全量检查 | `check` | 扫描目录 → 解析 AST → 执行 Rhai 规则 → 输出报告 |
| 增量检查 | `check-diff` | 只检查相对 git 基线改动过的语句（PR 场景） |
| 项目初始化 | `init` | 生成配置、内置规则、示例目录结构（幂等） |
| 备份回滚生成 | `gen-rollback` | 为每条 DDL/DML 生成 backup/rollback/manifest/cleanup 四件套 |
| 重放清单导出 | `replay-export` | 导出 `sql-manifest.json`，供 Java 重放工程做执行计划采集 |
| AST 解释 | `explain` | 打印某文件的 AST，写规则时用来确认可用字段 |
| 逻辑外键挖掘 | `sqlguard-mine` | 从 Mapper JOIN 条件反推表间关系，产出 `relations.json` |

**设计边界（重要）**：SqlGuard 只做**静态分析**，不连接数据库、不执行 SQL。`replay-export` 只导出清单，
真正的重放由独立的 Java 工程 `sqlguard-replay`（仓库 `replay/` 目录）完成。

---

## 2. 30 秒快速开始

```bash
# ① 初始化（生成 sqlguard.toml + sqlguard.rules.toml + 内置规则 + 示例目录）
mkdir my-sql-project && cd my-sql-project
sqlguard init .

# ② 写一条 SQL
mkdir -p sql/ddl
cat > sql/ddl/create_users.sql <<'SQL'
CREATE TABLE users (
    id   INT PRIMARY KEY,
    name VARCHAR(100) NOT NULL
);
SQL

# ③ 检查
sqlguard check ./sql
```

无违规时的输出：

```
══════════════════════════════════════════════════════════════
 SqlGuard Report
══════════════════════════════════════════════════════════════

Files checked: 1

✓ No violations found

────────────────────────────────────────
Summary: All checks passed
```

想立刻看到"有违规"长什么样，直接用发布包自带的示例：

```bash
sqlguard check examples/dml/violations      # 包内附带的违规样例
sqlguard check examples/dml/clean           # 包内附带的合规样例
```

---

## 3. 核心概念

| 概念 | 说明 |
|------|------|
| **script_type** | 文件分类结果：`ddl` / `dml` / `sql` / `other`，由 `[[classification.rules]]` 的 glob 规则决定；决定哪些规则对该文件生效（`applies_to`） |
| **规则（rule）** | 一个 `.rhai` 脚本 + 一条 `[[rules]]` 注册项，含 `id` / `group` / `severity` / `applies_to` |
| **group** | 规则分组（如 `ddl-safety`、`dml-safety`、`file-format`），用于 CLI 批量筛选 |
| **severity** | `error` 会让 `check` 返回退出码 1（阻断 CI）；`warning` 只提示 |
| **dialect** | SQL 方言，决定用什么语法解析；`gaussdb` 会先做 Oracle/MySQL → PG 词法重写 |
| **dialect_fallback** | 主方言解析失败时的第二选择（逐语句回退），链尾恒为 `generic` |
| **target_dir** | 命令最后的 `[PATH]` 参数，扫描根与配置文件查找起点 |

### 3.1 目录结构约束

`[structure]` 段可要求项目必须存在某些目录（如 `sql/ddl`、`sql/dml`），缺失即报错。
`strict = true` 时多余目录也报错，`allow_extra` 列出豁免项。

---

## 4. 命令参考

### 4.1 `sqlguard check` —— 全量检查

```
sqlguard check [OPTIONS] [PATH]
```

| 选项 | 说明 |
|------|------|
| `[PATH]` | 待检查目录，默认 `.` |
| `-c, --config <FILE>` | 配置文件路径，默认 `sqlguard.toml` |
| `-f, --format <FMT>` | 报告格式：`plain` / `json` / `html` / `sarif` / `all`，也可逗号组合，默认 `plain` |
| `-o, --output-dir <DIR>` | 输出目录（`json`/`html`/`sarif` 必需） |
| `--rules <IDS>` | 只跑指定 id，逗号分隔，支持前缀通配（`DDL*`） |
| `--groups <G>` | 只跑指定分组 |
| `--exclude-rules <IDS>` | 排除指定 id |
| `--exclude-groups <G>` | 排除指定分组 |
| `--dialect <D>` | 覆盖配置的方言 |
| `--dialect-fallback <D>` | 覆盖回退方言；传 `generic` 等于关闭回退 |
| `--cache` / `--no-cache` | 强制开启 / 关闭文件缓存（二者互斥） |
| `--encoding <E>` | 覆盖扫描文件编码，如 `gbk` |

```bash
sqlguard check ./sql
sqlguard check ./sql -f json,html,sarif -o reports/
sqlguard check ./sql --groups ddl-safety --exclude-rules DDL003
sqlguard check ./src --dialect gaussdb --dialect-fallback oracle
```

### 4.2 `sqlguard check-diff` —— 增量检查

只检查相对 git 基线改动过的 SQL 语句，适合 PR 流水线。

```
sqlguard check-diff --base <BASE> [OPTIONS] [PATH]
```

`--base` **必填**（commit / branch / tag，如 `origin/main`、`HEAD~1`）。其余选项与 `check` 相同。

```bash
sqlguard check-diff --base origin/main -f json -o reports/
sqlguard check-diff --base HEAD~1
```

工作机理：

1. 取 `<base>` 与 `HEAD` 的 merge-base，diff 到**工作区**（暂存 / 未暂存 / 未跟踪的新文件都算改动）；
2. 对改动文件跑全部规则（跨语句规则不被破坏）；
3. 用语句级 `[line, end_line] ∩ hunk` 交集过滤违规；新增文件整体算改动，不做 hunk 过滤；
4. 输出报告，`error` 级违规返回退出码 1。

> CI 里必须 `git fetch` 到基线分支且 `fetch-depth: 0`，否则 diff 取不到。

### 4.3 `sqlguard init` —— 初始化项目

```bash
sqlguard init [PATH]           # 幂等：只补缺失文件，已存在的原样保留并提示 skipped
sqlguard init [PATH] --force   # 覆盖已存在的配置与规则文件
```

产物：

| 文件 / 目录 | 说明 |
|------------|------|
| `sqlguard.toml` | 主配置（结构 / 分类 / 输出 / Mapper / 扫描 / 文件检查 / 回滚） |
| `sqlguard.rules.toml` | 规则注册表（`[[rules]]`） |
| `config/rules/ddl/*.rhai`、`config/rules/dml/*.rhai` | 内置规则脚本（7 DDL + 18 DML） |
| `config/rules/lib/helpers.rhai` | 规则公共辅助函数（引擎自动 prepend，规则脚本无需 import） |
| `sql/ddl`、`sql/dml`、`sql/others` | 示例目录结构 |

> **为什么默认幂等**：避免覆盖外部工具链（如 gates-toolkit）按模板渲染过的定制配置。
> 确需整体重写才加 `--force`。

### 4.4 `sqlguard gen-rollback` —— 备份回滚脚本生成

```
sqlguard gen-rollback [OPTIONS] [PATH]
```

| 选项 | 说明 |
|------|------|
| `[PATH]` | 待扫描目录，默认 `.` |
| `-o, --output-dir <DIR>` | 输出目录 |
| `--dialect <D>` | `mysql` / `mariadb` / `postgresql` / `gaussdb`（`postgres`、`gauss` 为别名） |
| `--lock-scope <S>` | `auto` / `global` / `table` / `snapshot` / `none` |
| `--lock-timeout <SEC>` | 锁等待超时（秒） |
| `--accept-table-lock-risk` | `lock_scope=table` 时必填（接受隐式提交释放锁的风险） |
| `--fail-on-warning` | warning 也按 error 处理（退出 2） |
| `--allow-partial` | 允许 partial 回滚计划（不因 `safety.partial` 退出 2） |
| `--review-report` | 额外生成 `rollback-review-report.html`（DBA 复核报告） |
| `--encoding <E>` | 覆盖扫描编码 |

产物：`backup.sql`、`rollback.sql`、`rollback-manifest.json`、`cleanup.sql`（+ 可选的复核报告）。
详细语义见 [§9](#9-备份与回滚生成)。

### 4.5 `sqlguard replay-export` —— 重放清单导出

```
sqlguard replay-export [OPTIONS] [PATH]
```

| 选项 | 说明 |
|------|------|
| `-o, --output-dir <DIR>` | 输出目录，生成 `sql-manifest.json`，默认 `.` |
| `--types <T>` | 只导出指定类型：`select,insert,update,delete,merge,ddl,other`，空 = 全部 |
| `--base <GIT-REF>` | 增量导出：只导出基线以来新增/修改的语句；被删语句写入 `sql-manifest-removed.json` |
| `--encoding <E>` | 覆盖扫描编码 |

> 本子命令**只导出清单，不连数据库、不执行重放**。重放由 `replay/` 下的 Java 工程完成
> （仅支持 GaussDB/openGauss 镜像库，JDBC 驱动通过 `--driver-jar` 运行期加载）。

### 4.6 `sqlguard explain` —— AST 解释

写规则时用来确认"某个字段在 AST 里叫什么、值是什么"。

```bash
sqlguard explain examples/ddl/clean/create_users.sql
sqlguard explain examples/ddl/clean/create_users.sql --json
sqlguard explain UserMapper.xml --mapper
sqlguard explain query.sql --dialect mysql
```

### 4.7 `sqlguard-mine` —— 逻辑外键挖掘（独立二进制）

```bash
sqlguard-mine -p src/main/resources/mapper -o relations.json --dialect gaussdb
```

| 选项 | 说明 |
|------|------|
| `-p, --path <DIR>` | Mapper XML / SQL 目录，默认 `.`（**递归**扫描子目录；不接受单文件） |
| `--dialect <D>` | 方言，默认 `generic` |
| `--dialect-fallback <D>` | 回退方言 |
| `--base <GIT-REF>` | 增量：只挖改动过的 Mapper 中受影响的语句 |
| `--include-plain-xml` | 连不带 `Mapper.xml` 后缀的 XML 也一起扫（默认只扫 `*Mapper.xml`） |
| `--encoding <E>` | XML 编码，默认 `utf-8` |
| `-o, --output <FILE>` | 输出文件，默认 `relations.json` |

用途：团队禁用物理外键时，用 JOIN 的等值条件反推表间关系，补全数据字典 / ER 图。

### 4.8 `mapdiag` —— Mapper 解析诊断（可选）

```bash
mapdiag <mapper-dir> [show] [sample-cap]
```

批量 dump Mapper 提取出的 SQL 与解析结果，按失败原因归类（PARSE / DYN 等）。
仅在排查"大量 Mapper 解析失败"时需要，**日常检查不用它**。

---

## 5. 配置文件

### 5.1 两个配置文件

| 文件 | 内容 |
|------|------|
| `sqlguard.toml` | 主配置：方言、目录结构、文件分类、扫描行为、输出、Mapper、文件检查、缓存、回滚 |
| `sqlguard.rules.toml` | 规则注册表：`[[rules]]` 数组 |

主配置里用 `rules_file = "sqlguard.rules.toml"` 引用规则文件；不写时工具会在同目录自动查找
`sqlguard.rules.toml`。**建议保持两个文件分离**，避免主配置随规则增多而膨胀。

查找顺序：CLI `-c` 指定的路径 → 当前目录 → `[PATH]` 目录。

### 5.2 方言

```toml
dialect = "mysql"            # generic / mysql / postgresql / gaussdb / oracle / ansi
dialect_fallback = "oracle"  # 可选；generic 表示关闭回退
```

| 方言 | 说明 |
|------|------|
| `generic` | 通用方言（默认） |
| `mysql` | MySQL |
| `postgresql` | PostgreSQL |
| `gaussdb` | GaussDB/openGauss：先做 Oracle/MySQL → PG 词法重写（`MINUS`→`EXCEPT`、`SYSDATE`→`CURRENT_TIMESTAMP`、`NVL`→`COALESCE`、`DUAL`→子查询、反引号→双引号），再用 PG 解析；默认回退 `oracle` |
| `oracle` | Oracle |
| `ansi` | ANSI SQL |

混合方言（如 GaussDB 的"PG 内核 + Oracle 外壳"）依赖**逐语句回退链**：主方言解析失败 → 回退方言 → `generic`。
详见 `docs/dialect-fallback.md`。

### 5.3 各配置段速查

| 段 | 关键字段 | 作用 |
|----|---------|------|
| 顶层 | `dialect`、`dialect_fallback`、`rules_file` | 方言与规则文件引用 |
| `[structure]` | `paths`、`strict`、`allow_extra` | 目录结构约束 |
| `[scan]` | `paths`、`exclude_dirs`、`encoding` | 扫描白名单 / 跳过目录 / 文件编码 |
| `[[classification.rules]]` | `name`、`pattern`、`type`、`priority` | 文件分类（glob，priority 降序首个命中） |
| `[mapper]` | `enabled`、`paths`、`patterns` | MyBatis Mapper 模式 |
| `[output]` | `formats` | 默认报告格式 |
| `[file_check]` | `enabled`、`check_encoding`、`check_line_ending`、`*_severity` | FILE001 / FILE002 文件格式检查 |
| `[cache]` | `enabled`、`cache_file` | 文件级缓存 |
| `[rollback]` | `enabled`、`dialect`、`lock_scope`、… | 备份回滚生成 |

**完整字段与注释请直接看 `config/sqlguard.toml.example`**（发布包内已附带，逐字段有中文注释）。

### 5.4 内置规则库的位置

`sqlguard init` 会把内置规则脚本写到项目的 `config/rules/` 下。若你更希望"所有项目共用一份"，
安装脚本已把它们放到：

| 系统 | 路径 |
|------|------|
| Linux / macOS | `/usr/local/share/sqlguard/rules`（`--user` 安装时为 `~/.local/share/sqlguard/rules`） |
| Windows | `%LOCALAPPDATA%\SqlGuard\share\rules` |

把它们复制进项目即可；规则脚本是纯文本 `.rhai`，可直接按团队规范修改。

---

## 6. 规则体系

### 6.1 内置规则

7 条 DDL + 18 条 DML + 2 条文件格式检查，完整清单（编号、触发条件、正反例）见 **`docs/default-rules.md`**。

快速索引（节选）：

| 编号 | 规则 | 说明 |
|------|------|------|
| DDL001 | `no_drop_table` | 禁止 `DROP TABLE` |
| DDL002 | `primary_key_required` | 建表必须有主键 |
| DDL003 | `table_name_naming` | 表名命名规范 |
| DDL004 | `backup_table_naming` | 备份表命名（与回滚模块 `bks_` 前缀对齐） |
| DDL005 | `index_naming_convention` | 索引命名规范 |
| DDL006 | `no_redundant_index` | 禁止冗余索引 |
| DML001 | `no_select_all` | 禁止 `SELECT *` |
| DML002 | `no_delete_update_without_where` | DELETE/UPDATE 必须有 WHERE |
| DML003 | `insert_columns_required` | INSERT 必须显式列名 |
| DML004 | `no_join_without_condition` | JOIN 必须带 ON 条件 |
| DML005 | `subquery_alias_required` | 子查询必须有别名 |
| FILE001 | 编码检查 | 必须 UTF-8 无 BOM |
| FILE002 | 换行符检查 | 必须为 LF |

### 6.2 规则筛选（CLI）

```bash
sqlguard check ./sql --rules DDL001,DDL002   # 只跑指定编号
sqlguard check ./sql --rules DDL*            # 前缀通配
sqlguard check ./sql --groups ddl-safety     # 按分组
sqlguard check ./sql --exclude-groups experimental
sqlguard check ./sql --groups ddl-safety --exclude-rules DDL003
```

语义：

- 白名单（`--rules` / `--groups`）为空 = 不限制；
- 黑名单（`--exclude-*`）**优先级高于白名单**；
- `enabled = false` 的规则永不执行；
- 筛选只影响规则执行，不影响目录结构检查与文件分类。

### 6.3 自定义规则

三步：

1. 在 `config/rules/dml/`（或 `ddl/`）下写 `.rhai` 脚本：

```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "DROP_TABLE" {
        violations.push(violation("DROP TABLE not allowed", s.line(), s.column()));
    }
}
```

2. 在 `sqlguard.rules.toml` 注册：

```toml
[[rules]]
id = "DML999"
name = "my_custom_rule"
group = "custom"
description = "Demo rule"
enabled = true
script_path = "config/rules/dml/my_custom_rule.rhai"
applies_to = ["dml"]
severity = "warning"
```

3. 单规则验证：`sqlguard check ./sql --rules DML999`

引擎注入的全局变量：

- `context`：`sql_content`、`file_path`、`script_type`、`ast` …
- `violations`：空数组，脚本 `push` 上报

内置辅助函数（`helpers.rhai`，引擎自动 prepend，无需 import）：

| 函数 | 说明 |
|------|------|
| `guard_parse_error(context)` | AST 是否有解析错误 |
| `parse_error_violation(context)` | 构造解析错误违规 |
| `violation(msg, line, col)` | 构造带行列号的违规 |
| `violation_line(msg, line)` | 构造仅带行号的违规 |
| `violation_msg(msg)` | 构造纯消息违规 |

**完整 AST API 与编写规范见 `docs/rule-scripting.md`。**

---

## 7. MyBatis Mapper 模式

### 7.1 启用

```toml
[mapper]
enabled = true
paths = ["src/main/resources/mapper"]
patterns = ["**/*Mapper.xml"]
```

`paths` 支持 glob（含 `* ? [ {` 时按 glob 处理）：

```toml
paths = ["src/**/mapper", "modules/**/resources/mapper"]   # 命中任意层级的 mapper 目录
paths = ["**/legacy/legacy_report.xml"]                    # 直接指定文件
```

语义：glob 命中**目录** → 按 `patterns` 递归收集其下 XML；glob 命中**文件** → 直接收录，不再受 `patterns` 过滤；
同一文件多条目命中自动去重；递归同样跳过 `[scan] exclude_dirs` 黑名单。

### 7.2 处理流程

1. 收集 `paths` 下匹配 `patterns` 的 `.xml`；
2. 第一遍扫描收集所有 `<sql id="...">` 片段；
3. 第二遍扫描提取 `<select>` / `<insert>` / `<update>` / `<delete>` 内的 SQL：
   - 剥离动态标签（`<if>` / `<where>` / `<foreach>` 等）保留内部文本；
   - 解析 `<include refid="..."/>` 并递归替换（最大深度 10）；
   - `#{name, jdbcType=VARCHAR}` → `?`；
   - `${tableName}` → `_var_tableName`（合成合法标识符）；
4. 每条 SQL 独立交给规则引擎，行号映射回 XML 原文件。

### 7.3 已知限制

- `<include>` 仅支持**同文件内**引用（不跨 namespace）；
- 动态标签剥离后，sqlparser 可能因分支导致语法不完整而解析失败（可用 `mapdiag` 归类排查）；
- 当前版本 statement 类型统一映射为 `dml`。

---

## 8. 报告格式

| 格式 | 文件 | 用途 |
|------|------|------|
| `plain` | （终端输出） | 彩色终端输出，ERROR 红 / WARNING 黄 / INFO 青 |
| `json` | `sqlguard-report.json` | 结构化数据，CI 解析 |
| `html` | `sqlguard-report.html` | 卡片式网页报告，可排序表格，适合归档 |
| `sarif` | `sqlguard-report.sarif` | SARIF v2.1.0，可上传 GitHub Code Scanning / Azure DevOps / GitLab，违规以 PR 注释呈现 |

```bash
sqlguard check ./sql -f all -o reports/          # 一次生成全部格式
sqlguard check ./sql -f json,sarif -o reports/
```

JSON 每条违规字段：`rule_id`、`rule`、`group`、`severity`、`file`、`line`、`end_line`、`column`、`message`。

---

## 9. 备份与回滚生成

`gen-rollback` 为每条 DDL/DML 生成四件套，供发布平台在变更失败时回滚。

| 文件 | 作用 | 执行时机 |
|------|------|---------|
| `backup.sql` | 备份变更前数据/结构 | 变更前 |
| `rollback.sql` | LIFO 序回滚（DELETE → 还原 → RENAME 切换 → DROP 临时对象） | 变更失败 |
| `rollback-manifest.json` | 元数据（语句序号、安全分类、`review_level`、期望 schema、警告） | 发布平台读取 |
| `rollback-review-report.html` | DBA 复核报告（`--review-report`） | 人工复核 |
| `cleanup.sql` | 清理备份表（默认不自动执行） | 回滚后（可选） |

### 9.1 Review 契约

每条语句按 `SafetyClass` 自动标注 `review_level`，无需配置：

| review_level | 触发条件 | 发布平台行为 |
|---|---|---|
| `none` | reliable 且无风险 flag | 自动放行 |
| `optional` | 仅 `requires_lock` / `counter_unrestored` | 提示，不阻断 |
| `required` | `irreversible` / `irreversible_if_backup_missing` / `!reliable` / `partial` | **强制阻断**，等待人工复核 |

manifest 顶层汇总：`review_required`、`required_review_count`、`optional_review_count`、`auto_approved_count`。
不变式：**退出码 2 ⇒ `review_required = true`**。

### 9.2 关键配置项

| 字段 | 说明 |
|------|------|
| `dialect` | 影响标识符引用、`CREATE TABLE LIKE`、`RENAME` 差异 |
| `lock_scope` | `auto`：含 DDL → `global`，纯 DML → `snapshot`；`table` 需 `accept_table_lock_risk = true` |
| `binlog_strategy` | `auto`：含 DDL 不设 `sql_log_bin=0`，纯 DML 设（避免 GTID 主从不一致） |
| `on_long_transaction` | 备份前查 `innodb_trx` / `pg_stat_activity`，超阈值 `abort` / `warn` / `ignore` |
| `coalesce_locks` | 合并连续同表锁区间；`conservative`（仅全表 LIKE）/ `aggressive` |
| `assert_on_schema_mismatch` | 执行期比对 `expected_schema`，漂移按 `abort` / `warn` / `ignore` |
| `on_partitioned_table` | 分区表策略：`abort` / `warn` / `fallback` |
| `backup_table_retention_days` | 备份表保留天数 |

已支持语句（回滚策略）见仓库文档 `docs/backup-rollback-design.md`（发布包未随附，可从仓库获取）。

> `AUTO_INCREMENT` / `SEQUENCE` 当前值无法静态还原，会标记 `counter_unrestored`，需 DBA 手工修复。

---

## 10. 重放清单导出

```bash
sqlguard replay-export ./sql -o manifest_out/
sqlguard replay-export ./sql -o manifest_out/ --types select,insert
sqlguard replay-export ./sql -o manifest_out/ --base origin/main     # 增量（CI 推荐）
```

每条语句记录：类型（select/insert/update/delete/merge/ddl/other）、来源文件、行号范围；
Mapper 动态分支会按 `<if>` / `<foreach>` 组合展开为多个变体。

增量模式：新增文件整文件导出（`change: "added"`）；改动文件内语句与 hunk 有交集才导出（`change: "modified"`）；
被删语句写入 `sql-manifest-removed.json`。清单 `version` 保持 1，旧版重放工程可直接消费。

| 重放工程（Java）约束 | 说明 |
|---|---|
| 工程位置 | 仓库 `replay/` 目录，Maven 项目（`mvn package`） |
| 目标库 | 仅 GaussDB / openGauss 镜像库 |
| JDBC 驱动 | 不在 `pom.xml` 声明，运行期 `--driver-jar` 动态加载 |
| 是否随本包分发 | **否**，需单独构建 |

---

## 11. CI/CD 集成

### 11.1 GitHub Actions（全量）

```yaml
- name: SqlGuard Check
  run: sqlguard check ./sql
```

### 11.2 GitHub Actions（PR 增量，推荐）

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
          fetch-depth: 0                 # ★ 必须有完整 git 历史
      - name: Fetch base branch
        run: git fetch origin ${{ github.base_ref }}
      - name: SqlGuard incremental check
        run: sqlguard check-diff --base origin/${{ github.base_ref }} -f json -o reports/
```

### 11.3 上传 SARIF 到 GitHub Code Scanning

```yaml
      - name: SqlGuard check
        run: sqlguard check ./sql -f sarif -o reports/
      - name: Upload SARIF
        uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: reports/sqlguard-report.sarif
```

需要 `permissions: security-events: write`。

### 11.4 分阶段检查

```bash
sqlguard check ./sql --groups ddl-safety           # PR 只查 DDL 安全
sqlguard check ./sql --exclude-groups experimental # 完整检查但排除实验规则
```

发布包 `ci/` 目录另附可直接复制的流水线片段（GitHub Actions / GitLab CI / CNB）。

---

## 12. 维护：缓存、编码、升级、卸载

### 12.1 文件级缓存

```toml
[cache]
enabled = false                        # 默认关闭；大仓库可设 true
cache_file = ".sqlguard-cache.json"
```

按 `mtime + size` 判断文件是否变化，未变则复用上次结果。整体失效触发项：
SqlGuard 版本、方言、编码、CLI 筛选参数、主配置 / 规则配置 / 任一 `.rhai` 脚本变化。

CLI 覆盖：`--cache` 强制开、`--no-cache` 强制关（互斥，优先级高于配置）。
FILE001/FILE002 属轻量检查，不进缓存。

### 12.2 编码

```toml
[scan]
encoding = "utf-8"      # gbk / gb2312 / gb18030 / big5 / shift_jis / utf-16le / ...
```

支持 WHATWG 编码标签。文件带 BOM 时按 BOM 判定并剥离（BOM 优先于配置）。
配置为非 UTF-8 时，FILE001「必须 UTF-8 无 BOM」自动跳过。

### 12.3 升级

覆盖安装即可（见 `docs/INSTALL.md` §9）。升级后先跑一次 `sqlguard check` 确认配置未被拒绝。

### 12.4 卸载

```bash
sudo ./scripts/uninstall.sh            # Linux/macOS
.\scripts\uninstall.ps1                # Windows
```

手动安装的直接删除二进制文件与安装目录即可。

---

## 13. 退出码与故障排查

| 退出码 | 含义 | 处理 |
|--------|------|------|
| 0 | 无 `error` 级违规，目录结构无误 | 通过 |
| 1 | 存在 `error` 级违规，或目录结构问题 | 修复 SQL 或调整配置 |
| 2 | 配置错误 / 文件读取失败等系统错误；`gen-rollback` 中代表不可逆 / 不可靠 / partial | 检查配置字段、路径权限 |

`gen-rollback` 的退出码遵循 manifest：`0`=全部可靠，`1`=仅 warning，`2`=error。

### 13.1 排查清单

| 现象 | 排查方向 |
|------|---------|
| 大量 `PARSE_ERROR` | 方言选错 → 试 `--dialect` / `--dialect-fallback`；Mapper 动态标签剥离导致语法不完整属已知限制 |
| 规则没生效 | 检查 `enabled`、`applies_to` 与文件 `script_type`、是否被 `--exclude-*` 过滤 |
| 扫描很慢 / 扫到不该扫的目录 | 配 `[scan] paths` 白名单 + `exclude_dirs`；确认没把 `.git`、构建产物目录纳入 |
| 中文乱码 | 配 `[scan] encoding` 或 `--encoding`（常见 `gbk`）；确认文件是否带 BOM |
| `check-diff` 报 git 错 | 需要 `git`，且 CI 要 `fetch-depth: 0` 并 fetch 基线分支 |
| 缓存结果"看起来不对" | 用 `--no-cache` 强制全量跑一次 |
| 不确定 AST 里有什么字段 | `sqlguard explain <file> [--json]` |

更多问答见 `docs/FAQ.md`。

---

## 14. 附录

### 14.1 发布包目录地图

```
sql-guard-0.2.7/
├── README.md              包总览与快速开始
├── VERSION                版本与构建元数据
├── CHANGELOG.md           版本变更记录
├── LICENSE                MIT
├── SHA256SUMS             全部文件校验和
├── VERIFY.md              校验说明
├── bin/                   各平台二进制
│   ├── linux-x86_64-musl/
│   ├── linux-aarch64-musl/
│   └── windows-x86_64/
├── docs/                  本手册 / 安装指南 / FAQ / 规则文档
├── scripts/               安装 / 卸载 / 校验 / 构建脚本
├── config/                内置规则库 + 配置模板
├── examples/              SQL 样例（clean / violations）
└── ci/                    CI 流水线片段
```

### 14.2 环境变量

| 变量 | 用途 |
|------|------|
| `SQLGUARD_PLATFORM_DIR` | 安装脚本无法识别平台时，强制指定 `bin/` 下的子目录名 |
| `NO_COLOR` | 部分终端可据此关闭彩色输出 |

### 14.3 相关文档

| 文档 | 内容 |
|------|------|
| `docs/default-rules.md` | 内置规则完整清单（编号、触发条件、正反例） |
| `docs/rule-scripting.md` | Rhai 规则编写指南与 AST API 参考 |
| `docs/dialect-fallback.md` | 方言与逐语句回退链设计 |
| `docs/FAQ.md` | 常见问题 |
| `config/sqlguard.toml.example` | 主配置全字段注释 |
| `config/sqlguard.rules.toml.example` | 规则注册表示例 |

### 14.4 许可证

MIT，见 `LICENSE`。
