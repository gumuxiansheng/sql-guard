//! SQL 解析器属性测试（模糊测试）。
//!
//! 用 proptest 生成随机/半随机 SQL 文本，验证解析器不会 panic、
//! 且对合法 SQL 能正确识别语句类型。
//!
//! 运行：cargo test --test proptest_parser

use proptest::prelude::*;

use sqlguard::config::CheckDialect;
use sqlguard::rule::engine::parser::parse_sql_to_ast;

proptest! {
    #[test]
    fn parser_never_panics_on_arbitrary_input(s in ".*") {
        let ast = parse_sql_to_ast(&s, CheckDialect::Generic);
        prop_assert!(ast.statements.len() <= 10000);
    }

    #[test]
    fn parser_never_panics_on_random_bytes(s in "[\\x00-\\x7f]{0,500}") {
        let ast = parse_sql_to_ast(&s, CheckDialect::MySql);
        prop_assert!(ast.statements.len() <= 10000);
    }

    #[test]
    fn parser_handles_random_select(cols in "[a-z]{1,10}", table in "[a-z]{1,10}") {
        let sql = format!("SELECT {} FROM {};", cols, table);
        let ast = parse_sql_to_ast(&sql, CheckDialect::Generic);
        prop_assert!(!ast.statements.is_empty());
        prop_assert_eq!(&ast.statements[0].kind, "SELECT");
    }

    #[test]
    fn parser_handles_multiple_statements(n in 1usize..20) {
        let sql: String = std::iter::repeat_n("SELECT 1;", n)
            .collect();
        let ast = parse_sql_to_ast(&sql, CheckDialect::Generic);
        prop_assert_eq!(ast.statements.len(), n);
        for stmt in &ast.statements {
            prop_assert_eq!(&stmt.kind, "SELECT");
        }
    }

    #[test]
    fn parser_line_numbers_monotonic(n in 1usize..10) {
        let sql: String = std::iter::repeat_n("SELECT 1;\n", n)
            .collect();
        let ast = parse_sql_to_ast(&sql, CheckDialect::Generic);
        for window in ast.statements.windows(2) {
            prop_assert!(window[1].line >= window[0].line,
                "line numbers should be non-decreasing: {} < {}",
                window[1].line, window[0].line);
        }
    }

    #[test]
    fn parser_dialect_fallback_consistency(s in "SELECT [a-z]+ FROM [a-z]+;") {
        let mysql_ast = parse_sql_to_ast(&s, CheckDialect::MySql);
        let generic_ast = parse_sql_to_ast(&s, CheckDialect::Generic);
        let pg_ast = parse_sql_to_ast(&s, CheckDialect::PostgreSql);
        prop_assert!(!mysql_ast.statements.is_empty());
        prop_assert!(!generic_ast.statements.is_empty());
        prop_assert!(!pg_ast.statements.is_empty());
    }

    #[test]
    fn parser_only_semicolons(s in ";+") {
        let ast = parse_sql_to_ast(&s, CheckDialect::Generic);
        prop_assert!(ast.statements.len() <= s.len());
    }

    #[test]
    fn parser_deeply_nested_subqueries(depth in 1usize..15) {
        let mut sql = String::from("SELECT 1");
        for _ in 0..depth {
            sql = format!("SELECT id FROM ({}) AS sub", sql);
        }
        sql.push(';');
        let ast = parse_sql_to_ast(&sql, CheckDialect::Generic);
        prop_assert!(ast.statements.len() <= 1);
    }

    #[test]
    fn parser_long_in_list(count in 1usize..100) {
        let values: Vec<String> = (0..count).map(|i| i.to_string()).collect();
        let sql = format!("SELECT id FROM t WHERE id IN ({});", values.join(", "));
        let ast = parse_sql_to_ast(&sql, CheckDialect::Generic);
        prop_assert!(!ast.statements.is_empty());
        prop_assert_eq!(&ast.statements[0].kind, "SELECT");
    }
}

#[test]
fn parser_empty_input_safe() {
    let ast = parse_sql_to_ast("", CheckDialect::Generic);
    assert!(ast.statements.is_empty());
}
