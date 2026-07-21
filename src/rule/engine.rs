use std::fs;
use std::path::Path;

use rhai::{Array, Dynamic, Engine, Map, Scope};
use sqlparser::ast::{
    ColumnOption, FromTable, JoinConstraint, JoinOperator, Query, SelectItem, SetExpr, Statement,
    TableConstraint, TableFactor,
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
}

/// 单条 SQL 语句的信息。
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
}

/// CREATE TABLE 语句信息。
#[derive(Debug, Clone, Default)]
pub struct CreateInfo {
    pub table_name: String,
    pub columns: Vec<ColumnInfo>,
    pub has_primary_key: bool,
    pub if_not_exists: bool,
}

/// 单个列定义信息。
#[derive(Debug, Clone, Default)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_primary_key: bool,
    pub is_not_null: bool,
    pub is_unique: bool,
}

/// DROP 语句信息。
#[derive(Debug, Clone, Default)]
pub struct DropInfo {
    pub object_type: String,
    pub name: String,
    pub if_exists: bool,
}

/// JOIN 子句信息。
#[derive(Debug, Clone, Default)]
pub struct JoinInfo {
    pub table_name: String,
    pub join_type: String,
    pub has_condition: bool,
}

/// SELECT 语句信息。
#[derive(Debug, Clone, Default)]
pub struct SelectInfo {
    pub has_wildcard: bool,
    pub projection: Vec<String>,
    pub from_table: Option<String>,
    pub joins: Vec<JoinInfo>,
    pub has_subquery_in_from: bool,
    pub from_subquery_has_alias: bool,
    pub has_unqualified_column: bool,
    pub union: bool,
    pub union_all: bool,
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
    }
}

fn convert_statement(stmt: &Statement, line: i64, column: i64, end_line: i64) -> StmtInfo {
    match stmt {
        Statement::CreateTable { name, columns, constraints, if_not_exists, .. } => {
            let table_name = name.to_string();
            let mut cols = Vec::new();
            let mut pk_in_column = false;

            for col_def in columns {
                let mut info = ColumnInfo {
                    name: col_def.name.to_string(),
                    data_type: col_def.data_type.to_string(),
                    is_primary_key: false,
                    is_not_null: false,
                    is_unique: false,
                };
                for opt in &col_def.options {
                    match &opt.option {
                        ColumnOption::NotNull => info.is_not_null = true,
                        ColumnOption::Unique { is_primary, .. } => {
                            info.is_unique = true;
                            if *is_primary {
                                info.is_primary_key = true;
                                pk_in_column = true;
                            }
                        }
                        _ => {}
                    }
                }
                cols.push(info);
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
                }),
                drop_object: None,
                select: None,
                insert: None,
                update: None,
                delete: None,
            }
        }
        Statement::Drop { object_type, if_exists, names, .. } => {
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
            }
        }
        Statement::Insert { table_name, columns, .. } => {
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
            }
        }
        Statement::Update { table, selection, .. } => {
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
            }
        }
        Statement::Delete { tables, from, selection, .. } => {
            // 优先取 `tables`（MySQL 多表 DELETE），否则从 `from` 取首个表
            let table_name = if !tables.is_empty() {
                tables.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
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
            }
        }
        Statement::AlterTable { .. } => StmtInfo {
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
        },
    }
}

fn analyze_query(q: &Query) -> SelectInfo {
    let mut info = SelectInfo {
        has_wildcard: false,
        projection: Vec::new(),
        from_table: None,
        joins: Vec::new(),
        has_subquery_in_from: false,
        from_subquery_has_alias: true,
        has_unqualified_column: false,
        union: false,
        union_all: false,
    };

    // 检查是否为 UNION 查询
    match &*q.body {
        SetExpr::SetOperation {
            op, ..
        } => {
            info.union = true;
            let op_str = format!("{:?}", op).to_uppercase();
            info.union_all = op_str.contains("ALL");
        }
        _ => {}
    }

    if let SetExpr::Select(s) = &*q.body {
        let has_multiple_tables = s.from.len() > 1
            || s.from.iter().any(|t| !t.joins.is_empty());

        for item in &s.projection {
            match item {
                SelectItem::Wildcard(_) => info.has_wildcard = true,
                SelectItem::QualifiedWildcard(_, _) => info.has_wildcard = true,
                SelectItem::UnnamedExpr(expr) => {
                    let text = expr.to_string();
                    info.projection.push(text.clone());
                    if has_multiple_tables && !text.contains('.') {
                        if matches!(expr, sqlparser::ast::Expr::Identifier(_)) {
                            info.has_unqualified_column = true;
                        }
                    }
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    let text = format!("{} AS {}", expr, alias);
                    info.projection.push(text);
                    if has_multiple_tables && !expr.to_string().contains('.') {
                        info.has_unqualified_column = true;
                    }
                }
            }
        }

        // 分析 FROM 子句
        for table_with_joins in &s.from {
            // 分析主表
            match &table_with_joins.relation {
                TableFactor::Table { name, .. } => {
                    if info.from_table.is_none() {
                        info.from_table = Some(name.to_string());
                    }
                }
                TableFactor::Derived { alias, .. } => {
                    info.has_subquery_in_from = true;
                    if alias.is_none() {
                        info.from_subquery_has_alias = false;
                    }
                }
                _ => {}
            }

            // 分析 JOIN
            for join in &table_with_joins.joins {
                let (table_name, join_type, has_condition) = match &join.join_operator {
                    JoinOperator::Inner(constraint) | JoinOperator::LeftOuter(constraint)
                    | JoinOperator::RightOuter(constraint) | JoinOperator::FullOuter(constraint) => {
                        let table_name = match &join.relation {
                            TableFactor::Table { name, .. } => name.to_string(),
                            _ => join.relation.to_string(),
                        };
                        let join_type = match &join.join_operator {
                            JoinOperator::Inner(_) => "INNER",
                            JoinOperator::LeftOuter(_) => "LEFT",
                            JoinOperator::RightOuter(_) => "RIGHT",
                            JoinOperator::FullOuter(_) => "FULL",
                            _ => "OTHER",
                        }.to_string();
                        let has_condition = match constraint {
                            JoinConstraint::On(_) | JoinConstraint::Using(_) | JoinConstraint::Natural => true,
                            JoinConstraint::None => false,
                        };
                        (table_name, join_type, has_condition)
                    }
                    JoinOperator::CrossJoin => {
                        let table_name = match &join.relation {
                            TableFactor::Table { name, .. } => name.to_string(),
                            _ => join.relation.to_string(),
                        };
                        (table_name, "CROSS".to_string(), true)
                    }
                    _ => {
                        let table_name = match &join.relation {
                            TableFactor::Table { name, .. } => name.to_string(),
                            _ => join.relation.to_string(),
                        };
                        (table_name, "OTHER".to_string(), false)
                    }
                };

                info.joins.push(JoinInfo {
                    table_name,
                    join_type,
                    has_condition,
                });
            }
        }
    }
    info
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
                        (
                            v.clone_cast::<String>(),
                            None,
                            None,
                        )
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
                            context.ast.statement_range_at(l).map(|(_, end)| {
                                (end as usize) + line_offset
                            })
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

    // SqlAst 方法
    // 返回 Vec<CustomType> 的闭包需显式转为 Array，否则 Rhai for 循环无法迭代
    engine.register_fn("statements", |ast: &mut SqlAst| -> Array {
        ast.statements().into_iter().map(|s| Dynamic::from(s)).collect()
    });
    engine.register_fn("has_parse_error", |ast: &mut SqlAst| ast.has_parse_error());
    engine.register_fn("parse_error", |ast: &mut SqlAst| ast.parse_error());
    engine.register_fn("kinds", |ast: &mut SqlAst| ast.kinds());
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
    engine.register_fn("create_table", |s: &mut StmtInfo| s.create_table());
    engine.register_fn("drop_object", |s: &mut StmtInfo| s.drop_object());
    engine.register_fn("select", |s: &mut StmtInfo| s.select());
    engine.register_fn("insert", |s: &mut StmtInfo| s.insert());
    engine.register_fn("update", |s: &mut StmtInfo| s.update());
    engine.register_fn("delete", |s: &mut StmtInfo| s.delete());

    // CreateInfo 方法
    engine.register_fn("table_name", |c: &mut CreateInfo| c.table_name());
    engine.register_fn("columns", |c: &mut CreateInfo| -> Array {
        c.columns().into_iter().map(|col| Dynamic::from(col)).collect()
    });
    engine.register_fn("has_primary_key", |c: &mut CreateInfo| c.has_primary_key());
    engine.register_fn("if_not_exists", |c: &mut CreateInfo| c.if_not_exists());
    engine.register_fn("column_names", |c: &mut CreateInfo| c.column_names());

    // ColumnInfo 方法
    engine.register_fn("name", |c: &mut ColumnInfo| c.name());
    engine.register_fn("data_type", |c: &mut ColumnInfo| c.data_type());
    engine.register_fn("is_primary_key", |c: &mut ColumnInfo| c.is_primary_key());
    engine.register_fn("is_not_null", |c: &mut ColumnInfo| c.is_not_null());
    engine.register_fn("is_unique", |c: &mut ColumnInfo| c.is_unique());

    // DropInfo 方法
    engine.register_fn("object_type", |d: &mut DropInfo| d.object_type());
    engine.register_fn("name", |d: &mut DropInfo| d.name());
    engine.register_fn("if_exists", |d: &mut DropInfo| d.if_exists());

    // SelectInfo 方法
    engine.register_fn("has_wildcard", |s: &mut SelectInfo| s.has_wildcard());
    engine.register_fn("projection", |s: &mut SelectInfo| s.projection());
    engine.register_fn("has_from_table", |s: &mut SelectInfo| s.has_from_table());
    engine.register_fn("from_table", |s: &mut SelectInfo| s.from_table());
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

    // JoinInfo 方法
    engine.register_fn("table_name", |j: &mut JoinInfo| j.table_name());
    engine.register_fn("join_type", |j: &mut JoinInfo| j.join_type());
    engine.register_fn("has_condition", |j: &mut JoinInfo| j.has_condition());

    // InsertInfo 方法
    engine.register_fn("table_name", |i: &mut InsertInfo| i.table_name());
    engine.register_fn("columns", |i: &mut InsertInfo| i.columns());
    engine.register_fn("has_columns", |i: &mut InsertInfo| i.has_columns());

    // UpdateInfo 方法
    engine.register_fn("table_name", |u: &mut UpdateInfo| u.table_name());
    engine.register_fn("has_where", |u: &mut UpdateInfo| u.has_where());
    engine.register_fn("where_clause", |u: &mut UpdateInfo| u.where_clause());

    // DeleteInfo 方法
    engine.register_fn("table_name", |d: &mut DeleteInfo| d.table_name());
    engine.register_fn("has_where", |d: &mut DeleteInfo| d.has_where());
    engine.register_fn("where_clause", |d: &mut DeleteInfo| d.where_clause());

    engine
}
