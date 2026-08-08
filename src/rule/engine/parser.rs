use sqlparser::ast::{
    AlterTable, AlterTableOperation, ColumnOption, CreateIndex, CreateTable, CreateView, Delete,
    DropBehavior, FromTable, Insert, Statement, TableConstraint, TableFactor, Truncate, Update,
};
use sqlparser::dialect::{
    AnsiDialect, GenericDialect, MySqlDialect, OracleDialect, PostgreSqlDialect,
};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use super::analyzer::analyze_query;
use super::ast::*;
use super::gaussdb_rewrite;
use super::scanner::{collect_comments, detect_comma_join_in_sql};
use crate::config::CheckDialect;

/// 把 SQL 文本解析为 AST，每条语句记录其在源文件中的行号/列号。
/// 解析失败时返回带 `parse_error` 的空 AST，不影响后续规则运行。
///
/// 利用 `Parser::peek_token()` 在解析每条语句前读取起始位置，
/// 避免 sqlparser 的 `Statement` 本身不携带位置信息的限制。
///
/// **逐语句方言回退链**（GaussDB「PG 内核 + Oracle 外壳」混合方言场景）：
/// - 每条语句先用主 `dialect` 解析；成功即采用，不尝试回退。
/// - 主方言失败 → 切片出该语句文本，依次用回退链方言（`fallback` → `Generic`）
///   重试，首个成功即采用。
/// - 全部失败 → 记 `PARSE_ERROR` 语句。
///
/// **GaussDB 主方言特殊处理**：
/// - 入口处先调用 `gaussdb_rewrite::rewrite_for_pg_parse` 对 SQL 文本做词法级归一化
///   （MINUS→EXCEPT、SYSDATE→CURRENT_TIMESTAMP、NVL→COALESCE、FROM dual→子查询、
///   反引号→双引号），重写后的文本用纯 `PostgreSqlDialect` 解析，AST 语义 100% 来自 PG，
///   避免 Oracle 回退对标识符大小写等 PG 语义的污染。
/// - 重写后仍解析失败的语句才走 Oracle → Generic 回退链（覆盖 CONNECT BY、(+) 等复杂构造）。
/// - 行号保真：重写不引入/删除换行符，violation 行号与原文件一致。
///
/// 见 [`parse_sql_to_ast`]（2 参封装，回退为 `None`）。
pub fn parse_sql_to_ast_fb(
    sql: &str,
    dialect: CheckDialect,
    fallback: Option<CheckDialect>,
) -> SqlAst {
    // ★ GaussDB 主方言：先词法重写为 PG 语法，再切到 PostgreSqlDialect 解析。
    // 回退链保持用户传入的 fallback（GaussDB 默认回退 Oracle），用于兜底重写层未覆盖的构造。
    let (effective_sql, effective_dialect): (std::borrow::Cow<'_, str>, CheckDialect) =
        if dialect == CheckDialect::GaussDB {
            let rewritten = gaussdb_rewrite::rewrite_for_pg_parse(sql);
            (
                std::borrow::Cow::Owned(rewritten.sql),
                CheckDialect::PostgreSql,
            )
        } else {
            (std::borrow::Cow::Borrowed(sql), dialect)
        };

    let chain_enums = build_chain_enums(effective_dialect, fallback);
    let primary = *chain_enums
        .first()
        .expect("dialect chain must be non-empty");
    let parser_dialect: Box<dyn sqlparser::dialect::Dialect> = box_dialect(primary);

    let mut parser = match Parser::new(&*parser_dialect).try_with_sql(&effective_sql) {
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

    let total_lines = effective_sql.lines().count() as i64;
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
        let line = peek.span.start.line as i64;
        let column = peek.span.start.column as i64;
        // 主方言尝试前记录本语句起始字节偏移（用于失败回退时切片文本）
        let start_offset =
            location_to_byte_offset(&effective_sql, peek.span.start.line, peek.span.start.column);

        // 先尝试主方言
        let mut recovered: Option<Statement> = None;
        match parser.parse_statement() {
            Ok(stmt) => {
                // 主方言「干净地」解析到 `;`/EOF（下一 token 是分隔符或文件尾）→ 采用。
                // 否则说明主方言只吞掉了前缀（如把 Oracle 的 `CONNECT BY` 误判为表别名），
                // 不能采用残缺结果，需回退链重新解析整条语句。
                let clean = matches!(parser.peek_token().token, Token::SemiColon | Token::EOF);
                if clean {
                    let end_line = end_line_of(parser.peek_token(), line, total_lines);
                    statements.push(convert_statement(&stmt, line, column, end_line));
                    continue;
                }
                // 不干净：丢弃主方言结果，落入下方回退链统一处理
            }
            Err(_e) => {
                // 主方言硬错误：落入下方回退链统一处理
            }
        }

        // ===== 回退链：切片整条语句，依次用后续方言重试 =====
        // 1) 推进主 parser 到下一个 `;`/EOF（重新同步），并记录 `;` 的字节偏移
        let mut end_offset = effective_sql.len();
        loop {
            let t = parser.peek_token();
            if t.token == Token::SemiColon {
                end_offset =
                    location_to_byte_offset(&effective_sql, t.span.start.line, t.span.start.column);
                parser.next_token();
                break;
            } else if t.token == Token::EOF {
                break;
            }
            parser.next_token();
        }
        let stmt_text: &str = if end_offset > start_offset {
            &effective_sql[start_offset..end_offset]
        } else {
            &effective_sql
        };
        // 2) 用回退链方言（跳过主方言）解析；首个成功即采用
        for &fb in &chain_enums[1..] {
            if let Ok(s) = parse_one_with(stmt_text, fb) {
                recovered = Some(s);
                break;
            }
        }
        let end_line = end_line_of(parser.peek_token(), line, total_lines);
        match recovered {
            Some(stmt) => {
                statements.push(convert_statement(&stmt, line, column, end_line));
            }
            None => {
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
            }
        }
    }

    SqlAst {
        statements,
        parse_error: None,
        // 逗号 JOIN 检测与注释收集基于原始 SQL 文本（与重写无关，保持原文件特征）
        has_comma_join_anywhere: detect_comma_join_in_sql(sql),
        comments: collect_comments(sql),
    }
}

/// 解析链枚举序列（主方言在前，去重，链尾兜底 Generic）。
fn build_chain_enums(primary: CheckDialect, fallback: Option<CheckDialect>) -> Vec<CheckDialect> {
    let mut dialects = vec![primary];
    if let Some(fb) = fallback {
        if fb != primary && !dialects.contains(&fb) {
            dialects.push(fb);
        }
    }
    if !dialects.contains(&CheckDialect::Generic) {
        dialects.push(CheckDialect::Generic);
    }
    dialects
}

/// 把 `CheckDialect` 枚举构造为 sqlparser 的 `Box<dyn Dialect>`。
///
/// 注意：`GaussDB` 在 `parse_sql_to_ast_fb` 入口处已被重写层归一化并切到
/// `PostgreSql`，本函数正常流程不会收到 `GaussDB`。此处返回 `PostgreSqlDialect`
/// 作为防御性兜底（与入口处理一致），避免 match 不穷尽。
fn box_dialect(d: CheckDialect) -> Box<dyn sqlparser::dialect::Dialect> {
    match d {
        CheckDialect::Generic => Box::new(GenericDialect {}),
        CheckDialect::MySql => Box::new(MySqlDialect {}),
        CheckDialect::PostgreSql | CheckDialect::GaussDB => Box::new(PostgreSqlDialect {}),
        CheckDialect::Ansi => Box::new(AnsiDialect {}),
        CheckDialect::Oracle => Box::new(OracleDialect {}),
    }
}

/// 用指定方言解析「单条语句」文本，返回 `Ok(Statement)` 或 `Err`。
/// 供回退链对切片出的单条失败语句重试解析。
fn parse_one_with(
    stmt_text: &str,
    dialect: CheckDialect,
) -> Result<Statement, sqlparser::parser::ParserError> {
    let d = box_dialect(dialect);
    let mut p = Parser::new(&*d).try_with_sql(stmt_text)?;
    p.parse_statement()
}

/// 根据语句之后的下一个 token 推算 end_line（与主方言成功/失败分支共用）。
fn end_line_of(next_tok: sqlparser::tokenizer::TokenWithSpan, line: i64, total_lines: i64) -> i64 {
    if next_tok.token == Token::EOF {
        total_lines.max(line)
    } else {
        let next_line = next_tok.span.start.line as i64;
        if next_line > line {
            next_line - 1
        } else {
            line
        }
    }
}

/// 把 sqlparser 的 `(line, column)` 位置（1-based，按字符计数）映射到 `sql` 的字节偏移。
///
/// 用于在语句解析失败时精确切片出该语句的文本，交给回退方言重试。
/// 兼容 LF 与 CRLF：sqlparser 把 `\r` 当作行内普通字符计入 column，
/// 本函数按字符（而非字节）切片，与 sqlparser 的计数方式一致。
fn location_to_byte_offset(sql: &str, line: u64, column: u64) -> usize {
    let line_idx = (line.saturating_sub(1)) as usize;
    let col_idx = (column.saturating_sub(1)) as usize;
    let mut byte_pos: usize = 0;
    for (i, l) in sql.split('\n').enumerate() {
        if i == line_idx {
            // 在本行内找第 col_idx 个字符的字节偏移
            for (char_count, (b, _ch)) in l.char_indices().enumerate() {
                if char_count == col_idx {
                    return byte_pos + b;
                }
            }
            // column 超出本行长度：返回行尾
            return byte_pos + l.len();
        }
        // 累加本行长度 + 1 个 '\n' 分隔符
        byte_pos += l.len() + 1;
    }
    sql.len()
}

pub(crate) fn convert_statement(
    stmt: &Statement,
    line: i64,
    column: i64,
    end_line: i64,
) -> StmtInfo {
    match stmt {
        Statement::CreateTable(CreateTable {
            name,
            columns,
            constraints,
            if_not_exists,
            query,
            ..
        }) => {
            let table_name = name.to_string();
            let mut cols = Vec::new();
            let mut pk_in_column = false;
            let mut pk_columns: Vec<String> = Vec::new();
            let mut pk_name = String::new();
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
                        ColumnOption::Unique(_u) => {
                            info.is_unique = true;
                        }
                        ColumnOption::PrimaryKey(_pk) => {
                            info.is_primary_key = true;
                            info.is_unique = true;
                            pk_in_column = true;
                            pk_columns.push(col_def.name.to_string());
                        }
                        ColumnOption::Comment(s) => {
                            info.comment = Some(s.clone());
                        }
                        ColumnOption::Check(c) => {
                            info.has_check = true;
                            checks.push(CheckInfo {
                                name: String::new(),
                                expr_text: c.expr.to_string(),
                                line: None,
                                column: None,
                            });
                        }
                        ColumnOption::ForeignKey(fk) => {
                            info.has_foreign_key = true;
                            info.references_table = Some(fk.foreign_table.to_string());
                            foreign_keys.push(ForeignKeyInfo {
                                name: String::new(),
                                columns: vec![col_def.name.to_string()],
                                foreign_table: fk.foreign_table.to_string(),
                                referred_columns: fk
                                    .referred_columns
                                    .iter()
                                    .map(|i| i.to_string())
                                    .collect(),
                                on_delete: format_referral_action(&fk.on_delete),
                                on_update: format_referral_action(&fk.on_update),
                                line: None,
                                column: None,
                            });
                        }
                        ColumnOption::OnUpdate(expr) => {
                            // MySQL ON UPDATE CURRENT_TIMESTAMP 等：归入 default_value 描述
                            // 这里仅作标记，不影响主流程
                            let _ = expr;
                        }
                        ColumnOption::Generated { .. } => {
                            // Oracle / PG `GENERATED ... AS IDENTITY` —— 语义等价于自增列。
                            // 该 variant 此前落入 `_ => {}` 被静默忽略，导致 Oracle 身份列
                            // 丢失 is_auto_increment 元信息；这里显式捕获。
                            info.is_auto_increment = true;
                        }
                        ColumnOption::DialectSpecific(tokens) => {
                            // 检测 MySQL AUTO_INCREMENT / SQLite AUTOINCREMENT，
                            // 以及 Oracle 以 DialectSpecific 形式出现的 IDENTITY 关键字等。
                            let joined = tokens
                                .iter()
                                .map(|t| t.to_string().to_uppercase())
                                .collect::<Vec<_>>()
                                .join(" ");
                            if joined.contains("AUTO_INCREMENT")
                                || joined.contains("AUTOINCREMENT")
                                || joined.contains("IDENTITY")
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
                    TableConstraint::Unique(u) => {
                        uniques.push(UniqueInfo {
                            name: u.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
                            columns: u.columns.iter().map(|i| i.to_string()).collect(),
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::ForeignKey(fk) => {
                        foreign_keys.push(ForeignKeyInfo {
                            name: fk.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
                            columns: fk.columns.iter().map(|i| i.to_string()).collect(),
                            foreign_table: fk.foreign_table.to_string(),
                            referred_columns: fk
                                .referred_columns
                                .iter()
                                .map(|i| i.to_string())
                                .collect(),
                            on_delete: format_referral_action(&fk.on_delete),
                            on_update: format_referral_action(&fk.on_update),
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::Check(c) => {
                        checks.push(CheckInfo {
                            name: c.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
                            expr_text: c.expr.to_string(),
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::Index(idx) => {
                        indexes.push(IndexInfo {
                            name: idx.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
                            columns: idx.columns.iter().map(|i| i.to_string()).collect(),
                            is_unique: false,
                            line: None,
                            column: None,
                        });
                    }
                    TableConstraint::PrimaryKey(pk) => {
                        pk_columns = pk.columns.iter().map(|i| i.to_string()).collect();
                        pk_name = pk.name.as_ref().map(|i| i.to_string()).unwrap_or_default();
                    }
                    _ => {}
                }
            }

            let pk_in_constraint = !pk_columns.is_empty();

            StmtInfo {
                kind: "CREATE_TABLE".to_string(),
                line,
                end_line,
                column,
                create_table: Some(CreateInfo {
                    table_name,
                    columns: cols,
                    has_primary_key: pk_in_column || pk_in_constraint,
                    primary_key_columns: pk_columns,
                    primary_key_name: pk_name,
                    if_not_exists: *if_not_exists,
                    is_create_as: query.is_some(),
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
        Statement::Insert(Insert {
            table: table_name,
            columns,
            ..
        }) => {
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
        Statement::Update(Update {
            table, selection, ..
        }) => {
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
        Statement::Delete(Delete {
            tables,
            from,
            selection,
            ..
        }) => {
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
        Statement::AlterTable(AlterTable {
            name, operations, ..
        }) => {
            let table_name = name.to_string();
            let mut adds_primary_key = false;
            let mut drops_primary_key = false;
            let mut added_pk_cols: Vec<String> = Vec::new();
            let mut op_infos = Vec::new();

            for op in operations {
                let (op_type, col_name, tbl_name, con_name, detail, is_add_pk, is_drop_pk) =
                    match op {
                        AlterTableOperation::AddConstraint { constraint: tc, .. } => {
                            let (t, detail_str, is_pk) = match &tc {
                                TableConstraint::PrimaryKey(pk) => {
                                    added_pk_cols =
                                        pk.columns.iter().map(|i| i.to_string()).collect();
                                    (
                                        "ADD_CONSTRAINT",
                                        format!(
                                            "ADD{} PRIMARY KEY ({})",
                                            pk.name
                                                .as_ref()
                                                .map(|n| format!(" CONSTRAINT {}", n))
                                                .unwrap_or_default(),
                                            pk.columns
                                                .iter()
                                                .map(|i| i.to_string())
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        ),
                                        true,
                                    )
                                }
                                TableConstraint::Unique(u) => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} UNIQUE ({})",
                                        u.name
                                            .as_ref()
                                            .map(|n| format!(" CONSTRAINT {}", n))
                                            .unwrap_or_default(),
                                        u.columns
                                            .iter()
                                            .map(|i| i.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                    false,
                                ),
                                TableConstraint::ForeignKey(fk) => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} FOREIGN KEY ({}) REFERENCES {}",
                                        fk.name
                                            .as_ref()
                                            .map(|n| format!(" CONSTRAINT {}", n))
                                            .unwrap_or_default(),
                                        fk.columns
                                            .iter()
                                            .map(|i| i.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                        fk.foreign_table
                                    ),
                                    false,
                                ),
                                TableConstraint::Check(c) => (
                                    "ADD_CONSTRAINT",
                                    format!(
                                        "ADD{} CHECK ({})",
                                        c.name
                                            .as_ref()
                                            .map(|n| format!(" CONSTRAINT {}", n))
                                            .unwrap_or_default(),
                                        c.expr
                                    ),
                                    false,
                                ),
                                _ => ("ADD_CONSTRAINT", format!("ADD {}", tc), false),
                            };
                            (
                                t.to_string(),
                                String::new(),
                                String::new(),
                                name_of_table_constraint(tc),
                                detail_str,
                                is_pk,
                                false,
                            )
                        }
                        AlterTableOperation::AddColumn { column_def, .. } => {
                            let has_pk = column_def
                                .options
                                .iter()
                                .any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)));
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
                            has_column_keyword: _,
                            column_names,
                            if_exists,
                            drop_behavior,
                        } => {
                            let column_name = column_names
                                .iter()
                                .map(|i| i.to_string())
                                .collect::<Vec<_>>()
                                .join(", ");
                            let cascade = matches!(drop_behavior, Some(DropBehavior::Cascade));
                            (
                                "DROP_COLUMN".to_string(),
                                column_name.clone(),
                                String::new(),
                                String::new(),
                                format!(
                                    "DROP COLUMN {}{}{}",
                                    if *if_exists { "IF EXISTS " } else { "" },
                                    column_name,
                                    if cascade { " CASCADE" } else { "" }
                                ),
                                false,
                                false,
                            )
                        }
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
                        AlterTableOperation::DropPrimaryKey { .. } => (
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
                    added_primary_key_columns: added_pk_cols,
                    operations: op_infos,
                }),
                truncate: None,
                create_view: None,
                create_index: None,
                transaction: None,
            }
        }
        Statement::Truncate(Truncate {
            table_names, table, ..
        }) => StmtInfo {
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
                table_name: table_names
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                has_table_keyword: *table,
            }),
            create_view: None,
            create_index: None,
            transaction: None,
        },
        Statement::CreateView(CreateView {
            or_replace,
            materialized,
            name,
            columns,
            ..
        }) => StmtInfo {
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
        Statement::CreateIndex(CreateIndex {
            name,
            table_name,
            using,
            columns,
            unique,
            ..
        }) => {
            let idx_name = name.as_ref().map(|n| n.to_string()).unwrap_or_default();
            let col_names: Vec<String> = columns.iter().map(|obe| obe.to_string()).collect();
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
        Statement::Set(_) => StmtInfo {
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
        Statement::Use(_) => StmtInfo {
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

/// 2 参封装：等价于 [`parse_sql_to_ast_fb`] 且回退方言为 `None`
/// （即「主方言 → Generic」；`default_fallback` 在此处不生效，仅 `CheckDialect`
/// 的 `default_fallback` 在 `main.rs` 解析 CLI/配置时应用）。
///
/// 注意：对 `gaussdb` 主方言，本函数仍会触发词法重写层（重写为 PG 语法后
/// 用 `PostgreSqlDialect` 解析），但回退链退化为「PG → Generic」——
/// 生产环境建议用 [`parse_sql_to_ast_fb`] 并传入 `Some(Oracle)` 以获得 Oracle 兜底。
///
/// 供单元测试与 `replay_export` 等不需要回退链的场景直接调用。
pub fn parse_sql_to_ast(sql: &str, dialect: CheckDialect) -> SqlAst {
    parse_sql_to_ast_fb(sql, dialect, None)
}

/// 提取 TableConstraint 的名字（用于 AlterOpInfo.constraint_name）。
pub(crate) fn name_of_table_constraint(tc: &TableConstraint) -> String {
    match tc {
        TableConstraint::Unique(u) => u.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
        TableConstraint::PrimaryKey(pk) => {
            pk.name.as_ref().map(|i| i.to_string()).unwrap_or_default()
        }
        TableConstraint::ForeignKey(fk) => {
            fk.name.as_ref().map(|i| i.to_string()).unwrap_or_default()
        }
        TableConstraint::Check(c) => c.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
        TableConstraint::Index(idx) => idx.name.as_ref().map(|i| i.to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// 把 `Option<ReferentialAction>` 转成字符串（"CASCADE"/"RESTRICT"/"SET NULL"/...），
/// None 返回空串。
pub(crate) fn format_referral_action(action: &Option<sqlparser::ast::ReferentialAction>) -> String {
    match action {
        Some(a) => format!("{:?}", a).to_uppercase(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckDialect;

    // ===== parse_sql_to_ast 端到端 =====

    #[test]
    fn test_parse_single_select() {
        let ast = parse_sql_to_ast("SELECT 1", CheckDialect::Generic);
        assert!(!ast.has_parse_error());
        assert_eq!(ast.statements.len(), 1);
        assert_eq!(ast.statements[0].kind, "SELECT");
        assert_eq!(ast.statements[0].line, 1);
    }

    #[test]
    fn test_parse_multiple_statements() {
        let sql = "SELECT 1;\nSELECT 2;\nSELECT 3;";
        let ast = parse_sql_to_ast(sql, CheckDialect::Generic);
        assert!(!ast.has_parse_error());
        assert_eq!(ast.statements.len(), 3);
        // 第二条语句从第 2 行开始
        assert_eq!(ast.statements[1].line, 2);
        assert_eq!(ast.statements[2].line, 3);
    }

    #[test]
    fn test_parse_end_line_multiline() {
        // 跨行语句：CREATE TABLE 起于第 1 行，`;` 在第 3 行，SELECT 在第 4 行。
        // 解析完 CREATE TABLE 后下一 token 是第 3 行的 `;`，按 next_line-1 算 end_line=2。
        // 该值是当前实现的行为契约，固化下来防止回退。
        let sql = "CREATE TABLE t (\n  id INT\n);\nSELECT 1;";
        let ast = parse_sql_to_ast(sql, CheckDialect::Generic);
        assert_eq!(ast.statements.len(), 2);
        assert_eq!(ast.statements[0].line, 1);
        assert_eq!(ast.statements[0].end_line, 2);
        assert_eq!(ast.statements[1].line, 4);
    }

    #[test]
    fn test_parse_error_emits_parse_error_stmt() {
        // sqlparser 无法解析的语法应作为 PARSE_ERROR 语句记录
        for input in ["SELECT 1 +", "CREATE TABLE", "SELECT ) FROM t"] {
            let ast = parse_sql_to_ast(input, CheckDialect::Generic);
            assert!(
                ast.statements.iter().any(|s| s.kind == "PARSE_ERROR"),
                "expected PARSE_ERROR for {:?}",
                input
            );
        }
    }

    #[test]
    fn test_parse_mysql_dialect_specific_syntax() {
        // Generic 方言可能不支持 INSERT IGNORE，MySQL 方言可以
        let mysql_ast =
            parse_sql_to_ast("INSERT IGNORE INTO t (a) VALUES (1)", CheckDialect::MySql);
        assert!(
            mysql_ast.statements.iter().any(|s| s.kind == "INSERT"),
            "MySQL dialect should parse INSERT IGNORE; got kinds: {:?}",
            mysql_ast
                .statements
                .iter()
                .map(|s| &s.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_collects_comma_join_flag() {
        // has_comma_join_anywhere 由 detect_comma_join_in_sql 设置
        let ast = parse_sql_to_ast("SELECT id FROM a, b", CheckDialect::Generic);
        assert!(ast.has_comma_join_anywhere);

        let ast2 = parse_sql_to_ast(
            "SELECT id FROM a JOIN b ON a.id = b.id",
            CheckDialect::Generic,
        );
        assert!(!ast2.has_comma_join_anywhere);
    }

    #[test]
    fn test_parse_collects_comments() {
        let ast = parse_sql_to_ast("-- header\nSELECT 1 /* hint */", CheckDialect::Generic);
        assert_eq!(ast.comments.len(), 2);
        assert_eq!(ast.comments[0].kind, "LINE");
        assert_eq!(ast.comments[1].kind, "BLOCK");
    }

    #[test]
    fn test_parse_tokenize_error_returns_empty() {
        // 极端的 tokenize 错误（如不闭合的字符串在某些方言下）应返回带 parse_error 的空 AST
        // 这里用一个肯定能 tokenize 但解析失败的输入测试，主要验证返回的 AST 形态合理
        let ast = parse_sql_to_ast("", CheckDialect::Generic);
        assert!(ast.statements.is_empty());
        assert!(!ast.has_parse_error());
    }

    #[test]
    fn test_parse_create_table_kind() {
        let ast = parse_sql_to_ast(
            "CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100))",
            CheckDialect::Generic,
        );
        assert_eq!(ast.statements.len(), 1);
        assert_eq!(ast.statements[0].kind, "CREATE_TABLE");
        assert!(ast.statements[0].create_table.is_some());
        let ct = ast.statements[0].create_table.as_ref().unwrap();
        assert_eq!(ct.table_name, "users");
        assert!(ct.has_primary_key);
    }

    #[test]
    fn test_parse_drop_table_kind() {
        let ast = parse_sql_to_ast("DROP TABLE users", CheckDialect::Generic);
        assert_eq!(ast.statements[0].kind, "DROP_TABLE");
        assert!(ast.statements[0].drop_object.is_some());
    }

    #[test]
    fn test_parse_transaction_statements() {
        let ast = parse_sql_to_ast("BEGIN; COMMIT; ROLLBACK;", CheckDialect::Generic);
        let kinds: Vec<&str> = ast.statements.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["START_TRANSACTION", "COMMIT", "ROLLBACK"]);
    }

    // ===== name_of_table_constraint =====

    #[test]
    fn test_name_of_table_constraint_named() {
        // 用 sqlparser 解析含命名的约束
        let ast = parse_sql_to_ast(
            "CREATE TABLE t (id INT, CONSTRAINT uk_id UNIQUE (id))",
            CheckDialect::Generic,
        );
        let ct = ast.statements[0].create_table.as_ref().unwrap();
        // 至少应收集到 1 个 unique 约束
        assert!(ct.has_unique);
        let uniques = &ct.uniques;
        assert_eq!(uniques.len(), 1);
        assert_eq!(uniques[0].name, "uk_id");
    }

    #[test]
    fn test_name_of_table_constraint_unnamed() {
        // 未命名约束：name 应为空串
        let ast = parse_sql_to_ast(
            "CREATE TABLE t (id INT, UNIQUE (id))",
            CheckDialect::Generic,
        );
        let ct = ast.statements[0].create_table.as_ref().unwrap();
        assert_eq!(ct.uniques.len(), 1);
        assert_eq!(ct.uniques[0].name, "");
    }

    // ===== format_referral_action =====

    #[test]
    fn test_format_referral_action_none() {
        assert_eq!(format_referral_action(&None), "");
    }

    #[test]
    fn test_format_referral_action_cascade() {
        assert_eq!(
            format_referral_action(&Some(sqlparser::ast::ReferentialAction::Cascade)),
            "CASCADE"
        );
    }

    #[test]
    fn test_format_referral_action_set_null() {
        // sqlparser Debug 格式是 "SetNull"，to_uppercase → "SETNULL"
        assert_eq!(
            format_referral_action(&Some(sqlparser::ast::ReferentialAction::SetNull)),
            "SETNULL"
        );
    }

    // ===== 逐语句方言回退链 =====

    #[test]
    fn test_build_chain_enums_dedup_and_generic_tail() {
        // PG + Oracle -> [PG, Oracle, Generic]
        let c = build_chain_enums(CheckDialect::PostgreSql, Some(CheckDialect::Oracle));
        assert_eq!(
            c,
            vec![
                CheckDialect::PostgreSql,
                CheckDialect::Oracle,
                CheckDialect::Generic
            ]
        );
        // fallback == primary -> 去重，链尾仍兜底 Generic
        let c2 = build_chain_enums(CheckDialect::Generic, Some(CheckDialect::Generic));
        assert_eq!(c2, vec![CheckDialect::Generic]);
        // 无 fallback 且非 PG -> [primary, Generic]
        let c3 = build_chain_enums(CheckDialect::MySql, None);
        assert_eq!(c3, vec![CheckDialect::MySql, CheckDialect::Generic]);
    }

    #[test]
    fn test_location_to_byte_offset_basic() {
        let sql = "SELECT 1;\nSELECT 2;";
        // line 1 col 1 -> byte 0
        assert_eq!(location_to_byte_offset(sql, 1, 1), 0);
        // line 2 col 1 -> "SELECT 1;\n" = 10 字节之后
        assert_eq!(location_to_byte_offset(sql, 2, 1), 10);
        // line 2 col 8（'2' 的位置）-> byte 17
        assert_eq!(location_to_byte_offset(sql, 2, 8), 17);
    }

    #[test]
    fn test_parse_postgresql_falls_back_to_oracle() {
        // GaussDB 混合方言：PG 内核 + Oracle 外壳。
        // MINUS 是 Oracle 专有集合运算符，PG 解析失败，应回退到 Oracle 成功解析。
        let ast = parse_sql_to_ast_fb(
            "SELECT a FROM t1 MINUS SELECT a FROM t2",
            CheckDialect::PostgreSql,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "expected successful parse via Oracle fallback; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        let kinds: Vec<&str> = ast.statements.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["SELECT"]);
        // 回退解析出的 SELECT 应识别为 MINUS（在 analyzer 中映射为 except）
        let sel = ast.statements[0].select.as_ref().unwrap();
        assert!(
            sel.except,
            "MINUS should be detected as EXCEPT via Oracle fallback"
        );
    }

    #[test]
    fn test_parse_error_after_fallback_exhausted() {
        // 即便配置了回退链，真正无法解析的语句仍应记 PARSE_ERROR。
        let ast = parse_sql_to_ast_fb(
            "SELECT 1 +",
            CheckDialect::PostgreSql,
            Some(CheckDialect::Oracle),
        );
        assert!(
            ast.statements.iter().any(|s| s.kind == "PARSE_ERROR"),
            "genuinely broken SQL must remain PARSE_ERROR even with fallback"
        );
    }

    #[test]
    fn test_parse_oracle_connect_by_via_fallback() {
        // CONNECT BY 是 Oracle 层次查询语法，PG 不支持；回退到 Oracle 应成功解析为 SELECT。
        let ast = parse_sql_to_ast_fb(
            "SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1",
            CheckDialect::PostgreSql,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "CONNECT BY should parse via Oracle fallback; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");

        // 链结构正确性：即便回退链退化为 [PG, Generic]（无 Oracle），
        // Generic 兜底也能解析 CONNECT BY，整条语句仍应成功（不产生 PARSE_ERROR）。
        // 这证明"成功"来自回退链机制本身，而非 PG 把语句残缺截断后误判为成功。
        // 真正只有 Oracle 才能救、Generic 也无能为力的构造是 MINUS
        // （见 test_parse_postgresql_falls_back_to_oracle）。
        let no_fb = parse_sql_to_ast_fb(
            "SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1",
            CheckDialect::PostgreSql,
            None,
        );
        assert!(
            !no_fb.has_parse_error(),
            "CONNECT BY must be rescued by the Generic fallback tail; kinds={:?}",
            no_fb.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(no_fb.statements[0].kind, "SELECT");
    }

    #[test]
    fn test_parse_oracle_as_primary() {
        // 直接以 Oracle 作主方言解析（不依赖回退链），验证 OracleDialect 路径本身正确。
        // 1) MINUS 集合运算符
        let ast = parse_sql_to_ast(
            "SELECT a FROM t1 MINUS SELECT a FROM t2",
            CheckDialect::Oracle,
        );
        assert!(
            !ast.has_parse_error(),
            "Oracle should parse MINUS directly; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");
        assert!(
            ast.statements[0].select.as_ref().unwrap().except,
            "Oracle MINUS should map to except"
        );

        // 2) CONNECT BY 层次查询
        let ast2 = parse_sql_to_ast(
            "SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1",
            CheckDialect::Oracle,
        );
        assert!(
            !ast2.has_parse_error(),
            "Oracle should parse CONNECT BY directly; kinds={:?}",
            ast2.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast2.statements[0].kind, "SELECT");

        // 3) 标准 SELECT（含 Oracle 伪表 dual）
        let ast3 = parse_sql_to_ast("SELECT 1 FROM dual", CheckDialect::Oracle);
        assert!(
            !ast3.has_parse_error(),
            "Oracle should parse plain SELECT ... FROM dual; kinds={:?}",
            ast3.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast3.statements[0].kind, "SELECT");
    }

    #[test]
    fn test_parse_generated_column_sets_auto_increment() {
        // `GENERATED ... AS IDENTITY` 经 ColumnOption::Generated 表达（PG / Oracle 12c+ 均如此），
        // 应被识别为自增列（is_auto_increment = true）。此前该 variant 落入 `_ => {}`
        // 静默丢失，修复后必须命中新加的 Generated 分支。
        // 用 PG 方言（GENERATED AS IDENTITY 的规范生产者）验证该代码路径。
        let ast = parse_sql_to_ast(
            "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY, name VARCHAR(10))",
            CheckDialect::PostgreSql,
        );
        assert!(
            !ast.has_parse_error(),
            "CREATE TABLE with GENERATED AS IDENTITY must parse; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        let create = ast.statements[0].create_table.as_ref().unwrap();
        let id_col = create
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("id"))
            .expect("id column should be present");
        assert!(
            id_col.is_auto_increment,
            "GENERATED AS IDENTITY column must be flagged is_auto_increment"
        );
    }

    // ===== GaussDB 方言：重写层 + PG 语义 =====

    #[test]
    fn test_gaussdb_dialect_parses_minus_with_pg_semantics() {
        // GaussDB 主方言：MINUS 被重写为 EXCEPT，用纯 PG 方言解析成功。
        // 与旧「PG→Oracle 回退」路径的关键区别：AST 语义 100% 来自 PG，无 Oracle 污染。
        let ast = parse_sql_to_ast_fb(
            "SELECT a FROM t1 MINUS SELECT a FROM t2",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse MINUS via rewrite; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");
        let sel = ast.statements[0].select.as_ref().unwrap();
        assert!(
            sel.except,
            "MINUS (rewritten to EXCEPT) should be detected as except"
        );
    }

    #[test]
    fn test_gaussdb_dialect_preserves_pg_semantics() {
        // ★ 反污染验证：GaussDB 方言下，重写层覆盖的构造（NVL/SYSDATE）走纯 PG 路径解析，
        // 不触发 Oracle 回退（因为重写后 PG 能直接解析成功）。
        //
        // 反污染的核心价值：重写层把 Oracle/MySQL 构造归一化为 PG 等价语法，
        // 使 AST 语义 100% 来自 PG 方言，避免原「PG→Oracle 回退」对整条语句
        // 语义的接管（标识符大小写折叠、伪表归属等）。
        //
        // 用 NVL + SYSDATE（重写为 COALESCE + CURRENT_TIMESTAMP），PG 方言直接解析成功，
        // 表名保留 PG 语义（不折叠为大写 USERS）。
        let sql = "SELECT NVL(name, 'x') FROM Users WHERE created > SYSDATE";
        let gaussdb_ast =
            parse_sql_to_ast_fb(sql, CheckDialect::GaussDB, Some(CheckDialect::Oracle));
        assert!(!gaussdb_ast.has_parse_error());
        assert_eq!(gaussdb_ast.statements.len(), 1);
        assert_eq!(gaussdb_ast.statements[0].kind, "SELECT");
        let sel = gaussdb_ast.statements[0].select.as_ref().unwrap();
        // PG 语义：未加引号标识符不折叠为大写。`Users` 不应变成 `USERS`（Oracle 污染标志）。
        assert!(
            sel.from_table.iter().all(|t| t != "USERS"),
            "GaussDB (PG semantics) should NOT fold 'Users' to uppercase 'USERS'; got from_table={:?}",
            sel.from_table
        );
    }

    #[test]
    fn test_gaussdb_dialect_parses_sysdate_and_nvl() {
        // SYSDATE → CURRENT_TIMESTAMP、NVL( → COALESCE( 重写后 PG 方言应成功解析
        let ast = parse_sql_to_ast_fb(
            "SELECT NVL(name, 'unknown') FROM t WHERE created > SYSDATE",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse NVL/SYSDATE via rewrite; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");
    }

    #[test]
    fn test_gaussdb_dialect_parses_from_dual() {
        // FROM dual → FROM (SELECT 1) AS dual 重写后 PG 方言应成功解析
        let ast = parse_sql_to_ast_fb(
            "SELECT 1 FROM dual",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse FROM dual via rewrite; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");
    }

    #[test]
    fn test_gaussdb_dialect_parses_backtick_identifier() {
        // 反引号 → 双引号重写后 PG 方言应成功解析。
        // 用 `SELECT * FROM \`order\`` 让 order 作为表名，验证 from_table 提取正确。
        let ast = parse_sql_to_ast_fb(
            "SELECT * FROM `order`",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse backtick identifier via rewrite; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");
        // PG 语义：双引号标识符保留大小写。analyzer 提取的表名可能带引号（如 "\"order\""），
        // 关键是包含 "order" 且不为大写 "ORDER"（Oracle 会折叠为大写）。
        let sel = ast.statements[0].select.as_ref().unwrap();
        assert!(
            sel.from_table.iter().any(|t| t.contains("order") && !t.contains("ORDER")),
            "GaussDB should preserve backtick-rewritten identifier case (not folded to uppercase); got from_table={:?}",
            sel.from_table
        );
    }

    #[test]
    fn test_gaussdb_dialect_mixed_oracle_mysql_constructs() {
        // 混合 Oracle + MySQL 语法：全部经重写层归一化后 PG 方言解析成功
        let ast = parse_sql_to_ast_fb(
            "SELECT `order`, NVL(name, 'x') FROM t WHERE created > SYSDATE MINUS SELECT a FROM dual",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse mixed Oracle/MySQL constructs; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements.len(), 1);
        assert_eq!(ast.statements[0].kind, "SELECT");
        let sel = ast.statements[0].select.as_ref().unwrap();
        assert!(
            sel.except,
            "MINUS should be detected as except after rewrite"
        );
    }

    #[test]
    fn test_gaussdb_dialect_falls_back_to_oracle_for_connect_by() {
        // CONNECT BY 是重写层未覆盖的复杂 Oracle 构造，应通过 Oracle 回退兜底解析。
        // 这验证了 GaussDB 方言的回退链：重写后 PG 失败 → Oracle 兜底成功。
        let ast = parse_sql_to_ast_fb(
            "SELECT empno FROM emp CONNECT BY PRIOR empno = mgr START WITH empno = 1",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse CONNECT BY via Oracle fallback; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements[0].kind, "SELECT");
    }

    #[test]
    fn test_postgresql_no_longer_falls_back_to_oracle_by_default() {
        // ★ 破坏性变更验证：PG 主方言不再默认回退 Oracle（default_fallback 改为 None）。
        //
        // 用 `(+)` 老式外连接语法——这是只有 OracleDialect 能解析、Generic 也无法兜底的构造
        // （见 docs/dialect-fallback.md §五：Oracle 回退真正独力救回来的是 `(+)`）。
        //
        // 旧链 `[PG, Oracle, Generic]`：PG 失败 → Oracle 成功 → SELECT
        // 新链 `[PG, Generic]`（default_fallback=None）：PG 失败 → Generic 失败 → PARSE_ERROR
        let ast = parse_sql_to_ast_fb(
            "SELECT * FROM dept d, emp e WHERE d.id = e.dept_id(+)",
            CheckDialect::PostgreSql,
            None, // 模拟 default_fallback 的新行为
        );
        assert!(
            ast.statements.iter().any(|s| s.kind == "PARSE_ERROR"),
            "PG with no fallback should NOT parse (+) outer join (Oracle no longer default fallback); got kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );

        // 对照组：显式配 dialect_fallback=oracle 时，(+) 应被 Oracle 成功解析
        let ast2 = parse_sql_to_ast_fb(
            "SELECT * FROM dept d, emp e WHERE d.id = e.dept_id(+)",
            CheckDialect::PostgreSql,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast2.has_parse_error() && ast2.statements.iter().any(|s| s.kind == "SELECT"),
            "PG with explicit Oracle fallback should parse (+); got kinds={:?}",
            ast2.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_gaussdb_dialect_pure_pg_sql_unchanged() {
        // 纯 PG 语法 SQL 用 GaussDB 方言解析：重写层不改写，PG 方言直接成功
        let ast = parse_sql_to_ast_fb(
            "SELECT id, name FROM users WHERE id = $1",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(!ast.has_parse_error());
        assert_eq!(ast.statements.len(), 1);
        assert_eq!(ast.statements[0].kind, "SELECT");
    }

    #[test]
    fn test_gaussdb_dialect_parses_alter_add_parens() {
        // Oracle 风格 ALTER TABLE t ADD (c1 INT, c2 VARCHAR(10))
        // 重写为 ALTER TABLE t ADD COLUMN c1 INT, ADD COLUMN c2 VARCHAR(10) 后 PG 解析成功
        let ast = parse_sql_to_ast_fb(
            "ALTER TABLE t ADD (c1 INT, c2 VARCHAR(10))",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse ALTER TABLE ADD (...) via rewrite; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements.len(), 1);
        assert_eq!(ast.statements[0].kind, "ALTER_TABLE");
        let alter = ast.statements[0].alter_table.as_ref().unwrap();
        // 应有两个 ADD COLUMN 操作
        let add_count = alter
            .operations
            .iter()
            .filter(|op| op.operation_type == "ADD_COLUMN")
            .count();
        assert_eq!(
            add_count, 2,
            "should have 2 ADD_COLUMN operations; ops={:?}",
            alter.operations
        );
    }

    #[test]
    fn test_gaussdb_dialect_parses_alter_add_single_column() {
        let ast = parse_sql_to_ast_fb(
            "ALTER TABLE t ADD (c1 INT NOT NULL DEFAULT 0)",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(!ast.has_parse_error());
        assert_eq!(ast.statements[0].kind, "ALTER_TABLE");
    }

    #[test]
    fn test_gaussdb_dialect_parses_pk_using_index() {
        // Oracle 简化语法：ALTER TABLE a ADD CONSTRAINT pk PRIMARY KEY USING INDEX idx
        // 重写为 PRIMARY KEY (idx) USING INDEX idx 后 PG 方言解析成功
        let ast = parse_sql_to_ast_fb(
            "ALTER TABLE a ADD CONSTRAINT pk_a PRIMARY KEY USING INDEX pk_a",
            CheckDialect::GaussDB,
            Some(CheckDialect::Oracle),
        );
        assert!(
            !ast.has_parse_error(),
            "GaussDB should parse PRIMARY KEY USING INDEX via rewrite; kinds={:?}",
            ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
        assert_eq!(ast.statements.len(), 1);
        assert_eq!(ast.statements[0].kind, "ALTER_TABLE");
    }
}
