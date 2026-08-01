//! 临时诊断工具：dump mapper 提取出的 processed_sql 及其解析结果，按失败原因归类。
use std::collections::BTreeMap;
use std::path::PathBuf;

use sqlguard::config::CheckDialect;
use sqlguard::mapper;
use sqlguard::rule::engine::parser::parse_sql_to_ast_fb;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: mapdiag <mapper-dir>");
    let show = std::env::args().nth(2).unwrap_or_default();
    let mut files: Vec<PathBuf> = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "xml").unwrap_or(false) {
            files.push(p);
        }
    }
    files.sort();

    let mut total = 0usize;
    let mut failed = 0usize;
    let mut rescued_by_alt = 0usize;
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    // 每个类别保留最多 N 个样例，便于观察触发写法
    let cap: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(6);
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let ok = |sql: &str| {
        let ast = parse_sql_to_ast_fb(sql, CheckDialect::GaussDB, Some(CheckDialect::Oracle));
        ast.parse_error.is_none() && !ast.statements.iter().any(|st| st.kind == "PARSE_ERROR")
    };

    for f in &files {
        let extracted = match mapper::extract_sql_from_xml(f) {
            Ok(v) => v,
            Err(e) => {
                *cats.entry(format!("XML-ERROR: {}", e)).or_default() += 1;
                continue;
            }
        };
        for s in extracted {
            total += 1;
            // 主渲染先做标点补丁再判解析（与 main.rs 一致）
            let primary = mapper::dynamic::repair_dynamic_artifacts(&s.processed_sql);
            if ok(&primary) {
                continue;
            }
            // 主渲染失败 → 依次尝试 ExclusiveNested / FirstBranch 备用渲染
            let mut rescued = false;
            for cand in [s.processed_sql_alt.as_deref(), s.processed_sql_alt2.as_deref()]
                .into_iter()
                .flatten()
            {
                if ok(&mapper::dynamic::repair_dynamic_artifacts(cand)) {
                    rescued = true;
                    break;
                }
            }
            if rescued {
                rescued_by_alt += 1;
                continue;
            }
            failed += 1;
            let sql = primary.trim();
            let cat = categorize(sql, &s.statement_type);
            *cats.entry(cat.clone()).or_default() += 1;
            let bucket = samples.entry(cat).or_default();
            if bucket.len() < cap {
                bucket.push(format!(
                    "{}#{} :: {}",
                    f.file_name().unwrap().to_string_lossy(),
                    s.statement_id,
                    squeeze(sql, 400)
                ));
            }
        }
    }

    println!(
        "files={} statements={} failed={} ({:.1}%)  [alt 兜底救回 {}]",
        files.len(),
        total,
        failed,
        100.0 * failed as f64 / total.max(1) as f64,
        rescued_by_alt
    );
    println!("\n=== 失败归类 ===");
    let mut v: Vec<_> = cats.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    for (k, n) in &v {
        println!("{:>6}  {}", n, k);
    }
    if show == "-v" {
        println!("\n=== 样例 ===");
        for (k, _) in &v {
            if let Some(bucket) = samples.get(*k) {
                println!("\n--- [{}] ({} 例)", k, bucket.len());
                for s in bucket {
                    println!("  {}", s);
                }
            }
        }
    }
}

fn squeeze(s: &str, n: usize) -> String {
    let joined: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(n).collect()
}

fn categorize(sql: &str, ty: &str) -> String {
    let u = squeeze(sql, 100000).to_uppercase();
    if ty == "update" && !u.contains(" SET ") && !u.starts_with("SET ") {
        return "UPDATE 缺 SET（<trim prefix=\"set\">/<set> 被剥离）".to_string();
    }
    if u.contains("WHERE AND ") || u.contains("WHERE OR ") {
        return "WHERE 后残留 AND/OR".to_string();
    }
    if u.starts_with("AND ") || u.contains(" FROM ") && u.contains("\u{0}") {
        return "以 AND 开头".to_string();
    }
    if u.contains("BEGIN ") || u.contains("DECLARE ") {
        return "PL/SQL 块".to_string();
    }
    if u.contains("MERGE ") {
        return "MERGE 语句".to_string();
    }
    if u.matches('(').count() != u.matches(')').count() {
        return "括号不配对（foreach 的 open/close 丢失）".to_string();
    }
    if u.contains(", FROM") || u.contains(",FROM") {
        return "SELECT 列表尾逗号（<trim suffixOverrides=\",\"> 丢失）".to_string();
    }
    if u.contains("VALUES") && (u.contains(",)") || u.contains("(,")) {
        return "INSERT VALUES 逗号问题".to_string();
    }
    if u.contains("CONNECT BY") || u.contains("START WITH") {
        return "Oracle 层次查询".to_string();
    }
    if u.contains("(+)") {
        return "Oracle 老式外连接 (+)".to_string();
    }
    if u.contains("SET ") && ty == "update" && u.contains("SET WHERE") {
        return "UPDATE SET 为空".to_string();
    }
    format!("其他（{}）", ty)
}
