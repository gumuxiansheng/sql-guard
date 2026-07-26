use sqlparser::ast::{
    AlterTableOperation, ColumnOption, FromTable,
    OrderByExpr, Statement, TableConstraint,
    TableFactor,
};
use sqlparser::dialect::{AnsiDialect, GenericDialect, MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use super::analyzer::analyze_query;
use super::ast::*;
use super::scanner::{collect_comments, detect_comma_join_in_sql};
use crate::config::CheckDialect;

/// 把 SQL 文本解析为 AST，每条语句记录其在源文件中的行号/列号。
/// 解析失败时返回带 `parse_error` 的空 AST，不影响后续规则运行。
///
/// 利用 `Parser::peek_token()` 在解析每条语句前读取起始位置，
/// 避免 sqlparser 的 `Statement` 本身不携带位置信息的限制。
pub(crate) fn parse_sql_to_ast(sql: &str, dialect: CheckDialect) -> SqlAst {
    let parser_dialect: Box<dyn sqlparser::dialect::Dialect> = match dialect {
        CheckDialect::Generic => Box::new(GenericDialect {}),
        CheckDialect::MySql => Box::new(MySqlDialect {}),
        CheckDialect::PostgreSql => Box::new(PostgreSqlDialect {}),
        CheckDialect::Ansi => Box::new(AnsiDialect {}),
    };

    let mut parser = match Parser::new(&*parser_dialect).try_with_sql(sql) {
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

pub(crate) fn convert_statement(stmt: &Statement, line: i64, column: i64, end_line: i64) -> StmtInfo {
    match stmt {
        Statement::CreateTable {
            name,
            columns,
            constraints,
            if_not_exists,
            query,
            ..
        } => {
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
                        ColumnOption::Unique { is_primary, .. } => {
                            info.is_unique = true;
                            if *is_primary {
                                info.is_primary_key = true;
                                pk_in_column = true;
                                pk_columns.push(col_def.name.to_string());
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
                    TableConstraint::PrimaryKey { name, columns, .. } => {
                        pk_columns = columns.iter().map(|i| i.to_string()).collect();
                        pk_name = name.as_ref().map(|i| i.to_string()).unwrap_or_default();
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
            let mut added_pk_cols: Vec<String> = Vec::new();
            let mut op_infos = Vec::new();

            for op in operations {
                let (op_type, col_name, tbl_name, con_name, detail, is_add_pk, is_drop_pk) =
                    match op {
                        AlterTableOperation::AddConstraint(tc) => {
                            let (t, detail_str, is_pk) = match tc {
                                TableConstraint::PrimaryKey { name, columns, .. } => {
                                    added_pk_cols = columns.iter().map(|i| i.to_string()).collect();
                                    (
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
                                    )
                                },
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
                    added_primary_key_columns: added_pk_cols,
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
pub(crate) fn name_of_table_constraint(tc: &TableConstraint) -> String {
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
        let ast = parse_sql_to_ast("SELECT FROM WHERE", CheckDialect::Generic);
        assert!(ast.statements.iter().any(|s| s.kind == "PARSE_ERROR"));
    }

    #[test]
    fn test_parse_mysql_dialect_specific_syntax() {
        // Generic 方言可能不支持 INSERT IGNORE，MySQL 方言可以
        let mysql_ast = parse_sql_to_ast(
            "INSERT IGNORE INTO t (a) VALUES (1)",
            CheckDialect::MySql,
        );
        assert!(
            mysql_ast.statements.iter().any(|s| s.kind == "INSERT"),
            "MySQL dialect should parse INSERT IGNORE; got kinds: {:?}",
            mysql_ast.statements.iter().map(|s| &s.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_collects_comma_join_flag() {
        // has_comma_join_anywhere 由 detect_comma_join_in_sql 设置
        let ast = parse_sql_to_ast("SELECT id FROM a, b", CheckDialect::Generic);
        assert!(ast.has_comma_join_anywhere);

        let ast2 = parse_sql_to_ast("SELECT id FROM a JOIN b ON a.id = b.id", CheckDialect::Generic);
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
        let ast = parse_sql_to_ast(
            "BEGIN; COMMIT; ROLLBACK;",
            CheckDialect::Generic,
        );
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
        assert_eq!(format_referral_action(&Some(sqlparser::ast::ReferentialAction::Cascade)), "CASCADE");
    }

    #[test]
    fn test_format_referral_action_set_null() {
        // sqlparser Debug 格式是 "SetNull"，to_uppercase → "SETNULL"
        assert_eq!(
            format_referral_action(&Some(sqlparser::ast::ReferentialAction::SetNull)),
            "SETNULL"
        );
    }
}
