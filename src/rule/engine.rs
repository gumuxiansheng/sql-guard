use std::fs;
use std::path::Path;

use rhai::{Array, Dynamic, Engine, Map, Scope};
use sqlparser::ast::{
    AlterTableOperation, ColumnOption, Expr, FromTable, GroupByExpr, JoinConstraint, JoinOperator,
    OrderByExpr, Query, SelectItem, SetExpr, SetOperator, SetQuantifier, Statement, TableConstraint,
    TableFactor, TableWithJoins, WindowType,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use crate::config::{Config, RuleConfig};
use crate::error::{SqlGuardError, Violation};

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
fn matches_any(patterns: &[String], value: &str) -> bool {
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

// ===== SQL 解析 =====

/// 把 SQL 文本解析为 AST，每条语句记录其在源文件中的行号/列号。
/// 解析失败时返回带 `parse_error` 的空 AST，不影响后续规则运行。
///
/// 利用 `Parser::peek_token()` 在解析每条语句前读取起始位置，
/// 避免 sqlparser 的 `Statement` 本身不携带位置信息的限制。
fn parse_sql_to_ast(sql: &str) -> SqlAst {
    let dialect = GenericDialect {};

    let mut parser = match Parser::new(&dialect).try_with_sql(sql) {
        Ok(p) => p,
        Err(e) => {
            return SqlAst {
                statements: Vec::new(),
                parse_error: Some(format!("SQL tokenize error: {}", e)),
                has_comma_join_anywhere: false,
                comments: Vec::new(),
            };
        }
    };

    let total_lines = sql.lines().count() as i64;
    let mut statements = Vec::new();

    loop {
        // 跳过语句间的空分号（`;` 是分隔符，不属于前一条语句）
        while parser.peek_token().token == Token::SemiColon {
            parser.next_token();
        }

        let peek = parser.peek_token();
        if peek.token == Token::EOF {
            break;
        }
        let line = peek.location.line as i64;
        let column = peek.location.column as i64;

        match parser.parse_statement() {
            Ok(stmt) => {
                // 计算 end_line：解析完语句后，下一个 token 的位置是该语句之后
                // 的第一个 token（通常是 `;` 或 EOF 或下一条语句的首 token）。
                // - 若下一 token 在新行，说明本语句末行 = next_line - 1
                // - 若下一 token 在同一行（如 `;` 或 `SELECT 1; SELECT 2;`），
                //   本语句末行就是当前 line
                // - 若 EOF，用文件总行数
                let next_tok = parser.peek_token();
                let end_line = if next_tok.token == Token::EOF {
                    total_lines.max(line)
                } else {
                    let next_line = next_tok.location.line as i64;
                    // 下一个 token 还在本语句内（同行分号等）→ end = line
                    // 下一个 token 在后续行 → end = next_line - 1
                    if next_line > line {
                        next_line - 1
                    } else {
                        line
                    }
                };
                statements.push(convert_statement(&stmt, line, column, end_line));
            }
            Err(_e) => {
                // 单条语句解析失败时记录错误位置，然后跳过到下一个分号
                let next_tok = parser.peek_token();
                let end_line = if next_tok.token == Token::EOF {
                    total_lines.max(line)
                } else {
                    let next_line = next_tok.location.line as i64;
                    if next_line > line { next_line - 1 } else { line }
                };
                statements.push(StmtInfo {
                    kind: "PARSE_ERROR".to_string(),
                    line,
                    end_line,
                    column,
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
                loop {
                    let t = parser.peek_token();
                    if t.token == Token::SemiColon || t.token == Token::EOF {
                        if t.token == Token::SemiColon {
                            parser.next_token();
                        }
                        break;
                    }
                    parser.next_token();
                }
            }
        }
    }

    SqlAst {
        statements,
        parse_error: None,
        has_comma_join_anywhere: detect_comma_join_in_sql(sql),
        comments: collect_comments(sql),
    }
}

fn convert_statement(stmt: &Statement, line: i64, column: i64, end_line: i64) -> StmtInfo {
    match stmt {
        Statement::CreateTable {
            name,
            columns,
            constraints,
            if_not_exists,
            ..
        } => {
            let table_name = name.to_string();
            let mut cols = Vec::new();
            let mut pk_in_column = false;
            // 表级约束收集
            let mut foreign_keys = Vec::new();
            let mut checks = Vec::new();
            let mut indexes = Vec::new();
            let mut uniques = Vec::new();

            for col_def in columns {
                let mut info = ColumnInfo {
                    name: col_def.name.to_string(),
                    data_type: col_def.data_type.to_string(),
                    is_primary_key: false,
                    is_not_null: false,
                    is_unique: false,
                    default_value: None,
                    is_auto_increment: false,
                    comment: None,
                    has_check: false,
                    references_table: None,
                    has_foreign_key: false,
                    line: None,
                    column: None,
                };
                for opt in &col_def.options {
                    match &opt.option {
                        ColumnOption::NotNull => info.is_not_null = true,
                        ColumnOption::Null => info.is_not_null = false,
                        ColumnOption::Default(expr) => {
                            info.default_value = Some(expr.to_string());
                        }
                        ColumnOption::Unique { is_primary, .. } => {
                            info.is_unique = true;
                            if *is_primary {
                                info.is_primary_key = true;
                                pk_in_column = true;
                            }
                        }
                        ColumnOption::Comment(s) => {
                            info.comment = Some(s.clone());
                        }
                        ColumnOption::Check(expr) => {
                            info.has_check = true;
                            checks.push(CheckInfo {
                                name: String::new(),
                                expr_text: expr.to_string(),
                                line: None,
                                column: None,
                            });
                        }
                        ColumnOption::ForeignKey {
                            foreign_table,
                            referred_columns,
                            on_delete,
                            on_update,
                            ..
                        } => {
                            info.has_foreign_key = true;
                            info.references_table = Some(foreign_table.to_string());
                            foreign_keys.push(ForeignKeyInfo {
                                name: String::new(),
                                columns: vec![col_def.name.to_string()],
                                foreign_table: foreign_table.to_string(),
                                referred_columns: referred_columns
                                    .iter()
                                    .map(|i| i.to_string())
                                    .collect(),
                                on_delete: format_referral_action(on_delete),
                                on_update: format_referral_action(on_update),
                                line: None,
                                column: None,
                            });
                        }
                        ColumnOption::OnUpdate(expr) => {
                            // MySQL ON UPDATE CURRENT_TIMESTAMP 等：归入 default_value 描述
                            // 这里仅作标记，不影响主流程
                            let _ = expr;
                        }
                        ColumnOption::DialectSpecific(tokens) => {
                            // 检测 MySQL AUTO_INCREMENT / SQLite AUTOINCREMENT
                            let joined = tokens
                                .iter()
                                .map(|t| t.to_string().to_uppercase())
                                .collect::<Vec<_>>()
                                .join(" ");
                            if joined.contains("AUTO_INCREMENT")
                                || joined.contains("AUTOINCREMENT")
                            {
                                info.is_auto_increment = true;
                            }
                        }
                        _ => {}
                    }
                }
                cols.push(info);
            }

            // 表级约束（主键不在此处收集，由下方 pk_in_constraint 单独扫描）
            for con in constraints {
                match con {
                    TableConstraint::Unique { name, columns, .. } => {
                        uniques.push(UniqueInfo {
                            name: name
                                .as_ref()
                                .map(|i| i.to_string())
                                .unwrap_or_default(),
                            columns: columns.iter().map(|i| i.to_string()).collect(),
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::ForeignKey {
                        name,
                        columns,
                        foreign_table,
                        referred_columns,
                        on_delete,
                        on_update,
                        ..
                    } => {
                        foreign_keys.push(ForeignKeyInfo {
                            name: name
                                .as_ref()
                                .map(|i| i.to_string())
                                .unwrap_or_default(),
                            columns: columns.iter().map(|i| i.to_string()).collect(),
                            foreign_table: foreign_table.to_string(),
                            referred_columns: referred_columns
                                .iter()
                                .map(|i| i.to_string())
                                .collect(),
                            on_delete: format_referral_action(on_delete),
                            on_update: format_referral_action(on_update),
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::Check { name, expr } => {
                        checks.push(CheckInfo {
                            name: name
                                .as_ref()
                                .map(|i| i.to_string())
                                .unwrap_or_default(),
                            expr_text: expr.to_string(),
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::Index { name, columns, .. } => {
                        indexes.push(IndexInfo {
                            name: name
                                .as_ref()
                                .map(|i| i.to_string())
                                .unwrap_or_default(),
                            columns: columns.iter().map(|i| i.to_string()).collect(),
                            is_unique: false,
                            line: None,
                            column: None,
                        });
                    }
                    _ => {}
                }
            }

            let pk_in_constraint = constraints
                .iter()
                .any(|con| matches!(con, TableConstraint::PrimaryKey { .. }));

            StmtInfo {
                kind: "CREATE_TABLE".to_string(),
                line,
                end_line,
                column,
                create_table: Some(CreateInfo {
                    table_name,
                    columns: cols,
                    has_primary_key: pk_in_column || pk_in_constraint,
                    if_not_exists: *if_not_exists,
                    has_foreign_key: !foreign_keys.is_empty(),
                    has_check: !checks.is_empty(),
                    has_index: !indexes.is_empty(),
                    has_unique: !uniques.is_empty(),
                    foreign_keys,
                    checks,
                    indexes,
                    uniques,
                }),
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
            }
        }
        Statement::Drop {
            object_type,
            if_exists,
            names,
            ..
        } => {
            let obj_type = format!("{:?}", object_type).to_uppercase();
            let name_list: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            let kind = format!("DROP_{}", obj_type);
            StmtInfo {
                kind,
                line,
                end_line,
                column,
                create_table: None,
                drop_object: Some(DropInfo {
                    object_type: obj_type,
                    name: name_list.join(", "),
                    if_exists: *if_exists,
                }),
                select: None,
                insert: None,
                update: None,
                delete: None,
                alter_table: None,
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::Query(q) => {
            let info = analyze_query(q);
            StmtInfo {
                kind: "SELECT".to_string(),
                line,
                end_line,
                column,
                create_table: None,
                drop_object: None,
                select: Some(info),
                insert: None,
                update: None,
                delete: None,
                alter_table: None,
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::Insert {
            table_name, columns, ..
        } => {
            let col_names: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
            StmtInfo {
                kind: "INSERT".to_string(),
                line,
                end_line,
                column,
                create_table: None,
                drop_object: None,
                select: None,
                insert: Some(InsertInfo {
                    table_name: table_name.to_string(),
                    columns: col_names,
                }),
                update: None,
                delete: None,
                alter_table: None,
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::Update {
            table, selection, ..
        } => {
            let table_name = match &table.relation {
                TableFactor::Table { name, .. } => name.to_string(),
                _ => table.relation.to_string(),
            };
            StmtInfo {
                kind: "UPDATE".to_string(),
                line,
                end_line,
                column,
                create_table: None,
                drop_object: None,
                select: None,
                insert: None,
                update: Some(UpdateInfo {
                    table_name,
                    where_clause: selection.as_ref().map(|e| e.to_string()),
                }),
                delete: None,
                alter_table: None,
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::Delete {
            tables, from, selection, ..
        } => {
            // 优先取 `tables`（MySQL 多表 DELETE），否则从 `from` 取首个表
            let table_name = if !tables.is_empty() {
                tables
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                let from_tables = match from {
                    FromTable::WithFromKeyword(v) | FromTable::WithoutKeyword(v) => v,
                };
                from_tables
                    .first()
                    .and_then(|t| {
                        if let TableFactor::Table { name, .. } = &t.relation {
                            Some(name.to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            };
            StmtInfo {
                kind: "DELETE".to_string(),
                line,
                end_line,
                column,
                create_table: None,
                drop_object: None,
                select: None,
                insert: None,
                update: None,
                delete: Some(DeleteInfo {
                    table_name,
                    where_clause: selection.as_ref().map(|e| e.to_string()),
                }),
                alter_table: None,
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::AlterTable {
            name, operations, ..
        } => {
            let table_name = name.to_string();
            let mut adds_primary_key = false;
            let mut drops_primary_key = false;
            let mut op_infos = Vec::new();

            for op in operations {
                let (op_type, col_name, tbl_name, con_name, detail, is_add_pk, is_drop_pk) =
                    match op {
                        AlterTableOperation::AddConstraint(tc) => {
                            let (t, detail_str, is_pk) = match tc {
                                TableConstraint::PrimaryKey { name, columns, .. } => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} PRIMARY KEY ({})",
                                        name.as_ref().map(|n| format!(" CONSTRAINT {}", n)).unwrap_or_default(),
                                        columns
                                            .iter()
                                            .map(|i| i.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                    true,
                                ),
                                TableConstraint::Unique { name, columns, .. } => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} UNIQUE ({})",
                                        name.as_ref().map(|n| format!(" CONSTRAINT {}", n)).unwrap_or_default(),
                                        columns
                                            .iter()
                                            .map(|i| i.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                    false,
                                ),
                                TableConstraint::ForeignKey {
                                    name,
                                    columns,
                                    foreign_table,
                                    ..
                                } => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} FOREIGN KEY ({}) REFERENCES {}",
                                        name.as_ref().map(|n| format!(" CONSTRAINT {}", n)).unwrap_or_default(),
                                        columns
                                            .iter()
                                            .map(|i| i.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                        foreign_table
                                    ),
                                    false,
                                ),
                                TableConstraint::Check { name, expr, .. } => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} CHECK ({})",
                                        name.as_ref().map(|n| format!(" CONSTRAINT {}", n)).unwrap_or_default(),
                                        expr
                                    ),
                                    false,
                                ),
                                _ => ("ADD_CONSTRAINT", format!("ADD {}", tc), false),
                            };
                            (t.to_string(), String::new(), String::new(),
                             name_of_table_constraint(tc), detail_str, is_pk, false)
                        }
                        AlterTableOperation::AddColumn { column_def, .. } => {
                            let has_pk = column_def.options.iter().any(|o| {
                                matches!(o.option, ColumnOption::Unique { is_primary: true, .. })
                            });
                            (
                                "ADD_COLUMN".to_string(),
                                column_def.name.to_string(),
                                String::new(),
                                String::new(),
                                format!("ADD COLUMN {}", column_def),
                                has_pk,
                                false,
                            )
                        }
                        AlterTableOperation::DropColumn {
                            column_name,
                            if_exists,
                            cascade,
                        } => (
                            "DROP_COLUMN".to_string(),
                            column_name.to_string(),
                            String::new(),
                            String::new(),
                            format!(
                                "DROP COLUMN{}{}{}",
                                if *if_exists { " IF EXISTS" } else { "" },
                                format!(" {}", column_name),
                                if *cascade { " CASCADE" } else { "" }
                            ),
                            false,
                            false,
                        ),
                        AlterTableOperation::AlterColumn { column_name, op } => (
                            "ALTER_COLUMN".to_string(),
                            column_name.to_string(),
                            String::new(),
                            String::new(),
                            format!("ALTER COLUMN {} {}", column_name, op),
                            false,
                            false,
                        ),
                        AlterTableOperation::RenameColumn {
                            old_column_name,
                            new_column_name,
                        } => (
                            "RENAME_COLUMN".to_string(),
                            old_column_name.to_string(),
                            String::new(),
                            String::new(),
                            format!("RENAME COLUMN {} TO {}", old_column_name, new_column_name),
                            false,
                            false,
                        ),
                        AlterTableOperation::RenameTable { table_name } => (
                            "RENAME_TABLE".to_string(),
                            String::new(),
                            table_name.to_string(),
                            String::new(),
                            format!("RENAME TO {}", table_name),
                            false,
                            false,
                        ),
                        AlterTableOperation::DropPrimaryKey => (
                            "DROP_PRIMARY_KEY".to_string(),
                            String::new(),
                            String::new(),
                            String::new(),
                            "DROP PRIMARY KEY".to_string(),
                            false,
                            true,
                        ),
                        AlterTableOperation::DropConstraint { name, .. } => (
                            "DROP_CONSTRAINT".to_string(),
                            String::new(),
                            String::new(),
                            name.to_string(),
                            format!("DROP CONSTRAINT {}", name),
                            false,
                            false,
                        ),
                        _ => (
                            "OTHER".to_string(),
                            String::new(),
                            String::new(),
                            String::new(),
                            format!("{}", op),
                            false,
                            false,
                        ),
                    };
                if is_add_pk {
                    adds_primary_key = true;
                }
                if is_drop_pk {
                    drops_primary_key = true;
                }
                op_infos.push(AlterOpInfo {
                    operation_type: op_type,
                    column_name: col_name,
                    table_name: tbl_name,
                    constraint_name: con_name,
                    detail,
                });
            }
            StmtInfo {
                kind: "ALTER_TABLE".to_string(),
                line,
                end_line,
                column,
                create_table: None,
                drop_object: None,
                select: None,
                insert: None,
                update: None,
                delete: None,
                alter_table: Some(AlterTableInfo {
                    table_name,
                    adds_primary_key,
                    drops_primary_key,
                    operations: op_infos,
                }),
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::Truncate { table_name, table, .. } => StmtInfo {
            kind: "TRUNCATE".to_string(),
            line,
            end_line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
            insert: None,
            update: None,
            delete: None,
            alter_table: None,
            truncate: Some(TruncateInfo {
                table_name: table_name.to_string(),
                has_table_keyword: *table,
            }),
            create_view: None,
            create_index: None,
            transaction: None,
        },
        Statement::CreateView {
            or_replace,
            materialized,
            name,
            columns,
            ..
        } => StmtInfo {
            kind: "CREATE_VIEW".to_string(),
            line,
            end_line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
            insert: None,
            update: None,
            delete: None,
            alter_table: None,
            truncate: None,
            create_view: Some(ViewInfo {
                name: name.to_string(),
                materialized: *materialized,
                is_replace: *or_replace,
                column_count: columns.len() as i64,
            }),
            create_index: None,
            transaction: None,
        },
        Statement::CreateIndex {
            name,
            table_name,
            using,
            columns,
            unique,
            ..
        } => {
            let idx_name = name
                .as_ref()
                .map(|n| n.to_string())
                .unwrap_or_default();
            let col_names: Vec<String> = columns
                .iter()
                .map(|obe: &OrderByExpr| obe.expr.to_string())
                .collect();
            let _ = using;
            StmtInfo {
                kind: "CREATE_INDEX".to_string(),
                line,
                end_line,
                column,
                create_table: None,
                drop_object: None,
                select: None,
                insert: None,
                update: None,
                delete: None,
                alter_table: None,
                truncate: None,
                create_view: None,
                create_index: Some(CreateIndexInfo {
                    name: idx_name,
                    table_name: table_name.to_string(),
                    columns: col_names,
                    is_unique: *unique,
                }),
                transaction: None,
            }
        }
        Statement::StartTransaction { .. } => StmtInfo {
            kind: "START_TRANSACTION".to_string(),
            line,
            end_line,
            column,
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
            transaction: Some(TransactionInfo {
                kind: "START_TRANSACTION".to_string(),
            }),
        },
        Statement::Commit { .. } => StmtInfo {
            kind: "COMMIT".to_string(),
            line,
            end_line,
            column,
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
            transaction: Some(TransactionInfo {
                kind: "COMMIT".to_string(),
            }),
        },
        Statement::Rollback { .. } => StmtInfo {
            kind: "ROLLBACK".to_string(),
            line,
            end_line,
            column,
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
            transaction: Some(TransactionInfo {
                kind: "ROLLBACK".to_string(),
            }),
        },
        Statement::Grant { .. } => StmtInfo {
            kind: "GRANT".to_string(),
            line,
            end_line,
            column,
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
        },
        Statement::Revoke { .. } => StmtInfo {
            kind: "REVOKE".to_string(),
            line,
            end_line,
            column,
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
        },
        Statement::Merge { .. } => StmtInfo {
            kind: "MERGE".to_string(),
            line,
            end_line,
            column,
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
        },
        Statement::SetVariable { .. } => StmtInfo {
            kind: "SET_VARIABLE".to_string(),
            line,
            end_line,
            column,
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
        },
        Statement::Use { .. } => StmtInfo {
            kind: "USE".to_string(),
            line,
            end_line,
            column,
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
        },
        _ => StmtInfo {
            kind: "OTHER".to_string(),
            line,
            end_line,
            column,
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
        },
    }
}

/// 提取 TableConstraint 的名字（用于 AlterOpInfo.constraint_name）。
fn name_of_table_constraint(tc: &TableConstraint) -> String {
    match tc {
        TableConstraint::Unique { name, .. }
        | TableConstraint::PrimaryKey { name, .. }
        | TableConstraint::ForeignKey { name, .. }
        | TableConstraint::Check { name, .. }
        | TableConstraint::Index { name, .. } => {
            name.as_ref().map(|i| i.to_string()).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// 把 `Option<ReferentialAction>` 转成字符串（"CASCADE"/"RESTRICT"/"SET NULL"/...），
/// None 返回空串。
fn format_referral_action(action: &Option<sqlparser::ast::ReferentialAction>) -> String {
    match action {
        Some(a) => format!("{:?}", a).to_uppercase(),
        None => String::new(),
    }
}

/// 递归检查整个查询树（顶层投影、集合运算各分支、FROM/JOIN 子查询）是否含 SELECT *。
fn query_has_wildcard(q: &Query) -> bool {
    set_expr_has_wildcard(&q.body)
}

/// 递归检查一个 SetExpr 是否含通配符。覆盖嵌套子查询，确保 SELECT * 不被漏报。
fn set_expr_has_wildcard(e: &SetExpr) -> bool {
    match e {
        SetExpr::Select(s) => {
            let proj_wc = s.projection.iter().any(|item| {
                matches!(item, SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _))
            });
            let derived_wc = |tf: &TableWithJoins| -> bool {
                let rel_wc =
                    matches!(&tf.relation, TableFactor::Derived { subquery, .. } if query_has_wildcard(subquery));
                let join_wc = tf
                    .joins
                    .iter()
                    .any(|j| matches!(&j.relation, TableFactor::Derived { subquery, .. } if query_has_wildcard(subquery)));
                rel_wc || join_wc
            };
            proj_wc || s.from.iter().any(derived_wc)
        }
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_has_wildcard(left) || set_expr_has_wildcard(right)
        }
        SetExpr::Query(inner) => query_has_wildcard(inner),
        _ => false,
    }
}

/// 分析单个 SELECT/Query，构造 SelectInfo。
///
/// 覆盖：通配符、投影、FROM/JOIN（含别名/ON 条件）、集合运算、WHERE/GROUP BY/HAVING/
/// QUALIFY、ORDER BY/LIMIT/OFFSET/FETCH（Query 层）、DISTINCT、CTE/WITH、子查询递归、
/// 窗口函数、WHERE/HAVING/投影的顶层 ExprInfo、隐式逗号 JOIN。
fn analyze_query(q: &Query) -> SelectInfo {
    let mut info = SelectInfo::default();

    // 检查是否为集合运算（UNION / INTERSECT / EXCEPT）。
    // 注意：ALL / DISTINCT 在 set_quantifier（而非 op）中，op 只是运算类型。
    if let SetExpr::SetOperation {
        op, set_quantifier, ..
    } = &*q.body
    {
        match op {
            SetOperator::Union => {
                info.union = true;
                info.union_all = matches!(
                    set_quantifier,
                    SetQuantifier::All | SetQuantifier::AllByName
                );
            }
            SetOperator::Intersect => info.intersect = true,
            SetOperator::Except => info.except = true,
        }
    }

    // 递归识别整个查询（含集合运算各分支、FROM/JOIN 子查询）中的 SELECT *，
    // 否则 SELECT * 出现在 UNION 分支或子查询里会被 DML001 漏报。
    info.has_wildcard = info.has_wildcard || query_has_wildcard(q);

    // CTE / WITH（在 Query 层，无论是否集合运算都要查）
    if let Some(with) = &q.with {
        info.has_cte = true;
        info.is_recursive = with.recursive;
        for cte in &with.cte_tables {
            info.ctes.push(CteInfo {
                name: cte.alias.name.to_string(),
                column_count: cte.alias.columns.len() as i64,
                is_recursive: with.recursive,
            });
        }
    }

    // Query 层的 ORDER BY / LIMIT / OFFSET / FETCH（无论是否集合运算都在 q 上）
    info.has_order_by = !q.order_by.is_empty();
    info.has_limit = q.limit.is_some();
    info.has_offset = q.offset.is_some();
    info.has_fetch = q.fetch.is_some();

    // Select 层的字段（仅 SetExpr::Select 时有效）
    if let SetExpr::Select(s) = &*q.body {
        // DISTINCT
        info.has_distinct = s.distinct.is_some();

        // WHERE / GROUP BY / HAVING / QUALIFY
        info.has_where = s.selection.is_some();
        info.where_clause = s.selection.as_ref().map(|e| e.to_string());
        info.where_expr = s.selection.as_ref().map(analyze_expr);
        info.has_group_by = matches!(&s.group_by, GroupByExpr::Expressions(es) if !es.is_empty())
            || matches!(&s.group_by, GroupByExpr::All);
        info.has_having = s.having.is_some();
        info.having_expr = s.having.as_ref().map(analyze_expr);
        info.has_qualify = s.qualify.is_some();

        let has_multiple_tables = s.from.len() > 1 || s.from.iter().any(|t| !t.joins.is_empty());

        // 投影 + 窗口函数收集（在投影中扫描）
        let mut window_funcs = Vec::new();
        for item in &s.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    info.has_wildcard = true;
                    info.projection_exprs.push(ExprInfo {
                        kind: "WILDCARD".to_string(),
                        text: "*".to_string(),
                        ..Default::default()
                    });
                }
                SelectItem::QualifiedWildcard(obj, _) => {
                    info.has_wildcard = true;
                    info.projection_exprs.push(ExprInfo {
                        kind: "WILDCARD".to_string(),
                        text: obj.to_string() + ".*",
                        ..Default::default()
                    });
                }
                SelectItem::UnnamedExpr(expr) => {
                    let text = expr.to_string();
                    info.projection.push(text.clone());
                    if has_multiple_tables && !text.contains('.') {
                        if matches!(expr, Expr::Identifier(_)) {
                            info.has_unqualified_column = true;
                        }
                    }
                    info.projection_exprs.push(analyze_expr(expr));
                    collect_window_funcs_in_expr(expr, &mut window_funcs);
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    let text = format!("{} AS {}", expr, alias);
                    info.projection.push(text);
                    // 仅当别名表达式是"裸列"（无表限定的标识符）才算未限定列；
                    // 函数调用（如 COUNT(*)）、字面量、计算表达式不算，否则误报。
                    if has_multiple_tables && matches!(expr, Expr::Identifier(_)) {
                        info.has_unqualified_column = true;
                    }
                    info.projection_exprs.push(analyze_expr(expr));
                    collect_window_funcs_in_expr(expr, &mut window_funcs);
                }
            }
        }

        // WHERE / HAVING 中的窗口函数（虽然窗口函数在 WHERE 中非法，但保险起见也扫一下）
        if let Some(expr) = &s.selection {
            collect_window_funcs_in_expr(expr, &mut window_funcs);
        }
        if let Some(expr) = &s.having {
            collect_window_funcs_in_expr(expr, &mut window_funcs);
        }

        if !window_funcs.is_empty() {
            info.has_window_function = true;
            info.window_functions = window_funcs;
        }

        // 分析 FROM 子句
        for table_with_joins in &s.from {
            // 分析主表
            match &table_with_joins.relation {
                TableFactor::Table { name, alias, .. } => {
                    if info.from_table.is_none() {
                        info.from_table = Some(name.to_string());
                    }
                    if info.from_table_alias.is_none() {
                        info.from_table_alias =
                            alias.as_ref().map(|a| a.name.to_string());
                    }
                }
                TableFactor::Derived { alias, .. } => {
                    info.has_subquery_in_from = true;
                    // 默认值为 false；遇到带别名子查询时改为 true，
                    // 否则规则 subquery_alias_required 会误报"已带别名的子查询缺别名"
                    if alias.is_some() {
                        info.from_subquery_has_alias = true;
                    }
                }
                _ => {}
            }

            // 分析 JOIN
            for join in &table_with_joins.joins {
                let (table_name, join_type, has_condition, alias, condition_text) =
                    analyze_join_operator(&join.join_operator, &join.relation);
                info.joins.push(JoinInfo {
                    table_name,
                    join_type,
                    has_condition,
                    alias,
                    condition_text,
                    line: None,
                    column: None,
                });
            }
        }

        // 隐式逗号 JOIN：FROM a, b（sqlparser 把它解析成多个 TableWithJoins 元素）
        if s.from.len() > 1 {
            info.has_comma_join = true;
        }
    }

    // 子查询递归收集（FROM 子查询 / WHERE 子查询 / EXISTS / 集合运算的括号分支）
    let mut subs = Vec::new();
    collect_subqueries_in_query(q, &mut subs, 0);
    if !subs.is_empty() {
        info.has_subquery = true;
        info.subqueries = subs;
    }

    info
}

/// 分析一个 JOIN 的操作符与目标表，返回
/// (table_name, join_type, has_condition, alias, condition_text)。
fn analyze_join_operator(
    op: &JoinOperator,
    relation: &TableFactor,
) -> (String, String, bool, Option<String>, Option<String>) {
    let (table_name, alias) = table_factor_name_and_alias(relation);
    match op {
        JoinOperator::Inner(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint) => {
            let join_type = match op {
                JoinOperator::Inner(_) => "INNER",
                JoinOperator::LeftOuter(_) => "LEFT",
                JoinOperator::RightOuter(_) => "RIGHT",
                JoinOperator::FullOuter(_) => "FULL",
                JoinOperator::LeftSemi(_) => "LEFT_SEMI",
                JoinOperator::RightSemi(_) => "RIGHT_SEMI",
                JoinOperator::LeftAnti(_) => "LEFT_ANTI",
                JoinOperator::RightAnti(_) => "RIGHT_ANTI",
                _ => "OTHER",
            }
            .to_string();
            let (has_condition, condition_text) = analyze_join_constraint(constraint);
            (table_name, join_type, has_condition, alias, condition_text)
        }
        JoinOperator::CrossJoin => (table_name, "CROSS".to_string(), true, alias, None),
        _ => (table_name, "OTHER".to_string(), false, alias, None),
    }
}

/// 从 TableFactor 提取表名（或派生表的字符串形式）与别名。
fn table_factor_name_and_alias(tf: &TableFactor) -> (String, Option<String>) {
    match tf {
        TableFactor::Table { name, alias, .. } => (
            name.to_string(),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::Derived { alias, subquery, .. } => (
            format!("({})", subquery),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::TableFunction { expr, alias } => (
            format!("TABLE({})", expr),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::Function { name, alias, .. } => (
            name.to_string(),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        _ => (tf.to_string(), None),
    }
}

/// 分析 JoinConstraint，返回 (has_condition, condition_text)。
/// On(expr) 时 condition_text = Some(expr.to_string())。
fn analyze_join_constraint(constraint: &JoinConstraint) -> (bool, Option<String>) {
    match constraint {
        JoinConstraint::On(expr) => (true, Some(expr.to_string())),
        JoinConstraint::Using(_) => (true, None),
        JoinConstraint::Natural => (true, None),
        JoinConstraint::None => (false, None),
    }
}

/// 收集查询中所有子查询，递归下钻（深度限制 20 层防止恶意/异常输入导致栈溢出）。
///
/// 覆盖：
/// - `TableFactor::Derived { subquery, .. }`（FROM 子查询，含 JOIN 中的）
/// - `Expr::Subquery(Box<Query>)` / `Expr::Exists { subquery }` / `Expr::InSubquery`
///   / `Expr::ArraySubquery`（WHERE/SELECT 中的标量子查询/EXISTS/IN）
/// - `SetExpr::Query(inner)`（集合运算的括号分支）
/// - 递归下钻到子查询的子查询
fn collect_subqueries_in_query(q: &Query, out: &mut Vec<SelectInfo>, depth: usize) {
    if depth > 20 {
        return;
    }
    // WITH 中的 CTE 子查询也算（每个 Cte.query 都是子查询）
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            out.push(analyze_query(&cte.query));
            collect_subqueries_in_query(&cte.query, out, depth + 1);
        }
    }

    match &*q.body {
        SetExpr::Select(s) => {
            // FROM / JOIN 中的 Derived
            for twj in &s.from {
                if let TableFactor::Derived { subquery, .. } = &twj.relation {
                    out.push(analyze_query(subquery));
                    collect_subqueries_in_query(subquery, out, depth + 1);
                }
                for j in &twj.joins {
                    if let TableFactor::Derived { subquery, .. } = &j.relation {
                        out.push(analyze_query(subquery));
                        collect_subqueries_in_query(subquery, out, depth + 1);
                    }
                }
            }
            // WHERE / HAVING / 投影中的子查询
            if let Some(expr) = &s.selection {
                collect_subqueries_in_expr(expr, out, depth);
            }
            if let Some(expr) = &s.having {
                collect_subqueries_in_expr(expr, out, depth);
            }
            if let Some(expr) = &s.qualify {
                collect_subqueries_in_expr(expr, out, depth);
            }
            for item in &s.projection {
                match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        collect_subqueries_in_expr(e, out, depth);
                    }
                    _ => {}
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            collect_subqueries_in_set_expr(left, out, depth);
            collect_subqueries_in_set_expr(right, out, depth);
        }
        SetExpr::Query(inner) => {
            out.push(analyze_query(inner));
            collect_subqueries_in_query(inner, out, depth + 1);
        }
        _ => {}
    }
}

fn collect_subqueries_in_set_expr(e: &SetExpr, out: &mut Vec<SelectInfo>, depth: usize) {
    if depth > 20 {
        return;
    }
    match e {
        SetExpr::Select(s) => {
            for twj in &s.from {
                if let TableFactor::Derived { subquery, .. } = &twj.relation {
                    out.push(analyze_query(subquery));
                    collect_subqueries_in_query(subquery, out, depth + 1);
                }
                for j in &twj.joins {
                    if let TableFactor::Derived { subquery, .. } = &j.relation {
                        out.push(analyze_query(subquery));
                        collect_subqueries_in_query(subquery, out, depth + 1);
                    }
                }
            }
            if let Some(expr) = &s.selection {
                collect_subqueries_in_expr(expr, out, depth);
            }
            for item in &s.projection {
                match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        collect_subqueries_in_expr(e, out, depth);
                    }
                    _ => {}
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            collect_subqueries_in_set_expr(left, out, depth + 1);
            collect_subqueries_in_set_expr(right, out, depth + 1);
        }
        SetExpr::Query(inner) => {
            out.push(analyze_query(inner));
            collect_subqueries_in_query(inner, out, depth + 1);
        }
        _ => {}
    }
}

/// 递归遍历表达式，把所有遇到的子查询（Subquery / Exists / InSubquery / ArraySubquery）
/// 推入 out，并继续下钻子查询内部。
fn collect_subqueries_in_expr(e: &Expr, out: &mut Vec<SelectInfo>, depth: usize) {
    if depth > 20 {
        return;
    }
    match e {
        Expr::Subquery(q) => {
            out.push(analyze_query(q));
            collect_subqueries_in_query(q, out, depth + 1);
        }
        Expr::Exists { subquery, .. } => {
            out.push(analyze_query(subquery));
            collect_subqueries_in_query(subquery, out, depth + 1);
        }
        Expr::InSubquery { subquery, .. } => {
            out.push(analyze_query(subquery));
            collect_subqueries_in_query(subquery, out, depth + 1);
        }
        Expr::ArraySubquery(q) => {
            out.push(analyze_query(q));
            collect_subqueries_in_query(q, out, depth + 1);
        }
        // 递归下钻常见容器
        Expr::BinaryOp { left, right, .. } => {
            collect_subqueries_in_expr(left, out, depth);
            collect_subqueries_in_expr(right, out, depth);
        }
        Expr::UnaryOp { expr, .. } => collect_subqueries_in_expr(expr, out, depth),
        Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            collect_subqueries_in_expr(expr, out, depth)
        }
        Expr::InList { expr, list, .. } => {
            collect_subqueries_in_expr(expr, out, depth);
            for x in list {
                collect_subqueries_in_expr(x, out, depth);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_subqueries_in_expr(expr, out, depth);
            collect_subqueries_in_expr(low, out, depth);
            collect_subqueries_in_expr(high, out, depth);
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            if let Some(o) = operand {
                collect_subqueries_in_expr(o, out, depth);
            }
            for c in conditions {
                collect_subqueries_in_expr(c, out, depth);
            }
            for r in results {
                collect_subqueries_in_expr(r, out, depth);
            }
            if let Some(e) = else_result {
                collect_subqueries_in_expr(e, out, depth);
            }
        }
        Expr::Cast { expr, .. } => collect_subqueries_in_expr(expr, out, depth),
        Expr::Function(f) => {
            for arg in &f.args {
                let inner = match arg {
                    sqlparser::ast::FunctionArg::Named { arg, .. } => arg,
                    sqlparser::ast::FunctionArg::Unnamed(arg) => arg,
                };
                if let sqlparser::ast::FunctionArgExpr::Expr(e) = inner {
                    collect_subqueries_in_expr(e, out, depth);
                }
            }
        }
        _ => {}
    }
}

/// 在表达式中扫描窗口函数（`Function { over: Some(_), .. }`），把每个窗口函数
/// 的元信息推入 out。会下钻到 BinaryOp / UnaryOp / Case / Cast / Function 等容器。
fn collect_window_funcs_in_expr(e: &Expr, out: &mut Vec<WindowFuncInfo>) {
    match e {
        Expr::Function(f) => {
            if let Some(over) = &f.over {
                let (has_partition_by, has_order_by, has_window_frame) = match over {
                    WindowType::WindowSpec(spec) => (
                        !spec.partition_by.is_empty(),
                        !spec.order_by.is_empty(),
                        spec.window_frame.is_some(),
                    ),
                    WindowType::NamedWindow(_) => (false, false, false),
                };
                let func_name = f
                    .name
                    .0
                    .last()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                out.push(WindowFuncInfo {
                    function_name: func_name,
                    has_partition_by,
                    has_order_by,
                    has_window_frame,
                });
            }
            // 函数参数中可能还嵌套窗口函数，下钻
            for arg in &f.args {
                let inner = match arg {
                    sqlparser::ast::FunctionArg::Named { arg, .. } => arg,
                    sqlparser::ast::FunctionArg::Unnamed(arg) => arg,
                };
                if let sqlparser::ast::FunctionArgExpr::Expr(inner_e) = inner {
                    collect_window_funcs_in_expr(inner_e, out);
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_window_funcs_in_expr(left, out);
            collect_window_funcs_in_expr(right, out);
        }
        Expr::UnaryOp { expr, .. } => collect_window_funcs_in_expr(expr, out),
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            if let Some(o) = operand {
                collect_window_funcs_in_expr(o, out);
            }
            for c in conditions {
                collect_window_funcs_in_expr(c, out);
            }
            for r in results {
                collect_window_funcs_in_expr(r, out);
            }
            if let Some(e) = else_result {
                collect_window_funcs_in_expr(e, out);
            }
        }
        Expr::Cast { expr, .. } => collect_window_funcs_in_expr(expr, out),
        Expr::IsNull(expr) | Expr::IsNotNull(expr) => collect_window_funcs_in_expr(expr, out),
        Expr::InList { expr, list, .. } => {
            collect_window_funcs_in_expr(expr, out);
            for x in list {
                collect_window_funcs_in_expr(x, out);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_window_funcs_in_expr(expr, out);
            collect_window_funcs_in_expr(low, out);
            collect_window_funcs_in_expr(high, out);
        }
        _ => {}
    }
}

/// 分析表达式的顶层信息（不递归暴露子表达式）。
fn analyze_expr(e: &Expr) -> ExprInfo {
    let text = e.to_string();
    let (kind, function_name, is_literal, is_column, is_subquery, operator, column_name, has_null_test) = match e {
        Expr::Identifier(ident) => (
            "IDENTIFIER",
            String::new(),
            false,
            true,
            false,
            String::new(),
            ident.value.clone(),
            false,
        ),
        Expr::CompoundIdentifier(idents) => (
            "COMPOUND_IDENTIFIER",
            String::new(),
            false,
            true,
            false,
            String::new(),
            idents.last().map(|i| i.value.clone()).unwrap_or_default(),
            false,
        ),
        Expr::Value(_) => ("LITERAL", String::new(), true, false, false, String::new(), String::new(), false),
        Expr::BinaryOp { op, .. } => (
            "BINARY_OP",
            String::new(),
            false,
            false,
            false,
            format!("{:?}", op).to_uppercase(),
            String::new(),
            false,
        ),
        Expr::UnaryOp { op, .. } => (
            "UNARY_OP",
            String::new(),
            false,
            false,
            false,
            format!("{:?}", op).to_uppercase(),
            String::new(),
            false,
        ),
        Expr::Function(f) => (
            "FUNCTION",
            f.name.0.last().map(|i| i.value.clone()).unwrap_or_default(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Case { .. } => ("CASE", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::Subquery(_) => ("SUBQUERY", String::new(), false, false, true, String::new(), String::new(), false),
        Expr::Exists { .. } => ("EXISTS", String::new(), false, false, true, String::new(), String::new(), false),
        Expr::InList { .. } => ("IN_LIST", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::InSubquery { .. } => (
            "IN_SUBQUERY",
            String::new(),
            false,
            false,
            true,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Between { .. } => ("BETWEEN", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::Cast { .. } => ("CAST", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::IsNull(_) => ("IS_NULL", String::new(), false, false, false, String::new(), String::new(), true),
        Expr::IsNotNull(_) => ("IS_NOT_NULL", String::new(), false, false, false, String::new(), String::new(), true),
        Expr::TypedString { .. } => ("TYPED_STRING", String::new(), true, false, false, String::new(), String::new(), false),
        Expr::Interval(_) => ("INTERVAL", String::new(), true, false, false, String::new(), String::new(), false),
        _ => ("OTHER", String::new(), false, false, false, String::new(), String::new(), false),
    };
    ExprInfo {
        kind: kind.to_string(),
        text,
        function_name,
        is_literal,
        is_column,
        is_subquery,
        operator,
        column_name,
        has_null_test,
        line: None,
        column: None,
    }
}

/// 扫描 SQL 源文本，检测是否出现 `FROM ... , ...` 形式的隐式逗号 JOIN。
///
/// 跟踪：字符串字面量（`'...'`、`"..."` 标识符）、行注释（`-- ...`）、
/// 块注释（`/* ... */`）、paren depth。从 FROM 关键字开始（不区分大小写、词边界），
/// 到 WHERE/GROUP/HAVING/ORDER/LIMIT/OFFSET/UNION/EXCEPT/INTERSECT/FOR/FETCH/RETURNING
/// 等子句终止关键字或语句结尾结束。跟踪期间 paren_depth=0 处的逗号 → 视为隐式 JOIN。
fn detect_comma_join_in_sql(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    let bytes = upper.as_bytes();
    let raw = sql.as_bytes();
    let n = bytes.len();

    let mut i = 0;
    // 状态机：是否在 FROM 子句内（顶层 paren_depth=0 处的 FROM）
    let mut in_from = false;
    let mut paren_depth: i32 = 0;

    while i < n {
        let b = bytes[i];

        // 行注释 --...\n
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 块注释 /* ... */（可能跨行，支持嵌套一层的简单实现）
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // 单引号字符串
        if b == b'\'' {
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    // 转义 '' 跳过
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 双引号字符串（标识符或字符串，统一跳过）
        if b == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'"' {
                    if i + 1 < n && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 反引号（MySQL 标识符）
        if b == b'`' {
            i += 1;
            while i < n && bytes[i] != b'`' {
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }

        if b == b'(' {
            paren_depth += 1;
            i += 1;
            continue;
        }
        if b == b')' {
            if paren_depth > 0 {
                paren_depth -= 1;
            }
            i += 1;
            continue;
        }

        // 在 FROM 子句内、paren_depth=0 处的逗号 = 隐式 JOIN
        if in_from && paren_depth == 0 && b == b',' {
            return true;
        }

        // 关键字识别（词边界）
        if b.is_ascii_alphabetic() {
            // 找到词尾
            let start = i;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &raw[start..i];
            let word_str = std::str::from_utf8(word).unwrap_or("").to_uppercase();

            // 词边界检查（前后非字母数字下划线）
            let prev_ok = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            let next_ok = i == n
                || !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_');
            if !prev_ok || !next_ok {
                continue;
            }

            if !in_from && word_str == "FROM" {
                in_from = true;
            } else if in_from {
                // FROM 子句终止关键字
                if matches!(
                    word_str.as_str(),
                    "WHERE" | "GROUP" | "HAVING" | "ORDER" | "LIMIT" | "OFFSET"
                        | "UNION" | "EXCEPT" | "INTERSECT" | "FOR" | "FETCH" | "RETURNING"
                        | "QUALIFY" | "WINDOW" | "INTO"
                ) {
                    in_from = false;
                }
            }
            continue;
        }

        // 分号重置 FROM 状态
        if b == b';' {
            in_from = false;
            paren_depth = 0;
        }
        i += 1;
    }
    false
}

/// 扫描 SQL 源文本，收集所有注释（行注释与块注释）。
///
/// - 行注释：`-- ...` 直到行尾
/// - 块注释：`/* ... */` 可能跨行
/// 行号通过计数 `\n` 累计（从 1 开始）。
fn collect_comments(sql: &str) -> Vec<CommentInfo> {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut comments = Vec::new();
    let mut line: i64 = 1;
    let mut i = 0;

    while i < n {
        let b = bytes[i];

        // 行注释 --...\n
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            let start = i;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            let text = std::str::from_utf8(&bytes[start..i])
                .unwrap_or("")
                .to_string();
            comments.push(CommentInfo {
                text,
                line,
                kind: "LINE".to_string(),
            });
            continue;
        }
        // 块注释 /* ... */
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let start = i;
            let start_line = line;
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            // 跳过结束 */
            if i + 1 < n {
                i += 2;
            } else {
                i = n;
            }
            let text = std::str::from_utf8(&bytes[start..i.min(n)])
                .unwrap_or("")
                .to_string();
            comments.push(CommentInfo {
                text,
                line: start_line,
                kind: "BLOCK".to_string(),
            });
            continue;
        }
        // 单引号字符串（避免字符串内的 -- 或 /* 被误识别）
        if b == b'\'' {
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            continue;
        }
        // 双引号
        if b == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'"' {
                    if i + 1 < n && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            continue;
        }
        // 反引号
        if b == b'`' {
            i += 1;
            while i < n && bytes[i] != b'`' {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }

        if b == b'\n' {
            line += 1;
        }
        i += 1;
    }

    comments
}

// ===== 规则执行 =====

/// 对单个文件应用所有适用规则。AST 在此解析一次，供该文件的所有规则复用。
///
/// `filter` 用于按 CLI 传入的 id/分组筛选规则；`RuleFilter::default()` 表示不筛选。
///
/// `engine` 由调用方构建一次后传入复用，避免每条规则重建 Rhai 引擎。
///
/// `line_offset` 用于把规则产出的行号偏移到目标坐标系：
/// - 脚本模式传 `0`
/// - Mapper 模式传 `raw_xml_line - 1`（即 `<select>` 标签起始行号减 1），
///   使违规行号指向 XML 文件中的实际行
pub fn run_rules_for_file(
    engine: &Engine,
    file_path: &Path,
    sql_content: &str,
    script_type: &str,
    config: &Config,
    config_dir: &Path,
    filter: &RuleFilter,
    line_offset: usize,
) -> Result<Vec<Violation>, SqlGuardError> {
    let mut violations = Vec::new();

    let ast = parse_sql_to_ast(sql_content);

    let context = RuleContext {
        sql_content: sql_content.to_string(),
        file_path: file_path.to_string_lossy().to_string(),
        file_name: file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        script_type: script_type.to_string(),
        line_count: sql_content.lines().count(),
        ast,
    };

    for rule_config in &config.rules {
        if !rule_config.enabled {
            continue;
        }
        if !rule_config
            .applies_to
            .iter()
            .any(|t| t == script_type)
        {
            continue;
        }
        if !filter.matches(rule_config) {
            continue;
        }

        let script_path = config.resolve_script_path(&rule_config.script_path, config_dir);
        if !script_path.exists() {
            violations.push(Violation {
                rule_id: rule_config.id.clone(),
                rule_name: rule_config.name.clone(),
                rule_group: rule_config.group.clone(),
                severity: rule_config.severity.clone(),
                message: format!("Rule script not found: {}", script_path.display()),
                file_path: file_path.to_path_buf(),
                script_type: script_type.to_string(),
                line: None,
                end_line: None,
                column: None,
            });
            continue;
        }

        match run_single_rule(engine, &context, rule_config, &script_path, line_offset) {
            Ok(rule_violations) => violations.extend(rule_violations),
            Err(e) => {
                violations.push(Violation {
                    rule_id: rule_config.id.clone(),
                    rule_name: rule_config.name.clone(),
                    rule_group: rule_config.group.clone(),
                    severity: rule_config.severity.clone(),
                    message: format!("Rule execution error: {}", e),
                    file_path: file_path.to_path_buf(),
                    script_type: script_type.to_string(),
                    line: None,
                    end_line: None,
                    column: None,
                });
            }
        }
    }

    Ok(violations)
}

fn run_single_rule(
    engine: &Engine,
    context: &RuleContext,
    rule_config: &RuleConfig,
    script_path: &Path,
    line_offset: usize,
) -> Result<Vec<Violation>, SqlGuardError> {
    let script = fs::read_to_string(script_path).map_err(|e| {
        SqlGuardError::ScriptError(format!(
            "Failed to read script '{}': {}",
            script_path.display(),
            e
        ))
    })?;

    let mut scope = Scope::new();

    let mut ctx_map = Map::new();
    ctx_map.insert(
        "sql_content".into(),
        Dynamic::from(context.sql_content.clone()),
    );
    ctx_map.insert("file_path".into(), Dynamic::from(context.file_path.clone()));
    ctx_map.insert("file_name".into(), Dynamic::from(context.file_name.clone()));
    ctx_map.insert(
        "script_type".into(),
        Dynamic::from(context.script_type.clone()),
    );
    ctx_map.insert(
        "line_count".into(),
        Dynamic::from_int(context.line_count as i64),
    );
    ctx_map.insert("ast".into(), Dynamic::from(context.ast.clone()));
    scope.push("context", Dynamic::from(ctx_map));

    let violations_array: Array = Array::new();
    scope.push("violations", Dynamic::from(violations_array));

    let result = engine.eval_with_scope::<Dynamic>(&mut scope, &script);

    match result {
        Ok(_) => {
            let final_violations = scope.get_value::<Array>("violations").unwrap_or_default();

            let violations: Vec<Violation> = final_violations
                .iter()
                .map(|v| {
                    let (msg, line, column) = if v.is_string() {
                        (v.clone_cast::<String>(), None, None)
                    } else if v.is_map() {
                        let m = v.clone_cast::<Map>();
                        let message = m
                            .get("message")
                            .and_then(|d| d.clone().try_cast::<String>())
                            .unwrap_or_else(|| "Unknown violation".to_string());
                        let line = extract_int(&m, "line");
                        let column = extract_int(&m, "column");
                        (message, line, column)
                    } else {
                        (v.to_string(), None, None)
                    };
                    Violation {
                        rule_id: rule_config.id.clone(),
                        rule_name: rule_config.name.clone(),
                        rule_group: rule_config.group.clone(),
                        severity: rule_config.severity.clone(),
                        message: msg,
                        file_path: Path::new(&context.file_path).to_path_buf(),
                        script_type: context.script_type.clone(),
                        // 应用行号偏移：脚本模式 line_offset=0 无影响；
                        // Mapper 模式把规则产出的"SQL 内行号"映射到 XML 行号
                        line: line.map(|l| (l as usize) + line_offset),
                        // end_line 自动从 AST 回填：找到包含 violation.line 的语句范围。
                        // 此处先用 line_offset 映射 line 到目标坐标系，再回查 AST
                        // （AST 中的 line 是 SQL 内行号，所以回查用原始 line，映射后写入 end_line）
                        end_line: line.and_then(|l| {
                            context
                                .ast
                                .statement_range_at(l)
                                .map(|(_, end)| (end as usize) + line_offset)
                        }),
                        column: column.map(|c| c as usize),
                    }
                })
                .collect();

            Ok(violations)
        }
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.to_lowercase().contains("runtime error") || err_msg.contains("throw") {
                Ok(vec![Violation {
                    rule_id: rule_config.id.clone(),
                    rule_name: rule_config.name.clone(),
                    rule_group: rule_config.group.clone(),
                    severity: rule_config.severity.clone(),
                    message: err_msg,
                    file_path: Path::new(&context.file_path).to_path_buf(),
                    script_type: context.script_type.clone(),
                    line: None,
                    end_line: None,
                    column: None,
                }])
            } else {
                Err(SqlGuardError::ScriptError(err_msg))
            }
        }
    }
}

/// 从 Rhai Map 中提取整数字段。Rhai 默认 INT 为 i64，开启 `only_i32` 时为 i32，这里都兼容。
fn extract_int(m: &Map, key: &str) -> Option<i64> {
    let d = m.get(key)?;
    d.clone()
        .try_cast::<i64>()
        .or_else(|| d.clone().try_cast::<i32>().map(|i| i as i64))
}

/// 构建一个 Rhai 引擎并注册所有 AST 包装类型的方法。
///
/// 引擎本身可重入，注册方法只在构建时执行一次；
/// 调用方应在外层构建一次，传 `&Engine` 给 [`run_rules_for_file`] 复用，
/// 避免每个文件/每条规则都重建引擎。
pub fn build_engine() -> Engine {
    let mut engine = Engine::new();

    // 注册所有自定义类型，使 Rhai 能正确识别和迭代包含它们的 Array
    engine.register_type_with_name::<SqlAst>("SqlAst");
    engine.register_type_with_name::<StmtInfo>("StmtInfo");
    engine.register_type_with_name::<CreateInfo>("CreateInfo");
    engine.register_type_with_name::<DropInfo>("DropInfo");
    engine.register_type_with_name::<SelectInfo>("SelectInfo");
    engine.register_type_with_name::<ColumnInfo>("ColumnInfo");
    engine.register_type_with_name::<InsertInfo>("InsertInfo");
    engine.register_type_with_name::<UpdateInfo>("UpdateInfo");
    engine.register_type_with_name::<DeleteInfo>("DeleteInfo");
    engine.register_type_with_name::<JoinInfo>("JoinInfo");
    engine.register_type_with_name::<AlterTableInfo>("AlterTableInfo");
    engine.register_type_with_name::<AlterOpInfo>("AlterOpInfo");
    engine.register_type_with_name::<TruncateInfo>("TruncateInfo");
    engine.register_type_with_name::<ViewInfo>("ViewInfo");
    engine.register_type_with_name::<CreateIndexInfo>("CreateIndexInfo");
    engine.register_type_with_name::<TransactionInfo>("TransactionInfo");
    engine.register_type_with_name::<CteInfo>("CteInfo");
    engine.register_type_with_name::<WindowFuncInfo>("WindowFuncInfo");
    engine.register_type_with_name::<ExprInfo>("ExprInfo");
    engine.register_type_with_name::<ForeignKeyInfo>("ForeignKeyInfo");
    engine.register_type_with_name::<CheckInfo>("CheckInfo");
    engine.register_type_with_name::<IndexInfo>("IndexInfo");
    engine.register_type_with_name::<UniqueInfo>("UniqueInfo");
    engine.register_type_with_name::<CommentInfo>("CommentInfo");

    // SqlAst 方法
    // 返回 Vec<CustomType> 的闭包需显式转为 Array，否则 Rhai for 循环无法迭代
    engine.register_fn("statements", |ast: &mut SqlAst| -> Array {
        ast.statements().into_iter().map(|s| Dynamic::from(s)).collect()
    });
    engine.register_fn("has_parse_error", |ast: &mut SqlAst| ast.has_parse_error());
    engine.register_fn("parse_error", |ast: &mut SqlAst| ast.parse_error());
    engine.register_fn("kinds", |ast: &mut SqlAst| -> Array {
        ast.kinds().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("create_tables", |ast: &mut SqlAst| -> Array {
        ast.create_tables().into_iter().map(|c| Dynamic::from(c)).collect()
    });
    engine.register_fn("drop_objects", |ast: &mut SqlAst| -> Array {
        ast.drop_objects().into_iter().map(|d| Dynamic::from(d)).collect()
    });
    engine.register_fn("selects", |ast: &mut SqlAst| -> Array {
        ast.selects().into_iter().map(|s| Dynamic::from(s)).collect()
    });
    engine.register_fn("has_create_table", |ast: &mut SqlAst| ast.has_create_table());
    engine.register_fn("has_drop_table", |ast: &mut SqlAst| ast.has_drop_table());
    engine.register_fn("has_comma_join_anywhere", |ast: &mut SqlAst| {
        ast.has_comma_join_anywhere()
    });
    engine.register_fn("comments", |ast: &mut SqlAst| -> Array {
        ast.comments()
            .into_iter()
            .map(|c| Dynamic::from(c))
            .collect()
    });

    // StmtInfo 方法
    engine.register_fn("kind", |s: &mut StmtInfo| s.kind());
    engine.register_fn("line", |s: &mut StmtInfo| s.line());
    engine.register_fn("end_line", |s: &mut StmtInfo| s.end_line());
    engine.register_fn("line_range", |s: &mut StmtInfo| s.line_range());
    engine.register_fn("column", |s: &mut StmtInfo| s.column());
    engine.register_fn("has_create_table", |s: &mut StmtInfo| s.has_create_table());
    engine.register_fn("has_drop_object", |s: &mut StmtInfo| s.has_drop_object());
    engine.register_fn("has_select", |s: &mut StmtInfo| s.has_select());
    engine.register_fn("has_insert", |s: &mut StmtInfo| s.has_insert());
    engine.register_fn("has_update", |s: &mut StmtInfo| s.has_update());
    engine.register_fn("has_delete", |s: &mut StmtInfo| s.has_delete());
    engine.register_fn("has_alter_table", |s: &mut StmtInfo| s.has_alter_table());
    engine.register_fn("has_truncate", |s: &mut StmtInfo| s.has_truncate());
    engine.register_fn("has_create_view", |s: &mut StmtInfo| s.has_create_view());
    engine.register_fn("has_create_index", |s: &mut StmtInfo| s.has_create_index());
    engine.register_fn("has_transaction", |s: &mut StmtInfo| s.has_transaction());
    engine.register_fn("create_table", |s: &mut StmtInfo| s.create_table());
    engine.register_fn("drop_object", |s: &mut StmtInfo| s.drop_object());
    engine.register_fn("select", |s: &mut StmtInfo| s.select());
    engine.register_fn("insert", |s: &mut StmtInfo| s.insert());
    engine.register_fn("update", |s: &mut StmtInfo| s.update());
    engine.register_fn("delete", |s: &mut StmtInfo| s.delete());
    engine.register_fn("alter_table", |s: &mut StmtInfo| s.alter_table());
    engine.register_fn("truncate", |s: &mut StmtInfo| s.truncate());
    engine.register_fn("create_view", |s: &mut StmtInfo| s.create_view());
    engine.register_fn("create_index", |s: &mut StmtInfo| s.create_index());
    engine.register_fn("transaction", |s: &mut StmtInfo| s.transaction());

    // CreateInfo 方法
    engine.register_fn("table_name", |c: &mut CreateInfo| c.table_name());
    engine.register_fn("columns", |c: &mut CreateInfo| -> Array {
        c.columns().into_iter().map(|col| Dynamic::from(col)).collect()
    });
    engine.register_fn("has_primary_key", |c: &mut CreateInfo| c.has_primary_key());
    engine.register_fn("if_not_exists", |c: &mut CreateInfo| c.if_not_exists());
    engine.register_fn("column_names", |c: &mut CreateInfo| -> Array {
        c.column_names().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("foreign_keys", |c: &mut CreateInfo| -> Array {
        c.foreign_keys()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("checks", |c: &mut CreateInfo| -> Array {
        c.checks().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("indexes", |c: &mut CreateInfo| -> Array {
        c.indexes().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("uniques", |c: &mut CreateInfo| -> Array {
        c.uniques().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("has_foreign_key", |c: &mut CreateInfo| c.has_foreign_key());
    engine.register_fn("has_check", |c: &mut CreateInfo| c.has_check());
    engine.register_fn("has_index", |c: &mut CreateInfo| c.has_index());
    engine.register_fn("has_unique", |c: &mut CreateInfo| c.has_unique());

    // ColumnInfo 方法
    engine.register_fn("name", |c: &mut ColumnInfo| c.name());
    engine.register_fn("data_type", |c: &mut ColumnInfo| c.data_type());
    engine.register_fn("is_primary_key", |c: &mut ColumnInfo| c.is_primary_key());
    engine.register_fn("is_not_null", |c: &mut ColumnInfo| c.is_not_null());
    engine.register_fn("is_unique", |c: &mut ColumnInfo| c.is_unique());
    engine.register_fn("default_value", |c: &mut ColumnInfo| c.default_value());
    engine.register_fn("has_default_value", |c: &mut ColumnInfo| c.has_default_value());
    engine.register_fn("is_auto_increment", |c: &mut ColumnInfo| c.is_auto_increment());
    engine.register_fn("comment", |c: &mut ColumnInfo| c.comment());
    engine.register_fn("has_comment", |c: &mut ColumnInfo| c.has_comment());
    engine.register_fn("has_check", |c: &mut ColumnInfo| c.has_check());
    engine.register_fn("references_table", |c: &mut ColumnInfo| c.references_table());
    engine.register_fn("has_foreign_key", |c: &mut ColumnInfo| c.has_foreign_key());
    engine.register_fn("line", |c: &mut ColumnInfo| c.line());
    engine.register_fn("column", |c: &mut ColumnInfo| c.column());

    // DropInfo 方法
    engine.register_fn("object_type", |d: &mut DropInfo| d.object_type());
    engine.register_fn("name", |d: &mut DropInfo| d.name());
    engine.register_fn("if_exists", |d: &mut DropInfo| d.if_exists());

    // SelectInfo 方法
    engine.register_fn("has_wildcard", |s: &mut SelectInfo| s.has_wildcard());
    engine.register_fn("projection", |s: &mut SelectInfo| -> Array {
        s.projection().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("has_from_table", |s: &mut SelectInfo| s.has_from_table());
    engine.register_fn("from_table", |s: &mut SelectInfo| s.from_table());
    engine.register_fn("from_table_alias", |s: &mut SelectInfo| s.from_table_alias());
    engine.register_fn("has_from_table_alias", |s: &mut SelectInfo| s.has_from_table_alias());
    engine.register_fn("joins", |s: &mut SelectInfo| -> Array {
        s.joins().into_iter().map(|j| Dynamic::from(j)).collect()
    });
    engine.register_fn("has_joins", |s: &mut SelectInfo| s.has_joins());
    engine.register_fn("has_cross_join", |s: &mut SelectInfo| s.has_cross_join());
    engine.register_fn(
        "has_join_without_condition",
        |s: &mut SelectInfo| s.has_join_without_condition(),
    );
    engine.register_fn("has_subquery_in_from", |s: &mut SelectInfo| s.has_subquery_in_from());
    engine.register_fn("from_subquery_has_alias", |s: &mut SelectInfo| s.from_subquery_has_alias());
    engine.register_fn("has_unqualified_column", |s: &mut SelectInfo| s.has_unqualified_column());
    engine.register_fn("is_union", |s: &mut SelectInfo| s.is_union());
    engine.register_fn("is_union_all", |s: &mut SelectInfo| s.is_union_all());
    engine.register_fn("is_intersect", |s: &mut SelectInfo| s.is_intersect());
    engine.register_fn("is_except", |s: &mut SelectInfo| s.is_except());
    engine.register_fn("has_where", |s: &mut SelectInfo| s.has_where());
    engine.register_fn("where_clause", |s: &mut SelectInfo| s.where_clause());
    engine.register_fn("has_group_by", |s: &mut SelectInfo| s.has_group_by());
    engine.register_fn("has_having", |s: &mut SelectInfo| s.has_having());
    engine.register_fn("has_qualify", |s: &mut SelectInfo| s.has_qualify());
    engine.register_fn("has_order_by", |s: &mut SelectInfo| s.has_order_by());
    engine.register_fn("has_limit", |s: &mut SelectInfo| s.has_limit());
    engine.register_fn("has_offset", |s: &mut SelectInfo| s.has_offset());
    engine.register_fn("has_fetch", |s: &mut SelectInfo| s.has_fetch());
    engine.register_fn("has_distinct", |s: &mut SelectInfo| s.has_distinct());
    engine.register_fn("ctes", |s: &mut SelectInfo| -> Array {
        s.ctes().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("has_cte", |s: &mut SelectInfo| s.has_cte());
    engine.register_fn("is_recursive", |s: &mut SelectInfo| s.is_recursive());
    engine.register_fn("subqueries", |s: &mut SelectInfo| -> Array {
        s.subqueries()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("has_subquery", |s: &mut SelectInfo| s.has_subquery());
    engine.register_fn("has_window_function", |s: &mut SelectInfo| s.has_window_function());
    engine.register_fn("window_functions", |s: &mut SelectInfo| -> Array {
        s.window_functions()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("where_expr", |s: &mut SelectInfo| s.where_expr());
    engine.register_fn("has_where_expr", |s: &mut SelectInfo| s.has_where_expr());
    engine.register_fn("having_expr", |s: &mut SelectInfo| s.having_expr());
    engine.register_fn("has_having_expr", |s: &mut SelectInfo| s.has_having_expr());
    engine.register_fn("projection_exprs", |s: &mut SelectInfo| -> Array {
        s.projection_exprs()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("has_comma_join", |s: &mut SelectInfo| s.has_comma_join());

    // JoinInfo 方法
    engine.register_fn("table_name", |j: &mut JoinInfo| j.table_name());
    engine.register_fn("join_type", |j: &mut JoinInfo| j.join_type());
    engine.register_fn("has_condition", |j: &mut JoinInfo| j.has_condition());
    engine.register_fn("alias", |j: &mut JoinInfo| j.alias());
    engine.register_fn("has_alias", |j: &mut JoinInfo| j.has_alias());
    engine.register_fn("condition_text", |j: &mut JoinInfo| j.condition_text());
    engine.register_fn("has_condition_text", |j: &mut JoinInfo| j.has_condition_text());
    engine.register_fn("line", |j: &mut JoinInfo| j.line());
    engine.register_fn("column", |j: &mut JoinInfo| j.column());

    // InsertInfo 方法
    engine.register_fn("table_name", |i: &mut InsertInfo| i.table_name());
    engine.register_fn("columns", |i: &mut InsertInfo| -> Array {
        i.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("has_columns", |i: &mut InsertInfo| i.has_columns());

    // UpdateInfo 方法
    engine.register_fn("table_name", |u: &mut UpdateInfo| u.table_name());
    engine.register_fn("has_where", |u: &mut UpdateInfo| u.has_where());
    engine.register_fn("where_clause", |u: &mut UpdateInfo| u.where_clause());

    // DeleteInfo 方法
    engine.register_fn("table_name", |d: &mut DeleteInfo| d.table_name());
    engine.register_fn("has_where", |d: &mut DeleteInfo| d.has_where());
    engine.register_fn("where_clause", |d: &mut DeleteInfo| d.where_clause());

    // AlterTableInfo 方法
    engine.register_fn("table_name", |a: &mut AlterTableInfo| a.table_name());
    engine.register_fn("adds_primary_key", |a: &mut AlterTableInfo| a.adds_primary_key());
    engine.register_fn("drops_primary_key", |a: &mut AlterTableInfo| a.drops_primary_key());
    engine.register_fn("operations", |a: &mut AlterTableInfo| -> Array {
        a.operations()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });

    // AlterOpInfo 方法
    engine.register_fn("operation_type", |a: &mut AlterOpInfo| a.operation_type());
    engine.register_fn("column_name", |a: &mut AlterOpInfo| a.column_name());
    engine.register_fn("table_name", |a: &mut AlterOpInfo| a.table_name());
    engine.register_fn("constraint_name", |a: &mut AlterOpInfo| a.constraint_name());
    engine.register_fn("detail", |a: &mut AlterOpInfo| a.detail());

    // TruncateInfo 方法
    engine.register_fn("table_name", |t: &mut TruncateInfo| t.table_name());
    engine.register_fn("has_table_keyword", |t: &mut TruncateInfo| t.has_table_keyword());

    // ViewInfo 方法
    engine.register_fn("name", |v: &mut ViewInfo| v.name());
    engine.register_fn("materialized", |v: &mut ViewInfo| v.materialized());
    engine.register_fn("is_replace", |v: &mut ViewInfo| v.is_replace());
    engine.register_fn("column_count", |v: &mut ViewInfo| v.column_count());

    // CreateIndexInfo 方法
    engine.register_fn("name", |c: &mut CreateIndexInfo| c.name());
    engine.register_fn("table_name", |c: &mut CreateIndexInfo| c.table_name());
    engine.register_fn("columns", |c: &mut CreateIndexInfo| -> Array {
        c.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("is_unique", |c: &mut CreateIndexInfo| c.is_unique());

    // TransactionInfo 方法
    engine.register_fn("kind", |t: &mut TransactionInfo| t.kind());

    // CteInfo 方法
    engine.register_fn("name", |c: &mut CteInfo| c.name());
    engine.register_fn("column_count", |c: &mut CteInfo| c.column_count());
    engine.register_fn("is_recursive", |c: &mut CteInfo| c.is_recursive());

    // WindowFuncInfo 方法
    engine.register_fn("function_name", |w: &mut WindowFuncInfo| w.function_name());
    engine.register_fn("has_partition_by", |w: &mut WindowFuncInfo| w.has_partition_by());
    engine.register_fn("has_order_by", |w: &mut WindowFuncInfo| w.has_order_by());
    engine.register_fn("has_window_frame", |w: &mut WindowFuncInfo| w.has_window_frame());

    // ExprInfo 方法
    engine.register_fn("kind", |e: &mut ExprInfo| e.kind());
    engine.register_fn("text", |e: &mut ExprInfo| e.text());
    engine.register_fn("function_name", |e: &mut ExprInfo| e.function_name());
    engine.register_fn("is_literal", |e: &mut ExprInfo| e.is_literal());
    engine.register_fn("is_column", |e: &mut ExprInfo| e.is_column());
    engine.register_fn("is_subquery", |e: &mut ExprInfo| e.is_subquery());
    engine.register_fn("operator", |e: &mut ExprInfo| e.operator());
    engine.register_fn("column_name", |e: &mut ExprInfo| e.column_name());
    engine.register_fn("has_null_test", |e: &mut ExprInfo| e.has_null_test());
    engine.register_fn("line", |e: &mut ExprInfo| e.line());
    engine.register_fn("column", |e: &mut ExprInfo| e.column());

    // ForeignKeyInfo 方法
    engine.register_fn("name", |f: &mut ForeignKeyInfo| f.name());
    engine.register_fn("columns", |f: &mut ForeignKeyInfo| -> Array {
        f.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("foreign_table", |f: &mut ForeignKeyInfo| f.foreign_table());
    engine.register_fn("referred_columns", |f: &mut ForeignKeyInfo| -> Array {
        f.referred_columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("on_delete", |f: &mut ForeignKeyInfo| f.on_delete());
    engine.register_fn("on_update", |f: &mut ForeignKeyInfo| f.on_update());
    engine.register_fn("line", |f: &mut ForeignKeyInfo| f.line());
    engine.register_fn("column", |f: &mut ForeignKeyInfo| f.column());

    // CheckInfo 方法
    engine.register_fn("name", |c: &mut CheckInfo| c.name());
    engine.register_fn("expr_text", |c: &mut CheckInfo| c.expr_text());
    engine.register_fn("line", |c: &mut CheckInfo| c.line());
    engine.register_fn("column", |c: &mut CheckInfo| c.column());

    // IndexInfo 方法
    engine.register_fn("name", |i: &mut IndexInfo| i.name());
    engine.register_fn("columns", |i: &mut IndexInfo| -> Array {
        i.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("is_unique", |i: &mut IndexInfo| i.is_unique());
    engine.register_fn("line", |i: &mut IndexInfo| i.line());
    engine.register_fn("column", |i: &mut IndexInfo| i.column());

    // UniqueInfo 方法
    engine.register_fn("name", |u: &mut UniqueInfo| u.name());
    engine.register_fn("columns", |u: &mut UniqueInfo| -> Array {
        u.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("line", |u: &mut UniqueInfo| u.line());
    engine.register_fn("column", |u: &mut UniqueInfo| u.column());

    // CommentInfo 方法
    engine.register_fn("text", |c: &mut CommentInfo| c.text());
    engine.register_fn("line", |c: &mut CommentInfo| c.line());
    engine.register_fn("kind", |c: &mut CommentInfo| c.kind());

    engine
}
