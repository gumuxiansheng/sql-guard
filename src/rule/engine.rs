use std::fs;
use std::path::Path;

use rhai::{Array, Dynamic, Engine, Map, Scope};
use sqlparser::ast::{
    ColumnOption, Query, SelectItem, SetExpr, Statement, TableConstraint, TableFactor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

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
        // 1. 黑名单优先：命中即排除
        if matches_any(&self.exclude_rules, &rule.id) {
            return false;
        }
        if let Some(g) = &rule.group {
            if matches_any(&self.exclude_groups, g) {
                return false;
            }
        }

        // 2. 白名单为空表示不限制
        let in_rules = self.include_rules.is_empty() || matches_any(&self.include_rules, &rule.id);
        let in_groups = self.include_groups.is_empty()
            || rule
                .group
                .as_ref()
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
    pub column: i64,
    pub create_table: Option<CreateInfo>,
    pub drop_object: Option<DropInfo>,
    pub select: Option<SelectInfo>,
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

/// SELECT 语句信息。
#[derive(Debug, Clone, Default)]
pub struct SelectInfo {
    pub has_wildcard: bool,
    pub projection: Vec<String>,
    pub from_table: Option<String>,
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
}

impl StmtInfo {
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
    pub fn line(&self) -> i64 {
        self.line
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
    pub fn create_table(&self) -> CreateInfo {
        self.create_table.clone().unwrap_or_default()
    }
    pub fn drop_object(&self) -> DropInfo {
        self.drop_object.clone().unwrap_or_default()
    }
    pub fn select(&self) -> SelectInfo {
        self.select.clone().unwrap_or_default()
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
}

// ===== SQL 解析 =====

/// 把 SQL 文本解析为 AST。解析失败时返回带 `parse_error` 的空 AST，
/// 不影响后续规则运行（规则可选择是否上报解析错误）。
fn parse_sql_to_ast(sql: &str) -> SqlAst {
    let dialect = GenericDialect {};
    match Parser::parse_sql_with_offsets(&dialect, sql) {
        Ok(stmts) => {
            let statements: Vec<StmtInfo> = stmts
                .iter()
                .map(|(stmt, (line, col))| convert_statement(stmt, *line as i64, *col as i64))
                .collect();
            SqlAst {
                statements,
                parse_error: None,
            }
        }
        Err(e) => SqlAst {
            statements: Vec::new(),
            parse_error: Some(format!("SQL parse error: {}", e)),
        },
    }
}

fn convert_statement(stmt: &Statement, line: i64, column: i64) -> StmtInfo {
    match stmt {
        Statement::CreateTable(c) => {
            let table_name = c.name.to_string();
            let mut columns = Vec::new();
            let mut pk_in_column = false;

            for col_def in &c.columns {
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
                        ColumnOption::PrimaryKey => {
                            info.is_primary_key = true;
                            pk_in_column = true;
                        }
                        ColumnOption::Unique => {
                            info.is_unique = true;
                        }
                        _ => {}
                    }
                }
                columns.push(info);
            }

            let pk_in_constraint = c
                .constraints
                .iter()
                .any(|con| matches!(con, TableConstraint::PrimaryKey { .. }));

            StmtInfo {
                kind: "CREATE_TABLE".to_string(),
                line,
                column,
                create_table: Some(CreateInfo {
                    table_name,
                    columns,
                    has_primary_key: pk_in_column || pk_in_constraint,
                    if_not_exists: c.if_not_exists,
                }),
                drop_object: None,
                select: None,
            }
        }
        Statement::Drop(d) => {
            let object_type = format!("{:?}", d.object_type).to_uppercase();
            let names: Vec<String> = d.names.iter().map(|n| n.to_string()).collect();
            let kind = format!("DROP_{}", object_type);
            StmtInfo {
                kind,
                line,
                column,
                create_table: None,
                drop_object: Some(DropInfo {
                    object_type,
                    name: names.join(", "),
                    if_exists: d.if_exists,
                }),
                select: None,
            }
        }
        Statement::Query(q) => {
            // `q` 是 `&Box<Query>` 或 `&Query`，自动多引用解引用到 `&Query`。
            let info = analyze_query(q);
            StmtInfo {
                kind: "SELECT".to_string(),
                line,
                column,
                create_table: None,
                drop_object: None,
                select: Some(info),
            }
        }
        Statement::Insert { .. } => StmtInfo {
            kind: "INSERT".to_string(),
            line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
        },
        Statement::Update { .. } => StmtInfo {
            kind: "UPDATE".to_string(),
            line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
        },
        Statement::Delete { .. } => StmtInfo {
            kind: "DELETE".to_string(),
            line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
        },
        Statement::AlterTable { .. } => StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
        },
        _ => StmtInfo {
            kind: "OTHER".to_string(),
            line,
            column,
            create_table: None,
            drop_object: None,
            select: None,
        },
    }
}

fn analyze_query(q: &Query) -> SelectInfo {
    let mut info = SelectInfo {
        has_wildcard: false,
        projection: Vec::new(),
        from_table: None,
    };
    if let SetExpr::Select(s) = &*q.body {
        for item in &s.projection {
            match item {
                SelectItem::Wildcard => info.has_wildcard = true,
                SelectItem::QualifiedWildcard(_) => info.has_wildcard = true,
                SelectItem::UnnamedExpr(expr) => info.projection.push(expr.to_string()),
                SelectItem::ExprWithAlias(expr, alias) => {
                    info.projection.push(format!("{} AS {}", expr, alias));
                }
            }
        }
        if let Some(first_from) = s.from.first() {
            if let TableFactor::Table { name, .. } = &first_from.relation {
                info.from_table = Some(name.to_string());
            }
        }
    }
    info
}

// ===== 规则执行 =====

/// 对单个文件应用所有适用规则。AST 在此解析一次，供该文件的所有规则复用。
///
/// `filter` 用于按 CLI 传入的 id/分组筛选规则；`RuleFilter::default()` 表示不筛选。
pub fn run_rules_for_file(
    file_path: &Path,
    sql_content: &str,
    script_type: &str,
    config: &Config,
    config_dir: &Path,
    filter: &RuleFilter,
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
                column: None,
            });
            continue;
        }

        match run_single_rule(&context, rule_config, &script_path) {
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
                    column: None,
                });
            }
        }
    }

    Ok(violations)
}

fn run_single_rule(
    context: &RuleContext,
    rule_config: &RuleConfig,
    script_path: &Path,
) -> Result<Vec<Violation>, SqlGuardError> {
    let script = fs::read_to_string(script_path).map_err(|e| {
        SqlGuardError::ScriptError(format!(
            "Failed to read script '{}': {}",
            script_path.display(),
            e
        )
    })?;

    let mut engine = build_engine();

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
                        line: line.map(|l| l as usize),
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
/// 每个文件每条规则都会调用一次，因为 Rhai 引擎本身很轻量；
/// 如有性能需求，后续可缓存 AST 包装类型的注册（用 `Engine::register_type_with_name`）。
fn build_engine() -> Engine {
    let mut engine = Engine::new();

    // SqlAst 方法
    engine.register_fn("statements", |ast: &mut SqlAst| ast.statements());
    engine.register_fn("has_parse_error", |ast: &mut SqlAst| ast.has_parse_error());
    engine.register_fn("parse_error", |ast: &mut SqlAst| ast.parse_error());
    engine.register_fn("kinds", |ast: &mut SqlAst| ast.kinds());
    engine.register_fn("create_tables", |ast: &mut SqlAst| ast.create_tables());
    engine.register_fn("drop_objects", |ast: &mut SqlAst| ast.drop_objects());
    engine.register_fn("selects", |ast: &mut SqlAst| ast.selects());
    engine.register_fn("has_create_table", |ast: &mut SqlAst| ast.has_create_table());
    engine.register_fn("has_drop_table", |ast: &mut SqlAst| ast.has_drop_table());

    // StmtInfo 方法
    engine.register_fn("kind", |s: &mut StmtInfo| s.kind());
    engine.register_fn("line", |s: &mut StmtInfo| s.line());
    engine.register_fn("column", |s: &mut StmtInfo| s.column());
    engine.register_fn("has_create_table", |s: &mut StmtInfo| s.has_create_table());
    engine.register_fn("has_drop_object", |s: &mut StmtInfo| s.has_drop_object());
    engine.register_fn("has_select", |s: &mut StmtInfo| s.has_select());
    engine.register_fn("create_table", |s: &mut StmtInfo| s.create_table());
    engine.register_fn("drop_object", |s: &mut StmtInfo| s.drop_object());
    engine.register_fn("select", |s: &mut StmtInfo| s.select());

    // CreateInfo 方法
    engine.register_fn("table_name", |c: &mut CreateInfo| c.table_name());
    engine.register_fn("columns", |c: &mut CreateInfo| c.columns());
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

    engine
}
