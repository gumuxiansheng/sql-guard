//! `sqlguard explain` 子命令实现。
//!
//! 解析 SQL 文件（或 Mapper XML）并输出 AST 结构，帮助规则开发者理解
//! Rhai 规则脚本中可用的字段和值。

use std::path::Path;

use crate::config::CheckDialect;
use crate::error::SqlGuardError;
use crate::mapper;
use crate::rule::engine::{ast as sql_ast, parser};

/// explain 子命令入口。
///
/// 调用方（main.rs）负责加载配置、解析方言、读取文件内容。
/// 本函数只做解析 + 输出。
pub fn run_explain(
    target_path: &Path,
    dialect: CheckDialect,
    fallback: Option<CheckDialect>,
    is_mapper: bool,
    json: bool,
) -> Result<(), SqlGuardError> {
    let content = std::fs::read_to_string(target_path).map_err(|e| {
        SqlGuardError::CheckError(format!(
            "Failed to read file '{}': {}",
            target_path.display(),
            e
        ))
    })?;

    if is_mapper {
        explain_mapper(&content, &dialect, fallback.as_ref(), json, target_path)?;
    } else {
        explain_sql(&content, &dialect, fallback.as_ref(), json, target_path)?;
    }

    Ok(())
}

// ===== SQL 脚本模式 =====

fn explain_sql(
    content: &str,
    dialect: &CheckDialect,
    fallback: Option<&CheckDialect>,
    json: bool,
    file_path: &Path,
) -> Result<(), SqlGuardError> {
    let ast = parser::parse_sql_to_ast_fb(content, *dialect, fallback.cloned());

    if json {
        print_json(&ast, file_path, content, dialect, fallback);
        return Ok(());
    }

    print_human_readable(&ast, file_path, content, dialect, fallback);
    Ok(())
}

// ===== Mapper XML 模式 =====

fn explain_mapper(
    _content: &str,
    dialect: &CheckDialect,
    fallback: Option<&CheckDialect>,
    json: bool,
    file_path: &Path,
) -> Result<(), SqlGuardError> {
    let stmts = mapper::parser::extract_sql_from_xml(file_path, "utf-8")?;

    if stmts.is_empty() {
        eprintln!(
            "No SQL statements found in mapper XML: {}",
            file_path.display()
        );
        return Ok(());
    }

    if json {
        println!("{{");
        println!(
            "  \"file\": \"{}\",",
            escape_json(&file_path.to_string_lossy())
        );
        println!("  \"dialect\": \"{}\",", dialect.as_str());
        if let Some(fb) = fallback {
            println!("  \"dialect_fallback\": \"{}\",", fb.as_str());
        }
        println!("  \"statement_count\": {},", stmts.len());
        println!("  \"statements\": [");
        for (i, stmt) in stmts.iter().enumerate() {
            let ast = parser::parse_sql_to_ast_fb(&stmt.processed_sql, *dialect, fallback.cloned());
            if i > 0 {
                println!(",");
            }
            print!("    {{");
            print!(
                "\"statement_id\": \"{}\", ",
                escape_json(&stmt.statement_id)
            );
            print!("\"statement_type\": \"{}\", ", stmt.statement_type);
            print!("\"raw_xml_line\": {}, ", stmt.raw_xml_line);
            print!("\"has_dynamic\": {}, ", stmt.has_dynamic);
            print!(
                "\"processed_sql\": \"{}\", ",
                escape_json(&stmt.processed_sql)
            );
            if let Some(alt) = &stmt.processed_sql_alt {
                print!("\"processed_sql_alt\": \"{}\", ", escape_json(alt));
            }
            print!("\"ast\": ");
            print_ast_json_inline(&ast);
            print!("}}");
        }
        println!();
        println!("  ]");
        println!("}}");
        return Ok(());
    }

    // 人类可读格式
    println!("=== Mapper File: {} ===", file_path.display());
    println!(
        "Dialect: {}{}",
        dialect.as_str(),
        match fallback {
            Some(fb) => format!(" → {} → generic", fb.as_str()),
            None => " → generic".to_string(),
        }
    );
    println!("Statements: {}", stmts.len());
    println!();

    for (i, stmt) in stmts.iter().enumerate() {
        println!("--- Statement #{} ---", i + 1);
        println!("  ID: {}", stmt.statement_id);
        println!("  Type: {}", stmt.statement_type);
        println!("  XML Line: {}", stmt.raw_xml_line);
        println!("  Has Dynamic: {}", stmt.has_dynamic);
        if stmt.has_dynamic {
            println!("  ⚠ Contains ${{}} substitution — AST may be incomplete");
        }
        println!();

        // 先尝试主渲染
        let ast = parser::parse_sql_to_ast_fb(&stmt.processed_sql, *dialect, fallback.cloned());

        if ast.has_parse_error() {
            // 尝试备用渲染
            let candidates: Vec<(&str, &str)> =
                vec![("processed_sql", stmt.processed_sql.as_str())]
                    .into_iter()
                    .chain(
                        stmt.processed_sql_alt
                            .as_deref()
                            .map(|s| ("processed_sql_alt", s)),
                    )
                    .chain(
                        stmt.processed_sql_alt2
                            .as_deref()
                            .map(|s| ("processed_sql_alt2", s)),
                    )
                    .collect();

            let mut found = false;
            for (label, sql) in &candidates[1..] {
                let alt_ast = parser::parse_sql_to_ast_fb(sql, *dialect, fallback.cloned());
                if !alt_ast.has_parse_error() {
                    println!("  ⚠ Main render failed to parse, using {}:", label);
                    println!("  Rendered SQL: {}", sql.trim());
                    println!();
                    print_ast_human(&alt_ast, 2);
                    found = true;
                    break;
                }
            }

            if !found {
                println!("  ⚠ Parse error in all renderings: {}", ast.parse_error());
                println!("  Rendered SQL (primary): {}", stmt.processed_sql.trim());
                println!();
                // 仍然打印部分 AST（statements 可能为空）
                print_ast_human(&ast, 2);
            }
        } else {
            println!("  Rendered SQL: {}", stmt.processed_sql.trim());
            println!();
            print_ast_human(&ast, 2);
        }

        if i + 1 < stmts.len() {
            println!();
        }
    }

    Ok(())
}

// ===== 人类可读输出 =====

fn print_human_readable(
    ast: &sql_ast::SqlAst,
    file_path: &Path,
    content: &str,
    dialect: &CheckDialect,
    fallback: Option<&CheckDialect>,
) {
    let line_count = content.lines().count();

    println!("=== File: {} ===", file_path.display());
    println!(
        "Dialect: {}{}",
        dialect.as_str(),
        match fallback {
            Some(fb) => format!(" → {} → generic", fb.as_str()),
            None => " → generic".to_string(),
        }
    );
    println!("Lines: {}", line_count);
    println!("Statements: {}", ast.statements.len());

    if ast.has_parse_error() {
        println!("Parse Error: {}", ast.parse_error());
    }

    if ast.has_comma_join_anywhere {
        println!("Has comma JOIN (implicit): true");
    }

    if !ast.comments.is_empty() {
        println!("Comments: {}", ast.comments.len());
    }

    println!();
    print_ast_human(ast, 0);
}

fn print_ast_human(ast: &sql_ast::SqlAst, indent: usize) {
    let pad = "  ".repeat(indent);

    if ast.statements.is_empty() {
        println!("{}(no statements parsed)", pad);
        return;
    }

    for (i, stmt) in ast.statements.iter().enumerate() {
        println!(
            "{}[{}] {}  (line {}, end_line {}, col {})",
            pad,
            i + 1,
            stmt.kind,
            stmt.line,
            stmt.end_line,
            stmt.column
        );

        // CREATE_TABLE
        if let Some(ct) = &stmt.create_table {
            print_create_info(ct, indent + 1);
        }

        // DROP
        if let Some(drop) = &stmt.drop_object {
            let p = "  ".repeat(indent + 1);
            println!("{}type: {}", p, drop.object_type);
            println!("{}name: {}", p, drop.name);
            println!("{}if_exists: {}", p, drop.if_exists);
        }

        // SELECT
        if let Some(sel) = &stmt.select {
            print_select_info(sel, indent + 1);
        }

        // INSERT
        if let Some(ins) = &stmt.insert {
            let p = "  ".repeat(indent + 1);
            println!("{}table: {}", p, ins.table_name);
            println!("{}columns: {:?}", p, ins.columns);
        }

        // UPDATE
        if let Some(upd) = &stmt.update {
            let p = "  ".repeat(indent + 1);
            println!("{}table: {}", p, upd.table_name);
            if let Some(wc) = &upd.where_clause {
                println!("{}where: {}", p, wc);
            } else {
                println!("{}where: (none)", p);
            }
        }

        // DELETE
        if let Some(del) = &stmt.delete {
            let p = "  ".repeat(indent + 1);
            println!("{}table: {}", p, del.table_name);
            if let Some(wc) = &del.where_clause {
                println!("{}where: {}", p, wc);
            } else {
                println!("{}where: (none)", p);
            }
        }

        // ALTER TABLE
        if let Some(alt) = &stmt.alter_table {
            let p = "  ".repeat(indent + 1);
            println!("{}table: {}", p, alt.table_name);
            println!("{}adds_primary_key: {}", p, alt.adds_primary_key);
            println!("{}drops_primary_key: {}", p, alt.drops_primary_key);
            if !alt.added_primary_key_columns.is_empty() {
                println!("{}added_pk_columns: {:?}", p, alt.added_primary_key_columns);
            }
            if !alt.operations.is_empty() {
                println!("{}operations:", p);
                for op in &alt.operations {
                    let p2 = "  ".repeat(indent + 2);
                    println!("{}- type: {}", p2, op.operation_type);
                    if !op.column_name.is_empty() {
                        println!("{}  column: {}", p2, op.column_name);
                    }
                    if !op.constraint_name.is_empty() {
                        println!("{}  constraint: {}", p2, op.constraint_name);
                    }
                    if !op.detail.is_empty() {
                        println!("{}  detail: {}", p2, op.detail);
                    }
                }
            }
        }

        // TRUNCATE
        if let Some(tr) = &stmt.truncate {
            let p = "  ".repeat(indent + 1);
            println!("{}table: {}", p, tr.table_name);
            println!("{}has_table_keyword: {}", p, tr.has_table_keyword);
        }

        // CREATE VIEW
        if let Some(v) = &stmt.create_view {
            let p = "  ".repeat(indent + 1);
            println!("{}name: {}", p, v.name);
            println!("{}materialized: {}", p, v.materialized);
            println!("{}is_replace: {}", p, v.is_replace);
            println!("{}column_count: {}", p, v.column_count);
        }

        // CREATE INDEX
        if let Some(ci) = &stmt.create_index {
            let p = "  ".repeat(indent + 1);
            println!("{}name: {}", p, ci.name);
            println!("{}table: {}", p, ci.table_name);
            println!("{}columns: {:?}", p, ci.columns);
            println!("{}is_unique: {}", p, ci.is_unique);
        }

        // 事务
        if let Some(tx) = &stmt.transaction {
            let p = "  ".repeat(indent + 1);
            println!("{}kind: {}", p, tx.kind);
        }
    }

    // 注释
    if !ast.comments.is_empty() {
        println!();
        let p = "  ".repeat(indent);
        println!("{}Comments:", p);
        for c in &ast.comments {
            let text_preview = if c.text.len() > 60 {
                format!("{}...", &c.text[..57])
            } else {
                c.text.clone()
            };
            println!("{}  [{}] line {}: {}", p, c.kind, c.line, text_preview);
        }
    }
}

fn print_create_info(ct: &sql_ast::CreateInfo, indent: usize) {
    let p = "  ".repeat(indent);
    println!("{}table: {}", p, ct.table_name);
    if ct.if_not_exists {
        println!("{}if_not_exists: true", p);
    }
    if ct.is_create_as {
        println!("{}is_create_as: true (CTAS)", p);
    }
    println!("{}has_primary_key: {}", p, ct.has_primary_key);
    if !ct.primary_key_columns.is_empty() {
        println!("{}primary_key_columns: {:?}", p, ct.primary_key_columns);
    }
    if !ct.primary_key_name.is_empty() {
        println!("{}primary_key_name: {}", p, ct.primary_key_name);
    }

    // 列
    if !ct.columns.is_empty() {
        println!("{}columns:", p);
        for col in &ct.columns {
            let p2 = "  ".repeat(indent + 1);
            let mut flags = Vec::new();
            if col.is_primary_key {
                flags.push("pk".to_string());
            }
            if col.is_not_null {
                flags.push("not_null".to_string());
            }
            if col.is_unique {
                flags.push("unique".to_string());
            }
            if col.is_auto_increment {
                flags.push("auto_inc".to_string());
            }
            if col.has_check {
                flags.push("check".to_string());
            }
            if col.has_foreign_key {
                flags.push("fk".to_string());
            }

            let flags_str = if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(", "))
            };

            let mut line = format!("{}- {} {}", p2, col.name, col.data_type);
            if !flags.is_empty() {
                line.push_str(&flags_str);
            }
            println!("{}", line);

            if let Some(default) = &col.default_value {
                println!("{}  default: {}", p2, default);
            }
            if let Some(comment) = &col.comment {
                println!("{}  comment: {}", p2, comment);
            }
            if let Some(ref_table) = &col.references_table {
                println!("{}  references: {}", p2, ref_table);
            }
        }
    }

    // 外键
    if !ct.foreign_keys.is_empty() {
        println!("{}foreign_keys:", p);
        for fk in &ct.foreign_keys {
            let p2 = "  ".repeat(indent + 1);
            println!(
                "{}- name: {}",
                p2,
                if fk.name.is_empty() {
                    "(unnamed)"
                } else {
                    &fk.name
                }
            );
            println!("{}  columns: {:?}", p2, fk.columns);
            println!(
                "{}  references: {}({:?})",
                p2, fk.foreign_table, fk.referred_columns
            );
            if !fk.on_delete.is_empty() {
                println!("{}  on_delete: {}", p2, fk.on_delete);
            }
            if !fk.on_update.is_empty() {
                println!("{}  on_update: {}", p2, fk.on_update);
            }
        }
    }

    // CHECK 约束
    if !ct.checks.is_empty() {
        println!("{}checks:", p);
        for ck in &ct.checks {
            let p2 = "  ".repeat(indent + 1);
            let expr_preview = if ck.expr_text.len() > 80 {
                format!("{}...", &ck.expr_text[..77])
            } else {
                ck.expr_text.clone()
            };
            println!(
                "{}- name: {}  expr: {}",
                p2,
                if ck.name.is_empty() {
                    "(unnamed)"
                } else {
                    &ck.name
                },
                expr_preview
            );
        }
    }

    // 索引
    if !ct.indexes.is_empty() {
        println!("{}indexes:", p);
        for idx in &ct.indexes {
            let p2 = "  ".repeat(indent + 1);
            println!(
                "{}- name: {}  columns: {:?}  unique: {}",
                p2,
                if idx.name.is_empty() {
                    "(unnamed)"
                } else {
                    &idx.name
                },
                idx.columns,
                idx.is_unique
            );
        }
    }

    // UNIQUE 约束
    if !ct.uniques.is_empty() {
        println!("{}uniques:", p);
        for uq in &ct.uniques {
            let p2 = "  ".repeat(indent + 1);
            println!(
                "{}- name: {}  columns: {:?}",
                p2,
                if uq.name.is_empty() {
                    "(unnamed)"
                } else {
                    &uq.name
                },
                uq.columns
            );
        }
    }
}

fn print_select_info(sel: &sql_ast::SelectInfo, indent: usize) {
    let p = "  ".repeat(indent);

    // 集合运算
    if sel.union {
        println!("{}set_op: UNION", p);
    } else if sel.union_all {
        println!("{}set_op: UNION ALL", p);
    } else if sel.intersect {
        println!("{}set_op: INTERSECT", p);
    } else if sel.except {
        println!("{}set_op: EXCEPT", p);
    }

    // 通配符
    if sel.has_wildcard {
        println!("{}has_wildcard: true (SELECT *)", p);
    }

    // 投影
    if !sel.projection.is_empty() {
        println!("{}projection: {:?}", p, sel.projection);
    }

    // FROM
    if let Some(table) = &sel.from_table {
        let alias_str = sel
            .from_table_alias
            .as_deref()
            .map(|a| format!(" AS {}", a))
            .unwrap_or_default();
        println!("{}from: {}{}", p, table, alias_str);
    }
    if sel.has_subquery_in_from {
        println!("{}has_subquery_in_from: true", p);
    }

    // JOIN
    if !sel.joins.is_empty() {
        println!("{}joins:", p);
        for j in &sel.joins {
            let p2 = "  ".repeat(indent + 1);
            let alias_str = j
                .alias
                .as_deref()
                .map(|a| format!(" AS {}", a))
                .unwrap_or_default();
            println!("{}- {} {}{}", p2, j.join_type, j.table_name, alias_str);
            if j.has_condition {
                if let Some(cond) = &j.condition_text {
                    let preview = if cond.len() > 80 {
                        format!("{}...", &cond[..77])
                    } else {
                        cond.clone()
                    };
                    println!("{}  on: {}", p2, preview);
                } else {
                    println!("{}  on: (present)", p2);
                }
            } else {
                println!("{}  on: (none — potential cartesian product)", p2);
            }
        }
    }

    // 隐式逗号 JOIN
    if sel.has_comma_join {
        println!("{}has_comma_join: true (FROM a, b)", p);
    }

    // WHERE / GROUP BY / HAVING / QUALIFY
    if sel.has_where {
        if let Some(wc) = &sel.where_clause {
            let preview = if wc.len() > 80 {
                format!("{}...", &wc[..77])
            } else {
                wc.clone()
            };
            println!("{}where: {}", p, preview);
        } else {
            println!("{}where: (present)", p);
        }
    }
    if sel.has_group_by {
        println!("{}group_by: true", p);
    }
    if sel.has_having {
        println!("{}having: true", p);
    }
    if sel.has_qualify {
        println!("{}qualify: true", p);
    }

    // ORDER BY / LIMIT / OFFSET / FETCH / DISTINCT
    if sel.has_order_by {
        println!("{}order_by: true", p);
    }
    if sel.has_limit {
        println!("{}limit: true", p);
    }
    if sel.has_offset {
        println!("{}offset: true", p);
    }
    if sel.has_fetch {
        println!("{}fetch: true", p);
    }
    if sel.has_distinct {
        println!("{}distinct: true", p);
    }

    // CTE
    if sel.has_cte {
        println!("{}ctes: {}", p, sel.ctes.len());
        for cte in &sel.ctes {
            let p2 = "  ".repeat(indent + 1);
            println!(
                "{}- name: {}  columns: {}  recursive: {}",
                p2, cte.name, cte.column_count, cte.is_recursive
            );
        }
    }

    // 子查询
    if sel.has_subquery {
        println!("{}subqueries: {}", p, sel.subqueries.len());
        for (i, sq) in sel.subqueries.iter().enumerate() {
            let p2 = "  ".repeat(indent + 1);
            println!("{}[subquery {}]", p2, i + 1);
            print_select_info(sq, indent + 2);
        }
    }

    // 窗口函数
    if sel.has_window_function {
        println!("{}window_functions: {}", p, sel.window_functions.len());
        for wf in &sel.window_functions {
            let p2 = "  ".repeat(indent + 1);
            let mut parts = vec![wf.function_name.clone()];
            if wf.has_partition_by {
                parts.push("PARTITION BY".to_string());
            }
            if wf.has_order_by {
                parts.push("ORDER BY".to_string());
            }
            if wf.has_window_frame {
                parts.push("frame".to_string());
            }
            println!("{}- {}", p2, parts.join(" "));
        }
    }

    // 未限定列
    if sel.has_unqualified_column {
        println!("{}has_unqualified_column: true", p);
    }
}

// ===== JSON 输出 =====

fn print_json(
    ast: &sql_ast::SqlAst,
    file_path: &Path,
    content: &str,
    dialect: &CheckDialect,
    fallback: Option<&CheckDialect>,
) {
    let line_count = content.lines().count();
    println!("{{");
    println!(
        "  \"file\": \"{}\",",
        escape_json(&file_path.to_string_lossy())
    );
    println!("  \"dialect\": \"{}\",", dialect.as_str());
    if let Some(fb) = fallback {
        println!("  \"dialect_fallback\": \"{}\",", fb.as_str());
    }
    println!("  \"line_count\": {},", line_count);
    println!("  \"statement_count\": {},", ast.statements.len());
    println!("  \"has_parse_error\": {},", ast.has_parse_error());
    if ast.has_parse_error() {
        println!(
            "  \"parse_error\": \"{}\",",
            escape_json(&ast.parse_error())
        );
    }
    println!(
        "  \"has_comma_join_anywhere\": {},",
        ast.has_comma_join_anywhere
    );
    println!("  \"statements\": [");
    for (i, stmt) in ast.statements.iter().enumerate() {
        if i > 0 {
            println!(",");
        }
        print!("    ");
        print_stmt_json(stmt);
    }
    println!();
    println!("  ]");

    if !ast.comments.is_empty() {
        println!(",\"comments\": [");
        for (i, c) in ast.comments.iter().enumerate() {
            if i > 0 {
                println!(",");
            }
            print!(
                "    {{\"kind\": \"{}\", \"line\": {}, \"text\": \"{}\"}}",
                c.kind,
                c.line,
                escape_json(&c.text)
            );
        }
        println!();
        println!("  ]");
    }

    println!("}}");
}

fn print_stmt_json(stmt: &sql_ast::StmtInfo) {
    print!("{{");
    print!("\"kind\": \"{}\", ", stmt.kind);
    print!("\"line\": {}, ", stmt.line);
    print!("\"end_line\": {}, ", stmt.end_line);
    print!("\"column\": {}", stmt.column);

    if let Some(ct) = &stmt.create_table {
        print!(", \"create_table\": ");
        print_create_json(ct);
    }
    if let Some(drop) = &stmt.drop_object {
        print!(", \"drop\": {{");
        print!(
            "\"type\": \"{}\", \"name\": \"{}\", \"if_exists\": {}}}",
            drop.object_type,
            escape_json(&drop.name),
            drop.if_exists
        );
    }
    if let Some(sel) = &stmt.select {
        print!(", \"select\": ");
        print_select_json(sel);
    }
    if let Some(ins) = &stmt.insert {
        print!(", \"insert\": {{");
        print!(
            "\"table\": \"{}\", \"columns\": {:?}}}",
            escape_json(&ins.table_name),
            ins.columns
        );
    }
    if let Some(upd) = &stmt.update {
        print!(", \"update\": {{");
        print!(
            "\"table\": \"{}\", \"where\": {}}}",
            escape_json(&upd.table_name),
            upd.where_clause
                .as_ref()
                .map(|w| format!("\"{}\"", escape_json(w)))
                .unwrap_or_else(|| "null".to_string())
        );
    }
    if let Some(del) = &stmt.delete {
        print!(", \"delete\": {{");
        print!(
            "\"table\": \"{}\", \"where\": {}}}",
            escape_json(&del.table_name),
            del.where_clause
                .as_ref()
                .map(|w| format!("\"{}\"", escape_json(w)))
                .unwrap_or_else(|| "null".to_string())
        );
    }
    if let Some(alt) = &stmt.alter_table {
        print!(", \"alter_table\": {{");
        print!("\"table\": \"{}\", \"adds_primary_key\": {}, \"drops_primary_key\": {}, \"operations\": [",
            escape_json(&alt.table_name), alt.adds_primary_key, alt.drops_primary_key);
        for (i, op) in alt.operations.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{{\"type\": \"{}\", \"column\": \"{}\", \"constraint\": \"{}\", \"detail\": \"{}\"}}",
                op.operation_type,
                escape_json(&op.column_name),
                escape_json(&op.constraint_name),
                escape_json(&op.detail));
        }
        print!("]}}");
    }
    if let Some(tr) = &stmt.truncate {
        print!(
            ", \"truncate\": {{\"table\": \"{}\", \"has_table_keyword\": {}}}",
            escape_json(&tr.table_name),
            tr.has_table_keyword
        );
    }
    if let Some(v) = &stmt.create_view {
        print!(", \"create_view\": {{\"name\": \"{}\", \"materialized\": {}, \"is_replace\": {}, \"column_count\": {}}}",
            escape_json(&v.name), v.materialized, v.is_replace, v.column_count);
    }
    if let Some(ci) = &stmt.create_index {
        print!(", \"create_index\": {{\"name\": \"{}\", \"table\": \"{}\", \"columns\": {:?}, \"is_unique\": {}}}",
            escape_json(&ci.name), escape_json(&ci.table_name), ci.columns, ci.is_unique);
    }
    if let Some(tx) = &stmt.transaction {
        print!(", \"transaction\": {{\"kind\": \"{}\"}}", tx.kind);
    }

    print!("}}");
}

fn print_create_json(ct: &sql_ast::CreateInfo) {
    print!("{{");
    print!("\"table\": \"{}\", ", escape_json(&ct.table_name));
    print!("\"has_primary_key\": {}, ", ct.has_primary_key);
    print!("\"primary_key_columns\": {:?}, ", ct.primary_key_columns);
    print!("\"is_create_as\": {}, ", ct.is_create_as);
    print!("\"columns\": [");
    for (i, col) in ct.columns.iter().enumerate() {
        if i > 0 {
            print!(", ");
        }
        print!("{{\"name\": \"{}\", \"data_type\": \"{}\", \"pk\": {}, \"not_null\": {}, \"unique\": {}, \"auto_inc\": {}}}",
            escape_json(&col.name),
            escape_json(&col.data_type),
            col.is_primary_key, col.is_not_null, col.is_unique, col.is_auto_increment);
    }
    print!("]");
    if !ct.foreign_keys.is_empty() {
        print!(", \"foreign_keys\": [");
        for (i, fk) in ct.foreign_keys.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{{\"name\": \"{}\", \"columns\": {:?}, \"ref_table\": \"{}\", \"ref_columns\": {:?}}}",
                escape_json(&fk.name), fk.columns,
                escape_json(&fk.foreign_table), fk.referred_columns);
        }
        print!("]");
    }
    if !ct.checks.is_empty() {
        print!(", \"checks\": [");
        for (i, ck) in ct.checks.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!(
                "{{\"name\": \"{}\", \"expr\": \"{}\"}}",
                escape_json(&ck.name),
                escape_json(&ck.expr_text)
            );
        }
        print!("]");
    }
    print!("}}");
}

fn print_select_json(sel: &sql_ast::SelectInfo) {
    print!("{{");
    print!("\"has_wildcard\": {}, ", sel.has_wildcard);
    print!("\"projection\": {:?}, ", sel.projection);
    print!(
        "\"from_table\": {}, ",
        sel.from_table
            .as_ref()
            .map(|t| format!("\"{}\"", escape_json(t)))
            .unwrap_or_else(|| "null".to_string())
    );
    print!("\"has_where\": {}, ", sel.has_where);
    print!("\"has_group_by\": {}, ", sel.has_group_by);
    print!("\"has_having\": {}, ", sel.has_having);
    print!("\"has_order_by\": {}, ", sel.has_order_by);
    print!("\"has_limit\": {}, ", sel.has_limit);
    print!("\"has_distinct\": {}, ", sel.has_distinct);
    print!("\"union\": {}, ", sel.union);
    print!("\"union_all\": {}, ", sel.union_all);
    print!("\"intersect\": {}, ", sel.intersect);
    print!("\"except\": {}, ", sel.except);
    print!("\"has_cte\": {}, ", sel.has_cte);
    print!("\"has_subquery\": {}, ", sel.has_subquery);
    print!("\"has_window_function\": {}", sel.has_window_function);
    print!("}}");
}

fn print_ast_json_inline(ast: &sql_ast::SqlAst) {
    print!(
        "{{\"has_parse_error\": {}, \"statement_count\": {}}}",
        ast.has_parse_error(),
        ast.statements.len()
    );
}

// ===== 辅助函数 =====

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str, suffix: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(suffix).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn explain_simple_select() {
        let f = write_temp("SELECT id, name FROM users WHERE id = 1;\n", ".sql");
        let result = run_explain(f.path(), CheckDialect::Generic, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_create_table() {
        let sql = "CREATE TABLE t (id INT PRIMARY KEY, name VARCHAR(50) NOT NULL);\n";
        let f = write_temp(sql, ".sql");
        let result = run_explain(f.path(), CheckDialect::Generic, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_json_output() {
        let f = write_temp("SELECT * FROM t;\n", ".sql");
        let result = run_explain(f.path(), CheckDialect::Generic, None, false, true);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_parse_error_does_not_panic() {
        let f = write_temp("INSERT INTO VALUES (1);\n", ".sql");
        let result = run_explain(f.path(), CheckDialect::Generic, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_mapper_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mapper PUBLIC "-//mybatis.org//DTD Mapper 3.0//EN" "http://mybatis.org/dtd/mybatis-3-mapper.dtd">
<mapper namespace="com.test.UserMapper">
  <select id="findById" resultType="User">
    SELECT id, name FROM users WHERE id = #{id}
  </select>
  <delete id="deleteById">
    DELETE FROM users WHERE id = #{id}
  </delete>
</mapper>"#;
        let f = write_temp(xml, ".xml");
        let result = run_explain(f.path(), CheckDialect::Generic, None, true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_mapper_json_output() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mapper PUBLIC "-//mybatis.org//DTD Mapper 3.0//EN" "http://mybatis.org/dtd/mybatis-3-mapper.dtd">
<mapper namespace="com.test.UserMapper">
  <select id="findAll" resultType="User">
    SELECT * FROM users
  </select>
</mapper>"#;
        let f = write_temp(xml, ".xml");
        let result = run_explain(f.path(), CheckDialect::Generic, None, true, true);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_dialect_mysql() {
        let f = write_temp("SELECT id FROM users WHERE id = 1;\n", ".sql");
        let result = run_explain(f.path(), CheckDialect::MySql, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_multi_statement() {
        let sql =
            "CREATE TABLE t (id INT PRIMARY KEY);\nINSERT INTO t VALUES (1);\nSELECT * FROM t;\n";
        let f = write_temp(sql, ".sql");
        let result = run_explain(f.path(), CheckDialect::Generic, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn explain_alter_table() {
        let sql = "CREATE TABLE t (id INT);\nALTER TABLE t ADD PRIMARY KEY (id);\n";
        let f = write_temp(sql, ".sql");
        let result = run_explain(f.path(), CheckDialect::Generic, None, false, false);
        assert!(result.is_ok());
    }
}
