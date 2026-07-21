mod config;
mod cli;
mod error;
mod checker;
mod rule;
mod reporter;
mod mapper;
mod git_diff;

use std::path::Path;
use std::fs;

use clap::Parser;

use crate::cli::{Cli, Commands};
use crate::error::{SqlGuardError, Violation};
use crate::config::Config;
use crate::checker::directory;
use crate::checker::classification;
use crate::checker::encoding;
use crate::rule::engine;
use crate::reporter::{plain, json, html};

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
    }

    Ok(())
}

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

    let filter = engine::RuleFilter::from_cli(rules, groups, exclude_rules, exclude_groups);
    if !filter.is_empty() {
        eprintln!(
            "Rule filter active: include_rules={:?} include_groups={:?} exclude_rules={:?} exclude_groups={:?}",
            filter.include_rules, filter.include_groups, filter.exclude_rules, filter.exclude_groups
        );
    }

    let absolute_target = if target_dir.is_absolute() {
        target_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(target_dir))
            .unwrap_or_else(|_| target_dir.to_path_buf())
    };

    // 计算有效扫描白名单：scan.paths 优先，回退到 structure.paths，仍为空则扫描整个 target_dir
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
    let files_checked = sql_files.len() + mapper_files.len();

    // 构建一次 Rhai 引擎，全文件/全片段复用（mapper 模式可能产生大量片段，
    // 每片段重建引擎会导致性能问题）
    let engine_instance = engine::build_engine();

    let mut all_violations: Vec<Violation> = Vec::new();

    // ===== 脚本模式：.sql/.ddl/.dml 文件，每个文件作为单 SQL 单元 =====
    for file_path in &sql_files {
        let classification_result = classification::classify_file(file_path, &config.classification)?;

        // 文件级检查（编码 / 换行符），独立于 SQL 语法规则
        all_violations.extend(encoding::check_file(
            file_path,
            &classification_result.script_type,
            &config.file_check,
            &filter,
        ));

        let sql_content = fs::read_to_string(file_path)
            .map_err(|e| SqlGuardError::CheckError(format!("Failed to read '{}': {}", file_path.display(), e)))?;

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

        all_violations.extend(violations);
    }

    // ===== Mapper 模式：MyBatis XML，每个文件提取多条 SQL，逐条检查 =====
    if config.mapper.enabled {
        for file_path in &mapper_files {
            // 文件级检查（编码 / 换行符），对 XML 文件整体生效
            let mapper_script_type = classification::classify_file(file_path, &config.classification)
                .map(|r| r.script_type)
                .unwrap_or_else(|_| "mapper".to_string());
            all_violations.extend(encoding::check_file(
                file_path,
                &mapper_script_type,
                &config.file_check,
                &filter,
            ));

            // 容错：单文件解析失败不影响其他文件
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
                // line_offset 把规则产出的"SQL 内行号"映射回 XML 行号：
                // raw_xml_line 是 `<select>` 标签起始行（1-indexed），偏移为 raw_xml_line - 1
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

                all_violations.extend(violations);
            }
        }
    }

    let formats: Vec<&str> = match format {
        "all" => vec!["plain", "json", "html"],
        f => f.split(',').map(|s| s.trim()).collect(),
    };

    let output_dir_path = output_dir
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
            }
        })
        .unwrap_or_else(|| absolute_target.clone());

    if !output_dir_path.exists() {
        fs::create_dir_all(&output_dir_path)
            .map_err(|e| SqlGuardError::IoError(e))?;
    }

    for fmt in &formats {
        match *fmt {
            "plain" => {
                plain::print_plain_report(&all_violations, &missing, &unexpected, files_checked);
            }
            "json" => {
                match json::save_json_report(&output_dir_path, &all_violations, &missing, &unexpected, files_checked) {
                    Ok(path) => eprintln!("JSON report saved: {}", path),
                    Err(e) => eprintln!("Error saving JSON report: {}", e),
                }
            }
            "html" => {
                match html::save_html_report(&output_dir_path, &all_violations, &missing, &unexpected, files_checked) {
                    Ok(path) => eprintln!("HTML report saved: {}", path),
                    Err(e) => eprintln!("Error saving HTML report: {}", e),
                }
            }
            _ => eprintln!("Unknown format: {}", fmt),
        }
    }

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    let has_missing = !missing.is_empty();

    if has_errors || has_missing {
        Err(SqlGuardError::CheckError("Checks failed".to_string()))
    } else {
        Ok(())
    }
}

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
        // 仍输出空报告
        let output_dir_path = output_dir
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        if !output_dir_path.exists() {
            let _ = fs::create_dir_all(&output_dir_path);
        }
        let empty: Vec<Violation> = Vec::new();
        for fmt in format.split(',').map(|s| s.trim()) {
            match fmt {
                "plain" => plain::print_plain_report(&empty, &[], &[], 0),
                "json" => {
                    if let Ok(p) = json::save_json_report(&output_dir_path, &empty, &[], &[], 0) {
                        eprintln!("JSON report saved: {}", p);
                    }
                }
                _ => {}
            }
        }
        return Ok(());
    }

    eprintln!(
        "Incremental check on {} file(s) changed since {}",
        diffs.len(),
        base
    );

    let absolute_target = if target_dir.is_absolute() {
        target_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(target_dir))
            .unwrap_or_else(|_| target_dir.to_path_buf())
    };

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

    // 3. 输出报告（复用现有 reporter）
    let formats: Vec<&str> = match format {
        "all" => vec!["plain", "json", "html"],
        f => f.split(',').map(|s| s.trim()).collect(),
    };

    let output_dir_path = output_dir
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
            }
        })
        .unwrap_or_else(|| absolute_target.clone());

    if !output_dir_path.exists() {
        fs::create_dir_all(&output_dir_path).map_err(SqlGuardError::IoError)?;
    }

    for fmt in &formats {
        match *fmt {
            "plain" => {
                plain::print_plain_report(&all_violations, &[], &[], files_checked);
            }
            "json" => match json::save_json_report(
                &output_dir_path,
                &all_violations,
                &[],
                &[],
                files_checked,
            ) {
                Ok(path) => eprintln!("JSON report saved: {}", path),
                Err(e) => eprintln!("Error saving JSON report: {}", e),
            },
            "html" => match html::save_html_report(
                &output_dir_path,
                &all_violations,
                &[],
                &[],
                files_checked,
            ) {
                Ok(path) => eprintln!("HTML report saved: {}", path),
                Err(e) => eprintln!("Error saving HTML report: {}", e),
            },
            _ => eprintln!("Unknown format: {}", fmt),
        }
    }

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

fn run_init(target_dir: &Path) -> Result<(), SqlGuardError> {
    let rules_dir = target_dir.join("config").join("rules");

    let dirs = [
        rules_dir.join("ddl"),
        rules_dir.join("dml"),
    ];
    for dir in &dirs {
        fs::create_dir_all(dir)
            .map_err(|e| SqlGuardError::IoError(e))?;
    }

    let config_content = get_default_config_content();
    let config_path = target_dir.join("sqlguard.toml");
    fs::write(&config_path, config_content)
        .map_err(|e| SqlGuardError::IoError(e))?;

    let rules = vec![
        ("no_drop_table", "ddl", get_no_drop_table_script()),
        ("primary_key_required", "ddl", get_primary_key_script()),
        ("no_select_all", "dml", get_no_select_all_script()),
        ("no_delete_update_without_where", "dml", get_no_delete_update_without_where_script()),
        ("insert_columns_required", "dml", get_insert_columns_required_script()),
        ("subquery_alias_required", "dml", get_subquery_alias_required_script()),
        ("column_references_qualified", "dml", get_column_references_qualified_script()),
        ("no_join_without_condition", "dml", get_no_join_without_condition_script()),
        ("no_unused_join", "dml", get_no_unused_join_script()),
        ("no_unused_cte", "dml", get_no_unused_cte_script()),
        ("use_is_null", "dml", get_use_is_null_script()),
        ("use_coalesce", "dml", get_use_coalesce_script()),
        ("no_order_by_in_subquery", "dml", get_no_order_by_in_subquery_script()),
        ("union_all_preferred", "dml", get_union_all_preferred_script()),
        ("no_nested_case", "dml", get_no_nested_case_script()),
        ("no_constant_where", "dml", get_no_constant_where_script()),
    ];

    for (name, rule_type, content) in rules {
        let path = target_dir.join("config").join("rules").join(rule_type).join(format!("{}.rhai", name));
        fs::write(&path, content)
            .map_err(|e| SqlGuardError::IoError(e))?;
    }

    println!("Initialized SqlGuard configuration in {}", target_dir.display());
    println!("  - sqlguard.toml");
    println!("  - config/rules/ddl/no_drop_table.rhai");
    println!("  - config/rules/ddl/primary_key_required.rhai");
    println!("  - config/rules/dml/ (13 rule files)");
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
            allow_extra: vec![".gitkeep".to_string()],
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
allow_extra = [".gitkeep"]

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
# 在 sqlguard.toml 中将 enabled = false 改为 true 即可启用
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

fn get_no_drop_table_script() -> &'static str {
    r#"// no_drop_table.rhai - Disallow DROP TABLE in DDL scripts
// 基于 AST：遍历语句列表，对 DROP_TABLE 类型语句上报违规。
// 相比字符串匹配，AST 能精确识别语句类型，不会误匹配注释或子串。

let ast = context["ast"];

// 解析失败时也上报，避免漏检
if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(#{
            "message": "DROP TABLE is not allowed in DDL scripts: " + d.name(),
            "line": s.line(),
            "column": s.column()
        });
    }
}
"#
}

fn get_primary_key_script() -> &'static str {
    r#"// primary_key_required.rhai - CREATE TABLE must have a PRIMARY KEY
// 基于 AST：识别 CREATE TABLE 语句，检查列定义或表级约束中是否包含 PRIMARY KEY。
// 相比字符串匹配，AST 能正确识别 CREATE TABLE 结构，不会误判 ALTER TABLE 或注释。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        if !ct.has_primary_key() {
            violations.push(#{
                "message": "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_no_select_all_script() -> &'static str {
    r#"// no_select_all.rhai (DML001)
// 禁止使用 SELECT *，必须显式列出所需列。
// 原因：SELECT * 会返回所有列，可能导致不必要的数据传输、
// 隐式依赖表结构、并破坏视图/物化视图的兼容性。
// 基于 AST 精确识别通配符，不会误判注释或字符串字面量。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.kind() == "SELECT" {
        let sel = s.select();
        if sel.has_wildcard() {
            violations.push(#{
                "message": "SELECT * is not allowed. Specify columns explicitly.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_no_delete_update_without_where_script() -> &'static str {
    r#"// no_delete_update_without_where.rhai (DML002)
// DELETE 和 UPDATE 语句必须带 WHERE 条件，防止全表误操作。
// 基于 AST 精确判断 WHERE 子句是否存在。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_delete() {
        let d = s.delete();
        if !d.has_where() {
            violations.push(#{
                "message": "DELETE without WHERE clause will remove all rows from table: " + d.table_name() + ". Add a WHERE condition or use TRUNCATE if intentional.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
    if s.has_update() {
        let u = s.update();
        if !u.has_where() {
            violations.push(#{
                "message": "UPDATE without WHERE clause will modify all rows in table: " + u.table_name() + ". Add a WHERE condition.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_insert_columns_required_script() -> &'static str {
    r#"// insert_columns_required.rhai (DML003)
// INSERT 语句必须显式指定列名，禁止 INSERT INTO t VALUES(...)。
// 原因：不指定列名时，INSERT 隐式依赖表结构的列顺序，表结构变更会导致静默错误。
// 显式指定列名使代码自文档化，且对表结构变更更鲁棒。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_insert() {
        let ins = s.insert();
        if !ins.has_columns() {
            violations.push(#{
                "message": "INSERT must specify target columns explicitly: INSERT INTO " + ins.table_name() + " (col1, col2, ...) VALUES (...)",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_subquery_alias_required_script() -> &'static str {
    r#"// subquery_alias_required.rhai (DML004)
// FROM 子句中的子查询必须起别名，否则所有数据库都会报错。
// 基于 AST 检查 FROM 子句中的派生表（子查询）是否有别名。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_subquery_in_from() && !sel.from_subquery_has_alias() {
            violations.push(#{
                "message": "Subquery in FROM clause must have an alias: SELECT ... FROM (SELECT ...) AS alias",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_column_references_qualified_script() -> &'static str {
    r#"// column_references_qualified.rhai (DML005)
// 多表查询中，列引用必须带表名限定（t.col），避免歧义。
// 原因：当查询涉及多张表时，未限定的列名可能导致歧义或意外绑定，
// 且降低可读性。SQLFluff RF02 等效规则。
// 基于 AST 检查多表 SELECT 中是否存在裸列引用。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_unqualified_column() {
            violations.push(#{
                "message": "Column reference should be qualified with table name in multi-table query. Use table.column syntax.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_no_join_without_condition_script() -> &'static str {
    r#"// no_join_without_condition.rhai (DML006)
// JOIN 必须带 ON 条件，防止产生无意的笛卡尔积。
// 原因：无条件的 JOIN（CROSS JOIN 除外）通常是编码错误，
// 会导致大量无用数据行。SQLFluff AM05 等效规则。
// 基于 AST 检查 JOIN 子句是否包含 ON 或 USING 条件。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_join_without_condition() {
            violations.push(#{
                "message": "JOIN without ON or USING condition causes a Cartesian product. Add an explicit join condition.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_no_unused_join_script() -> &'static str {
    r#"// no_unused_join.rhai (DML101)
// 检测可能未使用的 JOIN 关系。
// 启发式：如果 JOIN 的表没有列在 SELECT 投影列表中（通过表名判断），可能未使用。
// 注意：此规则基于字符串匹配，可能存在误报，需人工复核。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_joins() {
            let projection_text = "";
            for col in sel.projection() {
                projection_text += col;
            }
            let upper_proj = projection_text.to_upper();

            for j in sel.joins() {
                let jt = j.table_name().to_upper();
                if !upper_proj.contains(jt) && j.join_type() != "CROSS" && j.join_type() != "" {
                    violations.push(#{
                        "message": "Possible unused JOIN to table '" + j.table_name() + "'. The table columns are not referenced in SELECT list.",
                        "line": s.line(),
                        "column": s.column()
                    });
                }
            }
        }
    }
}
"#
}

fn get_no_unused_cte_script() -> &'static str {
    r#"// no_unused_cte.rhai (DML102)
// 检测未使用的 CTE（WITH 子句）。
// 启发式：如果 CTE 的名称没有在主查询中被引用，则该 CTE 未使用。

let sql = context["sql_content"];
let upper = sql.to_upper();

let with_pattern = "WITH ";
let cte_start = upper.find(with_pattern);
if cte_start != () {
    let after_with = upper.sub_string(cte_start.len());
    let parts = after_with.split(" AS (");
    let first_cte_name = "";
    if parts.len() > 0 {
        let first_part = parts[0].trim();
        let name_end = first_part.find(" ");
        if name_end != () {
            first_cte_name = first_part.sub_string(0, name_end).trim();
        } else {
            first_cte_name = first_part;
        }
    }

    if first_cte_name != "" {
        let main_query_start = after_with.find(") ");
        if main_query_start != () {
            let main_query = after_with.sub_string(main_query_start);
            if !main_query.to_upper().contains(first_cte_name) {
                violations.push(#{
                    "message": "CTE '" + first_cte_name + "' is defined but not used in the main query. Remove unused CTE.",
                    "line": 1
                });
            }
        }
    }
}
"#
}

fn get_use_is_null_script() -> &'static str {
    r#"// use_is_null.rhai (DML103)
// 使用 IS NULL / IS NOT NULL 而非 = NULL / <> NULL。
// 原因：SQL 中 NULL = NULL 的结果是 NULL（不是 TRUE），
// WHERE name = NULL 永远不会返回任何行，这是常见编码错误。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let sql = context["sql_content"];
let lines = sql.split("\n");
let mut line_num = 1;
for line in lines {
    let uline = line.to_upper();
    if (uline.contains("= NULL") || uline.contains("=NULL")) && !uline.contains("IS NULL") && !uline.contains("IS NOT NULL") {
        violations.push(#{
            "message": "Use 'IS NULL' instead of '= NULL'. NULL comparisons always return FALSE with '='.",
            "line": line_num
        });
    }
    if (uline.contains("<> NULL") || uline.contains("<>NULL") || uline.contains("!= NULL") || uline.contains("!=NULL")) && !uline.contains("IS NOT NULL") {
        violations.push(#{
            "message": "Use 'IS NOT NULL' instead of '<> NULL' or '!= NULL'. NULL comparisons always return FALSE with '<>'.",
            "line": line_num
        });
    }
    line_num += 1;
}
"#
}

fn get_use_coalesce_script() -> &'static str {
    r#"// use_coalesce.rhai (DML104)
// 优先使用标准的 COALESCE 而非数据库专有的 NVL 或 ISNULL。
// COALESCE 是 SQL 标准函数，NVL（Oracle）和 ISNULL（SQL Server）是专有函数。

let sql = context["sql_content"];
let upper = sql.to_upper();

if upper.contains("NVL(") {
    violations.push(#{
        "message": "Use standard COALESCE() instead of Oracle-specific NVL(). COALESCE is cross-database compatible.",
        "line": 1
    });
}

if upper.contains("ISNULL(") {
    violations.push(#{
        "message": "Use standard COALESCE() instead of T-SQL-specific ISNULL(). COALESCE is cross-database compatible.",
        "line": 1
    });
}
"#
}

fn get_no_order_by_in_subquery_script() -> &'static str {
    r#"// no_order_by_in_subquery.rhai (DML105)
// 子查询中的 ORDER BY 通常无效（除非使用 LIMIT/OFFSET）。
// SQL 标准中，子查询是逻辑无序的，ORDER BY 在内层通常被优化器忽略。

let sql = context["sql_content"];
let lines = sql.split("\n");
let mut paren_depth = 0;
let mut found_issue = false;

for line in lines {
    let uline = line.to_upper();
    for ch in uline.chars() {
        if ch == '(' { paren_depth += 1; }
        if ch == ')' { paren_depth -= 1; if paren_depth < 0 { paren_depth = 0; } }
    }
    if paren_depth > 0 && uline.contains("ORDER BY") && !uline.contains("LIMIT") && !found_issue {
        violations.push(#{
            "message": "ORDER BY in subquery is typically ignored unless combined with LIMIT/OFFSET. Consider moving ORDER BY to the outer query.",
            "line": 1
        });
        found_issue = true;
    }
}
"#
}

fn get_union_all_preferred_script() -> &'static str {
    r#"// union_all_preferred.rhai (DML106)
// 除非需要去重，否则优先使用 UNION ALL 而非 UNION。
// UNION 会执行隐式 DISTINCT（排序去重），性能低于 UNION ALL。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.is_union() && !sel.is_union_all() {
            violations.push(#{
                "message": "UNION performs implicit DISTINCT. Use UNION ALL if duplicate elimination is not required, or explicitly use UNION DISTINCT for clarity.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
"#
}

fn get_no_nested_case_script() -> &'static str {
    r#"// no_nested_case.rhai (DML107)
// 避免嵌套 CASE 表达式，优先使用简单 CASE。
// 嵌套 CASE（CASE WHEN ... CASE WHEN ...）可读性差且易出错。

let sql = context["sql_content"];
let upper = sql.to_upper();

let mut case_count = 0;
let mut search_pos = 0;
while search_pos < upper.len() {
    let pos = upper.find_at("CASE", search_pos);
    if pos == () { break; }
    case_count += 1;
    search_pos = pos + 4;
}

if case_count > 1 {
    violations.push("Nested CASE expressions reduce readability. Consider rewriting with simple CASE or extracting into subqueries.");
}
"#
}

fn get_no_constant_where_script() -> &'static str {
    r#"// no_constant_where.rhai (DML108)
// 避免 WHERE 子句中使用常量条件（WHERE 1=1, WHERE true 等）。
// 通常是动态 SQL 拼接的产物或调试遗留代码。

let sql = context["sql_content"];
let upper = sql.to_upper();

let patterns = [
    "WHERE 1=1",
    "WHERE 1 = 1",
    "WHERE 1=0",
    "WHERE 1 = 0",
    "WHERE TRUE",
    "WHERE TRUE ",
    "WHERE FALSE",
    "WHERE FALSE ",
    "WHERE 1<>1",
    "WHERE 1 <> 1"
];

let lines = sql.split("\n");
let mut line_num = 1;
for line in lines {
    let uline = line.to_upper();
    for pat in patterns {
        if uline.contains(pat) {
            violations.push(#{
                "message": "Constant condition '" + pat + "' found in WHERE clause. This is likely dead code or a debugging artifact. Remove it.",
                "line": line_num
            });
        }
    }
    line_num += 1;
}
"#
}
