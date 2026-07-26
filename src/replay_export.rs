//! 动态重放清单导出。
//!
//! 复用现有的 SQL 脚本采集与 MyBatis Mapper XML 解析，把每条 SQL 语句
//! 及其类型、来源位置导出为 `sql-manifest.json`，供 Java 侧 `sqlguard-replay`
//! 在镜像库上重放、采集执行计划、识别慢 SQL 与次优计划。
//!
//! 设计要点：
//! - SQL 脚本：用 `parse_sql_to_ast` 切分多语句，按行范围截取语句文本。
//! - Mapper XML：直接使用 `extract_sql_from_xml` 的 `processed_sql`
//!   （已做 `#{}`→`?` 标准化与 `<include>` 解析），一条标签对应一条清单项。
//! - 事务控制语句（COMMIT/ROLLBACK/START TRANSACTION/SET/USE）不导出，
//!   它们不是独立的可重放单元。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::SqlGuardError;
use crate::mapper;
use crate::rule::engine::parser::parse_sql_to_ast;

/// 导出的重放清单，序列化为 `sql-manifest.json`。
#[derive(Debug, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub generator: String,
    pub generated_at: String,
    pub statement_count: usize,
    pub statements: Vec<ManifestStatement>,
}

/// 清单中的单条 SQL 语句。
#[derive(Debug, Serialize)]
pub struct ManifestStatement {
    /// 全局唯一 id，格式 `<source>#<序号>`（脚本）或 `<source>#<statement_id>`（mapper）。
    /// mapper 动态分支变体追加 `#v<序号>` 后缀。
    pub id: String,
    /// 可直接交给 JDBC 的 SQL 文本（Mapper 已标准化占位符为 `?`）。
    pub sql: String,
    /// 语句类型：select / insert / update / delete / merge / ddl / other。
    #[serde(rename = "type")]
    pub stmt_type: String,
    /// 来源文件路径（相对 target_dir 的展示路径）。
    pub source: String,
    /// 来源类别：`sql`（脚本文件）或 `mapper`（MyBatis XML）。
    pub source_type: String,
    /// 起始行（1-indexed）。
    pub line: i64,
    /// 结束行（1-indexed，含）。Mapper 单标签无精确结束行，取起始行。
    pub end_line: i64,
    /// Mapper 标签 id（`<select id="xxx">`），SQL 脚本为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
    /// 解析错误信息（仅当该语句无法解析时存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    /// 动态分支变体：指向原 mapper statement 的 id（如 `mapper/User.xml#selectById`）。
    /// 仅 mapper 动态分支展开的变体有此字段；SQL 脚本与无动态分支的 mapper 为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant_of: Option<String>,
    /// 动态分支组合描述（如 `if:name!=null=true,foreach:1elem`）。
    /// 仅 mapper 动态分支展开的变体有此字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant_label: Option<String>,
}

/// 解析类型过滤器。空集合表示不过滤（导出全部）。
pub fn parse_type_filter(s: &Option<String>) -> HashSet<String> {
    match s {
        Some(t) if !t.trim().is_empty() => t
            .split(',')
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => HashSet::new(),
    }
}

/// 收集并构建重放清单。
pub fn build_manifest(
    target_dir: &Path,
    sql_files: &[PathBuf],
    mapper_files: &[PathBuf],
    type_filter: &HashSet<String>,
) -> Result<Manifest, SqlGuardError> {
    let mut statements: Vec<ManifestStatement> = Vec::new();

    // SQL 脚本模式：解析多语句，按行范围截取文本
    // id 使用 per-file 序号（每个文件从 1 开始），保证前面文件增删不会平移后续编号。
    for file_path in sql_files {
        let content = fs::read_to_string(file_path).map_err(|e| {
            SqlGuardError::CheckError(format!("Failed to read '{}': {}", file_path.display(), e))
        })?;
        let mut seq: usize = 0;
        let ast = parse_sql_to_ast(&content, crate::config::CheckDialect::Generic);
        let total_lines = content.lines().count() as i64;
        let source = display_path(file_path, target_dir);
        let stmt_count = ast.statements.len();
        for (i, stmt) in ast.statements.iter().enumerate() {
            match replay_type(&stmt.kind) {
                None => continue,
                Some(ty) => {
                    if !type_filter.is_empty() && !type_filter.contains(ty) {
                        continue;
                    }
                    seq += 1;
                    let parse_error = if stmt.kind == "PARSE_ERROR" {
                        Some("parse error".to_string())
                    } else {
                        None
                    };
                    // 语句结束行：优先用下一条语句的起始行 - 1（能正确覆盖
                    // 结束符 `)`/`;` 单独成行的情况）；末条语句延伸到 EOF。
                    let effective_end = if i + 1 < stmt_count {
                        let next_line = ast.statements[i + 1].line;
                        if next_line > stmt.line {
                            next_line - 1
                        } else {
                            stmt.end_line
                        }
                    } else {
                        total_lines.max(stmt.end_line)
                    };
                    statements.push(ManifestStatement {
                        id: format!("{}#{}", source, seq),
                        sql: slice_by_lines(&content, stmt.line, effective_end),
                        stmt_type: ty.to_string(),
                        source: source.clone(),
                        source_type: "sql".to_string(),
                        line: stmt.line,
                        end_line: effective_end,
                        statement_id: None,
                        parse_error,
                        variant_of: None,
                        variant_label: None,
                    });
                }
            }
        }
    }

    // Mapper XML 模式：保留动态 SQL 结构，按分支组合展开为多个变体。
    // 每条 <select>/<insert>/<update>/<delete> 经 dynamic 模块展开后，
    // 无动态分支 → 1 条（不加 #vN）；有动态分支 → N 条（加 #v<序号>）。
    for file_path in mapper_files {
        let dyn_stmts = match mapper::dynamic::parse_dynamic_statements(file_path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "Warning: failed to parse mapper XML '{}': {}",
                    file_path.display(),
                    e
                );
                continue;
            }
        };
        let source = display_path(file_path, target_dir);
        for stmt in &dyn_stmts {
            let ty = stmt.statement_type.as_str();
            if !type_filter.is_empty() && !type_filter.contains(ty) {
                continue;
            }
            let line = stmt.raw_xml_line as i64;
            let base_id = format!("{}#{}", source, stmt.statement_id);
            let variants = mapper::dynamic::expand_variants(stmt, mapper::dynamic::DEFAULT_MAX_INDEPENDENT_IFS);

            // 判断是否有动态分支：变体数 > 1，或唯一变体的 label 非空
            let has_dynamic = variants.len() > 1
                || variants.iter().any(|v| !v.label.is_empty());

            for (vi, v) in variants.iter().enumerate() {
                // 解析变体 SQL 仅用于检测语法错误，不改写导出文本
                let parsed = parse_sql_to_ast(&v.sql, crate::config::CheckDialect::Generic);
                let parse_error = parsed
                    .parse_error
                    .clone()
                    .or_else(|| {
                        if parsed.statements.iter().any(|s| s.kind == "PARSE_ERROR") {
                            Some("parse error in mapper variant".to_string())
                        } else {
                            None
                        }
                    });

                let (id, variant_of, variant_label) = if has_dynamic {
                    (
                        format!("{}#v{}", base_id, vi + 1),
                        Some(base_id.clone()),
                        Some(v.label.clone()),
                    )
                } else {
                    (base_id.clone(), None, None)
                };

                statements.push(ManifestStatement {
                    id,
                    sql: v.sql.trim().to_string(),
                    stmt_type: ty.to_string(),
                    source: source.clone(),
                    source_type: "mapper".to_string(),
                    line,
                    end_line: line,
                    statement_id: Some(stmt.statement_id.clone()),
                    parse_error,
                    variant_of,
                    variant_label,
                });
            }
        }
    }

    Ok(Manifest {
        version: 1,
        generator: "sqlguard replay-export".to_string(),
        generated_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default(),
        statement_count: statements.len(),
        statements,
    })
}

/// 把清单序列化为 JSON 字符串。
pub fn manifest_to_json(manifest: &Manifest) -> Result<String, SqlGuardError> {
    serde_json::to_string_pretty(manifest)
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to serialize manifest: {}", e)))
}

/// 把 StmtInfo.kind 映射为重放类型。返回 None 表示该语句不导出（事务控制等）。
fn replay_type(kind: &str) -> Option<&'static str> {
    match kind {
        "SELECT" => Some("select"),
        "INSERT" => Some("insert"),
        "UPDATE" => Some("update"),
        "DELETE" => Some("delete"),
        "MERGE" => Some("merge"),
        "CREATE_TABLE" | "CREATE_INDEX" | "CREATE_VIEW" | "ALTER_TABLE" | "TRUNCATE"
        | "GRANT" | "REVOKE" => Some("ddl"),
        "PARSE_ERROR" => Some("other"),
        // 事务控制 / 会话控制：不是独立可重放单元
        "START_TRANSACTION" | "COMMIT" | "ROLLBACK" | "SET_VARIABLE" | "USE" => None,
        _ => {
            // DROP_TABLE / DROP_INDEX / DROP_VIEW / DROP_SCHEMA 等
            if kind.starts_with("DROP_") {
                Some("ddl")
            } else {
                Some("other")
            }
        }
    }
}

/// 按行范围（1-indexed，含）截取语句文本，去掉尾部分号。
///
/// 限制：sqlparser 仅提供起始行、无列号，同一行有多条语句时会截取到重叠/重复文本。
/// 对 MyBatis Mapper 不受此影响（按 statement_id 而非行号切分）。
fn slice_by_lines(content: &str, line: i64, end_line: i64) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start_idx = ((line - 1).max(0) as usize).min(lines.len());
    let end_exclusive = (end_line.max(0) as usize).min(lines.len());
    if start_idx >= end_exclusive {
        return String::new();
    }
    let text = lines[start_idx..end_exclusive].join("\n");
    text.trim_end()
        .trim_end_matches(';')
        .trim_end()
        .to_string()
}

/// 把绝对路径转成相对 target_dir 的展示路径（回退到原路径），统一正斜杠。
fn display_path(file_path: &Path, target_dir: &Path) -> String {
    file_path
        .strip_prefix(target_dir)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"))
}
