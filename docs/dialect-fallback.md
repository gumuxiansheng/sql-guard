# 逐语句方言回退链设计（dialect_fallback）

> 日期：2026-07-27
> 适用：SqlGuard v0.1.0（sqlparser 0.60）
> 背景：GaussDB「PG 内核 + Oracle 外壳」混合方言解析

---

## 一、问题背景

GaussDB 等国产库的常见形态是 **PostgreSQL 内核 + Oracle 兼容外壳**：一条 SQL 里既可能用 PG 语法，也可能混入 Oracle 专有语法，例如：

- `MINUS`（集合差，对应 PG/标准里的 `EXCEPT`）
- `CONNECT BY ... START WITH`（层次查询）
- `(+)` 老式外连接写法
- `DUAL` 哑表、`SYSDATE`、`NVL`、`DECODE`、`ROWNUM` 等 Oracle 伪列/函数

如果只用一个方言解析，**主方言遇到另一种方言的语法就会整条解析失败 → 记为 `PARSE_ERROR` 并被跳过**，导致该语句完全脱离规则检查。

单纯「升级 sqlparser 到更新版本」并不能解决：sqlparser 的每种 `Dialect` 都只认自己那套语法，没有一种方言能同时吃下 PG + Oracle 混合体。

---

## 二、设计：逐语句方言回退链

核心思路：**每条语句独立地走一条「主方言 → 回退方言 → Generic 兜底」的方言重试链**，首个成功解析的方言即被采用；全部失败才记 `PARSE_ERROR`。

```
主方言 (dialect)
   │ 解析失败 / 解析不干净
   ▼
回退方言 (dialect_fallback，可选)
   │ 解析失败
   ▼
GenericDialect（兜底，永远在链尾）
   │ 仍失败
   ▼
记 PARSE_ERROR（该语句被跳过）
```

链构造规则（`build_chain_enums`）：

```
chain = [primary]
if fallback 存在且与 primary 不同: chain.push(fallback)
chain.push(Generic)            // 链尾永远兜底
```

---

## 三、配置与 CLI

### 3.1 配置文件 `sqlguard.toml`

`Config` 新增可选字段 `dialect_fallback`：

```toml
dialect = "postgresql"
dialect_fallback = "oracle"   # 可选；不写时按 default_fallback 推导
```

`CheckDialect::default_fallback()` 推导规则：

| 主方言 `dialect` | 默认回退 `dialect_fallback` |
|---|---|
| `postgresql` | `oracle`（GaussDB 场景默认即开） |
| `generic` / `mysql` / `ansi` / `oracle` | 无（链退化为 `主 → Generic`） |

### 3.2 CLI 覆盖

两个子命令 `check` / `check-diff` 均支持：

```
--dialect-fallback <generic|mysql|postgresql|ansi|oracle>
```

- 传 `generic` = **显式关闭**回退：链退化为「主方言 → Generic」。
- CLI 值优先于配置文件；都未指定时走 `default_fallback()`。

运行时会在 stderr 打印实际生效的链，便于确认：

```
Dialect fallback chain: postgresql → oracle → generic
```

---

## 四、解析算法（src/rule/engine/parser.rs）

入口 `parse_sql_to_ast_fb(sql, dialect, fallback)`，内部对每条语句执行：

1. **主方言尝试**：`parser.parse_statement()`。
2. **「干净解析」判定（关键修复点）**：
   解析成功后检查**下一 token 是否为 `;` 或 `EOF`**。
   - 是 → 主方言完整吃下了这条语句，直接采用。
   - **否 → 主方言只吞掉了前缀（残缺解析），丢弃结果，落入回退链。**
3. **硬错误**（`parse_statement()` 返回 `Err`）→ 同样落入回退链。
4. **回退链**：把整条语句文本按字节偏移切片出来，依次用 `chain[1..]` 的每个方言 `parse_one_with` 重试，**首个成功即采用**。
5. **全失败** → 该语句记为 `kind = "PARSE_ERROR"`，后续被规则引擎跳过。

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

---

## 五、实际行为（端到端验证，已修正旧结论）

用一组混合方言语句验证（PG 内核 + Oracle 外壳）：

```sql
SELECT a FROM t1 MINUS SELECT a FROM t2;
SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1;
SELECT SYSDATE FROM dual;
CREATE TABLE t_main (id INT PRIMARY KEY, name VARCHAR(100));
SELECT * FROM dept d WHERE d.id = (SELECT id FROM emp e WHERE e.dept_id = d.id(+));
SELECT * FROM t WHERE ROWNUM <= 10;
```

| 模式 | 生效链 | 结果 |
|---|---|---|
| `--dialect postgresql`（默认回退 oracle） | `postgresql → oracle → generic` | **0 个 PARSE_ERROR**，6 条全部解析成功 |
| `--dialect postgresql --dialect-fallback generic` | `postgresql → generic` | 仅 **`(+)` 外连接**那一条解析失败；其余（含 `MINUS`、`CONNECT BY`、`ROWNUM`）均被 Generic 兜底解析 |

**重要修正**：早期记录曾误称「`--dialect-fallback generic` 时 `MINUS`/`CONNECT BY` 会退化为 `PARSE_ERROR`」——这是不准确的。在 sqlparser 0.60 中，`GenericDialect` 已经能解析 `MINUS` 与 `CONNECT BY`；`ROWNUM` 也能被当作普通标识符解析。因此 **Oracle 回退真正独力救回来的是 `(+)` 老式外连接语法**，以及对其他 Oracle 专有构造的额外兜底。

---

## 六、缓存失效

`cache::compute_run_signature` 已将 `dialect_fallback` 纳入运行签名。切换回退方言会改变解析结果，缓存会整体失效并重新检查，不会出现「改了方言却读了旧缓存」的错位。

---

## 七、已知局限

- 确实无法被任何链上方言解析的 SQL，仍记为 `PARSE_ERROR` 并跳过（符合预期）。
- sqlparser 的 `OracleDialect` 并非 100% 覆盖 Oracle 全部语法；极端专有构造仍可能解析失败。
- 回退链是按「整条语句」重试，不会把一条语句「拆成两半用两种方言各解析一部分」——这对绝大多数混合方言场景已足够，但理论上存在单语句内语法横跨两种方言且各自都不完整的边界情况。

---

## 八、相关测试

单测（`src/rule/engine/parser.rs`）：

- `test_build_chain_enums_dedup_and_generic_tail` — 链构造：去重 + 链尾恒为 Generic。
- `test_location_to_byte_offset_basic` — char 级偏移切片正确性。
- `test_parse_postgresql_falls_back_to_oracle` — `MINUS` 经 Oracle 回退解析为 `EXCEPT`。
- `test_parse_error_after_fallback_exhausted` — 真正无法解析的语句仍记 `PARSE_ERROR`。
- `test_parse_oracle_connect_by_via_fallback` — `CONNECT BY` 经回退链（Oracle 或 Generic 兜底）成功解析，不产生 `PARSE_ERROR`。
- `test_parse_oracle_as_primary` — **Oracle 直接作主方言**：`MINUS`/`CONNECT BY`/`SELECT ... FROM dual` 均正确解析（验证 OracleDialect 路径本身，不依赖回退）。
- `test_parse_generated_column_sets_auto_increment` — `GENERATED ALWAYS AS IDENTITY` 列被正确标记为 `is_auto_increment`（验证 `ColumnOption::Generated` 分支）。

全量回归：`cargo test` 291 用例全绿（lib 267 + integration 24）。

---

## 九、列级元信息（Oracle/PG 扩展）

解析 `CREATE TABLE` 的列选项时，补充了原本缺失的识别：

- **`ColumnOption::Generated { .. }`**：Oracle 12c+ / PG 的 `GENERATED ... AS IDENTITY` 此前落入 `_ => {}` 被静默忽略，导致身份列丢失 `is_auto_increment` 元信息。现已显式捕获并置 `is_auto_increment = true`。
- **`ColumnOption::DialectSpecific`**：在原有 `AUTO_INCREMENT` / `AUTOINCREMENT` 基础上，增加 `IDENTITY` 关键字识别（兜底 Oracle 以 `DialectSpecific` 形式产出身份列等情形）。

这让依赖于「自增列」元信息的规则（如主键/冗余索引判断）在 Oracle/PG 身份列上也工作正常。
