mod config;
mod cli;
mod error;
mod checker;
mod rule;
mod reporter;
mod mapper;
mod git_diff;
mod replay_export;

use std::path::{Path, PathBuf};
use std::fs;

use clap::Parser;

use crate::cli::{Cli, Commands};
use crate::error::{SqlGuardError, Violation};
use crate::config::Config;
use crate::checker::directory;
use crate::checker::classification;
use crate::checker::encoding;
use crate::rule::engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Check {
            path,
            config: config_path,
            format,
            output_dir,
            rules,
            groups,
            exclude_rules,
            exclude_groups,
        } => {
            run_check(
                &path,
                &config_path,
                &format,
                output_dir.as_deref(),
                &rules,
                &groups,
                &exclude_rules,
                &exclude_groups,
            )?;
        }
        Commands::Init { path } => {
            run_init(&path)?;
        }
        Commands::CheckDiff {
            base,
            path,
            config: config_path,
            format,
            output_dir,
            rules,
            groups,
            exclude_rules,
            exclude_groups,
        } => {
            run_check_diff(
                &base,
                &path,
                &config_path,
                &format,
                output_dir.as_deref(),
                &rules,
                &groups,
                &exclude_rules,
                &exclude_groups,
            )?;
        }
        Commands::ReplayExport {
            path,
            config: config_path,
            output_dir,
            types,
        } => {
            run_replay_export(&path, &config_path, &output_dir, &types)?;
        }
    }

    Ok(())
}

// ===== 公共辅助函数（任务 3：消除 run_check / run_check_diff 重复逻辑）=====

fn load_config(config_path: &Path) -> Result<(Config, PathBuf), SqlGuardError> {
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| config_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf());
    let config = if config_path.exists() {
        Config::load(config_path)?
    } else {
        Config::load(&config_dir.join("sqlguard.toml"))
            .unwrap_or_else(|_| generate_default_config())
    };
    Ok((config, config_dir))
}

fn resolve_absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn resolve_output_dir(output_dir: Option<&Path>, target_dir: &Path) -> PathBuf {
    output_dir
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
            }
        })
        .unwrap_or_else(|| target_dir.to_path_buf())
}

fn parse_formats(format: &str) -> Vec<&str> {
    match format {
        "all" => vec!["plain", "json", "html"],
        f => f.split(',').map(|s| s.trim()).collect(),
    }
}

fn check_files(
    config: &Config,
    config_dir: &Path,
    filter: &engine::RuleFilter,
    engine_instance: &rhai::Engine,
    sql_files: &[PathBuf],
    mapper_files: &[PathBuf],
) -> Result<(Vec<Violation>, usize), SqlGuardError> {
    let mut all_violations = Vec::new();
    let files_checked = sql_files.len() + mapper_files.len();

    // 脚本模式
    for file_path in sql_files {
        let classification_result = classification::classify_file(file_path, &config.classification)?;
        all_violations.extend(encoding::check_file(file_path, &classification_result.script_type, &config.file_check, filter));
        let sql_content = fs::read_to_string(file_path)
            .map_err(|e| SqlGuardError::CheckError(format!("Failed to read '{}': {}", file_path.display(), e)))?;
        let violations = engine::run_rules_for_file(engine_instance, file_path, &sql_content, &classification_result.script_type, config, config_dir, filter, 0)?;
        all_violations.extend(violations);
    }

    // Mapper 模式
    if config.mapper.enabled {
        for file_path in mapper_files {
            let mapper_script_type = classification::classify_file(file_path, &config.classification)
                .map(|r| r.script_type)
                .unwrap_or_else(|_| "mapper".to_string());
            all_violations.extend(encoding::check_file(file_path, &mapper_script_type, &config.file_check, filter));
            let extracted = match mapper::extract_sql_from_xml(file_path) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Warning: failed to parse mapper XML '{}': {}", file_path.display(), e);
                    continue;
                }
            };
            for sql in extracted {
                let script_type = mapper::map_statement_type(&sql.statement_type);
                let line_offset = sql.raw_xml_line.saturating_sub(1);
                let violations = engine::run_rules_for_file(engine_instance, file_path, &sql.processed_sql, script_type, config, config_dir, filter, line_offset)?;
                all_violations.extend(violations);
            }
        }
    }

    Ok((all_violations, files_checked))
}

// ===== run_check =====

fn run_check(
    target_dir: &Path,
    config_path: &Path,
    format: &str,
    output_dir: Option<&Path>,
    rules: &Option<String>,
    groups: &Option<String>,
    exclude_rules: &Option<String>,
    exclude_groups: &Option<String>,
) -> Result<(), SqlGuardError> {
    let (config, config_dir) = load_config(config_path)?;

    let filter = engine::RuleFilter::from_cli(rules, groups, exclude_rules, exclude_groups);
    if !filter.is_empty() {
        eprintln!(
            "Rule filter active: include_rules={:?} include_groups={:?} exclude_rules={:?} exclude_groups={:?}",
            filter.include_rules, filter.include_groups, filter.exclude_rules, filter.exclude_groups
        );
    }

    let absolute_target = resolve_absolute_path(target_dir);

    let effective_scan_paths: Vec<String> = if !config.scan.paths.is_empty() {
        config.scan.paths.clone()
    } else {
        config.structure.paths.clone()
    };
    let exclude_dirs: &[String] = &config.scan.exclude_dirs;

    let (missing, unexpected) =
        directory::check_directory_structure(&absolute_target, &config.structure, exclude_dirs);

    let sql_files = classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
    let mapper_files = mapper::collect_mapper_files(&absolute_target, &config.mapper, exclude_dirs);

    let engine_instance = engine::build_engine();
    let (all_violations, files_checked) = check_files(&config, &config_dir, &filter, &engine_instance, &sql_files, &mapper_files)?;

    let formats = parse_formats(format);
    let output_dir_path = resolve_output_dir(output_dir, &absolute_target);
    reporter::output_reports(&all_violations, &missing, &unexpected, files_checked, &formats, &output_dir_path)
        .map_err(SqlGuardError::CheckError)?;

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    let has_missing = !missing.is_empty();

    if has_errors || has_missing {
        Err(SqlGuardError::CheckError("Checks failed".to_string()))
    } else {
        Ok(())
    }
}

// ===== run_replay_export =====

fn run_replay_export(
    target_dir: &Path,
    config_path: &Path,
    output_dir: &Path,
    types: &Option<String>,
) -> Result<(), SqlGuardError> {
    let (config, _config_dir) = load_config(config_path)?;

    let absolute_target = resolve_absolute_path(target_dir);

    let effective_scan_paths: Vec<String> = if !config.scan.paths.is_empty() {
        config.scan.paths.clone()
    } else {
        config.structure.paths.clone()
    };
    let exclude_dirs: &[String] = &config.scan.exclude_dirs;

    let sql_files = classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
    let mapper_files = mapper::collect_mapper_files(&absolute_target, &config.mapper, exclude_dirs);

    let type_filter = replay_export::parse_type_filter(types);

    let manifest = replay_export::build_manifest(
        &absolute_target,
        &sql_files,
        &mapper_files,
        &type_filter,
    )?;

    let json = replay_export::manifest_to_json(&manifest)?;

    let output_dir_abs = resolve_absolute_path(output_dir);
    if !output_dir_abs.exists() {
        fs::create_dir_all(&output_dir_abs).map_err(SqlGuardError::IoError)?;
    }
    let manifest_path = output_dir_abs.join("sql-manifest.json");
    fs::write(&manifest_path, &json).map_err(SqlGuardError::IoError)?;

    eprintln!(
        "Replay manifest exported: {} ({} statements, {} sql files, {} mapper files)",
        manifest_path.display(),
        manifest.statement_count,
        sql_files.len(),
        mapper_files.len(),
    );

    Ok(())
}

// ===== run_check_diff =====

fn run_check_diff(
    base: &str,
    target_dir: &Path,
    config_path: &Path,
    format: &str,
    output_dir: Option<&Path>,
    rules: &Option<String>,
    groups: &Option<String>,
    exclude_rules: &Option<String>,
    exclude_groups: &Option<String>,
) -> Result<(), SqlGuardError> {
    let (config, config_dir) = load_config(config_path)?;

    let filter = engine::RuleFilter::from_cli(rules, groups, exclude_rules, exclude_groups);
    if !filter.is_empty() {
        eprintln!(
            "Rule filter active: include_rules={:?} include_groups={:?} exclude_rules={:?} exclude_groups={:?}",
            filter.include_rules, filter.include_groups, filter.exclude_rules, filter.exclude_groups
        );
    }

    // 1. 调用 git diff 获取改动文件 + hunk 行范围
    let mut patterns: Vec<&str> = vec!["*.sql", "*.ddl", "*.dml"];
    let mapper_patterns_owned: Vec<String> = if config.mapper.enabled {
        config.mapper.patterns.clone()
    } else {
        Vec::new()
    };
    for p in &mapper_patterns_owned {
        patterns.push(p.as_str());
    }

    eprintln!("Computing diff: {}...HEAD", base);
    let diffs = git_diff::get_diff(base, &patterns)?;

    if diffs.is_empty() {
        eprintln!("No SQL changes detected since {}", base);
        let formats = parse_formats(format);
        let output_dir_path = resolve_output_dir(output_dir, &std::env::current_dir().unwrap_or_default());
        reporter::output_reports(&[], &[], &[], 0, &formats, &output_dir_path)
            .map_err(SqlGuardError::CheckError)?;
        return Ok(());
    }

    eprintln!(
        "Incremental check on {} file(s) changed since {}",
        diffs.len(),
        base
    );

    let absolute_target = resolve_absolute_path(target_dir);

    // 2. 对每个改动文件跑规则，按 hunk 范围过滤
    let engine_instance = engine::build_engine();
    let mut all_violations: Vec<Violation> = Vec::new();
    let mut files_checked = 0;

    for file_diff in &diffs {
        let file_path = &file_diff.path;
        if !file_path.exists() {
            continue;
        }
        files_checked += 1;

        // 文件级检查（编码 / 换行符）：属于整文件属性，不做 hunk 过滤，
        // 只要该文件出现在本次 diff 中就检查。
        {
            let script_type = classification::classify_file(file_path, &config.classification)
                .map(|r| r.script_type)
                .unwrap_or_else(|_| "unknown".to_string());
            all_violations.extend(encoding::check_file(
                file_path,
                &script_type,
                &config.file_check,
                &filter,
            ));
        }

        let is_xml = file_path
            .extension()
            .map_or(false, |e| e.eq_ignore_ascii_case("xml"));

        if is_xml {
            // mapper 模式：提取所有片段，逐条跑 + 过滤
            let extracted = match mapper::extract_sql_from_xml(file_path) {
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
            for sql in extracted {
                let script_type = mapper::map_statement_type(&sql.statement_type);
                let line_offset = sql.raw_xml_line.saturating_sub(1);
                let violations = engine::run_rules_for_file(
                    &engine_instance,
                    file_path,
                    &sql.processed_sql,
                    script_type,
                    &config,
                    &config_dir,
                    &filter,
                    line_offset,
                )?;
                let filtered = filter_violations_by_diff(violations, file_diff);
                all_violations.extend(filtered);
            }
        } else {
            // 脚本模式
            let classification_result =
                classification::classify_file(file_path, &config.classification)?;
            let sql_content = fs::read_to_string(file_path).map_err(|e| {
                SqlGuardError::CheckError(format!(
                    "Failed to read '{}': {}",
                    file_path.display(),
                    e
                ))
            })?;
            let violations = engine::run_rules_for_file(
                &engine_instance,
                file_path,
                &sql_content,
                &classification_result.script_type,
                &config,
                &config_dir,
                &filter,
                0,
            )?;
            let filtered = filter_violations_by_diff(violations, file_diff);
            all_violations.extend(filtered);
        }
    }

    // 3. 输出报告
    let formats = parse_formats(format);
    let output_dir_path = resolve_output_dir(output_dir, &absolute_target);
    reporter::output_reports(&all_violations, &[], &[], files_checked, &formats, &output_dir_path)
        .map_err(SqlGuardError::CheckError)?;

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    if has_errors {
        Err(SqlGuardError::CheckError("Incremental checks failed".to_string()))
    } else {
        Ok(())
    }
}

/// 语句级交集过滤：保留 violation 的 [line, end_line] 与任一 hunk [s, e] 有交集的违规。
/// - 新增文件：全保留
/// - violation 无 line：保守保留（可能是规则脚本错误，宁可误报）
fn filter_violations_by_diff(
    violations: Vec<Violation>,
    file_diff: &git_diff::FileDiff,
) -> Vec<Violation> {
    if file_diff.is_new {
        return violations;
    }
    violations
        .into_iter()
        .filter(|v| {
            let line = match v.line {
                Some(l) => l,
                None => return true, // 无行号，保守保留
            };
            let end_line = v.end_line.unwrap_or(line);
            // 与任一 hunk 有交集：[line, end_line] ∩ [s, e] != ∅
            file_diff
                .hunks
                .iter()
                .any(|(s, e)| line <= *e && end_line >= *s)
        })
        .collect()
}

// ===== run_init =====

fn run_init(target_dir: &Path) -> Result<(), SqlGuardError> {
    let rules_dir = target_dir.join("config").join("rules");
    for dir in &[rules_dir.join("ddl"), rules_dir.join("dml")] {
        fs::create_dir_all(dir).map_err(SqlGuardError::IoError)?;
    }

    let config_content = get_default_config_content();
    fs::write(target_dir.join("sqlguard.toml"), config_content)
        .map_err(SqlGuardError::IoError)?;

    let rules_content = get_default_rules_content();
    fs::write(target_dir.join("sqlguard.rules.toml"), rules_content)
        .map_err(SqlGuardError::IoError)?;

    // (name, subdir, content) — content 通过 include_str! 编译时嵌入
    let rules: &[(&str, &str, &str)] = &[
        ("no_drop_table", "ddl", include_str!("../config/rules/ddl/no_drop_table.rhai")),
        ("primary_key_required", "ddl", include_str!("../config/rules/ddl/primary_key_required.rhai")),
        ("no_reserved_keyword_naming", "ddl", include_str!("../config/rules/ddl/no_reserved_keyword_naming.rhai")),
        ("backup_table_naming", "ddl", include_str!("../config/rules/ddl/backup_table_naming.rhai")),
        ("index_naming_convention", "ddl", include_str!("../config/rules/ddl/index_naming_convention.rhai")),
        ("no_redundant_index", "ddl", include_str!("../config/rules/ddl/no_redundant_index.rhai")),
        ("no_select_all", "dml", include_str!("../config/rules/dml/no_select_all.rhai")),
        ("no_delete_update_without_where", "dml", include_str!("../config/rules/dml/no_delete_update_without_where.rhai")),
        ("insert_columns_required", "dml", include_str!("../config/rules/dml/insert_columns_required.rhai")),
        ("subquery_alias_required", "dml", include_str!("../config/rules/dml/subquery_alias_required.rhai")),
        ("column_references_qualified", "dml", include_str!("../config/rules/dml/column_references_qualified.rhai")),
        ("no_join_without_condition", "dml", include_str!("../config/rules/dml/no_join_without_condition.rhai")),
        ("no_unused_join", "dml", include_str!("../config/rules/dml/no_unused_join.rhai")),
        ("no_unused_cte", "dml", include_str!("../config/rules/dml/no_unused_cte.rhai")),
        ("use_is_null", "dml", include_str!("../config/rules/dml/use_is_null.rhai")),
        ("use_coalesce", "dml", include_str!("../config/rules/dml/use_coalesce.rhai")),
        ("no_order_by_in_subquery", "dml", include_str!("../config/rules/dml/no_order_by_in_subquery.rhai")),
        ("union_all_preferred", "dml", include_str!("../config/rules/dml/union_all_preferred.rhai")),
        ("no_nested_case", "dml", include_str!("../config/rules/dml/no_nested_case.rhai")),
        ("no_constant_where", "dml", include_str!("../config/rules/dml/no_constant_where.rhai")),
    ];

    for (name, rule_type, content) in rules {
        let path = target_dir.join("config").join("rules").join(rule_type).join(format!("{}.rhai", name));
        fs::write(&path, content).map_err(SqlGuardError::IoError)?;
    }

    println!("Initialized SqlGuard configuration in {}", target_dir.display());
    println!("  - sqlguard.toml          # 主配置（结构/分类/输出/扫描/文件检查）");
    println!("  - sqlguard.rules.toml    # 规则配置（[[rules]] 单独拆分，避免文件过长）");
    println!("  - config/rules/ddl/ (6 rule files)");
    println!("  - config/rules/dml/ (14 rule files)");
    println!();
    println!("Run: sqlguard check <project_path>");
    Ok(())
}

fn generate_default_config() -> Config {
    Config {
        structure: crate::config::StructureConfig {
            paths: vec![
                "sql/ddl".to_string(),
                "sql/dml".to_string(),
                "sql/others".to_string(),
            ],
            strict: true,
            allow_extra: vec![".gitkeep".to_string(), "config/".to_string()],
        },
        classification: crate::config::ClassificationConfig {
            rules: vec![
                crate::config::ClassificationRule {
                    name: "ddl-by-dir".to_string(),
                    pattern: "**/ddl/**".to_string(),
                    script_type: "ddl".to_string(),
                    priority: 10,
                },
                crate::config::ClassificationRule {
                    name: "dml-by-dir".to_string(),
                    pattern: "**/dml/**".to_string(),
                    script_type: "dml".to_string(),
                    priority: 10,
                },
            ],
            default_type: "other".to_string(),
        },
        rules: vec![
            // ===== P0 规则：默认启用 =====
            crate::config::RuleConfig {
                id: "DDL001".to_string(),
                name: "no_drop_table".to_string(),
                group: Some("ddl-safety".to_string()),
                description: Some("Disallow DROP TABLE in DDL scripts".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/no_drop_table.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DDL002".to_string(),
                name: "primary_key_required".to_string(),
                group: Some("ddl-safety".to_string()),
                description: Some("CREATE TABLE must have a PRIMARY KEY".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/primary_key_required.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DDL003".to_string(),
                name: "no_reserved_keyword_naming".to_string(),
                group: Some("ddl-safety".to_string()),
                description: Some("Database object names must not use SQL reserved keywords".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/no_reserved_keyword_naming.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DDL004".to_string(),
                name: "backup_table_naming".to_string(),
                group: Some("ddl-convention".to_string()),
                description: Some("Backup tables created via CREATE TABLE AS SELECT must be prefixed with 'bks_'".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/backup_table_naming.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DDL005".to_string(),
                name: "index_naming_convention".to_string(),
                group: Some("ddl-convention".to_string()),
                description: Some("Indexes follow idx_/uk_/pk_ naming convention based on type and columns".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/index_naming_convention.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DDL006".to_string(),
                name: "no_redundant_index".to_string(),
                group: Some("ddl-performance".to_string()),
                description: Some("Avoid redundant indexes (duplicate PK indexes and leftmost-prefix duplicates)".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/no_redundant_index.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML001".to_string(),
                name: "no_select_all".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("Disallow SELECT * in DML scripts".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_select_all.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML002".to_string(),
                name: "no_delete_update_without_where".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("DELETE/UPDATE must have a WHERE clause".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_delete_update_without_where.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML003".to_string(),
                name: "insert_columns_required".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("INSERT must specify target columns".to_string()),
                enabled: true,
                script_path: "config/rules/dml/insert_columns_required.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML004".to_string(),
                name: "subquery_alias_required".to_string(),
                group: Some("dml-style".to_string()),
                description: Some("Subqueries in FROM must have an alias".to_string()),
                enabled: true,
                script_path: "config/rules/dml/subquery_alias_required.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML005".to_string(),
                name: "column_references_qualified".to_string(),
                group: Some("dml-style".to_string()),
                description: Some("Qualify column references with table name in multi-table queries".to_string()),
                enabled: true,
                script_path: "config/rules/dml/column_references_qualified.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML006".to_string(),
                name: "no_join_without_condition".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("JOIN must have ON or USING condition".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_join_without_condition.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
            },
            // ===== P1 规则：默认禁用，建议评估后启用 =====
            crate::config::RuleConfig {
                id: "DML101".to_string(),
                name: "no_unused_join".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Detect potentially unused JOINs".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_unused_join.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML102".to_string(),
                name: "no_unused_cte".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Detect unused CTEs (WITH clauses)".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_unused_cte.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML103".to_string(),
                name: "use_is_null".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Use IS NULL instead of = NULL".to_string()),
                enabled: false,
                script_path: "config/rules/dml/use_is_null.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML104".to_string(),
                name: "use_coalesce".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Use standard COALESCE instead of NVL/ISNULL".to_string()),
                enabled: false,
                script_path: "config/rules/dml/use_coalesce.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML105".to_string(),
                name: "no_order_by_in_subquery".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("ORDER BY in subquery is typically ignored".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_order_by_in_subquery.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML106".to_string(),
                name: "union_all_preferred".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Prefer UNION ALL over UNION unless dedup needed".to_string()),
                enabled: false,
                script_path: "config/rules/dml/union_all_preferred.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML107".to_string(),
                name: "no_nested_case".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Avoid nested CASE expressions".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_nested_case.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
            crate::config::RuleConfig {
                id: "DML108".to_string(),
                name: "no_constant_where".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Avoid constant conditions in WHERE clause".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_constant_where.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
            },
        ],
        rules_file: None,
        rules_dir: PathBuf::new(),
        output: crate::config::OutputConfig {
            formats: vec!["plain".to_string()],
            output_dir: None,
        },
        mapper: crate::config::MapperConfig::default(),
        scan: crate::config::ScanConfig::default(),
        file_check: crate::config::FileCheckConfig::default(),
    }
}

fn get_default_config_content() -> &'static str {
    r#"[structure]
paths = [
  "sql/ddl",
  "sql/dml",
  "sql/others",
]
strict = true
allow_extra = [".gitkeep", "config/"]

[classification]
default_type = "other"

[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10

[[classification.rules]]
name = "dml-by-dir"
pattern = "**/dml/**"
type = "dml"
priority = 10

[[classification.rules]]
name = "other-by-others-dir"
pattern = "**/others/**"
type = "other"
priority = 10

[[classification.rules]]
name = "sql-by-ext"
pattern = "*.sql"
type = "sql"
priority = 0

# ================================================================================
# 规则配置：单独拆分到 sqlguard.rules.toml，避免主配置文件随规则增多而过长。
# 不写本行时，工具也会自动在同目录查找 sqlguard.rules.toml。
# ================================================================================
rules_file = "sqlguard.rules.toml"

[output]
formats = ["plain", "json", "html"]

# MyBatis Mapper 模式：扫描 XML 中的 <select>/<insert>/<update>/<delete>。
# 缺省或 enabled = false 时完全保持现有行为（仅扫描 .sql/.ddl/.dml）。
# [mapper]
# enabled = true
# paths = ["src/main/resources/mapper"]
# patterns = ["**/*Mapper.xml", "**/*.xml"]

# ================================================================================
# 文件扫描行为配置 [scan]
# ================================================================================
# 控制白名单扫描根与黑名单跳过目录，避免递归进入 .git/target/node_modules 等大目录。
#
# exclude_dirs：递归扫描时跳过的目录名（按名称匹配，任意层级生效）。
#   默认值见下，未配置 [scan] 段时也按默认黑名单生效。
#   适用于：SQL 脚本扫描、Mapper XML 扫描、目录结构校验三个场景。
#
# paths：SQL 脚本扫描白名单（相对配置文件目录或绝对路径）。
#   为空时回退到 [structure].paths，仍为空则扫描整个 target_dir（兜底）。
#   指定后只扫描这些目录下的 .sql/.ddl/.dml，散落在白名单外的 SQL 会被忽略。
#
# [scan]
# paths = []
# exclude_dirs = [
#   ".git", ".svn", ".hg", ".bzr",       # 版本控制元数据
#   "target", "node_modules", "build", "dist", "out",  # 构建产物
#   ".idea", ".vscode",                   # IDE 配置
# ]

# ================================================================================
# 文件格式检查 [file_check]
# ================================================================================
# 对扫描到的每个文件做字节级检查（独立于 SQL 语法规则）：
#   FILE001  编码必须为 UTF-8 且不带 BOM（severity = error，必须）
#   FILE002  换行符应为 LF（severity = warning，提示）
# 两条检查归入 file-format 分组，可用 --exclude-rules FILE001,FILE002
# 或 --exclude-groups file-format 临时关闭。
# 缺省（未写 [file_check] 段）时按下方默认值启用。

[file_check]
enabled = true
check_encoding = true               # UTF-8 无 BOM 检查（FILE001）
check_line_ending = true            # 换行符 LF 检查（FILE002）
encoding_severity = "error"         # 编码违规级别（必须）
line_ending_severity = "warning"    # 换行符违规级别（提示）
"#
}

/// 默认规则配置内容（独立文件 sqlguard.rules.toml）。
///
/// 仅含 `[[rules]]` 数组；脚本路径（script_path）相对本文件所在目录解析。
fn get_default_rules_content() -> &'static str {
    r#"# ================================================================================
# 规则配置（独立文件）
#
# 每条 [[rules]] 对应一个 Rhai 脚本。脚本路径（script_path）相对本文件
# 所在目录解析，也支持绝对路径。
#
# 在 sqlguard.toml 中用 rules_file = "sqlguard.rules.toml" 引用本文件；
# 不写该行时，工具也会自动在同目录查找 sqlguard.rules.toml。
# ================================================================================

# ================================================================================
# P0 规则：默认启用，建议 CI 中保持开启
# ================================================================================

[[rules]]
id = "DDL001"
name = "no_drop_table"
group = "ddl-safety"
description = "Disallow DROP TABLE in DDL scripts"
enabled = true
script_path = "config/rules/ddl/no_drop_table.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "DDL002"
name = "primary_key_required"
group = "ddl-safety"
description = "CREATE TABLE must have a PRIMARY KEY"
enabled = true
script_path = "config/rules/ddl/primary_key_required.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL003"
name = "no_reserved_keyword_naming"
group = "ddl-safety"
description = "Database object names must not use SQL reserved keywords"
enabled = true
script_path = "config/rules/ddl/no_reserved_keyword_naming.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "DDL004"
name = "backup_table_naming"
group = "ddl-convention"
description = "Backup tables created via CREATE TABLE AS SELECT must be prefixed with 'bks_'"
enabled = true
script_path = "config/rules/ddl/backup_table_naming.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL005"
name = "index_naming_convention"
group = "ddl-convention"
description = "Indexes follow idx_/uk_/pk_ naming convention based on type and columns"
enabled = true
script_path = "config/rules/ddl/index_naming_convention.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL006"
name = "no_redundant_index"
group = "ddl-performance"
description = "Avoid redundant indexes (duplicate PK indexes and leftmost-prefix duplicates)"
enabled = true
script_path = "config/rules/ddl/no_redundant_index.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DML001"
name = "no_select_all"
group = "dml-safety"
description = "Disallow SELECT * in DML scripts"
enabled = true
script_path = "config/rules/dml/no_select_all.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML002"
name = "no_delete_update_without_where"
group = "dml-safety"
description = "DELETE/UPDATE must have a WHERE clause"
enabled = true
script_path = "config/rules/dml/no_delete_update_without_where.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML003"
name = "insert_columns_required"
group = "dml-safety"
description = "INSERT must specify target columns"
enabled = true
script_path = "config/rules/dml/insert_columns_required.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML004"
name = "subquery_alias_required"
group = "dml-style"
description = "Subqueries in FROM must have an alias"
enabled = true
script_path = "config/rules/dml/subquery_alias_required.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML005"
name = "column_references_qualified"
group = "dml-style"
description = "Qualify column references with table name in multi-table queries"
enabled = true
script_path = "config/rules/dml/column_references_qualified.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML006"
name = "no_join_without_condition"
group = "dml-safety"
description = "JOIN must have ON or USING condition"
enabled = true
script_path = "config/rules/dml/no_join_without_condition.rhai"
applies_to = ["dml"]
severity = "error"

# ================================================================================
# P1 规则：默认禁用，建议评估后启用
# 在 sqlguard.rules.toml 中将 enabled = false 改为 true 即可启用
# ================================================================================

[[rules]]
id = "DML101"
name = "no_unused_join"
group = "dml-performance"
description = "Detect potentially unused JOINs"
enabled = false
script_path = "config/rules/dml/no_unused_join.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML102"
name = "no_unused_cte"
group = "dml-performance"
description = "Detect unused CTEs (WITH clauses)"
enabled = false
script_path = "config/rules/dml/no_unused_cte.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML103"
name = "use_is_null"
group = "dml-convention"
description = "Use IS NULL instead of = NULL"
enabled = false
script_path = "config/rules/dml/use_is_null.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML104"
name = "use_coalesce"
group = "dml-convention"
description = "Use standard COALESCE instead of NVL/ISNULL"
enabled = false
script_path = "config/rules/dml/use_coalesce.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML105"
name = "no_order_by_in_subquery"
group = "dml-performance"
description = "ORDER BY in subquery is typically ignored"
enabled = false
script_path = "config/rules/dml/no_order_by_in_subquery.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML106"
name = "union_all_preferred"
group = "dml-performance"
description = "Prefer UNION ALL over UNION unless dedup needed"
enabled = false
script_path = "config/rules/dml/union_all_preferred.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML107"
name = "no_nested_case"
group = "dml-convention"
description = "Avoid nested CASE expressions"
enabled = false
script_path = "config/rules/dml/no_nested_case.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML108"
name = "no_constant_where"
group = "dml-convention"
description = "Avoid constant conditions in WHERE clause"
enabled = false
script_path = "config/rules/dml/no_constant_where.rhai"
applies_to = ["dml"]
severity = "warning"
"#
}
