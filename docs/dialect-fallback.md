# 逐语句方言回退链设计（dialect_fallback）

> 日期：2026-07-27（初版）/ 2026-07-29（v0.2.0：新增 GaussDB 方言 + PG 链去污染）
> 适用：SqlGuard v0.2.0（sqlparser 0.60）
> 背景：GaussDB「PG 内核 + Oracle/MySQL 外壳」混合方言解析

---

## 一、问题背景

GaussDB 等国产库的常见形态是 **PostgreSQL 内核 + Oracle/MySQL 兼容外壳**：一条 SQL 里既可能用 PG 语法，也可能混入 Oracle/MySQL 专有语法，例如：

- `MINUS`（集合差，对应 PG/标准里的 `EXCEPT`）
- `CONNECT BY ... START WITH`（层次查询）
- `(+)` 老式外连接写法
- `DUAL` 哑表、`SYSDATE`、`NVL`、`DECODE`、`ROWNUM` 等 Oracle 伪列/函数
- 反引号标识符 `` `order` ``（MySQL 兼容）

如果只用一个方言解析，**主方言遇到另一种方言的语法就会整条解析失败 → 记为 `PARSE_ERROR` 并被跳过**，导致该语句完全脱离规则检查。

单纯「升级 sqlparser 到更新版本」并不能解决：sqlparser 的每种 `Dialect` 都只认自己那套语法，没有一种方言能同时吃下 PG + Oracle + MySQL 混合体。

### 1.1 v0.1.0 的污染问题

v0.1.0 通过「PG 主方言 + Oracle 回退」解决混合方言解析，但引入了**语义污染**：

> 一条语句只要 PG 方言没干净解析，**整条语句**就会用 `OracleDialect` 重新解析。于是这条语句的 AST 语义就由 Oracle 方言决定，而不是 PG。

最严重的污染是**标识符大小写折叠**：
- PG 方言：未加引号的 `Users` → 折叠为 `users`（小写）
- Oracle 方言：未加引号的 `Users` → 折叠为 `USERS`（大写）

一条原本纯 PG 的 `SELECT * FROM Users`，如果碰巧带了某个 Oracle 语法（比如末尾 `MINUS`），整条语句被 Oracle 重解析，`Users` 变成 `USERS`，所有依赖表名的规则（主键声明匹配、外键引用、备份表命名）都会失配。

---

## 二、v0.2.0 设计：GaussDB 方言 + 词法重写层 + PG 链去污染

v0.2.0 引入三层设计，彻底消除污染：

### 2.1 新增 `gaussdb` 方言类别

`CheckDialect` 新增 `GaussDB` 变体，作为一等公民，不再依赖「PG 主 + Oracle 回退」的隐式组合。

### 2.2 GaussDB 词法重写层（核心）

在 sqlparser 解析前，对 SQL 文本做**词法级、可审计**的归一化重写，把 GaussDB 兼容的 Oracle/MySQL 专有构造改写为 PG 等价语法，再用纯 `PostgreSqlDialect` 解析。AST 语义 100% 来自 PG 方言，污染消除。

当前覆盖的规则（首版，全部默认开启）：

| 规则 ID | 原始构造 | 重写为 | 实现方式 |
|---|---|---|---|
| G_ORACLE_MINUS | `MINUS` | `EXCEPT` | 词法替换（词边界匹配） |
| G_ORACLE_SYSDATE | `SYSDATE` | `CURRENT_TIMESTAMP` | 词法替换（词边界匹配） |
| G_ORACLE_NVL | `NVL(` | `COALESCE(` | 词法替换（词边界 + 紧跟左括号） |
| G_ORACLE_DUAL | `FROM dual` / `FROM DUAL` | `FROM (SELECT 1) AS dual` | 词法+上下文 |
| G_MYSQL_BACKTICK | `` `ident` `` | `"ident"` | 词法替换（成对反引号） |

重写层的设计约束：
- **行号保真**：所有替换不引入/删除换行符，保证 violation 行号与原文件一致。
- **不误伤字符串/注释**：扫描器跳过单引号字符串 `'...'`（含 `''` 转义）、`--` 行注释、`/* */` 块注释。
- **UTF-8 安全**：非 ASCII 字符（如中文）按字符级透传，不做字节级 `as char` 转换，避免乱码。
- **可审计**：每次重写记录 `RewriteRecord`，供 debug 模式透出。

### 2.3 PG 链去污染（破坏性变更）

`default_fallback()` 行为变更：

| 主方言 `dialect` | v0.1.0 默认回退 | v0.2.0 默认回退 | 说明 |
|---|---|---|---|
| `postgresql` | `oracle` | **`None`** | **破坏性**：消除隐式污染。需 Oracle 兼容请用 `gaussdb` |
| `gaussdb` | （不存在） | `oracle` | 新增。重写层覆盖词法级构造，Oracle 兜底复杂构造 |
| 其余 | `None` | `None` | 无变化 |

### 2.4 逐语句回退链（保留）

GaussDB 重写后仍解析失败的语句（如 `CONNECT BY`、`(+)` 外连接），按「PG → Oracle → Generic」回退链重试。回退链算法与 v0.1.0 一致（见 §四）。

---

## 三、配置与 CLI

### 3.1 配置文件 `sqlguard.toml`

**推荐配置（GaussDB 场景）**：

```toml
dialect = "gaussdb"
# 不需要显式配 dialect_fallback —— GaussDB 默认回退 oracle
```

**兼容旧配置（v0.1.0 迁移）**：

```toml
dialect = "postgresql"
dialect_fallback = "oracle"   # 显式回退，行为与 v0.1.0 一致
```

`CheckDialect::default_fallback()` 推导规则（v0.2.0）：

| 主方言 `dialect` | 默认回退 `dialect_fallback` |
|---|---|
| `gaussdb` | `oracle`（覆盖重写层未处理的复杂构造） |
| `generic` / `mysql` / `postgresql` / `ansi` / `oracle` | 无（链退化为 `主 → Generic`） |

### 3.2 CLI 覆盖

两个子命令 `check` / `check-diff` 均支持：

```
--dialect <generic|mysql|postgresql|ansi|oracle|gaussdb>
--dialect-fallback <generic|mysql|postgresql|ansi|oracle|gaussdb>
```

- 传 `generic` = **显式关闭**回退：链退化为「主方言 → Generic」。
- CLI 值优先于配置文件；都未指定时走 `default_fallback()`。

`gen-rollback` 子命令的 `--dialect` 也支持 `gaussdb`（渲染层委托 PG）。

运行时会在 stderr 打印实际生效的链，便于确认：

```
Dialect fallback chain: gaussdb → oracle → generic
```

### 3.3 迁移指南

| 旧配置（v0.1.0） | 新配置（v0.2.0） | 行为变化 |
|---|---|---|
| `dialect = "postgresql"`（无 fallback） | `dialect = "gaussdb"` | 保持原兼容性（Oracle 语法仍能解析），且 AST 语义更干净 |
| `dialect = "postgresql"` + `dialect_fallback = "oracle"` | `dialect = "gaussdb"`（推荐）或保持原配置 | 保持原配置仍工作（显式 fallback 不受 default_fallback 变更影响） |
| `dialect = "postgresql"`，纯 PG 无 Oracle 语法 | 不变 | 解析链从 `PG→Oracle→Generic` 变为 `PG→Generic`，AST 更干净，个别原误解析的语句可能新报 PARSE_ERROR |

---

## 四、解析算法（src/rule/engine/parser.rs）

入口 `parse_sql_to_ast_fb(sql, dialect, fallback)`，流程：

1. **GaussDB 重写（仅 `dialect == GaussDB`）**：
   调用 `gaussdb_rewrite::rewrite_for_pg_parse(sql)`，把 SQL 文本词法归一化为 PG 等价语法。
   重写后 `effective_dialect` 切换为 `PostgreSql`，`fallback` 保持用户传入值（GaussDB 默认 `Some(Oracle)`）。
2. **主方言尝试**：`parser.parse_statement()`。
3. **「干净解析」判定（关键修复点）**：
   解析成功后检查**下一 token 是否为 `;` 或 `EOF`**。
   - 是 → 主方言完整吃下了这条语句，直接采用。
   - **否 → 主方言只吞掉了前缀（残缺解析），丢弃结果，落入回退链。**
4. **硬错误**（`parse_statement()` 返回 `Err`）→ 同样落入回退链。
5. **回退链**：把整条语句文本按字节偏移切片出来，依次用 `chain[1..]` 的每个方言 `parse_one_with` 重试，**首个成功即采用**。
6. **全失败** → 该语句记为 `kind = "PARSE_ERROR"`，后续被规则引擎跳过。

### 4.1 为什么必须检查「干净解析」

`CONNECT BY` 是触发这个修复的典型案例。PG 方言对
`SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1`
**不会硬报错**——它把 `SELECT empno FROM emp` 当一条（残缺）语句吞掉，只留下 `CONNECT BY ...` 作为「下一条语句」。

如果回退逻辑只在 `parse_statement()` 返回 `Err` 时才触发，那么：
- 主方言「成功」解析了残缺前缀（不报错）；
- 回退链永远不执行；
- `CONNECT BY` 部分被静默丢弃，规则拿到的 AST 是错的。

通过「下一 token 必须是 `;`/`EOF`」的干净度检查，残缺解析会被识别为软失败，整条语句重新进入回退链用 Oracle 重解析，得到完整 AST。

### 4.2 位置切片 `location_to_byte_offset`

回退时要把「整条语句」从原文切片出来，需要把 sqlparser 的 `Location{line, column}` 换算成字节偏移。

实测 sqlparser 0.60 的 `Location` 是 **字符级、1-based** 的（`tokenizer` 中 `col` 按 `char` 递增，换行归 1），且与 LF/CRLF 换行兼容。因此切片必须按 **char** 而非 **byte** 计算（多字节字符场景下 byte 偏移会与 sqlparser 计数错位）。

### 4.3 GaussDB 重写层的行号保真

重写层对所有替换保证不引入/删除换行符：
- 变长替换（如 `SYSDATE`(7) → `CURRENT_TIMESTAMP`(17)）仅在同一行内变长，不影响行号。
- 跨行重写（未来可能的 DECODE→CASE）需用占位空格填充，保持行号不变。
- 不允许跨行删除/插入换行。

因此 violation 行号与原文件一致，无需额外偏移计算。

---

## 五、实际行为（端到端验证）

用一组混合方言语句验证（PG 内核 + Oracle/MySQL 外壳）：

```sql
SELECT a FROM t1 MINUS SELECT a FROM t2;
SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1;
SELECT SYSDATE FROM dual;
CREATE TABLE t_main (id INT PRIMARY KEY, name VARCHAR(100));
SELECT * FROM dept d WHERE d.id = (SELECT id FROM emp e WHERE e.dept_id = d.id(+));
SELECT * FROM t WHERE ROWNUM <= 10;
SELECT `order` FROM t;
```

| 模式 | 生效链 | 结果 |
|---|---|---|
| `--dialect gaussdb`（推荐） | `gaussdb(重写→PG) → oracle → generic` | **0 个 PARSE_ERROR**，7 条全部解析成功。AST 语义 100% 来自 PG（标识符小写折叠） |
| `--dialect postgresql`（v0.2.0 默认无回退） | `postgresql → generic` | `MINUS` / `(+)` 等解析失败（PG 不认），记 PARSE_ERROR。证明污染消除 |
| `--dialect postgresql --dialect-fallback oracle`（兼容旧配置） | `postgresql → oracle → generic` | 0 个 PARSE_ERROR，但 AST 语义由 Oracle 决定（标识符大写折叠，污染仍存） |

**反污染验证**（`test_gaussdb_dialect_preserves_pg_identifier_case`）：
同一条 `SELECT * FROM Users MINUS SELECT * FROM OldUsers`：
- 旧链（PG→Oracle 回退）：`Users` 折叠为 `USERS`（污染）
- GaussDB 方言：`Users` 折叠为 `users`（干净）

---

## 六、缓存失效

`cache::compute_run_signature` 已将 `dialect` 与 `dialect_fallback` 纳入运行签名。
- `dialect=gaussdb` 唯一确定重写规则集（当前阶段 1 规则固定全开，无配置开关）。
- 切换方言或回退链会改变解析结果，缓存会整体失效并重新检查。
- 未来引入重写规则配置开关时，需把规则集 ID 纳入签名。

---

## 七、已知局限

- 确实无法被任何链上方言解析的 SQL，仍记为 `PARSE_ERROR` 并跳过（符合预期）。
- sqlparser 的 `OracleDialect` 并非 100% 覆盖 Oracle 全部语法；极端专有构造仍可能解析失败。
- 回退链是按「整条语句」重试，不会把一条语句「拆成两半用两种方言各解析一部分」。
- GaussDB 重写层首版仅覆盖 5 条词法级规则；`DECODE`/`ROWNUM`/`LIMIT N,M`/`(+)`/`CONNECT BY` 等复杂构造仍由 Oracle 回退兜底（未来阶段逐步增强重写层）。

---

## 八、相关测试

单测（`src/rule/engine/parser.rs`）：

- `test_build_chain_enums_dedup_and_generic_tail` — 链构造：去重 + 链尾恒为 Generic。
- `test_location_to_byte_offset_basic` — char 级偏移切片正确性。
- `test_parse_postgresql_falls_back_to_oracle` — `MINUS` 经 Oracle 回退解析为 `EXCEPT`（显式 fallback）。
- `test_parse_error_after_fallback_exhausted` — 真正无法解析的语句仍记 `PARSE_ERROR`。
- `test_parse_oracle_connect_by_via_fallback` — `CONNECT BY` 经回退链成功解析。
- `test_parse_oracle_as_primary` — Oracle 直接作主方言的路径验证。
- `test_parse_generated_column_sets_auto_increment` — `GENERATED ALWAYS AS IDENTITY` 列元信息。
- `test_gaussdb_dialect_parses_minus_with_pg_semantics` — GaussDB 方言经重写解析 MINUS。
- `test_gaussdb_dialect_preserves_pg_identifier_case` — ★ 反污染核心测试：标识符小写折叠。
- `test_gaussdb_dialect_parses_sysdate_and_nvl` — SYSDATE/NVL 重写验证。
- `test_gaussdb_dialect_parses_from_dual` — FROM dual 重写验证。
- `test_gaussdb_dialect_parses_backtick_identifier` — 反引号重写验证。
- `test_gaussdb_dialect_mixed_oracle_mysql_constructs` — 混合方言端到端。
- `test_gaussdb_dialect_falls_back_to_oracle_for_connect_by` — Oracle 回退兜底复杂构造。
- `test_postgresql_no_longer_falls_back_to_oracle_by_default` — ★ 破坏性变更验证：PG 不再默认回退 Oracle。
- `test_gaussdb_dialect_pure_pg_sql_unchanged` — 纯 PG 语法不受重写层影响。

单测（`src/rule/engine/gaussdb_rewrite.rs`）：30+ 用例覆盖 5 条规则、边界场景、UTF-8 安全、行号保真。

单测（`src/rollback/dialect.rs`）：`gaussdb_renderer_delegates_to_pg` / `renderer_for_gaussdb_returns_gaussdb_renderer`。

单测（`src/config.rs`）：`config_dialect_gaussdb_from_toml_and_default_fallback` / `config_dialect_from_str_accepts_gaussdb_aliases`。

---

## 九、列级元信息（Oracle/PG 扩展）

解析 `CREATE TABLE` 的列选项时，补充了原本缺失的识别：

- **`ColumnOption::Generated { .. }`**：Oracle 12c+ / PG 的 `GENERATED ... AS IDENTITY` 此前落入 `_ => {}` 被静默忽略，导致身份列丢失 `is_auto_increment` 元信息。现已显式捕获并置 `is_auto_increment = true`。
- **`ColumnOption::DialectSpecific`**：在原有 `AUTO_INCREMENT` / `AUTOINCREMENT` 基础上，增加 `IDENTITY` 关键字识别（兜底 Oracle 以 `DialectSpecific` 形式产出身份列等情形）。

这让依赖于「自增列」元信息的规则（如主键/冗余索引判断）在 Oracle/PG 身份列上也工作正常。

---

## 十、未来路线图

| 阶段 | 内容 | 风险 |
|---|---|---|
| 阶段 1（本次） | GaussDB 方言 + 5 条词法级重写 + PG 链去污染 + GaussDBRenderer | 低（纯增量，不破坏现有 PG/Oracle 显式配置路径） |
| 阶段 2 | DECODE→CASE / ROWNUM→LIMIT / LIMIT N,M→LIMIT M OFFSET N 重写 | 中（需括号/上下文匹配测试） |
| 阶段 3 | 自定义 `GaussDbDialect`（词法级扩展，如接受反引号） | 中 |
| 阶段 4 | `(+)` 外连接 / `CONNECT BY` 重写（不再依赖 Oracle 回退） | 高（语义复杂） |
