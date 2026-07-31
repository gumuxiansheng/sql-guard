//! 逻辑外键挖掘（relations mining）。
//!
//! 输入由 [`sqlguard::rule::engine::parser::parse_sql_to_ast_fb`] 解析得到的 [`SqlAst`]，
//! 遍历其中 SELECT 语句，从 `JOIN ... ON` 条件与隐式逗号 JOIN 的 WHERE 等值中
//! 提取「表.列 = 表.列」连接边，供上层聚合为逻辑外键目录。
//!
//! 本模块仅被 `sqlguard-mine` 等独立二进制引用；主 `sqlguard` 二进制不引用本模块，
//! 因此链接器不会将其编入主程序。

use std::collections::HashMap;

use sqlparser::ast::{BinaryOperator, Expr};
use sqlparser::dialect::{AnsiDialect, Dialect, GenericDialect, MySqlDialect, OracleDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use crate::rule::engine::ast::{SelectInfo, SqlAst};
use crate::config::CheckDialect;

/// 将 `CheckDialect` 映射为 sqlparser 的 `Dialect` trait object。
/// 与 `rule::engine::parser::box_dialect` 逻辑一致，这里保持独立以避免
/// 跨模块可见性问题。
fn box_dialect(d: CheckDialect) -> Box<dyn Dialect> {
    match d {
        CheckDialect::Generic => Box::new(GenericDialect {}),
        CheckDialect::MySql => Box::new(MySqlDialect {}),
        CheckDialect::PostgreSql => Box::new(PostgreSqlDialect {}),
        CheckDialect::Ansi => Box::new(AnsiDialect {}),
        CheckDialect::Oracle => Box::new(OracleDialect {}),
        // GaussDB 是 PG 内核 + Oracle/MySQL 外壳，AST 语义来自 PostgreSql
        // （见 `CheckDialect` 文档）。挖掘仅解析 ON/WHERE 条件文本，
        // 用 PostgreSqlDialect 即可正确覆盖其 PG 侧语法。
        CheckDialect::GaussDB => Box::new(PostgreSqlDialect {}),
    }
}

/// 一条从 ON / WHERE 等值条件中提取出的连接边（方向未定，由上层启发式判定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinEdge {
    pub left_table: String,
    pub left_column: String,
    pub right_table: String,
    pub right_column: String,
    pub join_type: String,
}

/// 从整个脚本 AST 提取所有连接边（跨多条语句）。
///
/// `dialect` 用于解析 JOIN ON / WHERE 条件文本中的表达式，应与解析 `ast`
/// 时使用的方言一致，避免方言特有语法（如 MySQL backtick）解析失败。
pub fn extract_join_edges(ast: &SqlAst, dialect: CheckDialect) -> Vec<JoinEdge> {
    let parser_dialect = box_dialect(dialect);
    let mut edges = Vec::new();
    for stmt in &ast.statements {
        if let Some(sel) = &stmt.select {
            edges.extend(extract_from_select(sel, &*parser_dialect));
        }
    }
    edges
}

/// 从单条 SELECT 提取连接边：显式 JOIN 的 ON 条件 + 隐式逗号 JOIN 的 WHERE 等值。
fn extract_from_select(sel: &SelectInfo, dialect: &dyn Dialect) -> Vec<JoinEdge> {
    let mut edges = Vec::new();

    // 构造别名 → 真实表名 映射（FROM 主表 + 各 JOIN 表）。
    let mut aliases: HashMap<String, String> = HashMap::new();
    let from_tbl = sel.from_table();
    let from_alias = sel.from_table_alias();
    if !from_tbl.is_empty() && !from_alias.is_empty() {
        aliases.insert(from_alias, from_tbl);
    }
    // 无别名的 FROM 主表直接用其表名引用，resolve_endpoint 的兜底逻辑已覆盖；
    // JOIN 表同理：有别名则登记，无别名则靠限定符即表名兜底。
    for j in sel.joins() {
        if let Some(a) = &j.alias {
            if !j.table_name.is_empty() {
                aliases.insert(a.clone(), j.table_name.clone());
            }
        }
    }

    // 显式 JOIN：解析 ON 条件文本为表达式，提取等值对。
    for j in sel.joins() {
        if !j.has_condition {
            continue;
        }
        let cond = match &j.condition_text {
            Some(c) => c,
            None => continue,
        };
        if let Some(expr) = parse_expr(cond, dialect) {
            let mut pairs = Vec::new();
            collect_equal_pairs(&expr, &mut pairs);
            for (l, r) in pairs {
                if let (Some(le), Some(re)) = (resolve_endpoint(&l, &aliases), resolve_endpoint(&r, &aliases)) {
                    edges.push(JoinEdge {
                        left_table: le.0,
                        left_column: le.1,
                        right_table: re.0,
                        right_column: re.1,
                        join_type: j.join_type.clone(),
                    });
                }
            }
        }
    }

    // 隐式逗号 JOIN（FROM a, b）：扫描 WHERE 中的跨表等值。
    if sel.has_comma_join {
        if let Some(wc) = &sel.where_clause {
            if let Some(expr) = parse_expr(wc, dialect) {
                let mut pairs = Vec::new();
                collect_equal_pairs(&expr, &mut pairs);
                for (l, r) in pairs {
                    if let (Some(le), Some(re)) = (resolve_endpoint(&l, &aliases), resolve_endpoint(&r, &aliases)) {
                        // 只统计跨表等值（同表自比较不是关系）。
                        if le.0 != re.0 {
                            edges.push(JoinEdge {
                                left_table: le.0,
                                left_column: le.1,
                                right_table: re.0,
                                right_column: re.1,
                                join_type: "IMPLICIT".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    edges
}

/// 把 `tbl.col` 解析为 (真实表名, 列名)。
///
/// 先查别名表；查不到时退化为「限定符即表名」——这恰好覆盖逗号 JOIN 用真实表名
/// （而非别名）引用的场景。无法拆出表限定符（裸列名）时返回 None。
fn resolve_endpoint(name: &str, aliases: &HashMap<String, String>) -> Option<(String, String)> {
    let (qual, col) = name.rsplit_once('.')?;
    if qual.is_empty() || col.is_empty() {
        return None;
    }
    let table = aliases.get(qual).cloned().unwrap_or_else(|| qual.to_string());
    Some((table, col.to_string()))
}

/// 把表达式文本解析为 sqlparser `Expr`，使用与解析整个 SQL 相同的方言，
/// 避免方言特有语法（如 MySQL backtick）在 GenericDialect 下解析失败。
fn parse_expr(text: &str, dialect: &dyn Dialect) -> Option<Expr> {
    let mut parser = Parser::new(dialect).try_with_sql(text).ok()?;
    parser.parse_expr().ok()
}

/// 递归收集 `AND` 链中的 `=` 等值对，返回 `tbl.col = tbl.col` 形式（未解析表名）。
fn collect_equal_pairs(expr: &Expr, out: &mut Vec<(String, String)>) {
    if let Expr::BinaryOp { left, op, right } = expr {
        match op {
            BinaryOperator::And => {
                collect_equal_pairs(left, out);
                collect_equal_pairs(right, out);
            }
            BinaryOperator::Eq => {
                if let (Some(l), Some(r)) = (qualified_name(left), qualified_name(right)) {
                    out.push((l, r));
                }
            }
            _ => {}
        }
    }
}

/// 提取限定列名文本（`a.b.c` → `a.b.c`）。
///
/// 只接受 `CompoundIdentifier`（含表限定符的列名）。裸 `Identifier`
/// （如 `col`）无法确定所属表，直接返回 None 在源头过滤。
fn qualified_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            Some(parts.iter().map(|i| i.value.as_str()).collect::<Vec<_>>().join("."))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckDialect;
    use crate::rule::engine::parser::parse_sql_to_ast_fb;

    fn edges_of(sql: &str) -> Vec<JoinEdge> {
        let ast = parse_sql_to_ast_fb(sql, CheckDialect::Generic, None);
        extract_join_edges(&ast, CheckDialect::Generic)
    }

    #[test]
    fn explicit_join_with_alias() {
        let edges = edges_of(
            "SELECT * FROM users u JOIN orders o ON u.id = o.user_id",
        );
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert_eq!(e.left_table, "users");
        assert_eq!(e.left_column, "id");
        assert_eq!(e.right_table, "orders");
        assert_eq!(e.right_column, "user_id");
        assert_eq!(e.join_type, "INNER");
    }

    #[test]
    fn composite_join_condition() {
        let edges = edges_of(
            "SELECT * FROM a JOIN b ON a.x = b.x AND a.y = b.y",
        );
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn implicit_comma_join_in_where() {
        let edges = edges_of(
            "SELECT * FROM a, b WHERE a.id = b.aid",
        );
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert_eq!(e.left_table, "a");
        assert_eq!(e.left_column, "id");
        assert_eq!(e.right_table, "b");
        assert_eq!(e.right_column, "aid");
        assert_eq!(e.join_type, "IMPLICIT");
    }

    #[test]
    fn left_join_captured() {
        let edges = edges_of(
            "SELECT * FROM users u LEFT JOIN profiles p ON u.id = p.user_id",
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].join_type, "LEFT");
    }

    /// 裸列名（无表限定符）不应被收集为连接边。
    /// `SELECT * FROM a, b WHERE x = y` 中的 `x` 和 `y` 无法确定所属表，必须跳过。
    #[test]
    fn bare_column_in_where_is_ignored() {
        let edges = edges_of(
            "SELECT * FROM a, b WHERE x = y",
        );
        assert_eq!(edges.len(), 0, "bare columns without table qualifier must not produce edges");
    }

    /// 混合条件：`a.id = b.aid AND x = y` 应只提取限定列名的等值对。
    #[test]
    fn mixed_qualified_and_bare_in_where() {
        let edges = edges_of(
            "SELECT * FROM a, b WHERE a.id = b.aid AND x = y",
        );
        assert_eq!(edges.len(), 1, "only qualified columns should produce edges");
        assert_eq!(edges[0].left_table, "a");
        assert_eq!(edges[0].right_table, "b");
    }

    /// 端到端：临时 Mapper XML → `extract_sql_from_xmls` → 解析 → `extract_join_edges`。
    /// 覆盖显式 JOIN（findOrderWithUser / findOrderItems）与隐式逗号 JOIN（legacyReport），
    /// 验证 MyBatis SQL 抽取 + sqlparser 0.60 新 `JoinOperator::Join` 变体识别的完整链路。
    #[test]
    fn end_to_end_mapper_pipeline() {
        use crate::mapper::parser::extract_sql_from_xmls;

        let xml = r#"<mapper namespace="com.example.OrderMapper">
  <select id="findOrderWithUser">
    SELECT o.id, u.name FROM orders o
    JOIN users u ON o.user_id = u.id
    WHERE o.status = #{status}
  </select>
  <select id="findOrderItems">
    SELECT * FROM orders o JOIN order_items oi ON o.id = oi.order_id
  </select>
  <select id="legacyReport">
    SELECT * FROM a, b WHERE a.id = b.aid AND a.x = b.x
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_relation_e2e.xml");
        std::fs::write(&path, xml).unwrap();

        let results = extract_sql_from_xmls(&[path.clone()]).unwrap();
        let mut total_edges = 0usize;
        let mut saw_explicit = false;
        for (_p, sqls) in &results {
            for sql in sqls {
                let ast = parse_sql_to_ast_fb(&sql.processed_sql, CheckDialect::Generic, None);
                for e in extract_join_edges(&ast, CheckDialect::Generic) {
                    total_edges += 1;
                    if e.join_type == "INNER" {
                        saw_explicit = true;
                    }
                }
            }
        }
        std::fs::remove_file(&path).ok();

        // findOrderWithUser: 1 (orders.user_id → users.id)
        // findOrderItems:    1 (orders.id → order_items.order_id)
        // legacyReport:      2 (a.id→b.aid, a.x→b.x)
        assert_eq!(total_edges, 4, "expected 4 join edges (2 explicit + 2 implicit)");
        assert!(saw_explicit, "explicit JOIN edges must be mined");
    }
}
