use crate::config::RuleConfig;

/// 规则筛选器，由 CLI 参数构造。空表示不限制。
///
/// - `include_rules` / `include_groups`: 白名单，规则需命中其一才执行；
///   两者都为空表示全部包含。
/// - `exclude_rules` / `exclude_groups`: 黑名单，命中任一则跳过。
///   黑名单优先于白名单。
///
/// 每个条目支持前缀通配，如 `DDL*` 匹配所有以 `DDL` 开头的 id。
#[derive(Debug, Clone, Default)]
pub struct RuleFilter {
    pub include_rules: Vec<String>,
    pub include_groups: Vec<String>,
    pub exclude_rules: Vec<String>,
    pub exclude_groups: Vec<String>,
}

impl RuleFilter {
    /// 从逗号分隔的字符串解析筛选列表，自动去空白、忽略空项。
    fn parse_csv(s: &Option<String>) -> Vec<String> {
        match s {
            Some(s) if !s.trim().is_empty() => s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn from_cli(
        rules: &Option<String>,
        groups: &Option<String>,
        exclude_rules: &Option<String>,
        exclude_groups: &Option<String>,
    ) -> Self {
        RuleFilter {
            include_rules: Self::parse_csv(rules),
            include_groups: Self::parse_csv(groups),
            exclude_rules: Self::parse_csv(exclude_rules),
            exclude_groups: Self::parse_csv(exclude_groups),
        }
    }

    /// 判断该过滤器是否实际上不限制任何规则。
    pub fn is_empty(&self) -> bool {
        self.include_rules.is_empty()
            && self.include_groups.is_empty()
            && self.exclude_rules.is_empty()
            && self.exclude_groups.is_empty()
    }

    /// 判断单条规则是否应该执行。
    pub fn matches(&self, rule: &RuleConfig) -> bool {
        self.matches_id_group(&rule.id, rule.group.as_deref())
    }

    /// 按规则 id 与分组判断是否应执行。供非 Rhai 的原生检查器（如文件格式检查）复用。
    ///
    /// 规则同 [`RuleFilter::matches`]：黑名单优先，白名单为空表示不限制，
    /// 每个条目支持前缀通配 `prefix*`。
    pub fn matches_id_group(&self, id: &str, group: Option<&str>) -> bool {
        // 1. 黑名单优先：命中即排除
        if matches_rule_id_any(&self.exclude_rules, id) {
            return false;
        }
        if let Some(g) = group {
            if matches_any(&self.exclude_groups, g) {
                return false;
            }
        }

        // 2. 白名单为空表示不限制
        let in_rules =
            self.include_rules.is_empty() || matches_rule_id_any(&self.include_rules, id);
        let in_groups = self.include_groups.is_empty()
            || group
                .map(|g| matches_any(&self.include_groups, g))
                .unwrap_or(false);

        in_rules && in_groups
    }
}

/// 判断 `value` 是否匹配 patterns 中的任一项。支持前缀通配 `prefix*`。
pub fn matches_any(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|pat| matches_plain(pat, value))
}

/// 单值匹配：精确相等，或 `prefix*` 前缀通配。
fn matches_plain(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

/// 单个过滤项是否匹配规则 id（支持 namespace 规范 id `<ns>:<id>`）。
///
/// 匹配语义（见 `docs/rule-pack-design.md` §5.3）：
/// - 过滤项**带** namespace（`ns:id` / `ns:*`）→ 规则 id 的 namespace 必须相同，再匹配短 id；
/// - 过滤项为**裸 id**（`DDL001` / `DDL*`）→ 忽略 namespace，匹配规则的短 id。
///
/// 无 namespace 的规则（裸 id）与 [`matches_any`] 行为完全一致。
pub fn matches_rule_id(pattern: &str, id: &str) -> bool {
    let short = crate::rule::pack::short_id(id);
    match pattern.split_once(':') {
        Some((pat_ns, pat_short)) => {
            let rule_ns = id.split_once(':').map(|(ns, _)| ns);
            rule_ns == Some(pat_ns) && matches_plain(pat_short, short)
        }
        None => matches_plain(pattern, short),
    }
}

/// 任一过滤项匹配规则 id（namespace 感知，见 [`matches_rule_id`]）。
pub fn matches_rule_id_any(patterns: &[String], id: &str) -> bool {
    patterns.iter().any(|pat| matches_rule_id(pat, id))
}

/// 规则执行上下文，传递给每条 Rhai 规则脚本。
///
/// `ast` 字段提供解析后的 SQL AST，规则脚本通过 `context["ast"]` 访问。
/// 同时保留 `sql_content` 用于无法通过 AST 表达的检查（如注释内容）。
#[derive(Debug, Clone)]
pub struct RuleContext {
    pub sql_content: String,
    pub file_path: String,
    pub file_name: String,
    pub script_type: String,
    pub line_count: usize,
    pub ast: SqlAst,
}

// ===== AST 包装类型 =====
// 所有类型必须满足 Rhai 的 `Clone + Send + Sync + 'static` 约束。
// 这里把 sqlparser 的 AST 转换为一组简化的、面向规则脚本的查询 API，
// 避免把 sqlparser 复杂的 enum 直接暴露给 Rhai（也不容易做版本兼容）。

/// 整个 SQL 脚本解析后的 AST。规则脚本通过 `context["ast"]` 获取。
#[derive(Debug, Clone, Default)]
pub struct SqlAst {
    pub statements: Vec<StmtInfo>,
    /// 如果解析失败，记录错误信息；规则可选择将其上报为违规。
    pub parse_error: Option<String>,
    /// 整个 SQL 文本中是否出现 FROM ... , ... 形式的隐式逗号 JOIN（全局）。
    /// 由 `detect_comma_join_in_sql` 扫描源文本得到，与 sqlparser 解析结果无关。
    pub has_comma_join_anywhere: bool,
    /// 源文本中的注释（行注释与块注释）。sqlparser 解析时丢弃注释，
    /// 这里通过独立的文本扫描得到，供规则做基于注释的判断。
    pub comments: Vec<CommentInfo>,
    /// **原始** SQL 文本（未经 `gaussdb_rewrite` 归一化）。
    ///
    /// 存在的意义：`parse_sql_to_ast_fb` 在 GaussDB 方言下会把文本重写为 PG 语法
    /// （如 `` `x` `` → `"x"`、`SYSDATE` → `CURRENT_TIMESTAMP`），重写后文本会丢失
    /// "原文是否用了反引号""原始长度"等信息。而"命名/引号/长度"类规则必须看原文，
    /// 因此这里额外保存一份原始文本，供 [`SqlAst::slice`] / [`SqlAst::stmt_text`] 使用。
    ///
    /// 内存代价为 O(文件大小)，与 `RuleContext.sql_content` 同量级。
    pub source: String,
}

/// 单个 SQL 语句的信息。
///
/// 注意：该结构体不 derive `Default`（与项目其他 Info 不同），因为构造时
/// 必须强制给定 `kind`、`line` 等字段，避免遗漏导致行号回填失效。
#[derive(Debug, Clone)]
pub struct StmtInfo {
    pub kind: String,
    pub line: i64,
    /// 语句结束行（含），用于增量校验时的行级范围判断。
    /// 近似值：取下一条语句/分号之前最后一行的行号。
    pub end_line: i64,
    pub column: i64,
    pub create_table: Option<CreateInfo>,
    pub drop_object: Option<DropInfo>,
    pub select: Option<SelectInfo>,
    pub insert: Option<InsertInfo>,
    pub update: Option<UpdateInfo>,
    pub delete: Option<DeleteInfo>,
    /// ALTER TABLE 详情（仅 `ALTER TABLE` 语句有值）。
    /// 用于检测通过 `ALTER TABLE ... ADD PRIMARY KEY` 补建主键，避免 DDL002 误报。
    pub alter_table: Option<AlterTableInfo>,
    /// TRUNCATE 详情。
    pub truncate: Option<TruncateInfo>,
    /// CREATE VIEW 详情。
    pub create_view: Option<ViewInfo>,
    /// CREATE INDEX 详情。
    pub create_index: Option<CreateIndexInfo>,
    /// 事务语句（START TRANSACTION / COMMIT / ROLLBACK）详情。
    pub transaction: Option<TransactionInfo>,
}

/// CREATE TABLE 语句信息。
#[derive(Debug, Clone, Default)]
pub struct CreateInfo {
    pub table_name: String,
    pub columns: Vec<ColumnInfo>,
    pub has_primary_key: bool,
    /// 主键列名列表（来自列级或表级 PRIMARY KEY 约束）。
    /// 供冗余索引等规则判断"某索引是否与主键重复"。
    pub primary_key_columns: Vec<String>,
    /// 表级 PRIMARY KEY 约束名（如 `CONSTRAINT pk_xxx PRIMARY KEY (...)` 中的 `pk_xxx`）。
    /// 列级 PK 或无名表级 PK 时为空字符串。
    pub primary_key_name: String,
    pub if_not_exists: bool,
    /// 是否为 `CREATE TABLE ... AS SELECT ...`（CTAS）。
    /// 此类语句无显式列定义，列由 SELECT 结果决定。
    pub is_create_as: bool,
    /// 表级 + 列级外键约束合并列表。
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// 表级 + 列级 CHECK 约束合并列表。
    pub checks: Vec<CheckInfo>,
    /// 表级 INDEX / KEY 约束列表（MySQL 风格）。
    pub indexes: Vec<IndexInfo>,
    /// 表级 UNIQUE 约束列表（不含主键）。
    pub uniques: Vec<UniqueInfo>,
    pub has_foreign_key: bool,
    pub has_check: bool,
    pub has_index: bool,
    pub has_unique: bool,
}

/// 单个列定义信息。
#[derive(Debug, Clone, Default)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_primary_key: bool,
    pub is_not_null: bool,
    pub is_unique: bool,
    pub default_value: Option<String>,
    pub is_auto_increment: bool,
    pub comment: Option<String>,
    pub has_check: bool,
    pub references_table: Option<String>,
    pub has_foreign_key: bool,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// DROP 语句信息。
#[derive(Debug, Clone, Default)]
pub struct DropInfo {
    pub object_type: String,
    pub name: String,
    pub if_exists: bool,
}

/// ALTER TABLE 语句信息。
///
/// 当前暴露主键相关意图，供 DDL002（`primary_key_required`）判断：
/// 表是否通过 `ALTER TABLE ... ADD PRIMARY KEY` 补建主键，或 `DROP PRIMARY KEY` 移除主键。
///
/// `operations` 字段补充每个 ALTER 操作的结构化信息（除主键外，还有 ADD/DROP COLUMN、
/// RENAME 等），供规则做更细致判断。
#[derive(Debug, Clone, Default)]
pub struct AlterTableInfo {
    pub table_name: String,
    /// 是否包含增加主键的操作：
    /// - `ADD PRIMARY KEY (...)` / `ADD CONSTRAINT x PRIMARY KEY (...)`（AddConstraint + PrimaryKey）
    /// - `ADD COLUMN col ... PRIMARY KEY`（列级主键）
    pub adds_primary_key: bool,
    /// 是否包含 `DROP PRIMARY KEY`（移除已有主键）
    pub drops_primary_key: bool,
    /// `ADD PRIMARY KEY (cols)` 中的列名列表，供冗余索引规则判断。
    pub added_primary_key_columns: Vec<String>,
    pub operations: Vec<AlterOpInfo>,
}

/// 单个 ALTER 操作的结构化信息。
/// `operation_type` 取值如 `ADD_COLUMN/DROP_COLUMN/ALTER_COLUMN/RENAME_COLUMN/
/// RENAME_TABLE/ADD_CONSTRAINT/DROP_CONSTRAINT/DROP_PRIMARY_KEY/DROP_FOREIGN_KEY/
/// DROP_UNIQUE/OTHER`。
#[derive(Debug, Clone, Default)]
pub struct AlterOpInfo {
    pub operation_type: String,
    pub column_name: String,
    pub table_name: String,
    pub constraint_name: String,
    pub detail: String,
}

/// JOIN 子句信息。
#[derive(Debug, Clone, Default)]
pub struct JoinInfo {
    pub table_name: String,
    pub join_type: String,
    pub has_condition: bool,
    /// JOIN 目标表的别名（如 `JOIN orders o` 中的 `o`）。
    pub alias: Option<String>,
    /// ON 子句文本（`JOIN ... ON expr` 中的 expr 文本），便于规则判断 ON 是否引用 JOIN 表。
    pub condition_text: Option<String>,
    /// JOIN 关键字是否显式指定了类型（LEFT / INNER / RIGHT / FULL / CROSS 等）。
    /// 仅裸 `JOIN`（sqlparser 的 `JoinOperator::Join`）为 false，供"必须显式指定 JOIN 类型"规则判断。
    pub explicit_join_type: bool,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// CTE（WITH 子句中的命名子查询）信息。
#[derive(Debug, Clone, Default)]
pub struct CteInfo {
    pub name: String,
    /// CTE 列数（alias.columns 的长度），0 表示未显式列出列。
    pub column_count: i64,
    pub is_recursive: bool,
}

/// 窗口函数信息。
#[derive(Debug, Clone, Default)]
pub struct WindowFuncInfo {
    pub function_name: String,
    pub has_partition_by: bool,
    pub has_order_by: bool,
    pub has_window_frame: bool,
}

/// `ORDER BY` 中的单个排序项。
///
/// sqlparser 0.60 的结构为
/// `OrderBy { kind: OrderByKind::Expressions(Vec<OrderByExpr>) }`，
/// 每项 `OrderByExpr { expr, options: OrderByOptions { asc, nulls_first } }`。
///
/// `asc` / `nulls_first` 是**三态**：`None` 表示原文未显式书写。
/// GaussDB 规范要求 `ORDER BY` 必须显式指定排序方式与 NULL 排序方式，
/// 因此"未显式指定"本身就是违规——规则需能区分 `None` 与 `Some(false)`。
/// Rhai 侧不暴露 `Option`，改用 `has_direction()` / `direction()` /
/// `has_nulls_spec()` / `nulls()` 这组方法表达三态。
#[derive(Debug, Clone, Default)]
pub struct OrderByItemInfo {
    /// 排序表达式文本（如 `created_at` / `t1.id` / `1`）。
    pub expr_text: String,
    /// `Some(true)` = ASC，`Some(false)` = DESC，`None` = 原文未指定。
    pub asc: Option<bool>,
    /// `Some(true)` = NULLS FIRST，`Some(false)` = NULLS LAST，`None` = 原文未指定。
    pub nulls_first: Option<bool>,
}

impl OrderByItemInfo {
    pub fn expr_text(&self) -> String {
        self.expr_text.clone()
    }
    /// 是否显式写了 `ASC` / `DESC`。
    pub fn has_direction(&self) -> bool {
        self.asc.is_some()
    }
    /// 方向文本：`ASC` / `DESC`；未显式指定时为空串。
    pub fn direction(&self) -> String {
        match self.asc {
            Some(true) => "ASC".to_string(),
            Some(false) => "DESC".to_string(),
            None => String::new(),
        }
    }
    /// 是否显式写为升序。
    pub fn is_asc(&self) -> bool {
        self.asc == Some(true)
    }
    /// 是否显式写为降序。
    pub fn is_desc(&self) -> bool {
        self.asc == Some(false)
    }
    /// 是否显式写了 `NULLS FIRST` / `NULLS LAST`。
    pub fn has_nulls_spec(&self) -> bool {
        self.nulls_first.is_some()
    }
    /// NULL 排序文本：`FIRST` / `LAST`；未显式指定时为空串。
    pub fn nulls(&self) -> String {
        match self.nulls_first {
            Some(true) => "FIRST".to_string(),
            Some(false) => "LAST".to_string(),
            None => String::new(),
        }
    }
    pub fn has_nulls_first(&self) -> bool {
        self.nulls_first == Some(true)
    }
    pub fn has_nulls_last(&self) -> bool {
        self.nulls_first == Some(false)
    }
}

/// 表达式顶层信息（不递归暴露子表达式，避免类型爆炸）。
#[derive(Debug, Clone, Default)]
pub struct ExprInfo {
    pub kind: String,
    pub text: String,
    pub function_name: String,
    pub is_literal: bool,
    pub is_column: bool,
    pub is_subquery: bool,
    pub operator: String,
    pub column_name: String,
    pub has_null_test: bool,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// SELECT 语句信息。
#[derive(Debug, Clone, Default)]
pub struct SelectInfo {
    pub has_wildcard: bool,
    /// 是否存在"裸" `SELECT *`（相对 `t.*` 这类带表限定的通配符）。
    /// 出现裸星号时无法判断各 JOIN 表是否被使用，规则应跳过。
    pub has_bare_wildcard: bool,
    /// 本查询层真实引用到的表限定符（大写、去重）。
    /// 来源：投影（含 `t.*` 限定通配符）、WHERE、GROUP BY、HAVING、QUALIFY、ORDER BY。
    /// 不含 JOIN 的 ON 条件——右表通常只在 ON 里出现，按 SQLFluff ST11 语义视为"未被使用"。
    pub referenced_qualifiers: Vec<String>,
    pub projection: Vec<String>,
    pub from_table: Option<String>,
    /// 主表别名（`FROM users u` 中的 `u`）。
    pub from_table_alias: Option<String>,
    pub joins: Vec<JoinInfo>,
    pub has_subquery_in_from: bool,
    pub from_subquery_has_alias: bool,
    pub has_unqualified_column: bool,
    pub union: bool,
    pub union_all: bool,
    /// 是否为 INTERSECT 集合运算（与 UNION 区分，供规则精确判断）。
    pub intersect: bool,
    /// 是否为 EXCEPT 集合运算。
    pub except: bool,
    // WHERE / GROUP BY / HAVING / QUALIFY
    pub has_where: bool,
    pub where_clause: Option<String>,
    pub has_group_by: bool,
    pub has_having: bool,
    pub has_qualify: bool,
    // ORDER BY / LIMIT / OFFSET / FETCH（在 Query 层，无论是否集合运算都要查）
    pub has_order_by: bool,
    /// `ORDER BY` 的逐项明细（含 ASC/DESC 与 NULLS FIRST/LAST 的显式性）。
    /// `OrderByKind::All`（DuckDB/ClickHouse 的 `ORDER BY ALL`）时为空列表，
    /// 此时以 `has_order_by` 为准。
    pub order_by_items: Vec<OrderByItemInfo>,
    pub has_limit: bool,
    pub has_offset: bool,
    pub has_fetch: bool,
    pub has_distinct: bool,
    // CTE / WITH
    pub ctes: Vec<CteInfo>,
    pub is_recursive: bool,
    pub has_cte: bool,
    // 子查询递归
    pub subqueries: Vec<SelectInfo>,
    pub has_subquery: bool,
    // 窗口函数
    pub has_window_function: bool,
    pub window_functions: Vec<WindowFuncInfo>,
    // 表达式树
    pub where_expr: Option<ExprInfo>,
    pub having_expr: Option<ExprInfo>,
    pub projection_exprs: Vec<ExprInfo>,
    // 隐式逗号 JOIN（FROM a, b）
    pub has_comma_join: bool,
    /// 本查询层 FROM 主表 + 各 JOIN 表 + 隐式逗号 JOIN 表的总表数。
    /// 供"单条查询 JOIN 表数上限"规则使用。
    pub table_count: i64,
}

/// INSERT 语句信息。
#[derive(Debug, Clone, Default)]
pub struct InsertInfo {
    pub table_name: String,
    pub columns: Vec<String>,
}

/// UPDATE 语句信息。`where_clause` 为 SQL 文本，便于规则做字符串匹配。
#[derive(Debug, Clone, Default)]
pub struct UpdateInfo {
    pub table_name: String,
    pub where_clause: Option<String>,
    /// `SET` 子句的目标列名（AST 可得：`Assignment.target`；元组赋值展开为各列）。
    /// 供 GaussDB「分布键值禁止 UPDATE」「禁止更新主键/唯一约束列」等规则使用。
    pub set_columns: Vec<String>,
    /// `SET` 子句整体文本（如 `a = 1, b = a + 1`），AST 可得。
    pub set_clause_text: String,
    /// `WHERE` 中是否含子查询（供「SUBSELECT 宜改 JOIN」提示）。
    pub has_subquery: bool,
    /// 是否含 `LIMIT`（sqlparser 0.60 `Update.limit`）。
    pub has_limit: bool,
    /// 原文中是否存在**顶层** `ORDER BY` 子句。
    ///
    /// ⚠️ 文本兜底：sqlparser 0.60 的 `Update` 结构**不含** `order_by` 字段
    /// （`UPDATE ... ORDER BY` 会直接解析失败、落入 `PARSE_ERROR`），
    /// 故该标志由语句原文的"顶层子句扫描"得出（跳过括号内子查询与字符串/注释）。
    pub has_order_by: bool,
    /// 原文中是否存在顶层 `GROUP BY` 子句（同 `has_order_by`，文本兜底）。
    pub has_group_by: bool,
}

/// DELETE 语句信息。`where_clause` 为 SQL 文本，便于规则做字符串匹配。
#[derive(Debug, Clone, Default)]
pub struct DeleteInfo {
    pub table_name: String,
    pub where_clause: Option<String>,
    /// `WHERE` 中是否含子查询。
    pub has_subquery: bool,
    /// 是否含 `LIMIT`（sqlparser 0.60 `Delete.limit`）。
    pub has_limit: bool,
    /// 是否含 `ORDER BY`（AST 可得：`Delete.order_by` 非空）。
    pub has_order_by: bool,
    /// 原文中是否存在顶层 `GROUP BY` 子句。
    ///
    /// ⚠️ 文本兜底：sqlparser 0.60 的 `Delete` 有 `order_by` 但**没有** `group_by`，
    /// 故该标志由语句原文的顶层子句扫描得出。
    pub has_group_by: bool,
}

/// TRUNCATE 语句信息。
#[derive(Debug, Clone, Default)]
pub struct TruncateInfo {
    pub table_name: String,
    pub has_table_keyword: bool,
}

/// CREATE VIEW 语句信息。
#[derive(Debug, Clone, Default)]
pub struct ViewInfo {
    pub name: String,
    pub materialized: bool,
    pub is_replace: bool,
    pub column_count: i64,
    /// 视图定义体（`AS SELECT ...`）的查询分析结果。
    ///
    /// 供 GaussDB「禁止在视图中排序」「禁止视图嵌套」「禁止对视图执行 SELECT 以外 DML」
    /// 等规则使用——这些规则必须能看进视图内部，仅凭"存在 CREATE VIEW"无法判定。
    /// 非 `CREATE VIEW` 语句取不到定义体时返回默认空对象。
    pub definition: SelectInfo,
    /// 是否为 `CREATE TEMP/TEMPORARY VIEW`。
    pub is_temporary: bool,
    /// 是否带 `IF NOT EXISTS`。
    pub if_not_exists: bool,
}

/// CREATE INDEX 语句信息。
#[derive(Debug, Clone, Default)]
pub struct CreateIndexInfo {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    /// 是否带 `CONCURRENTLY`（GaussDB 规范：有联机事务时必须加）。
    pub concurrently: bool,
    /// 是否带 `IF NOT EXISTS`。
    pub if_not_exists: bool,
    /// `USING <method>` 的索引方法（`btree` / `hash` / `gin` …），无则空串。
    pub using_method: String,
    /// `INCLUDE (col, ...)` 的附加列名列表。
    pub include_columns: Vec<String>,
}

/// 事务语句信息。`kind` 取值 `START_TRANSACTION/COMMIT/ROLLBACK`。
#[derive(Debug, Clone, Default)]
pub struct TransactionInfo {
    pub kind: String,
}

/// 外键约束信息。
#[derive(Debug, Clone, Default)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub foreign_table: String,
    pub referred_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// CHECK 约束信息。
#[derive(Debug, Clone, Default)]
pub struct CheckInfo {
    pub name: String,
    pub expr_text: String,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// 索引约束信息（MySQL 风格 KEY/INDEX）。
#[derive(Debug, Clone, Default)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// UNIQUE 约束信息（不含主键）。
#[derive(Debug, Clone, Default)]
pub struct UniqueInfo {
    pub name: String,
    pub columns: Vec<String>,
    /// 预留位置信息（sqlparser 0.45 AST 节点不带位置，故保持 None）。
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// 源文本中的注释信息。`kind` 取值 `LINE/BLOCK`。
#[derive(Debug, Clone, Default)]
pub struct CommentInfo {
    pub text: String,
    pub line: i64,
    pub kind: String,
}

// ===== Rhai 方法实现 =====
// 这些方法会被注册到 Rhai 引擎，规则脚本中可直接调用。

impl SqlAst {
    pub fn statements(&self) -> Vec<StmtInfo> {
        self.statements.clone()
    }
    pub fn has_parse_error(&self) -> bool {
        self.parse_error.is_some()
    }
    pub fn parse_error(&self) -> String {
        self.parse_error.clone().unwrap_or_default()
    }
    pub fn kinds(&self) -> Vec<String> {
        self.statements.iter().map(|s| s.kind.clone()).collect()
    }
    pub fn create_tables(&self) -> Vec<CreateInfo> {
        self.statements
            .iter()
            .filter_map(|s| s.create_table.clone())
            .collect()
    }
    pub fn drop_objects(&self) -> Vec<DropInfo> {
        self.statements
            .iter()
            .filter_map(|s| s.drop_object.clone())
            .collect()
    }
    pub fn selects(&self) -> Vec<SelectInfo> {
        self.statements
            .iter()
            .filter_map(|s| s.select.clone())
            .collect()
    }
    pub fn has_create_table(&self) -> bool {
        self.statements.iter().any(|s| s.kind == "CREATE_TABLE")
    }
    pub fn has_drop_table(&self) -> bool {
        self.statements.iter().any(|s| s.kind == "DROP_TABLE")
    }
    pub fn has_comma_join_anywhere(&self) -> bool {
        self.has_comma_join_anywhere
    }
    pub fn comments(&self) -> Vec<CommentInfo> {
        self.comments.clone()
    }

    /// 返回包含指定行号的语句的 (start_line, end_line)。
    /// 用于给 violation 自动回填 end_line（增量校验场景）。
    /// 若无语句覆盖该行，返回 None。
    pub fn statement_range_at(&self, line: i64) -> Option<(i64, i64)> {
        for s in &self.statements {
            if line >= s.line && line <= s.end_line {
                return Some((s.line, s.end_line));
            }
        }
        None
    }

    // ===== 原文切片 API（C4）=====
    // 目标：让规则能拿到"语句原文""任意行列范围的原文"，用于 AST 无法表达的检查
    // （是否用双引号定义对象名、字段名是否带引号、语句长度、原始书写形式等）。
    // 一律基于 `self.source`（**原始** SQL，而非 GaussDB 重写后的文本），
    // 因此反引号、原始长度、原始大小写都被保留。

    /// 原始 SQL 文本（未经方言归一化重写）。
    pub fn source(&self) -> String {
        self.source.clone()
    }

    /// 按行列范围切出原文片段。
    ///
    /// 坐标语义与 sqlparser 的 `Location` 一致：
    /// - 行号、列号均为 **1-based**，按 **字符**（而非字节）计数；
    /// - 返回 `[ (start_line, start_col), (end_line, end_col) )`，即**左闭右开**；
    /// - `start_col <= 1` 视为"从行首开始"；
    /// - `end_col <= 0` 视为"到行尾结束"；
    /// - 兼容 LF 与 CRLF（`\r` 作为行内普通字符计入列号，与 sqlparser 一致）。
    ///
    /// 越界时按"取到边界为止"处理，不 panic。
    pub fn slice(&self, start_line: i64, start_col: i64, end_line: i64, end_col: i64) -> String {
        if self.source.is_empty() {
            return String::new();
        }
        let l1 = start_line.max(1) as usize;
        let l2 = end_line.max(1) as usize;
        if l2 < l1 {
            return String::new();
        }
        let from_col = start_col.max(1) as usize - 1;
        let mut out = String::new();
        for (idx, line_text) in self.source.split('\n').enumerate() {
            let line_no = idx + 1;
            if line_no < l1 {
                continue;
            }
            if line_no > l2 {
                break;
            }
            if line_no > l1 {
                out.push('\n');
            }
            let chars: Vec<char> = line_text.chars().collect();
            let from = if line_no == l1 {
                from_col.min(chars.len())
            } else {
                0
            };
            let to = if line_no == l2 {
                if end_col <= 0 {
                    chars.len()
                } else {
                    (end_col as usize - 1).min(chars.len())
                }
            } else {
                chars.len()
            };
            if to > from {
                out.extend(chars[from..to].iter());
            }
        }
        out
    }

    /// 取某条语句的原文（按语句的 `line..=end_line` 整行区间切片后 trim）。
    ///
    /// 为什么用整行区间而不是精确列：`end_line` 由"下一条语句起始行的前一行"推得，
    /// 因此天然可能带上语句末尾的空白与同行尾随内容。trim 后覆盖绝大多数规则需求。
    /// 需要精确边界时请用 [`SqlAst::slice`] 配合语句的行列号。
    pub fn stmt_text(&self, stmt: &StmtInfo) -> String {
        self.stmt_text_by_range(stmt.line, stmt.end_line)
    }

    /// 取第 `index` 条语句（0-based）的原文；越界返回空串。
    pub fn stmt_text_at(&self, index: i64) -> String {
        if index < 0 {
            return String::new();
        }
        match self.statements.get(index as usize) {
            Some(s) => self.stmt_text(s),
            None => String::new(),
        }
    }

    /// 取覆盖指定行号（1-based）的那条语句的原文；无语句覆盖时返回空串。
    pub fn stmt_text_at_line(&self, line: i64) -> String {
        for s in &self.statements {
            if line >= s.line && line <= s.end_line {
                return self.stmt_text(s);
            }
        }
        String::new()
    }

    /// 取 `[start_line, end_line]`（1-based，闭区间）的整行文本并按 `\n` 连接，末尾 trim。
    pub fn line_range_text(&self, start_line: i64, end_line: i64) -> String {
        super::scanner::line_range_text(&self.source, start_line, end_line)
            .trim()
            .to_string()
    }

    fn stmt_text_by_range(&self, start_line: i64, end_line: i64) -> String {
        self.line_range_text(start_line, end_line)
    }
}

impl StmtInfo {
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
    pub fn line(&self) -> i64 {
        self.line
    }
    /// 语句结束行（含）。规则脚本可用于判断语句边界。
    pub fn end_line(&self) -> i64 {
        self.end_line
    }
    /// 返回 (start_line, end_line)，便于规则脚本一次性获取语句范围。
    pub fn line_range(&self) -> (i64, i64) {
        (self.line, self.end_line)
    }
    pub fn column(&self) -> i64 {
        self.column
    }
    pub fn has_create_table(&self) -> bool {
        self.create_table.is_some()
    }
    pub fn has_drop_object(&self) -> bool {
        self.drop_object.is_some()
    }
    pub fn has_select(&self) -> bool {
        self.select.is_some()
    }
    pub fn has_insert(&self) -> bool {
        self.insert.is_some()
    }
    pub fn has_update(&self) -> bool {
        self.update.is_some()
    }
    pub fn has_delete(&self) -> bool {
        self.delete.is_some()
    }
    pub fn has_alter_table(&self) -> bool {
        self.alter_table.is_some()
    }
    pub fn has_truncate(&self) -> bool {
        self.truncate.is_some()
    }
    pub fn has_create_view(&self) -> bool {
        self.create_view.is_some()
    }
    pub fn has_create_index(&self) -> bool {
        self.create_index.is_some()
    }
    pub fn has_transaction(&self) -> bool {
        self.transaction.is_some()
    }
    pub fn create_table(&self) -> CreateInfo {
        self.create_table.clone().unwrap_or_default()
    }
    pub fn drop_object(&self) -> DropInfo {
        self.drop_object.clone().unwrap_or_default()
    }
    pub fn select(&self) -> SelectInfo {
        self.select.clone().unwrap_or_default()
    }
    pub fn insert(&self) -> InsertInfo {
        self.insert.clone().unwrap_or_default()
    }
    pub fn update(&self) -> UpdateInfo {
        self.update.clone().unwrap_or_default()
    }
    pub fn delete(&self) -> DeleteInfo {
        self.delete.clone().unwrap_or_default()
    }
    pub fn alter_table(&self) -> AlterTableInfo {
        self.alter_table.clone().unwrap_or_default()
    }
    pub fn truncate(&self) -> TruncateInfo {
        self.truncate.clone().unwrap_or_default()
    }
    pub fn create_view(&self) -> ViewInfo {
        self.create_view.clone().unwrap_or_default()
    }
    pub fn create_index(&self) -> CreateIndexInfo {
        self.create_index.clone().unwrap_or_default()
    }
    pub fn transaction(&self) -> TransactionInfo {
        self.transaction.clone().unwrap_or_default()
    }
}

impl CreateInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn columns(&self) -> Vec<ColumnInfo> {
        self.columns.clone()
    }
    pub fn has_primary_key(&self) -> bool {
        self.has_primary_key
    }
    /// 主键列名列表（列级或表级 PRIMARY KEY 约束的列）。
    pub fn primary_key_columns(&self) -> Vec<String> {
        self.primary_key_columns.clone()
    }
    /// 表级 PRIMARY KEY 约束名，无名时返回空字符串。
    pub fn primary_key_name(&self) -> String {
        self.primary_key_name.clone()
    }
    pub fn if_not_exists(&self) -> bool {
        self.if_not_exists
    }
    /// 是否为 `CREATE TABLE ... AS SELECT ...`（CTAS）。
    pub fn is_create_as(&self) -> bool {
        self.is_create_as
    }
    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }
    pub fn foreign_keys(&self) -> Vec<ForeignKeyInfo> {
        self.foreign_keys.clone()
    }
    pub fn checks(&self) -> Vec<CheckInfo> {
        self.checks.clone()
    }
    pub fn indexes(&self) -> Vec<IndexInfo> {
        self.indexes.clone()
    }
    pub fn uniques(&self) -> Vec<UniqueInfo> {
        self.uniques.clone()
    }
    pub fn has_foreign_key(&self) -> bool {
        self.has_foreign_key
    }
    pub fn has_check(&self) -> bool {
        self.has_check
    }
    pub fn has_index(&self) -> bool {
        self.has_index
    }
    pub fn has_unique(&self) -> bool {
        self.has_unique
    }
}

impl ColumnInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn data_type(&self) -> String {
        self.data_type.clone()
    }
    pub fn is_primary_key(&self) -> bool {
        self.is_primary_key
    }
    pub fn is_not_null(&self) -> bool {
        self.is_not_null
    }
    pub fn is_unique(&self) -> bool {
        self.is_unique
    }
    pub fn default_value(&self) -> String {
        self.default_value.clone().unwrap_or_default()
    }
    pub fn has_default_value(&self) -> bool {
        self.default_value.is_some()
    }
    pub fn is_auto_increment(&self) -> bool {
        self.is_auto_increment
    }
    pub fn comment(&self) -> String {
        self.comment.clone().unwrap_or_default()
    }
    pub fn has_comment(&self) -> bool {
        self.comment.is_some()
    }
    pub fn has_check(&self) -> bool {
        self.has_check
    }
    pub fn references_table(&self) -> String {
        self.references_table.clone().unwrap_or_default()
    }
    pub fn has_foreign_key(&self) -> bool {
        self.has_foreign_key
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl DropInfo {
    pub fn object_type(&self) -> String {
        self.object_type.clone()
    }
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn if_exists(&self) -> bool {
        self.if_exists
    }
}

impl SelectInfo {
    pub fn has_wildcard(&self) -> bool {
        self.has_wildcard
    }
    pub fn projection(&self) -> Vec<String> {
        self.projection.clone()
    }
    /// 是否存在裸 `SELECT *`（`t.*` 这类限定通配符不算）。
    pub fn has_bare_wildcard(&self) -> bool {
        self.has_bare_wildcard
    }
    /// 本查询层引用到的表限定符（大写、去重）。
    pub fn referenced_qualifiers(&self) -> Vec<String> {
        self.referenced_qualifiers.clone()
    }
    /// 判断某个别名/表名是否在本查询体中被真实引用（大小写不敏感）。
    /// 用于 no_unused_join 判断 JOIN 的表是否出现在 SELECT/WHERE/GROUP BY/HAVING/ORDER BY 中。
    pub fn is_qualifier_referenced(&self, name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let upper = name.to_uppercase();
        self.referenced_qualifiers.iter().any(|q| q == &upper)
    }
    pub fn has_from_table(&self) -> bool {
        self.from_table.is_some()
    }
    pub fn from_table(&self) -> String {
        self.from_table.clone().unwrap_or_default()
    }
    pub fn from_table_alias(&self) -> String {
        self.from_table_alias.clone().unwrap_or_default()
    }
    pub fn has_from_table_alias(&self) -> bool {
        self.from_table_alias.is_some()
    }
    pub fn joins(&self) -> Vec<JoinInfo> {
        self.joins.clone()
    }
    pub fn has_joins(&self) -> bool {
        !self.joins.is_empty()
    }
    pub fn has_cross_join(&self) -> bool {
        self.joins.iter().any(|j| j.join_type == "CROSS")
    }
    pub fn has_join_without_condition(&self) -> bool {
        self.joins.iter().any(|j| !j.has_condition)
    }
    pub fn has_subquery_in_from(&self) -> bool {
        self.has_subquery_in_from
    }
    pub fn from_subquery_has_alias(&self) -> bool {
        self.from_subquery_has_alias
    }
    pub fn has_unqualified_column(&self) -> bool {
        self.has_unqualified_column
    }
    pub fn is_union(&self) -> bool {
        self.union
    }
    pub fn is_union_all(&self) -> bool {
        self.union_all
    }
    pub fn is_intersect(&self) -> bool {
        self.intersect
    }
    pub fn is_except(&self) -> bool {
        self.except
    }
    pub fn has_where(&self) -> bool {
        self.has_where
    }
    pub fn where_clause(&self) -> String {
        self.where_clause.clone().unwrap_or_default()
    }
    pub fn has_group_by(&self) -> bool {
        self.has_group_by
    }
    pub fn has_having(&self) -> bool {
        self.has_having
    }
    pub fn has_qualify(&self) -> bool {
        self.has_qualify
    }
    pub fn has_order_by(&self) -> bool {
        self.has_order_by
    }
    /// `ORDER BY` 的逐项明细（含 ASC/DESC 与 NULLS FIRST/LAST 的显式性）。
    pub fn order_by_items(&self) -> Vec<OrderByItemInfo> {
        self.order_by_items.clone()
    }
    /// 是否存在"未显式指定 ASC/DESC"的排序项。
    /// 对应 GaussDB 规范「ORDER BY 必须显式指定排序方式」。
    /// 无 ORDER BY 时返回 false（本方法不负责"必须有 ORDER BY"）。
    pub fn has_order_by_without_direction(&self) -> bool {
        self.order_by_items.iter().any(|o| !o.has_direction())
    }
    /// 是否存在"未显式指定 NULLS FIRST/LAST"的排序项。
    /// 对应 GaussDB 规范「ORDER BY 必须显式指定 NULL 的排序方式」。
    pub fn has_order_by_without_nulls_spec(&self) -> bool {
        self.order_by_items.iter().any(|o| !o.has_nulls_spec())
    }
    /// `ORDER BY` 整体文本（各排序项以 `, ` 连接），无则空串。
    pub fn order_by_text(&self) -> String {
        self.order_by_items
            .iter()
            .map(|o| {
                let mut s = o.expr_text.clone();
                if !o.direction().is_empty() {
                    s.push(' ');
                    s.push_str(&o.direction());
                }
                if !o.nulls().is_empty() {
                    s.push_str(" NULLS ");
                    s.push_str(&o.nulls());
                }
                s
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub fn has_limit(&self) -> bool {
        self.has_limit
    }
    pub fn has_offset(&self) -> bool {
        self.has_offset
    }
    pub fn has_fetch(&self) -> bool {
        self.has_fetch
    }
    pub fn has_distinct(&self) -> bool {
        self.has_distinct
    }
    pub fn ctes(&self) -> Vec<CteInfo> {
        self.ctes.clone()
    }
    pub fn has_cte(&self) -> bool {
        self.has_cte
    }
    pub fn is_recursive(&self) -> bool {
        self.is_recursive
    }
    pub fn subqueries(&self) -> Vec<SelectInfo> {
        self.subqueries.clone()
    }
    pub fn has_subquery(&self) -> bool {
        self.has_subquery
    }
    pub fn has_window_function(&self) -> bool {
        self.has_window_function
    }
    pub fn window_functions(&self) -> Vec<WindowFuncInfo> {
        self.window_functions.clone()
    }
    pub fn where_expr(&self) -> ExprInfo {
        self.where_expr.clone().unwrap_or_default()
    }
    pub fn has_where_expr(&self) -> bool {
        self.where_expr.is_some()
    }
    pub fn having_expr(&self) -> ExprInfo {
        self.having_expr.clone().unwrap_or_default()
    }
    pub fn has_having_expr(&self) -> bool {
        self.having_expr.is_some()
    }
    pub fn projection_exprs(&self) -> Vec<ExprInfo> {
        self.projection_exprs.clone()
    }
    pub fn has_comma_join(&self) -> bool {
        self.has_comma_join
    }
    /// 本查询层 FROM + JOIN（含隐式逗号 JOIN 的表）的总表数。
    pub fn table_count(&self) -> i64 {
        self.table_count
    }
}

impl JoinInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    /// 去掉 schema 限定的表名（`ofsm.cdeorg` → `cdeorg`）。
    /// 供规则在"列引用不带 schema"（`cdeorg.col`）时也能匹配到该表。
    pub fn table_name_leaf(&self) -> String {
        self.table_name
            .rsplit('.')
            .next()
            .unwrap_or(&self.table_name)
            .to_string()
    }
    pub fn join_type(&self) -> String {
        self.join_type.clone()
    }
    pub fn has_condition(&self) -> bool {
        self.has_condition
    }
    pub fn alias(&self) -> String {
        self.alias.clone().unwrap_or_default()
    }
    pub fn has_alias(&self) -> bool {
        self.alias.is_some()
    }
    pub fn condition_text(&self) -> String {
        self.condition_text.clone().unwrap_or_default()
    }
    pub fn has_condition_text(&self) -> bool {
        self.condition_text.is_some()
    }
    /// JOIN 关键字是否显式指定了类型（LEFT JOIN / INNER JOIN / CROSS JOIN 等）。
    /// 裸 `JOIN` 返回 false。
    pub fn has_explicit_join_type(&self) -> bool {
        self.explicit_join_type
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl InsertInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }
    pub fn has_columns(&self) -> bool {
        !self.columns.is_empty()
    }
}

impl UpdateInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn has_where(&self) -> bool {
        self.where_clause.is_some()
    }
    pub fn where_clause(&self) -> String {
        self.where_clause.clone().unwrap_or_default()
    }
    /// `SET` 子句的目标列名列表。
    pub fn set_columns(&self) -> Vec<String> {
        self.set_columns.clone()
    }
    /// 是否存在某个目标列被赋值（大小写不敏感，自动去掉 schema 前缀与引号）。
    ///
    /// 供「分布键值禁止 UPDATE」「禁止更新主键列」类规则使用：
    /// `u.sets_column("id")`。
    pub fn sets_column(&self, name: &str) -> bool {
        let target = crate::rule::engine::idents::normalize_ident(name);
        if target.is_empty() {
            return false;
        }
        self.set_columns
            .iter()
            .any(|c| crate::rule::engine::idents::normalize_ident(c) == target)
    }
    /// `SET` 子句整体文本。
    pub fn set_clause_text(&self) -> String {
        self.set_clause_text.clone()
    }
    /// `WHERE` 中是否含子查询。
    pub fn has_subquery(&self) -> bool {
        self.has_subquery
    }
    pub fn has_limit(&self) -> bool {
        self.has_limit
    }
    /// 是否含顶层 `ORDER BY`（文本兜底，见字段注释）。
    pub fn has_order_by(&self) -> bool {
        self.has_order_by
    }
    /// 是否含顶层 `GROUP BY`（文本兜底，见字段注释）。
    pub fn has_group_by(&self) -> bool {
        self.has_group_by
    }
}

impl DeleteInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn has_where(&self) -> bool {
        self.where_clause.is_some()
    }
    pub fn where_clause(&self) -> String {
        self.where_clause.clone().unwrap_or_default()
    }
    /// `WHERE` 中是否含子查询。
    pub fn has_subquery(&self) -> bool {
        self.has_subquery
    }
    pub fn has_limit(&self) -> bool {
        self.has_limit
    }
    pub fn has_order_by(&self) -> bool {
        self.has_order_by
    }
    /// 是否含顶层 `GROUP BY`（文本兜底，见字段注释）。
    pub fn has_group_by(&self) -> bool {
        self.has_group_by
    }
}

impl AlterTableInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn adds_primary_key(&self) -> bool {
        self.adds_primary_key
    }
    pub fn drops_primary_key(&self) -> bool {
        self.drops_primary_key
    }
    /// `ADD PRIMARY KEY (cols)` 中的列名列表。
    pub fn added_primary_key_columns(&self) -> Vec<String> {
        self.added_primary_key_columns.clone()
    }
    pub fn operations(&self) -> Vec<AlterOpInfo> {
        self.operations.clone()
    }
}

impl AlterOpInfo {
    pub fn operation_type(&self) -> String {
        self.operation_type.clone()
    }
    pub fn column_name(&self) -> String {
        self.column_name.clone()
    }
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn constraint_name(&self) -> String {
        self.constraint_name.clone()
    }
    pub fn detail(&self) -> String {
        self.detail.clone()
    }
}

impl TruncateInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn has_table_keyword(&self) -> bool {
        self.has_table_keyword
    }
}

impl ViewInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn materialized(&self) -> bool {
        self.materialized
    }
    pub fn is_replace(&self) -> bool {
        self.is_replace
    }
    pub fn column_count(&self) -> i64 {
        self.column_count
    }
    /// 视图定义体（`AS SELECT ...`）的查询分析结果。
    /// 非 `CREATE VIEW` 语句返回默认空对象（此时 `definition().has_from_table()` 为 false）。
    pub fn definition(&self) -> SelectInfo {
        self.definition.clone()
    }
    /// 定义体是否可用（即确实解析出了 `AS SELECT ...`）。
    pub fn has_definition(&self) -> bool {
        !self.definition.projection.is_empty()
            || self.definition.has_from_table()
            || self.definition.has_order_by
    }
    pub fn is_temporary(&self) -> bool {
        self.is_temporary
    }
    pub fn if_not_exists(&self) -> bool {
        self.if_not_exists
    }
}

impl CreateIndexInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn table_name(&self) -> String {
        self.table_name.clone()
    }
    pub fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }
    pub fn is_unique(&self) -> bool {
        self.is_unique
    }
    /// 是否带 `CONCURRENTLY`（GaussDB 规范：有联机事务时必须加）。
    pub fn concurrently(&self) -> bool {
        self.concurrently
    }
    pub fn if_not_exists(&self) -> bool {
        self.if_not_exists
    }
    /// `USING <method>` 的索引方法（`btree` / `hash` / `gin` …），无则空串。
    pub fn using_method(&self) -> String {
        self.using_method.clone()
    }
    /// `INCLUDE (col, ...)` 的附加列名列表。
    pub fn include_columns(&self) -> Vec<String> {
        self.include_columns.clone()
    }
}

impl TransactionInfo {
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
}

impl CteInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn column_count(&self) -> i64 {
        self.column_count
    }
    pub fn is_recursive(&self) -> bool {
        self.is_recursive
    }
}

impl WindowFuncInfo {
    pub fn function_name(&self) -> String {
        self.function_name.clone()
    }
    pub fn has_partition_by(&self) -> bool {
        self.has_partition_by
    }
    pub fn has_order_by(&self) -> bool {
        self.has_order_by
    }
    pub fn has_window_frame(&self) -> bool {
        self.has_window_frame
    }
}

impl ForeignKeyInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }
    pub fn foreign_table(&self) -> String {
        self.foreign_table.clone()
    }
    pub fn referred_columns(&self) -> Vec<String> {
        self.referred_columns.clone()
    }
    pub fn on_delete(&self) -> String {
        self.on_delete.clone()
    }
    pub fn on_update(&self) -> String {
        self.on_update.clone()
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl CheckInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn expr_text(&self) -> String {
        self.expr_text.clone()
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl IndexInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }
    pub fn is_unique(&self) -> bool {
        self.is_unique
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl UniqueInfo {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl ExprInfo {
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
    pub fn text(&self) -> String {
        self.text.clone()
    }
    pub fn function_name(&self) -> String {
        self.function_name.clone()
    }
    pub fn is_literal(&self) -> bool {
        self.is_literal
    }
    pub fn is_column(&self) -> bool {
        self.is_column
    }
    pub fn is_subquery(&self) -> bool {
        self.is_subquery
    }
    pub fn operator(&self) -> String {
        self.operator.clone()
    }
    pub fn column_name(&self) -> String {
        self.column_name.clone()
    }
    pub fn has_null_test(&self) -> bool {
        self.has_null_test
    }
    pub fn line(&self) -> i64 {
        self.line.unwrap_or(0)
    }
    pub fn column(&self) -> i64 {
        self.column.unwrap_or(0)
    }
}

impl CommentInfo {
    pub fn text(&self) -> String {
        self.text.clone()
    }
    pub fn line(&self) -> i64 {
        self.line
    }
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== matches_any =====

    #[test]
    fn test_matches_any_exact() {
        let pats = vec!["DDL001".to_string(), "DML002".to_string()];
        assert!(matches_any(&pats, "DDL001"));
        assert!(matches_any(&pats, "DML002"));
        assert!(!matches_any(&pats, "DDL002"));
        assert!(
            !matches_any(&pats, "ddl001"),
            "matches_any is case-sensitive"
        );
    }

    #[test]
    fn test_matches_any_wildcard_suffix() {
        let pats = vec!["DDL*".to_string()];
        assert!(matches_any(&pats, "DDL001"));
        assert!(matches_any(&pats, "DDLXXX"));
        assert!(!matches_any(&pats, "DML001"));
        // "DDL*" 的 prefix 是 "DDL"，"DDL".starts_with("DDL") == true，所以 "DDL" 也命中
        assert!(
            matches_any(&pats, "DDL"),
            "prefix wildcard matches the prefix itself"
        );
    }

    #[test]
    fn test_matches_any_empty() {
        let pats: Vec<String> = vec![];
        assert!(!matches_any(&pats, "anything"));
        // value 为空字符串
        let pats2 = vec!["DDL001".to_string()];
        assert!(!matches_any(&pats2, ""));
    }

    #[test]
    fn test_matches_any_wildcard_takes_priority() {
        // 模式同时含精确与通配：通配命中即可
        let pats = vec!["DDL001".to_string(), "DDL*".to_string()];
        assert!(matches_any(&pats, "DDL002"));
        assert!(matches_any(&pats, "DDL001"));
    }

    #[test]
    fn test_matches_any_star_only() {
        // 模式为 "*" 时 prefix="" 匹配任意值
        let pats = vec!["*".to_string()];
        assert!(matches_any(&pats, "anything"));
        assert!(matches_any(&pats, ""));
    }

    // ===== namespace 规范 id 匹配（M2，见 docs/rule-pack-design.md §5.3） =====

    #[test]
    fn test_matches_rule_id_namespace_semantics() {
        // `ns:id` → namespace 必须相同，再精确匹配短 id
        assert!(matches_rule_id("gaussdb:GDB001", "gaussdb:GDB001"));
        assert!(!matches_rule_id("gaussdb:GDB001", "other:GDB001"));
        assert!(!matches_rule_id("gaussdb:GDB001", "GDB001"));

        // 裸 id → 忽略 namespace，匹配短 id
        assert!(matches_rule_id("GDB001", "gaussdb:GDB001"));
        assert!(matches_rule_id("GDB001", "GDB001"));
        assert!(!matches_rule_id("GDB001", "gaussdb:GDB002"));

        // `ns:*` → 该 namespace 下全部规则
        assert!(matches_rule_id("gaussdb:*", "gaussdb:ANY"));
        assert!(!matches_rule_id("gaussdb:*", "other:ANY"));
        assert!(!matches_rule_id("gaussdb:*", "ANY"));

        // 裸前缀通配 → 匹配短 id 前缀（保持 `--rules DDL*` 直觉）
        assert!(matches_rule_id("GDB*", "gaussdb:GDB001"));
        assert!(matches_rule_id("GDB*", "GDB001"));
        assert!(!matches_rule_id("GDB*", "gaussdb:OTHER"));
    }

    #[test]
    fn test_rule_filter_namespaced_ids() {
        let f = RuleFilter::from_cli(&Some("gaussdb:*".to_string()), &None, &None, &None);
        assert!(f.matches_id_group("gaussdb:GDB001", None));
        assert!(!f.matches_id_group("other:GDB001", None));
        assert!(!f.matches_id_group("DDL001", None));

        // 裸 id 白名单跨 namespace 命中；黑名单优先
        let f = RuleFilter::from_cli(
            &Some("GDB001".to_string()),
            &None,
            &Some("gaussdb:GDB001".to_string()),
            &None,
        );
        assert!(!f.matches_id_group("gaussdb:GDB001", None));
        assert!(f.matches_id_group("other:GDB001", None));
        assert!(f.matches_id_group("GDB001", None));
    }

    // ===== RuleFilter =====

    #[test]
    fn test_rule_filter_from_cli_empty() {
        let f = RuleFilter::from_cli(&None, &None, &None, &None);
        assert!(f.is_empty());
        assert!(f.include_rules.is_empty());
        assert!(f.include_groups.is_empty());
    }

    #[test]
    fn test_rule_filter_from_cli_whitespace_handling() {
        // 空字符串、纯空白、前后逗号都应被清理
        let f = RuleFilter::from_cli(&Some("  ".to_string()), &None, &None, &None);
        assert!(f.is_empty());

        let f = RuleFilter::from_cli(&Some(",DDL001,,".to_string()), &None, &None, &None);
        assert_eq!(f.include_rules, vec!["DDL001".to_string()]);
    }

    #[test]
    fn test_rule_filter_from_cli_trims_items() {
        let f = RuleFilter::from_cli(
            &Some(" DDL001 , DML002 ".to_string()),
            &Some(" ddl-safety ".to_string()),
            &None,
            &None,
        );
        assert_eq!(
            f.include_rules,
            vec!["DDL001".to_string(), "DML002".to_string()]
        );
        assert_eq!(f.include_groups, vec!["ddl-safety".to_string()]);
    }

    #[test]
    fn test_rule_filter_empty_allows_all() {
        let f = RuleFilter::default();
        assert!(f.matches_id_group("DDL001", Some("ddl-safety")));
        assert!(f.matches_id_group("XYZ999", None));
    }

    #[test]
    fn test_rule_filter_exclude_takes_priority() {
        let f = RuleFilter::from_cli(
            &Some("DDL*".to_string()),
            &None,
            &Some("DDL001".to_string()),
            &None,
        );
        // 白名单匹配 DDL*，但黑名单显式排除 DDL001
        assert!(
            !f.matches_id_group("DDL001", None),
            "blacklist must beat whitelist"
        );
        assert!(f.matches_id_group("DDL002", None));
    }

    #[test]
    fn test_rule_filter_exclude_groups() {
        let f = RuleFilter::from_cli(&None, &None, &None, &Some("experimental*".to_string()));
        assert!(!f.matches_id_group("XYZ001", Some("experimental-rules")));
        assert!(f.matches_id_group("XYZ002", Some("stable")));
    }

    #[test]
    fn test_rule_filter_include_groups_excludes_no_group() {
        // 白名单 include_groups 非空时，group=None 的规则应被排除
        let f = RuleFilter::from_cli(&None, &Some("ddl-safety".to_string()), &None, &None);
        assert!(f.matches_id_group("DDL001", Some("ddl-safety")));
        assert!(
            !f.matches_id_group("DDL002", None),
            "no-group rule must be excluded when include_groups is non-empty"
        );
    }

    #[test]
    fn test_rule_filter_both_whitelists_non_empty() {
        // 两个白名单都非空时，规则必须同时命中 id 和 group
        let f = RuleFilter::from_cli(
            &Some("DDL*".to_string()),
            &Some("ddl-safety".to_string()),
            &None,
            &None,
        );
        assert!(f.matches_id_group("DDL001", Some("ddl-safety")));
        // id 命中但 group 不命中
        assert!(!f.matches_id_group("DDL001", Some("other")));
        // group 命中但 id 不命中
        assert!(!f.matches_id_group("XYZ001", Some("ddl-safety")));
    }

    // ===== SqlAst::statement_range_at =====

    #[test]
    fn test_statement_range_at_boundary() {
        // 构造两条语句：(1,3) 和 (5,7)
        let mut ast = SqlAst {
            statements: Vec::new(),
            parse_error: None,
            has_comma_join_anywhere: false,
            comments: Vec::new(),
            source: String::new(),
        };
        ast.statements.push(StmtInfo {
            kind: "OTHER".to_string(),
            line: 1,
            end_line: 3,
            column: 1,
            create_table: None,
            drop_object: None,
            select: None,
            insert: None,
            update: None,
            delete: None,
            alter_table: None,
            truncate: None,
            create_view: None,
            create_index: None,
            transaction: None,
        });
        ast.statements.push(StmtInfo {
            kind: "OTHER".to_string(),
            line: 5,
            end_line: 7,
            column: 1,
            create_table: None,
            drop_object: None,
            select: None,
            insert: None,
            update: None,
            delete: None,
            alter_table: None,
            truncate: None,
            create_view: None,
            create_index: None,
            transaction: None,
        });

        // 边界：起点、终点
        assert_eq!(ast.statement_range_at(1), Some((1, 3)));
        assert_eq!(ast.statement_range_at(3), Some((1, 3)));
        assert_eq!(ast.statement_range_at(5), Some((5, 7)));
        assert_eq!(ast.statement_range_at(7), Some((5, 7)));
        // 中间
        assert_eq!(ast.statement_range_at(2), Some((1, 3)));
        assert_eq!(ast.statement_range_at(6), Some((5, 7)));
        // 间隙：第 4 行无语句覆盖
        assert_eq!(ast.statement_range_at(4), None);
        // 范围外
        assert_eq!(ast.statement_range_at(0), None);
        assert_eq!(ast.statement_range_at(100), None);
    }
}
