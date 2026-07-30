//! Rhai 规则引擎性能基准测试。
//!
//! 运行：cargo bench --bench rule_engine
//!
//! 关注指标：
//! - build_engine 一次性开销（Rhai 引擎注册 ~30 个类型 + 上百个方法）
//! - 不同复杂度 SQL 的解析耗时
//! - 单文件规则执行吞吐（目标 1000+ SQL/秒）

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use sqlguard::config::CheckDialect;
use sqlguard::rule::engine::parser::{parse_sql_to_ast, parse_sql_to_ast_fb};
use sqlguard::rule::engine::runner::build_engine;

fn bench_build_engine(c: &mut Criterion) {
    c.bench_function("build_engine", |b| {
        b.iter(|| {
            black_box(build_engine());
        });
    });
}

fn bench_parse_simple_select(c: &mut Criterion) {
    let sql = "SELECT id, name FROM users WHERE id = 1;";
    c.bench_function("parse_simple_select", |b| {
        b.iter(|| {
            black_box(parse_sql_to_ast(black_box(sql), CheckDialect::MySql));
        });
    });
}

fn bench_parse_complex(c: &mut Criterion) {
    let sql = r#"
        WITH active_users AS (
            SELECT id, name FROM users WHERE status = 'active'
        )
        SELECT u.id, u.name, COUNT(o.id) AS order_count
        FROM active_users u
        LEFT JOIN orders o ON u.id = o.user_id
        WHERE u.id > 100
        GROUP BY u.id, u.name
        HAVING COUNT(o.id) > 0
        ORDER BY order_count DESC
        LIMIT 20 OFFSET 10;
    "#;
    c.bench_function("parse_complex_cte_join", |b| {
        b.iter(|| {
            black_box(parse_sql_to_ast(black_box(sql), CheckDialect::MySql));
        });
    });
}

fn bench_parse_with_fallback(c: &mut Criterion) {
    let sql = "SELECT id FROM users WHERE name LIKE '%test%';";
    c.bench_function("parse_with_fallback", |b| {
        b.iter(|| {
            black_box(parse_sql_to_ast_fb(
                black_box(sql),
                CheckDialect::MySql,
                Some(CheckDialect::Generic),
            ));
        });
    });
}

criterion_group!(
    benches,
    bench_build_engine,
    bench_parse_simple_select,
    bench_parse_complex,
    bench_parse_with_fallback,
);
criterion_main!(benches);
