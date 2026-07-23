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
        if matches_any(&self.exclude_rules, id) {
            return false;
        }
        if let Some(g) = group {
            if matches_any(&self.exclude_groups, g) {
                return false;
            }
        }

        // 2. 白名单为空表示不限制
        let in_rules = self.include_rules.is_empty() || matches_any(&self.include_rules, id);
        let in_groups = self.include_groups.is_empty()
            || group
                .map(|g| matches_any(&self.include_groups, g))
                .unwrap_or(false);

        in_rules && in_groups
    }
}

/// 判断 `value` 是否匹配 patterns 中的任一项。支持前缀通配 `prefix*`。
pub fn matches_any(patterns: &[String], value: &str) -> bool {
    for pat in patterns {
        if let Some(prefix) = pat.strip_suffix('*') {
            if value.starts_with(prefix) {
                return true;
            }
        } else if pat == value {
            return true;
        }
    }
    false
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
    pub if_not_exists: bool,
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
}

/// DELETE 语句信息。`where_clause` 为 SQL 文本，便于规则做字符串匹配。
#[derive(Debug, Clone, Default)]
pub struct DeleteInfo {
    pub table_name: String,
    pub where_clause: Option<String>,
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
}

/// CREATE INDEX 语句信息。
#[derive(Debug, Clone, Default)]
pub struct CreateIndexInfo {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
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
        self.statements
            .iter()
            .any(|s| s.kind == "DROP_TABLE")
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
    pub fn if_not_exists(&self) -> bool {
        self.if_not_exists
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
}

impl JoinInfo {
    pub fn table_name(&self) -> String {
        self.table_name.clone()
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
