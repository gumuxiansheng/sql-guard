# sqlguard-replay

在镜像库上重放 SQL 清单、采集执行计划与耗时、识别慢 SQL 与次优计划的 Java 工具。

SqlGuard Rust 二进制只负责导出 `sql-manifest.json`（`sqlguard replay-export` 子命令），**不执行重放**。本工程读取清单后连接镜像库，对每条 SQL 跑 `EXPLAIN`、计时、分析计划，产出 JSON + HTML 报告。

## 与 SqlGuard 的关系

```
┌──────────────────────────┐        ┌──────────────────────────┐
│   SqlGuard (Rust)        │        │  sqlguard-replay (Java)  │
│                          │        │                          │
│  sqlguard replay-export  │ manifest│   Main.java              │
│  ./sql -o manifest_out/  ├───────►│   --manifest <path>      │
│                          │  json  │        │                  │
│  - 解析 SQL 脚本/Mapper  │        │        ▼                  │
│  - 展开动态分支变体       │        │  Replayer                │
│  - 导出 sql-manifest.json│        │   ├─ 计时 (warmup+iter)   │
│                          │        │   ├─ EXPLAIN 采集计划     │
└──────────────────────────┘        │   ├─ PlanAnalyzer 启发式  │
                                    │   └─ SlowDetector 分级    │
                                    │        │                  │
                                    │        ▼                  │
                                    │  ReportWriter             │
                                    │   ├─ replay-report.json   │
                                    │   └─ replay-report.html   │
                                    └──────────────────────────┘
```

## 前置要求

- **JDK 8+**（`pom.xml` 中 `maven.compiler.release=8`）
- **Maven 3.6+**
- **JDBC 驱动**：GaussDB / openGauss / PostgreSQL / MySQL 任一，需自行获取 jar（不在 pom 声明，运行期动态加载）
- **镜像库**：与生产库数据量/统计信息相近的只读或可写镜像库；DML 重放会事务回滚，但建议用可回滚的隔离环境

## 构建

```bash
cd replay
mvn package
# 产物：target/sqlguard-replay-0.1.0.jar
```

JDBC 驱动不进构建依赖，运行期通过 `--driver-jar` 参数或自动发现 `replay/lib/*.jar` 加载。

## 快速开始

### 1. 导出清单（SqlGuard 侧）

```bash
sqlguard replay-export ./sql -o manifest_out/
# 生成 manifest_out/sql-manifest.json
```

### 2. 放置 JDBC 驱动

```bash
mkdir -p replay/lib
cp /path/to/opengauss-jdbc-*.jar replay/lib/
# 或任意位置，用 --driver-jar 指定
```

### 3. 执行重放

```bash
java -jar target/sqlguard-replay-0.1.0.jar \
    --manifest manifest_out/sql-manifest.json \
    --jdbc-url "jdbc:opengauss://mirror-db:5432/appdb" \
    --jdbc-user appuser \
    --jdbc-password secret \
    --output-dir replay-report/
```

未指定 `--driver-jar` 时，按以下顺序自动发现：
1. 当前工作目录下的 `replay/lib/*.jar`（按文件名排序取首个）
2. sqlguard-replay jar 所在目录的 `../lib/*.jar`（打包后从任意位置运行）

### 4. 查看报告

```
replay-report/
├── replay-report.json   # 结构化数据，CI 解析
└── replay-report.html   # 深色主题表格，人工审阅
```

退出码：`0` = 全部通过；`1` = 存在 slowError 或执行错误；`2` = 配置/系统错误。

## CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--manifest <path>` | `sql-manifest.json` | **必填**，SqlGuard 导出的清单 |
| `--jdbc-url <url>` | env `REPLAY_DB_URL` | **必填**，镜像库 JDBC URL |
| `--jdbc-user <u>` | env `REPLAY_DB_USER` | 数据库用户 |
| `--jdbc-password <p>` | env `REPLAY_DB_PASSWORD` | 数据库密码 |
| `--driver-jar <path>` | 自动发现 | JDBC 驱动 jar 路径 |
| `--driver-class <c>` | 按 URL 推断 | 驱动类全限定名（PG 默认 `org.postgresql.Driver`，MySQL 默认 `com.mysql.cj.jdbc.Driver`） |
| `--dialect <d>` | 按 URL 推断 | `pg` / `mysql`，显式覆盖自动推断 |
| `--output-dir <d>` | `.` | 报告输出目录 |
| `--explain-mode <m>` | `auto` | `explain`（纯计划）/ `analyze`（执行+计划）/ `auto`（select 走 ANALYZE，DML 走 EXPLAIN） |
| `--iterations <n>` | `3` | 计时迭代次数 |
| `--warmup <n>` | `1` | 预热次数（不计入样本） |
| `--pool-size <n>` | `4` | 连接池大小，`>1` 时并发重放 |
| `--statement-timeout-ms <ms>` | `30000` | 单条语句超时（PG: `SET statement_timeout`；MySQL: `SET MAX_EXECUTION_TIME`） |
| `--max-rows <n>` | `0` | SELECT 计时单次最大行数，`0` = 不限制（全量物化）。大结果集场景建议设置以避免计时被 IO 拖慢 |
| `--slow-warn-ms <ms>` | `100` | 慢 SQL 警告阈值（基于 p50 中位数） |
| `--slow-error-ms <ms>` | `1000` | 慢 SQL 错误阈值 |
| `--seq-scan-rows <n>` | `10000` | DP001/DP003/DP007 全表扫描行数阈值 |
| `--max-in-params <n>` | `1000` | DYN003 IN 子句参数数阈值 |
| `--types <csv>` | 空=全部 | 按类型过滤：`select,insert,update,delete,merge,ddl,other` |
| `--allow-ddl` | false | 允许重放 DDL（仅 CTAS 走 EXPLAIN，其他 DDL 仍跳过） |
| `--auto-param` | false | 无 fixture 时用默认值绑定占位符（避免全绑 NULL） |
| `--auto-param-value <v>` | `1` | `--auto-param` 模式下的默认值 |
| `--param-fixture <path>` | 无 | 参数 fixture JSON 文件 |
| `-h, --help` | — | 显示帮助并退出 |
| `-V, --version` | — | 显示版本号并退出 |

支持 `--key=value` 与 `--key value` 两种格式。

## 清单格式（sql-manifest.json）

由 SqlGuard `replay-export` 生成，本工程只消费不生产。

> **版本校验**：加载时校验 `version` 字段，当前支持版本 `1`。加载到不支持的版本会抛
> `UnsupportedManifestVersionException`（继承 `IOException`）并以退出码 2 终止，避免
> 未来清单格式变化时静默兼容失败导致字段缺失或解析错位。
>
> **增量清单（`replay-export --base <ref>`）**：清单新增可选字段 `base` / `incremental`、
> 语句级可选字段 `change`（`added` / `modified`），`version` 仍为 1，本工程直接兼容
> （未知字段被 `@JsonIgnoreProperties` 忽略）。增量清单只包含本次改动语句，可当作
> 全量子集直接重放；`sql-manifest-removed.json`（被删语句元信息）由 CI 侧从历史
> 报告归档中剔除对应 id，本工程不消费该文件。

```json
{
  "version": 1,
  "generator": "sqlguard replay-export",
  "generated_at": "1784909525",
  "statement_count": 10,
  "statements": [
    {
      "id": "sql/dml/query.sql#1",
      "sql": "SELECT id, name FROM users WHERE id = ?",
      "type": "select",
      "source": "sql/dml/query.sql",
      "source_type": "sql",
      "line": 1,
      "end_line": 1
    },
    {
      "id": "mapper/UserMapper.xml#findByCond#v1",
      "sql": "SELECT id, name FROM users WHERE name LIKE ? AND age > ?",
      "type": "select",
      "source": "mapper/UserMapper.xml",
      "source_type": "mapper",
      "line": 15,
      "end_line": 15,
      "statement_id": "findByCond",
      "variant_of": "mapper/UserMapper.xml#findByCond",
      "variant_label": "if:name!=null=true,if:age!=null=true"
    }
  ]
}
```

字段说明：

| 字段 | 说明 |
|------|------|
| `id` | 全局唯一，`<source>#<序号>`（脚本）或 `<source>#<statement_id>[#v<序号>]`（mapper 变体） |
| `sql` | 可直接交给 JDBC 的文本（mapper 已把 `#{}` 标准化为 `?`） |
| `type` | `select` / `insert` / `update` / `delete` / `merge` / `ddl` / `other` |
| `source_type` | `sql`（脚本文件）或 `mapper`（MyBatis XML） |
| `line` / `end_line` | 1-indexed 行号范围 |
| `statement_id` | mapper 标签 id，脚本为 null |
| `variant_of` | 动态分支变体指向的原 statement id，无动态分支为 null |
| `variant_label` | 动态分支组合描述（如 `if:name!=null=true,foreach:1elem`） |
| `parse_error` | 解析错误信息，正常为 null |

## 参数 fixture

无 fixture 时占位符全绑 NULL，会导致 `WHERE col = NULL` 恒为假、EXPLAIN 计划失真。两种应对方式：

### 方式 1：`--auto-param`（推荐快速验证）

```bash
java -jar sqlguard-replay-*.jar ... --auto-param --auto-param-value 1
```

所有占位符绑同一个固定值（默认 `1`）。对整数主键查询能正常走索引，字符串字段也不会报错（JDBC 做类型转换）。会触发 PARAM001 警告被抑制。

### 方式 2：`--param-fixture`（精确验证）

```json
{
  "mapper/UserMapper.xml#selectById": ["1"],
  "mapper/UserMapper.xml#findByCond#v1": ["alice", "18"],
  "sql/dml/query.sql#1": ["42"]
}
```

key 是清单中的 `id`，value 是按 `?` 出现顺序的字符串数组。未命中的语句仍按 `--auto-param` 策略回退。

## 安全模型

重放器按语句类型采用不同安全策略，确保镜像库不被污染：

| 语句类型 | 计时 | 计划采集 | 安全措施 |
|----------|------|----------|----------|
| `select` | 多次迭代计时 | `EXPLAIN ANALYZE`（AUTO/ANALYZE 模式）或 `EXPLAIN` | 只读，无需事务 |
| `insert`/`update`/`delete`/`merge` | 多次迭代计时，每次 `rollback` | `EXPLAIN`（AUTO 模式）或 `EXPLAIN ANALYZE` 包事务回滚（ANALYZE 模式） | 事务包裹后回滚，数据不变 |
| `ddl`（CTAS） | 不计时 | `EXPLAIN` | 仅 `--allow-ddl` 时执行；非 CTAS 直接跳过 |
| `ddl`（非 CTAS） | 跳过 | 跳过 | EXPLAIN 对非 CTAS DDL 无意义 |
| `other` | 跳过 | 跳过 | 不可重放 |

### 计时统计

- **warmup**：预热次数（默认 1），不计入样本
- **iterations**：正式采样次数（默认 3）
- **p50**：中位数，用于慢 SQL 分级（小样本稳定）
- **p99**：样本数 <100 时取 max 作为保守估计；≥100 时按分位计算
- 慢 SQL 分级基于 p50 而非 p99，避免单次 GC/抖动在 `iterations=3` 时触发 error 退出码

### DML 计时与回滚

```java
c.setAutoCommit(false);
for (int i = 0; i < warmup; i++) {
    runOnceDml(c, s);
    c.rollback();
}
for (int i = 0; i < iter; i++) {
    long t0 = System.nanoTime();
    runOnceDml(c, s);
    samples[i] = System.nanoTime() - t0;
    c.rollback();
}
```

每次执行后立即 `rollback`，连接归还池前 finally 再次 `rollback` + 恢复 `autoCommit`。

### SELECT 计时与行数限制

```java
long maxRows = config.getMaxRows();
try (PreparedStatement ps = c.prepareStatement(sql)) {
    binder.bind(ps, s);
    try (ResultSet rs = ps.executeQuery()) {
        if (maxRows > 0) {
            long counted = 0;
            while (rs.next() && counted < maxRows) counted++;
        } else {
            while (rs.next()) rs.getObject(1);  // 全量物化
        }
    }
}
```

- `--max-rows 0`（默认）：全量物化结果集，反映真实查询耗时
- `--max-rows N`：仅读取前 N 行，避免大结果集 IO 拖慢计时；适用于只关心计划与首屏耗时的场景

注意：`--max-rows` 仅影响 SELECT 计时阶段的行读取，不影响 EXPLAIN 计划采集。

## 执行计划分析

### 启发式规则

按方言分发不同分析器：

#### PostgreSQL / openGauss（PlanAnalyzer）

| 规则 | 触发条件 | 说明 |
|------|----------|------|
| DP001 | `Seq Scan` + `planRows > 阈值` | 大表全表扫描 |
| DP002 | `Nested Loop` 外层子节点 `planRows > 1000` | Nested Loop 外层估算行数过大 |
| DP003 | `Sort` + `planRows > 阈值` | 大排序节点 |
| DP004 | `hasActual` 且 `|actualRows - planRows| / planRows > 10` | 估算行数与实际行数偏差过大（统计信息陈旧） |
| DP007 | `Bitmap Heap Scan` + `planRows > 阈值` | Bitmap Heap Scan 回表量大 |
| DP008 | `Streaming*` + `planRows > 100000` | 分布式 Streaming 重分布行数过大（GaussDB） |

#### MySQL（MySqlPlanAnalyzer）

| 规则 | 触发条件 | 说明 |
|------|----------|------|
| DP001 | `type=ALL` + `rows > 阈值` | 大表全表扫描 |
| DP003 | `Extra` 含 `Using filesort` + `rows > 阈值` | 大排序 |
| DP006 | `Extra` 含 `Using temporary` | 使用临时表 |
| DP009 | `type=ALL` + `key=null` + `rows > 0` | 无索引访问 |

> PG 专有规则（DP002/DP004/DP007/DP008）不适用于 MySQL，跳过。DP005 预留未实现。

#### 跨方言规则（Replayer 内置）

| 规则 | 触发条件 | 说明 |
|------|----------|------|
| PARAM001 | 含占位符且无 fixture（非 auto-param 模式） | 全绑 NULL 导致 WHERE 失真，计时/计划不可信 |
| DYN003 | IN 子句参数数 > `--max-in-params` | 可能导致计划退化或超出数据库 IN 限制 |

### EXPLAIN 语法适配

| 方言 | 纯 EXPLAIN | EXPLAIN ANALYZE |
|------|------------|-----------------|
| PG/openGauss | `EXPLAIN (FORMAT JSON) <sql>` | `EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) <sql>` |
| MySQL | `EXPLAIN FORMAT=JSON <sql>` | fallback 到纯 EXPLAIN（MySQL 8.0.18+ 的 `EXPLAIN ANALYZE` 输出是表格非 JSON，不便解析） |

DML 的 `EXPLAIN ANALYZE` 在 PG 侧放入事务后 `rollback`，确保 DML 实际执行被撤销；MySQL 的 `EXPLAIN` 不执行 SQL，天然安全。

## 报告格式

### replay-report.json

```json
{
  "summary": {
    "total": 10,
    "replayed": 8,
    "skipped": 2,
    "slowWarn": 1,
    "slowError": 0,
    "errors": 0,
    "planFindings": 3
  },
  "statements": [
    {
      "id": "sql/dml/query.sql#1",
      "type": "select",
      "source": "sql/dml/query.sql",
      "line": 1,
      "sql": "SELECT id, name FROM users WHERE id = ?",
      "timings": {
        "iterations": 3,
        "minMs": 0.42,
        "p50Ms": 0.51,
        "p99Ms": 0.63,
        "avgMs": 0.52
      },
      "slowLevel": "none",
      "planTopNode": "Index Scan",
      "planTotalCost": 8.29,
      "planJson": "[{\"Plan\":{...},\"Execution Time\":0.42}]",
      "findings": [
        {
          "ruleId": "PARAM001",
          "severity": "warning",
          "message": "SQL 含 1 个占位符但无 fixture...",
          "nodeName": null,
          "relationName": null
        }
      ],
      "skipped": false
    }
  ]
}
```

### replay-report.html

深色主题表格，列：序号 / id / type / slowLevel / p99(ms) / planTopNode / cost / findings 数 / sql / error-skip。slowLevel 按颜色区分：`none` 蓝 / `warn` 黄 / `error` 红。每行下方可选折叠展开原始 EXPLAIN JSON（`<details>` 元素），便于深度调试。

## 架构

```
replay/src/main/java/com/sqlguard/replay/
├── Main.java                 # 入口：CLI 解析、驱动加载、连接池、调度、退出码
├── config/
│   └── ReplayConfig.java     # 不可变配置 bean（线程安全）
├── db/
│   └── ConnectionPool.java   # HikariCP 封装，按方言设置 statement_timeout
├── manifest/
│   ├── Manifest.java         # 清单顶层结构
│   ├── ManifestLoader.java   # JSON 加载
│   └── ManifestStatement.java# 单条语句
├── param/
│   ├── ParamBinder.java      # 占位符计数 + fixture 绑定 + auto-param 回退
│   ├── DefaultParamStrategy.java  # auto-param 策略接口
│   └── FixedValueStrategy.java    # 固定值策略实现
├── plan/
│   ├── ExplainAdapter.java   # EXPLAIN 语法适配器接口
│   ├── PgExplainAdapter.java # PG/openGauss 实现
│   ├── MySqlExplainAdapter.java # MySQL 实现
│   ├── PlanParser.java       # PG EXPLAIN JSON → PlanNode 树
│   ├── MySqlPlanParser.java  # MySQL EXPLAIN JSON → PlanNode 列表
│   ├── PlanAnalyzer.java     # PG 启发式规则（DP001-DP008）
│   ├── MySqlPlanAnalyzer.java# MySQL 启发式规则（DP001/003/006/009）
│   ├── PlanNode.java         # 计划节点（不可变）
│   └── PlanFinding.java      # 规则命中结果
├── replay/
│   ├── Replayer.java         # 核心重放器（无状态，线程安全）
│   ├── ReplayResult.java     # 单条结果
│   ├── StatementTimings.java # 耗时样本与统计量
│   ├── DbDialect.java        # 方言枚举
│   └── ExplainMode.java      # 计划采集模式枚举
├── report/
│   ├── ReplayReport.java     # 报告顶层结构
│   ├── StatementReport.java  # 单条报告项
│   └── ReportWriter.java     # 聚合 + JSON/HTML 渲染
└── slow/
    └── SlowDetector.java     # 基于 p50 的慢 SQL 分级
```

### 线程安全

- `Replayer`：无状态，单实例可被多线程并发调用
- `ReplayConfig`：不可变 bean
- `ParamBinder`：fixtures 为 `Collections.unmodifiableMap`，`autoParamStrategy` 由具体实现保证
- `ConnectionPool`：HikariDataSource 本身线程安全
- `ReplayResult` / `StatementTimings` / `PlanNode`：构造后不可变

并发重放时，`Main` 创建 `poolSize` 个线程的固定线程池，每个任务从 `ConnectionPool` 获取独立连接执行。

## 测试

```bash
cd replay
mvn test
```

测试覆盖（51 个用例，不依赖真实数据库）：

| 测试类 | 用例数 | 覆盖范围 |
|--------|--------|----------|
| `ManifestLoaderTest` | 10 | 加载 Rust 产出的样例清单、字段映射、动态分支变体、占位符计数（引号/注释/dollar-quoted）、**manifest version 校验** |
| `PlanParserTest` | 2 | PG EXPLAIN JSON 解析、DP001 命中 |
| `CtasDetectionTest` | 19 | **CTAS token 扫描器**：注释/字符串/引用标识符跳过、关键字边界、嵌套块注释、转义单引号 |
| `ReplayerIntegrationTest` | 11 | 端到端：计时、事务回滚、DDL 跳过、报告生成、并发安全、auto-param、MySQL 方言 |
| `InClauseCountTest` | 9 | DYN003 的 IN 子句参数计数（大小写、字符串字面量、INSERT/INDEX 误匹配、foreach 生成） |

集成测试用 H2 内存数据库（PostgreSQL / MySQL 兼容模式），不需要真实 GaussDB。H2 的 EXPLAIN 语法与真实 PG/MySQL 不完全一致，plan 采集可能失败，核心验证的是计时链路、事务回滚、报告生成、并发安全、auto-param 等不依赖 EXPLAIN 的能力。

## 依赖说明

| 依赖 | 版本 | 用途 |
|------|------|------|
| Jackson | 2.15.3 | 清单加载、报告序列化 |
| HikariCP | 4.0.3 | 连接池（4.x 是最后兼容 Java 8 的版本，**切勿升级到 5.x**） |
| JUnit 5 | 5.10.0 | 测试（test scope） |
| H2 | 2.2.224 | 集成测试内存数据库（test scope） |

JDBC 驱动（opengauss-jdbc / mysql-connector-java）**不在 pom 声明**，运行期通过 `URLClassLoader` 动态加载，避免私有 jar 进构建依赖。

## 限制与已知问题

1. **DDL 仅支持 CTAS 重放**：`CREATE TABLE ... AS SELECT` 走 EXPLAIN，其他 DDL（CREATE/ALTER/DROP）直接跳过——EXPLAIN 对非 CTAS DDL 无意义
2. **CTAS 检测要求 `AS SELECT` 紧接**：`CREATE TABLE t AS WITH cte AS (...) SELECT ...` 形式中 `AS` 后是 `WITH`，扫描器不识别（与原 substring 实现一致）。如需支持可扩展 `isCreateAsSelect` 在 `AS` 后跳过 `WITH ... SELECT` 组合
3. **MySQL 无 EXPLAIN ANALYZE**：MySQL 8.0.18+ 的 `EXPLAIN ANALYZE` 输出是表格非 JSON，本工具 fallback 到纯 EXPLAIN，不获取实际执行统计
4. **MySQL MAX_EXECUTION_TIME 仅对 SELECT 生效**：`SET SESSION MAX_EXECUTION_TIME` 不影响 DML/DDL，DML 计时若卡住需依赖连接超时
5. **慢 SQL 分级基于 p50**：`iterations=3` 时 p99=max，单次抖动即触发 error；改用 p50 更稳定，代价是对长尾不敏感

## License

MIT（随 SqlGuard 主工程）
