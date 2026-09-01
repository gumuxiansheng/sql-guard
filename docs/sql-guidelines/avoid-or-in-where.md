# SQL 编码规范：避免在 WHERE 子句中使用 OR 连接多个条件

> 对应内置规则：**DML111 `no_or_in_where`**（默认启用，`warning`）
> 适用语句：SELECT（含子查询）、UPDATE、DELETE 的 WHERE 子句

## 1. 为什么禁用

`WHERE col = 1 OR col = 2` 这类"析取（Disjunctive）条件"会让优化器难以或无法利用单列索引：

- **单列索引无法直接命中 OR**：索引只能沿某个有序路径范围扫描，`col = 1 OR col = 2` 需要"两段索引扫描再合并"，或退化为全表扫描。
- **跨列 OR 更差**：`a = 1 OR b = 2` 只能走 Index Merge（MySQL）或 BitmapOr（PostgreSQL）等特殊机制，而这类机制对**每一列都必须有索引**且优化器认为合并成本更低时才生效，否则直接全表扫描。
- **代价随条件数线性恶化**：OR 分支越多，合并路径越复杂，计划越不稳定。

**反例（建议禁止）**：

```sql
-- 同列多值：可改写为 IN
SELECT * FROM users WHERE status = 'active' OR status = 'pending';

-- 跨列析取：可改写为 UNION ALL
SELECT * FROM orders WHERE buyer_id = 100 OR seller_id = 100;

-- 深嵌套 OR 括号组合
SELECT * FROM users WHERE (a = 1 OR b = 2) AND c = 3;
```

## 2. 替代写法（按场景选择）

### 方案 A：IN 列表 —— 同一列、多个等值

```sql
-- 原写法
SELECT * FROM users WHERE status = 'active' OR status = 'pending';

-- 改写
SELECT * FROM users WHERE status IN ('active', 'pending');
```

- **索引利用**：对 `status` 的单列索引，`IN` 等价于多个等值条件的范围扫描（MySQL 显示为 `ref_or_null`/`range`，PG 展开为 `= ANY(array)` 走 BitmapOr 或索引扫描），与"逐值 Index Range Scan"效率相当，通常**优于 OR 的独立分支**。
- **可读性**：单一条件、长度随值数量线性增长，比一长串 OR 清晰得多。
- **适用**：OR 分支均是对**同一列**的等值比较。

### 方案 B：UNION ALL —— 不同列 / 无法归并到单列的析取

```sql
-- 原写法
SELECT * FROM orders WHERE buyer_id = 100 OR seller_id = 100;

-- 改写（UNION ALL 不去重，成本最低；如业务需要去重再用 UNION）
SELECT * FROM orders WHERE buyer_id = 100
UNION ALL
SELECT * FROM orders WHERE seller_id = 100;
```

- **索引利用**：两个分支各自独立执行，`buyer_id` 分支走 `idx_buyer_id`、`seller_id` 分支走 `idx_seller_id`，**两个索引都能用上**；而原 OR 写法可能一个都用不上。
- **执行效率**：UNION ALL 不做去重（不排序不哈希），只是结果集的追加合并；只有当两分支存在重叠行且业务需要去重时才应改用 UNION（会引入排序/哈希去重成本）。
- **适用**：跨列析取、或各分支需走不同索引 / 不同分区裁剪、以及 OR 分支内各自还带不同附加条件（如 `a=1 AND x<10 OR b=2 AND y>5`）的复杂场景。

### 方案 C：拆分查询 —— 分支结果集小 / 可并行 / 需要业务侧处理

```sql
-- 原写法
SELECT * FROM big_orders
WHERE created_at < '2024-01-01' OR status = 'VOID';

-- 改写：两条查询在应用层合并（可并发执行）
--   q1: SELECT * FROM big_orders WHERE created_at < '2024-01-01';
--   q2: SELECT * FROM big_orders WHERE status = 'VOID';
```

- **索引利用**：两个查询各自独立、各自选择最优索引（甚至可以是不同索引），互不拖累。
- **执行效率**：两条查询可**并行执行**，总耗时接近较慢的一条；每条的成本也远低于单条 OR 的全表扫描。
- **代价**：多一次网络往返、应用层需要合并去重，代码变多——仅当结果集较小、或单条 OR 查询已确认走全表扫描时才推荐。
- **适用**：OR 分支数量多、各分支独立性强、或与业务分页/批处理（如"冷热数据"分批取数）结合的场景。

### 补充写法：CASE 聚合 —— 分组统计类的"条件投影"

```sql
-- 反例（聚合内 OR 不友好）
SELECT dept,
       COUNT(*) FROM emp
WHERE dept = 'D1' OR dept = 'D2'
GROUP BY dept;

-- 改写（同一列 → IN）
SELECT dept, COUNT(*) FROM emp
WHERE dept IN ('D1', 'D2')
GROUP BY dept;
```

## 3. 索引利用与执行效率对比

| 写法 | 索引利用 | 执行效率 | 可读性 | 适用场景 |
|------|----------|----------|--------|----------|
| `WHERE a=1 OR b=2`（原） | 差：通常无法命中单列索引；需 Index Merge/BitmapOr 且**所有列都有索引**才可能用 | 低：无索引则全表扫描；索引合并成本高、计划不稳定 | 一般 | ❌ 尽量避免 |
| `WHERE col IN (1,2)` | 好：单列索引直接 range/ref 扫描 | 高：等价多等值范围扫描，开销近似"逐值扫索引" | 好 | 同列多值等值 |
| `UNION ALL` 两分支 | 好：每分支各自走自己的索引 | 高：无去重开销，只做结果追加；可并行（各引擎视计划） | 中：语句较长，需注意分支列对齐 | 跨列析取、多索引场景 |
| `UNION`（去重） | 好（同 UNION ALL） | 中：引入排序/哈希去重 | 中 | 分支有重叠行且需去重 |
| 拆分查询（应用层合并） | 好：各查询独立选索引 | 高（可并行），但有额外网络往返 | 低：代码变多 | 分支多、结果集小、可并行 |

## 4. 各数据库行为差异（补充说明）

- **MySQL**：优化器会自动把 `col = 1 OR col = 2` 转成 `col IN (1, 2)`（5.0+ 等值合并），但**跨列 OR 依赖 `index_merge`**，且要求每列都有可用索引，否则全表扫描。`IN` 列表过长（数百至上千）时可能退化。
- **PostgreSQL**：跨列 OR 可通过 BitmapOr 合并位图扫描，但同样要求各列有索引且选择性高；对无法用位图合并的计划会退化为顺序扫描。改写为 UNION ALL 通常更可控。
- **Oracle**：`IN` 列表有 **1000 项上限**（超限报 ORA-01795），超长时必须拆分或用 UNION ALL。
- **通用**：`IN` 的 NULL 语义与等值 OR 链一致（`NULL IN (1,2)` 为假）；但 `NOT IN` 与 `<>` 链的 NULL 语义不同（`NOT IN` 遇 NULL 恒为假），改写 `NOT IN` 时务必确认列无 NULL。

## 5. 例外情况（允许放行）

- OR 两侧都是**常量表达式**（如 `WHERE 1=1` 这类动态 SQL 占位）——那是另一条规则 DML108 的管辖范围。
- 字符串值 / 标识符内包含字母 "or"（如 `WHERE note = 'pending or done'`、`WHERE normal_flag = 1`）——检测器按 token 精确匹配 `OR` 关键字，不会误报。
- OR 出现在非 WHERE 位置（投影表达式 `CASE WHEN a=1 OR b=2 ...`、JOIN 的 ON 条件）——本规范仅约束 WHERE 子句。
- 确认 OR 分支数量少（2 个以内）、选择性极高、且 EXPLAIN 已确认走索引（如 MySQL `index_merge`、PG `BitmapOr`）时，可加 `-- sqlguard-disable-line DML111` 行内豁免。

## 6. 改写速查

| 原写法 | 改写建议 |
|--------|----------|
| `WHERE col = v1 OR col = v2 OR col = v3` | `WHERE col IN (v1, v2, v3)` |
| `WHERE a = 1 OR b = 2`（不同列） | `SELECT ... WHERE a=1 UNION ALL SELECT ... WHERE b=2` |
| `WHERE a = 1 OR b = 2`（结果集小、可并行） | 拆成两条查询，应用层并发 + 合并 |
| `WHERE (a=1 OR b=2) AND c=3` | `(SELECT ... WHERE a=1 AND c=3) UNION ALL (SELECT ... WHERE b=2 AND c=3)` |
