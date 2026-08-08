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
//!
//! 增量导出（`replay-export --base <ref>`）：复用 `git_diff` 的 hunk 解析，
//! 只导出自 git 基线以来新增/修改的语句，并通过旧侧 hunk 识别被删除的语句
//! （写入独立的 `sql-manifest-removed.json`）。详见
//! `docs/replay-export-incremental-design.md`。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::SqlGuardError;
use crate::git_diff::{self, FileDiff};
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
    /// 增量导出时的 git 基线（可选；全量导出缺省）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// 增量导出标记（可选；全量导出缺省）。旧版消费方忽略未知字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<bool>,
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
    /// 增量导出时的变更状态：`added`（新文件）/ `modified`（改动文件内命中 hunk）。
    /// 全量导出缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

/// 增量导出中被删除语句的元信息（序列化到 `sql-manifest-removed.json`）。
#[derive(Debug, Serialize)]
pub struct RemovedStatement {
    /// 与旧清单一致的 id（旧文件顺序编号）。
    pub id: String,
    /// 来源文件路径（相对 target_dir 的展示路径）。
    pub source: String,
    /// 来源类别：`sql` / `mapper`。
    pub source_type: String,
    /// 起始行（1-indexed，旧文件行号）。
    pub line: i64,
    /// 结束行（1-indexed，含，旧文件行号）。
    pub end_line: i64,
    /// Mapper 标签 id，SQL 脚本为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
}

/// 被删语句清单，序列化为 `sql-manifest-removed.json`。
#[derive(Debug, Serialize)]
pub struct RemovedManifest {
    pub version: u32,
    pub generator: String,
    pub base: String,
    pub generated_at: String,
    pub removed_count: usize,
    pub removed: Vec<RemovedStatement>,
}

/// 增量导出结果：主清单（仅含新增/修改语句）+ 被删语句列表。
pub struct IncrementalManifest {
    pub manifest: Manifest,
    pub removed: Vec<RemovedStatement>,
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

/// 收集并构建重放清单（全量导出）。
///
/// `trust_dynamic`：信任跳过 MyBatis `${}` 替换（对应 `Config.trust_dynamic_substitution`）。
/// `encoding`：读取 SQL / Mapper XML 文件所用的编码标签（`[scan] encoding`）。
pub fn build_manifest(
    target_dir: &Path,
    sql_files: &[PathBuf],
    mapper_files: &[PathBuf],
    type_filter: &HashSet<String>,
    trust_dynamic: bool,
    encoding: &str,
    progress: &mut dyn FnMut(usize, usize, &Path),
) -> Result<Manifest, SqlGuardError> {
    let mut statements: Vec<ManifestStatement> = Vec::new();
    let total = sql_files.len() + mapper_files.len();
    let mut done = 0;

    // SQL 脚本模式：解析多语句，按行范围截取文本
    // id 使用 per-file 序号（每个文件从 1 开始），保证前面文件增删不会平移后续编号。
    for file_path in sql_files {
        let content = crate::encoding::read_to_string(file_path, encoding)
            .map_err(SqlGuardError::CheckError)?;
        statements.extend(sql_file_statements(
            file_path,
            &content,
            target_dir,
            type_filter,
        ));
        done += 1;
        progress(done, total, file_path);
    }

    // Mapper XML 模式：保留动态 SQL 结构，按分支组合展开为多个变体。
    // 每条 <select>/<insert>/<update>/<delete> 经 dynamic 模块展开后，
    // 无动态分支 → 1 条（不加 #vN）；有动态分支 → N 条（加 #v<序号>）。
    for file_path in mapper_files {
        statements.extend(mapper_file_statements(
            file_path,
            target_dir,
            type_filter,
            trust_dynamic,
            encoding,
        ));
        done += 1;
        progress(done, total, file_path);
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
        base: None,
        incremental: None,
    })
}

/// 解析单个 SQL 脚本文件，生成清单条目。
///
/// 序号（`<source>#<N>`）在**全部通过类型过滤的语句**上递增（含后续可能被
/// hunk 过滤掉的语句），保证增量导出中保留语句的 id 与全量导出一致。
fn sql_file_statements(
    file_path: &Path,
    content: &str,
    target_dir: &Path,
    type_filter: &HashSet<String>,
) -> Vec<ManifestStatement> {
    let mut statements: Vec<ManifestStatement> = Vec::new();
    let mut seq: usize = 0;
    let ast = parse_sql_to_ast(content, crate::config::CheckDialect::Generic);
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
                    sql: slice_by_lines(content, stmt.line, effective_end),
                    stmt_type: ty.to_string(),
                    source: source.clone(),
                    source_type: "sql".to_string(),
                    line: stmt.line,
                    end_line: effective_end,
                    statement_id: None,
                    parse_error,
                    variant_of: None,
                    variant_label: None,
                    change: None,
                });
            }
        }
    }
    statements
}

/// 解析单个 Mapper XML 文件，生成清单条目（动态分支展开为多个变体）。
///
/// `trust_dynamic`：信任跳过 MyBatis `${}` 替换（对应 `Config.trust_dynamic_substitution`）。
/// 开启时，含 `${}` 的语句若因运行时片段残缺而解析失败，不报普通「解析错误」，
/// 而是标记为「含 ${} 运行时替换、未静态校验」——既诚实标注，又不退化成作者错误，
/// 与 `run_rules_for_file` 的 `DYN` 警告语义一致。
fn mapper_file_statements(
    file_path: &Path,
    target_dir: &Path,
    type_filter: &HashSet<String>,
    trust_dynamic: bool,
    encoding: &str,
) -> Vec<ManifestStatement> {
    let mut statements: Vec<ManifestStatement> = Vec::new();
    let dyn_stmts = match mapper::dynamic::parse_dynamic_statements(file_path, encoding) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "Warning: failed to parse mapper XML '{}': {}",
                file_path.display(),
                e
            );
            return statements;
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
        let variants =
            mapper::dynamic::expand_variants(stmt, mapper::dynamic::DEFAULT_MAX_INDEPENDENT_IFS);

        // 是否含 MyBatis `${}` 运行时文本替换（静态期不可解析）。
        let has_dollar = mapper::dynamic::contains_dollar_substitution(stmt);

        // 判断是否有动态分支：变体数 > 1，或唯一变体的 label 非空
        let has_dynamic = variants.len() > 1 || variants.iter().any(|v| !v.label.is_empty());

        for (vi, v) in variants.iter().enumerate() {
            // 解析变体 SQL 仅用于检测语法错误，不改写导出文本
            let parsed = parse_sql_to_ast(&v.sql, crate::config::CheckDialect::Generic);
            let parse_error = if parsed.parse_error.is_some()
                || parsed.statements.iter().any(|s| s.kind == "PARSE_ERROR")
            {
                if has_dollar && trust_dynamic {
                    // 信任跳过：解析失败源于运行时 `${}` 片段，预期内、非作者错误。
                    // 保留 parse_error（Java 重放侧据此跳过不可静态解析的 SQL），
                    // 但明确标注为「动态未校验」，区别于真正的语法缺陷。
                    Some(
                        "contains ${} runtime substitution; statically unchecked \
                         (trust_dynamic_substitution enabled)"
                            .to_string(),
                    )
                } else {
                    // 优先保留解析器原始错误详情（tokenize 失败如 `SQL tokenize error: ...`）；
                    // PARSE_ERROR 语句无原始详情，回退通用文案。
                    Some(
                        parsed
                            .parse_error
                            .clone()
                            .unwrap_or_else(|| "parse error in mapper variant".to_string()),
                    )
                }
            } else {
                None
            };

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
                change: None,
            });
        }
    }
    statements
}

/// 把清单序列化为 JSON 字符串。
pub fn manifest_to_json(manifest: &Manifest) -> Result<String, SqlGuardError> {
    serde_json::to_string_pretty(manifest)
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to serialize manifest: {}", e)))
}

/// 把被删语句清单序列化为 JSON 字符串。
pub fn removed_manifest_to_json(removed: &RemovedManifest) -> Result<String, SqlGuardError> {
    serde_json::to_string_pretty(removed).map_err(|e| {
        SqlGuardError::CheckError(format!("Failed to serialize removed manifest: {}", e))
    })
}

/// 构建增量重放清单：只导出自 git 基线以来新增/修改的语句，并识别被删除的语句。
///
/// - `diffs`：`git_diff::get_diff(base, ...)` 的输出，路径相对 git 运行目录
///   （当前工作目录）；本函数按「采集到的绝对文件路径相对 cwd / target_dir」匹配。
/// - 新增文件（`is_new`）整文件导出，`change = "added"`；改动文件内语句
///   `[line, end_line] ∩ hunk` 非空才导出，`change = "modified"`。
/// - 被删语句：对改动文件取 `git show <base>:<path>` 的旧内容解析，语句行范围
///   与旧侧 hunk（`old_hunks`）有交集即标为 removed；**但旧 id 仍存在于新清单的
///   语句不算删除**（那是修改，删除行的旧侧 hunk 会覆盖被替换行）。
///   removed 的 id 使用旧文件编号，与历史全量清单一致。
/// - `encoding`：读取 SQL / Mapper XML 文件所用的编码标签（`[scan] encoding`）。
///
/// 已知限制：脚本语句 id 为文件内顺序编号。若删除文件头部的语句导致后续语句
/// 编号平移，id 可能与新清单中的其他语句碰撞，被删除的语句会被误判为「修改」。
/// 与 `docs/replay-export-incremental-design.md` 中的方案 B（id 差集）限制一致；
/// CI 可周期性跑一次全量导出校准归档。
#[allow(clippy::too_many_arguments)]
pub fn build_incremental_manifest(
    target_dir: &Path,
    sql_files: &[PathBuf],
    mapper_files: &[PathBuf],
    diffs: &[FileDiff],
    base: &str,
    type_filter: &HashSet<String>,
    trust_dynamic: bool,
    encoding: &str,
    progress: &mut dyn FnMut(usize, usize, &Path),
) -> Result<IncrementalManifest, SqlGuardError> {
    build_incremental_manifest_with_loader(
        target_dir,
        sql_files,
        mapper_files,
        diffs,
        base,
        type_filter,
        trust_dynamic,
        encoding,
        &|diff| git_diff::git_show(base, &diff.path),
        progress,
    )
}

/// [`build_incremental_manifest`] 的可注入版本：旧文件内容由 `old_loader` 提供
/// （返回 `Ok(None)` 表示 base 中无此文件），便于单元测试不依赖真实 git。
///
/// `trust_dynamic`：信任跳过 MyBatis `${}` 替换（对应 `Config.trust_dynamic_substitution`）。
/// `encoding`：读取 SQL / Mapper XML 文件所用的编码标签（`[scan] encoding`）。
///
/// `progress`：每处理完一个文件调用一次 `(已完成数, 总文件数, 当前文件)`，用于进度展示；
/// 不需要进度时传入 `&mut |_, _, _| {}`。
#[allow(clippy::too_many_arguments)]
fn build_incremental_manifest_with_loader(
    target_dir: &Path,
    sql_files: &[PathBuf],
    mapper_files: &[PathBuf],
    diffs: &[FileDiff],
    base: &str,
    type_filter: &HashSet<String>,
    trust_dynamic: bool,
    encoding: &str,
    old_loader: &dyn Fn(&FileDiff) -> Result<Option<String>, SqlGuardError>,
    progress: &mut dyn FnMut(usize, usize, &Path),
) -> Result<IncrementalManifest, SqlGuardError> {
    let mut statements: Vec<ManifestStatement> = Vec::new();
    let mut removed: Vec<RemovedStatement> = Vec::new();
    let total = sql_files.len() + mapper_files.len();
    let mut done = 0;

    // SQL 脚本模式
    for file_path in sql_files {
        let diff = match match_diff(file_path, target_dir, diffs) {
            Some(d) => d,
            None => {
                done += 1;
                progress(done, total, file_path);
                continue; // 未改动文件不导出
            }
        };
        let content = crate::encoding::read_to_string(file_path, encoding)
            .map_err(SqlGuardError::CheckError)?;
        let source = display_path(file_path, target_dir);
        let change = if diff.is_new { "added" } else { "modified" };
        for mut stmt in sql_file_statements(file_path, &content, target_dir, type_filter) {
            if diff.is_new || intersects_hunks(stmt.line, stmt.end_line, &diff.hunks) {
                stmt.change = Some(change.to_string());
                statements.push(stmt);
            }
        }
        // 被删语句：仅对非新增文件、且存在旧侧删除范围时检测
        if !diff.is_new && !diff.old_hunks.is_empty() {
            if let Some(old_content) = old_loader(diff)? {
                removed.extend(removed_from_sql_file(
                    &old_content,
                    &source,
                    &diff.old_hunks,
                    type_filter,
                ));
            }
        }
        done += 1;
        progress(done, total, file_path);
    }

    // Mapper XML 模式
    for file_path in mapper_files {
        let diff = match match_diff(file_path, target_dir, diffs) {
            Some(d) => d,
            None => {
                done += 1;
                progress(done, total, file_path);
                continue;
            }
        };
        let source = display_path(file_path, target_dir);
        let change = if diff.is_new { "added" } else { "modified" };
        for mut stmt in
            mapper_file_statements(file_path, target_dir, type_filter, trust_dynamic, encoding)
        {
            // mapper 语句锚点为标签起始行；命中 hunk 即导出该标签全部变体
            if diff.is_new || intersects_hunks(stmt.line, stmt.end_line, &diff.hunks) {
                stmt.change = Some(change.to_string());
                statements.push(stmt);
            }
        }
        if !diff.is_new && !diff.old_hunks.is_empty() {
            if let Some(old_content) = old_loader(diff)? {
                removed.extend(removed_from_mapper_file(
                    &old_content,
                    &source,
                    &diff.old_hunks,
                    type_filter,
                ));
            }
        }
        done += 1;
        progress(done, total, file_path);
    }

    // 旧 id 仍存在于新清单的候选不是删除（是修改）：
    // 脚本 id 碰撞场景见函数文档的已知限制。
    let new_ids: HashSet<String> = statements.iter().map(|s| s.id.clone()).collect();
    removed.retain(|r| !id_still_exists(&r.id, &new_ids));

    let manifest = Manifest {
        version: 1,
        generator: "sqlguard replay-export".to_string(),
        generated_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default(),
        statement_count: statements.len(),
        statements,
        base: Some(base.to_string()),
        incremental: Some(true),
    };

    Ok(IncrementalManifest { manifest, removed })
}

/// removed id 是否仍存在于新清单（含 mapper 动态变体 `#vN` 后缀）。
fn id_still_exists(removed_id: &str, new_ids: &HashSet<String>) -> bool {
    new_ids.contains(removed_id)
        || new_ids
            .iter()
            .any(|id| id.starts_with(&format!("{}#v", removed_id)))
}

/// 从旧文件内容解析被删除的 SQL 语句。
///
/// id 按旧文件顺序编号（与历史全量清单一致），行号使用旧文件行号。
fn removed_from_sql_file(
    old_content: &str,
    source: &str,
    old_hunks: &[(usize, usize)],
    type_filter: &HashSet<String>,
) -> Vec<RemovedStatement> {
    // 复用 sql_file_statements 的行号/id 计算；路径仅用于 display_path，
    // 传入占位路径 + 根目录使 source 原样返回。
    let entries = sql_file_statements(Path::new(source), old_content, Path::new("/"), type_filter);
    entries
        .into_iter()
        .filter(|s| intersects_hunks(s.line, s.end_line, old_hunks))
        .map(|s| RemovedStatement {
            id: s.id,
            source: source.to_string(),
            source_type: "sql".to_string(),
            line: s.line,
            end_line: s.end_line,
            statement_id: None,
        })
        .collect()
}

/// 从旧 Mapper XML 内容解析被删除的语句（按标签粒度，不展开变体）。
fn removed_from_mapper_file(
    old_content: &str,
    source: &str,
    old_hunks: &[(usize, usize)],
    type_filter: &HashSet<String>,
) -> Vec<RemovedStatement> {
    let dyn_stmts = match mapper::dynamic::parse_dynamic_statements_from_content(old_content, None)
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "Warning: failed to parse old mapper content '{}': {}",
                source, e
            );
            return Vec::new();
        }
    };
    dyn_stmts
        .into_iter()
        .filter(|s| {
            let ty = s.statement_type.as_str();
            if !type_filter.is_empty() && !type_filter.contains(ty) {
                return false;
            }
            let line = s.raw_xml_line as i64;
            intersects_hunks(line, line, old_hunks)
        })
        .map(|s| RemovedStatement {
            id: format!("{}#{}", source, s.statement_id),
            source: source.to_string(),
            source_type: "mapper".to_string(),
            line: s.raw_xml_line as i64,
            end_line: s.raw_xml_line as i64,
            statement_id: Some(s.statement_id),
        })
        .collect()
}

/// 把采集到的绝对文件路径匹配到 git diff 的改动项（路径相对 cwd）。
///
/// 依次尝试：相对 cwd（git 在仓库根运行）、相对 target_dir（扫描子目录场景）。
fn match_diff<'a>(file: &Path, target_dir: &Path, diffs: &'a [FileDiff]) -> Option<&'a FileDiff> {
    let cwd = std::env::current_dir().ok();
    let rel = cwd
        .as_deref()
        .and_then(|c| file.strip_prefix(c).ok())
        .or_else(|| file.strip_prefix(target_dir).ok())
        .unwrap_or(file);
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    diffs
        .iter()
        .find(|d| d.path.to_string_lossy().replace('\\', "/") == rel_str)
}

/// 语句行范围 `[line, end_line]` 与任一 hunk `[s, e]` 是否有交集。
///
/// 提取为 `pub` 以便 `sqlguard-mine` 复用同一套「语句级 diff 交集」逻辑。
pub fn intersects_hunks(line: i64, end_line: i64, hunks: &[(usize, usize)]) -> bool {
    hunks.iter().any(|(s, e)| {
        let (s, e) = (*s as i64, *e as i64);
        s <= end_line && e >= line
    })
}

/// 把 StmtInfo.kind 映射为重放类型。返回 None 表示该语句不导出（事务控制等）。
fn replay_type(kind: &str) -> Option<&'static str> {
    match kind {
        "SELECT" => Some("select"),
        "INSERT" => Some("insert"),
        "UPDATE" => Some("update"),
        "DELETE" => Some("delete"),
        "MERGE" => Some("merge"),
        "CREATE_TABLE" | "CREATE_INDEX" | "CREATE_VIEW" | "ALTER_TABLE" | "TRUNCATE" | "GRANT"
        | "REVOKE" => Some("ddl"),
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
    text.trim_end().trim_end_matches(';').trim_end().to_string()
}

/// 把绝对路径转成相对 target_dir 的展示路径（回退到原路径），统一正斜杠。
fn display_path(file_path: &Path, target_dir: &Path) -> String {
    file_path
        .strip_prefix(target_dir)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // === parse_type_filter ===

    #[test]
    fn parse_type_filter_none() {
        let result = parse_type_filter(&None);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_type_filter_empty_string() {
        let result = parse_type_filter(&Some("".to_string()));
        assert!(result.is_empty());
    }

    #[test]
    fn parse_type_filter_single() {
        let result = parse_type_filter(&Some("select".to_string()));
        assert_eq!(result.len(), 1);
        assert!(result.contains("select"));
    }

    #[test]
    fn parse_type_filter_multiple() {
        let result = parse_type_filter(&Some("select, insert, delete".to_string()));
        assert_eq!(result.len(), 3);
        assert!(result.contains("select"));
        assert!(result.contains("insert"));
        assert!(result.contains("delete"));
    }

    #[test]
    fn parse_type_filter_normalizes_case() {
        let result = parse_type_filter(&Some("SELECT, Insert".to_string()));
        assert!(result.contains("select"));
        assert!(result.contains("insert"));
    }

    #[test]
    fn parse_type_filter_trims_whitespace() {
        let result = parse_type_filter(&Some("  select  ,  insert  ".to_string()));
        assert_eq!(result.len(), 2);
        assert!(result.contains("select"));
        assert!(result.contains("insert"));
    }

    // === replay_type ===

    #[test]
    fn replay_type_select() {
        assert_eq!(replay_type("SELECT"), Some("select"));
    }

    #[test]
    fn replay_type_insert() {
        assert_eq!(replay_type("INSERT"), Some("insert"));
    }

    #[test]
    fn replay_type_update() {
        assert_eq!(replay_type("UPDATE"), Some("update"));
    }

    #[test]
    fn replay_type_delete() {
        assert_eq!(replay_type("DELETE"), Some("delete"));
    }

    #[test]
    fn replay_type_ddl() {
        assert_eq!(replay_type("CREATE_TABLE"), Some("ddl"));
        assert_eq!(replay_type("ALTER_TABLE"), Some("ddl"));
        assert_eq!(replay_type("DROP_TABLE"), Some("ddl"));
        assert_eq!(replay_type("TRUNCATE"), Some("ddl"));
    }

    #[test]
    fn replay_type_transaction_skipped() {
        assert_eq!(replay_type("COMMIT"), None);
        assert_eq!(replay_type("ROLLBACK"), None);
        assert_eq!(replay_type("START_TRANSACTION"), None);
        assert_eq!(replay_type("SET_VARIABLE"), None);
        assert_eq!(replay_type("USE"), None);
    }

    #[test]
    fn replay_type_parse_error() {
        assert_eq!(replay_type("PARSE_ERROR"), Some("other"));
    }

    #[test]
    fn replay_type_unknown() {
        assert_eq!(replay_type("SOMETHING_NEW"), Some("other"));
    }

    // === slice_by_lines ===

    #[test]
    fn slice_by_lines_basic() {
        let content = "line1\nline2\nline3";
        assert_eq!(slice_by_lines(content, 1, 2), "line1\nline2");
    }

    #[test]
    fn slice_by_lines_single() {
        let content = "a\nb\nc";
        assert_eq!(slice_by_lines(content, 2, 2), "b");
    }

    #[test]
    fn slice_by_lines_trims_trailing_semicolon() {
        let content = "SELECT 1;\nSELECT 2;";
        assert_eq!(slice_by_lines(content, 1, 1), "SELECT 1");
    }

    #[test]
    fn slice_by_lines_empty_content() {
        assert_eq!(slice_by_lines("", 1, 5), "");
    }

    #[test]
    fn slice_by_lines_out_of_range() {
        let content = "a\nb";
        assert_eq!(slice_by_lines(content, 5, 10), "");
    }

    #[test]
    fn slice_by_lines_full_range() {
        let content = "a\nb\nc";
        assert_eq!(slice_by_lines(content, 1, 3), "a\nb\nc");
    }

    // === display_path ===

    #[test]
    fn display_path_relative() {
        let target = Path::new("/tmp/project");
        let file = Path::new("/tmp/project/sql/dml/users.sql");
        assert_eq!(display_path(file, target), "sql/dml/users.sql");
    }

    #[test]
    fn display_path_not_prefix() {
        let target = Path::new("/tmp/project");
        let file = Path::new("/other/dir/users.sql");
        assert_eq!(display_path(file, target), "/other/dir/users.sql");
    }

    #[test]
    fn display_path_normalizes_backslashes() {
        let target = Path::new("/tmp/project");
        let file = Path::new("/tmp/project/sql\\dml\\users.sql");
        let result = display_path(file, target);
        assert!(result.contains("sql/dml/users.sql") || result.contains("sql\\dml\\users.sql"));
    }

    // === manifest_to_json ===

    #[test]
    fn manifest_to_json_valid() {
        let manifest = Manifest {
            version: 1,
            generator: "test".to_string(),
            generated_at: "123".to_string(),
            statement_count: 0,
            statements: vec![],
            base: None,
            incremental: None,
        };
        let json = manifest_to_json(&manifest).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["statement_count"], 0);
        assert!(
            parsed.get("base").is_none(),
            "full export must not set base"
        );
    }

    #[test]
    fn manifest_to_json_incremental_fields() {
        let manifest = Manifest {
            version: 1,
            generator: "test".to_string(),
            generated_at: "123".to_string(),
            statement_count: 0,
            statements: vec![],
            base: Some("origin/main".to_string()),
            incremental: Some(true),
        };
        let json = manifest_to_json(&manifest).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["base"], "origin/main");
        assert_eq!(parsed["incremental"], true);
    }

    // === 增量导出 ===

    #[test]
    fn intersects_hunks_basic() {
        assert!(intersects_hunks(5, 8, &[(8, 15)]));
        assert!(intersects_hunks(10, 20, &[(15, 25)]));
        assert!(intersects_hunks(1, 3, &[(3, 3)]));
        assert!(!intersects_hunks(1, 3, &[(4, 10)]));
        assert!(!intersects_hunks(20, 30, &[(4, 10)]));
        assert!(!intersects_hunks(5, 5, &[]));
    }

    #[test]
    fn build_incremental_manifest_new_file_all_added() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("new.sql");
        std::fs::write(&sql_path, "SELECT 1;\nSELECT 2;\n").unwrap();

        let diff = FileDiff {
            path: PathBuf::from("new.sql"),
            hunks: vec![(1, 2)],
            old_hunks: vec![],
            is_new: true,
        };
        let result = build_incremental_manifest(
            dir.path(),
            &[sql_path],
            &[],
            &[diff],
            "origin/main",
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();

        assert_eq!(result.manifest.statement_count, 2);
        assert_eq!(result.manifest.base.as_deref(), Some("origin/main"));
        assert_eq!(result.manifest.incremental, Some(true));
        assert_eq!(result.removed.len(), 0);
        assert_eq!(
            result.manifest.statements[0].change.as_deref(),
            Some("added")
        );
        assert_eq!(
            result.manifest.statements[1].change.as_deref(),
            Some("added")
        );
    }

    #[test]
    fn build_incremental_manifest_unchanged_file_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("a.sql");
        std::fs::write(&sql_path, "SELECT 1;\nSELECT 2;\n").unwrap();
        let other_path = dir.path().join("b.sql");
        std::fs::write(&other_path, "SELECT 9;\n").unwrap();

        let diff = FileDiff {
            path: PathBuf::from("a.sql"),
            hunks: vec![(1, 1)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = build_incremental_manifest(
            dir.path(),
            &[sql_path, other_path.clone()],
            &[],
            &[diff],
            "HEAD~1",
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();

        // b.sql 不在 diff 中：整体不导出
        assert_eq!(result.manifest.statement_count, 1);
        assert_eq!(
            result.manifest.statements[0].change.as_deref(),
            Some("modified")
        );
        // 新增/修改侧 hunk 只命中第 1 条
        assert_eq!(result.manifest.statements[0].line, 1);
    }

    #[test]
    fn build_incremental_manifest_hunk_filters_sql() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("a.sql");
        std::fs::write(
            &sql_path,
            "SELECT 1;\nSELECT 2;\nSELECT 3;\nSELECT 4;\nSELECT 5;\n",
        )
        .unwrap();

        // 只命中第 3 条（行 3）
        let diff = FileDiff {
            path: PathBuf::from("a.sql"),
            hunks: vec![(3, 3)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = build_incremental_manifest(
            dir.path(),
            &[sql_path],
            &[],
            &[diff],
            "origin/main",
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();

        assert_eq!(result.manifest.statement_count, 1);
        assert_eq!(result.manifest.statements[0].line, 3);
        assert_eq!(result.manifest.statements[0].id, "a.sql#3");
        // id 与全量导出编号一致（序号按全部语句计数，而非只计保留语句）
        assert_eq!(
            result.manifest.statements[0].change.as_deref(),
            Some("modified")
        );
    }

    #[test]
    fn build_incremental_manifest_with_type_filter() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("a.sql");
        std::fs::write(&sql_path, "SELECT 1;\nINSERT INTO t VALUES (1);\n").unwrap();

        let diff = FileDiff {
            path: PathBuf::from("a.sql"),
            hunks: vec![(1, 2)],
            old_hunks: vec![],
            is_new: false,
        };
        let mut filter = HashSet::new();
        filter.insert("insert".to_string());

        let result = build_incremental_manifest(
            dir.path(),
            &[sql_path],
            &[],
            &[diff],
            "origin/main",
            &filter,
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();

        assert_eq!(result.manifest.statement_count, 1);
        assert_eq!(result.manifest.statements[0].stmt_type, "insert");
    }

    #[test]
    fn build_incremental_manifest_mapper_keeps_all_variants() {
        let dir = tempfile::tempdir().unwrap();
        let xml = r#"<mapper>
  <select id="findByCond">
    SELECT id, name FROM users
    <where>
      <if test="name != null">AND name = #{name}</if>
    </where>
  </select>
</mapper>"#;
        let xml_path = dir.path().join("User.xml");
        std::fs::write(&xml_path, xml).unwrap();

        let diff = FileDiff {
            path: PathBuf::from("User.xml"),
            hunks: vec![(3, 3)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = build_incremental_manifest(
            dir.path(),
            &[],
            std::slice::from_ref(&xml_path),
            &[diff],
            "origin/main",
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();

        // 标签起始行（第 2 行）不在 hunk 内：整标签不导出
        assert_eq!(result.manifest.statement_count, 0);

        // 命中标签起始行：导出该标签全部变体
        let diff2 = FileDiff {
            path: PathBuf::from("User.xml"),
            hunks: vec![(2, 2)],
            old_hunks: vec![],
            is_new: false,
        };
        let result2 = build_incremental_manifest(
            dir.path(),
            &[],
            &[xml_path],
            &[diff2],
            "origin/main",
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(result2.manifest.statement_count, 2);
        for s in &result2.manifest.statements {
            assert_eq!(s.statement_id.as_deref(), Some("findByCond"));
            assert_eq!(s.change.as_deref(), Some("modified"));
        }
    }

    #[test]
    fn removed_from_sql_file_basic() {
        let old = "SELECT 1;\nSELECT 2;\nSELECT 3;\n";
        // 旧侧删除行 2-3 → 语句 2、3 被删
        let removed = removed_from_sql_file(old, "a.sql", &[(2, 3)], &HashSet::new());
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].id, "a.sql#2");
        assert_eq!(removed[0].line, 2);
        assert_eq!(removed[1].id, "a.sql#3");
        assert_eq!(removed[1].source_type, "sql");
    }

    #[test]
    fn removed_from_mapper_file_basic() {
        let old = "<mapper>\n  <select id=\"gone\">\n    SELECT 1\n  </select>\n  <select id=\"kept\">\n    SELECT 2\n  </select>\n</mapper>";
        // 旧侧删除行 2 → 标签 gone（起始行 2）被删；kept（起始行 5）保留
        let removed = removed_from_mapper_file(old, "M.xml", &[(2, 2)], &HashSet::new());
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, "M.xml#gone");
        assert_eq!(removed[0].statement_id.as_deref(), Some("gone"));
    }

    #[test]
    fn id_still_exists_basic() {
        let mut ids = HashSet::new();
        ids.insert("a.sql#2".to_string());
        ids.insert("M.xml#findByCond#v1".to_string());
        ids.insert("M.xml#findByCond#v2".to_string());
        assert!(id_still_exists("a.sql#2", &ids));
        assert!(!id_still_exists("a.sql#3", &ids));
        // mapper 变体：基 id 也算存在
        assert!(id_still_exists("M.xml#findByCond", &ids));
        assert!(!id_still_exists("M.xml#gone", &ids));
    }

    #[test]
    fn build_incremental_manifest_modified_statement_not_removed() {
        // 修改的语句旧行也落在旧侧 hunk 内，但 id 仍存在 → 不是删除
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("a.sql");
        // 旧内容：SELECT 1; SELECT 2; SELECT 3;
        // 新内容：SELECT 1; SELECT 20;        （第 2 条修改、第 3 条删除）
        std::fs::write(&sql_path, "SELECT 1;\nSELECT 20;\n").unwrap();

        let diff = FileDiff {
            path: PathBuf::from("a.sql"),
            hunks: vec![(2, 2)],
            old_hunks: vec![(2, 3)],
            is_new: false,
        };
        let old_loader = |_d: &FileDiff| Ok(Some("SELECT 1;\nSELECT 2;\nSELECT 3;\n".to_string()));
        let result = build_incremental_manifest_with_loader(
            dir.path(),
            &[sql_path],
            &[],
            &[diff],
            "HEAD~1",
            &HashSet::new(),
            true,
            "utf-8",
            &old_loader,
            &mut |_, _, _| {},
        )
        .unwrap();

        assert_eq!(result.manifest.statement_count, 1);
        assert_eq!(result.manifest.statements[0].id, "a.sql#2");
        assert_eq!(result.manifest.statements[0].sql, "SELECT 20");
        // 仅第 3 条被删（#2 是修改，不在 removed）
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].id, "a.sql#3");
    }

    // === build_manifest (integration) ===

    #[test]
    fn build_manifest_empty() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = build_manifest(
            dir.path(),
            &[],
            &[],
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(manifest.statement_count, 0);
        assert!(manifest.statements.is_empty());
    }

    #[test]
    fn build_manifest_from_sql_file() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("test.sql");
        std::fs::write(&sql_path, "SELECT 1;\nSELECT 2;\n").unwrap();

        let manifest = build_manifest(
            dir.path(),
            &[sql_path],
            &[],
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(manifest.statement_count, 2);
        assert_eq!(manifest.statements[0].stmt_type, "select");
        assert_eq!(manifest.statements[0].source_type, "sql");
        assert_eq!(manifest.statements[0].line, 1);
        assert!(manifest.statements[0].id.contains("test.sql#1"));
    }

    #[test]
    fn build_manifest_with_type_filter() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("test.sql");
        std::fs::write(&sql_path, "SELECT 1;\nINSERT INTO t VALUES (1);\n").unwrap();

        let mut filter = HashSet::new();
        filter.insert("insert".to_string());

        let manifest = build_manifest(
            dir.path(),
            &[sql_path],
            &[],
            &filter,
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(manifest.statement_count, 1);
        assert_eq!(manifest.statements[0].stmt_type, "insert");
    }

    #[test]
    fn build_manifest_skips_transaction_control() {
        let dir = tempfile::tempdir().unwrap();
        let sql_path = dir.path().join("test.sql");
        std::fs::write(&sql_path, "START TRANSACTION;\nSELECT 1;\nCOMMIT;\n").unwrap();

        let manifest = build_manifest(
            dir.path(),
            &[sql_path],
            &[],
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        // Only SELECT 1 should be exported; START TRANSACTION and COMMIT are skipped
        assert_eq!(manifest.statement_count, 1);
        assert_eq!(manifest.statements[0].stmt_type, "select");
    }

    #[test]
    fn build_manifest_mapper_dollar_substitution_trusted() {
        let dir = tempfile::tempdir().unwrap();
        // 含 ${} 整段替换，渲染后结构残缺 → 静态解析失败（运行时才确定内容，预期内）
        let xml = r#"<mapper>
  <select id="dynamicCols">
    SELECT ${selectSql} FROM dual WHERE
  </select>
</mapper>"#;
        let xml_path = dir.path().join("Dyn.xml");
        std::fs::write(&xml_path, xml).unwrap();

        // 信任开启：解析失败标记为「动态未校验」，而非普通解析错误（Java 重放侧据此跳过，行为不变）
        let trusted = build_manifest(
            dir.path(),
            &[],
            std::slice::from_ref(&xml_path),
            &HashSet::new(),
            true,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(trusted.statement_count, 1);
        let pe = trusted.statements[0]
            .parse_error
            .as_deref()
            .expect("expected a parse marker when trusted");
        assert!(
            pe.contains("statically unchecked"),
            "trusted parse_error should note dynamic substitution, got: {}",
            pe
        );

        // 信任关闭：回退为普通解析错误文案
        let untrusted = build_manifest(
            dir.path(),
            &[],
            &[xml_path],
            &HashSet::new(),
            false,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        let pe2 = untrusted.statements[0]
            .parse_error
            .as_deref()
            .expect("expected a parse marker when untrusted");
        assert_eq!(pe2, "parse error in mapper variant");
    }

    #[test]
    fn build_manifest_mapper_tokenize_error_detail_preserved() {
        let dir = tempfile::tempdir().unwrap();
        // 首 token 即未闭合字符串 → try_with_sql 失败，parse_error 应保留原始 tokenize 详情
        let xml = r#"<mapper>
  <select id="badStr">
    'unterminated
  </select>
</mapper>"#;
        let xml_path = dir.path().join("Dyn.xml");
        std::fs::write(&xml_path, xml).unwrap();

        let manifest = build_manifest(
            dir.path(),
            &[],
            &[xml_path],
            &HashSet::new(),
            false,
            "utf-8",
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(manifest.statement_count, 1);
        let pe = manifest.statements[0]
            .parse_error
            .as_deref()
            .expect("expected a parse marker");
        assert!(
            pe.contains("tokenize error"),
            "should preserve original tokenize detail, got: {}",
            pe
        );
    }
}
