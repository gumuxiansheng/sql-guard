//! `sqlguard-mine` —— 逻辑外键挖掘独立可执行文件。
//!
//! 复用 `sqlguard` 库的 mapper 解析与 SQL AST 能力，从 MyBatis Mapper XML 的
//! JOIN 语句中积累「类外键」信息（团队禁止物理外键时的关系补全手段），
//! 产出 `relations.json` 逻辑外键目录。本二进制与主 `sqlguard` 完全独立，
//! 不会把挖掘逻辑链入主程序。

use std::collections::HashMap;

use clap::Parser;
use serde::Serialize;

use sqlguard::config::{CheckDialect, MapperConfig};
use sqlguard::mapper::collect_mapper_files;
use sqlguard::mapper::parser::extract_sql_from_xmls;
use sqlguard::relation::{extract_join_edges, JoinEdge};
use sqlguard::rule::engine::parser::parse_sql_to_ast_fb;

#[derive(Parser, Debug)]
#[clap(
    name = "sqlguard-mine",
    about = "Mine logical foreign keys (class FKs) from MyBatis mapper JOINs",
    version
)]
struct Cli {
    /// Directory or file containing MyBatis mapper XML files to scan.
    #[clap(short = 'p', long, default_value = ".")]
    path: String,

    /// SQL dialect: generic | mysql | postgresql | ansi | oracle.
    ///
    /// Invalid values are rejected with an error message.
    #[clap(long, default_value = "generic")]
    dialect: String,

    /// Dialect fallback: generic | mysql | postgresql | ansi | oracle (optional).
    ///
    /// Invalid values are rejected with an error message.
    #[clap(long)]
    dialect_fallback: Option<String>,

    /// Output JSON path for the logical-FK catalog.
    #[clap(short = 'o', long, default_value = "relations.json")]
    output: String,
}

#[derive(Debug, Clone, Serialize)]
struct Endpoint {
    table: String,
    column: String,
}

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    mapper: String,
    statement_id: String,
    line: usize,
    join_type: String,
}

#[derive(Debug, Clone, Serialize)]
struct Relation {
    from: Endpoint,
    to: Endpoint,
    join_types: Vec<String>,
    direction_hint: String,
    occurrences: Vec<Evidence>,
}

#[derive(Debug, Serialize)]
struct RelationsOutput {
    relations: Vec<Relation>,
    stats: Stats,
}

#[derive(Debug, Serialize)]
struct Stats {
    relation_count: usize,
    occurrence_total: usize,
}

/// 无向关系键：`table.column` 排序后拼接，保证 `a.x=b.y` 与 `b.y=a.x` 归并。
#[derive(Hash, Eq, PartialEq)]
struct RelationKey {
    a: String,
    b: String,
}

fn relation_key(edge: &JoinEdge) -> RelationKey {
    let mut parts = vec![
        format!("{}.{}", edge.left_table, edge.left_column),
        format!("{}.{}", edge.right_table, edge.right_column),
    ];
    parts.sort();
    RelationKey {
        a: parts[0].clone(),
        b: parts[1].clone(),
    }
}

/// 方向启发式：列名以 `_id` 结尾的一侧为外键持有方（指向另一侧的主键）。
fn direction_hint(left: &Endpoint, right: &Endpoint) -> &'static str {
    let l_fk = left.column.ends_with("_id");
    let r_fk = right.column.ends_with("_id");
    match (l_fk, r_fk) {
        (true, false) => "left->right",
        (false, true) => "right->left",
        _ => {
            if left.column == "id" && right.column.ends_with("_id") {
                "right->left"
            } else if right.column == "id" && left.column.ends_with("_id") {
                "left->right"
            } else {
                "undirected"
            }
        }
    }
}

fn relation_from_edge(edge: &JoinEdge) -> Relation {
    let left = Endpoint {
        table: edge.left_table.clone(),
        column: edge.left_column.clone(),
    };
    let right = Endpoint {
        table: edge.right_table.clone(),
        column: edge.right_column.clone(),
    };
    let hint = direction_hint(&left, &right);
    // 把外键持有方规范为 `from`，被引用方为 `to`。
    let (from, to) = if hint == "right->left" {
        (right, left)
    } else {
        (left, right)
    };
    Relation {
        from,
        to,
        join_types: vec![edge.join_type.clone()],
        direction_hint: hint.to_string(),
        occurrences: Vec::new(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let dialect = CheckDialect::from_str(&cli.dialect);
    if dialect.as_str() != cli.dialect.as_str() && dialect == CheckDialect::Generic {
        eprintln!("Warning: unknown dialect '{}', falling back to generic", cli.dialect);
    }
    let fallback = cli
        .dialect_fallback
        .as_deref()
        .map(CheckDialect::from_str)
        .or_else(|| dialect.default_fallback());
    if let Some(ref fb_str) = cli.dialect_fallback {
        let fb = CheckDialect::from_str(fb_str);
        if fb.as_str() != fb_str.as_str() && fb == CheckDialect::Generic {
            eprintln!("Warning: unknown dialect-fallback '{}', falling back to generic", fb_str);
        }
    }

    let mut mapper_cfg = MapperConfig::default();
    mapper_cfg.enabled = true;
    mapper_cfg.paths = vec![cli.path.clone()];

    let root = std::env::current_dir()?;
    let mapper_files = collect_mapper_files(&root, &mapper_cfg, &[]);
    eprintln!("Found {} mapper file(s) under '{}'", mapper_files.len(), cli.path);

    let mut relations: HashMap<RelationKey, Relation> = HashMap::new();
    let mut scanned = 0usize;

    for mf in &mapper_files {
        let results = extract_sql_from_xmls(&[mf.clone()])?;
        for (_path, sqls) in results {
            for sql in sqls {
                scanned += 1;
                let ast = parse_sql_to_ast_fb(&sql.processed_sql, dialect, fallback);
                for edge in extract_join_edges(&ast, dialect) {
                    let evidence = Evidence {
                        mapper: mf.display().to_string(),
                        statement_id: sql.statement_id.clone(),
                        line: sql.raw_xml_line,
                        join_type: edge.join_type.clone(),
                    };
                    let key = relation_key(&edge);
                    let rel = relations
                        .entry(key)
                        .or_insert_with(|| relation_from_edge(&edge));
                    // 收集新的 join_type（去重）
                    if !rel.join_types.contains(&edge.join_type) {
                        rel.join_types.push(edge.join_type.clone());
                    }
                    rel.occurrences.push(evidence);
                }
            }
        }
    }

    let relations_vec: Vec<Relation> = relations.into_values().collect();
    let relation_count = relations_vec.len();
    let occurrence_total: usize = relations_vec.iter().map(|r| r.occurrences.len()).sum();

    let output = RelationsOutput {
        relations: relations_vec,
        stats: Stats {
            relation_count,
            occurrence_total,
        },
    };

    let json = serde_json::to_string_pretty(&output)?;
    std::fs::write(&cli.output, json)?;

    eprintln!(
        "Mined {} mapper SQL statement(s); wrote {} relation(s) with {} total occurrence(s) to '{}'",
        scanned, relation_count, occurrence_total, cli.output
    );

    Ok(())
}
