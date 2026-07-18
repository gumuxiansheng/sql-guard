mod config;
mod cli;
mod error;
mod checker;
mod rule;
mod reporter;
mod mapper;

use std::path::Path;
use std::fs;

use clap::Parser;

use crate::cli::{Cli, Commands};
use crate::error::{SqlGuardError, Violation};
use crate::config::Config;
use crate::checker::directory;
use crate::checker::classification;
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

    let (missing, unexpected) = directory::check_directory_structure(&absolute_target, &config.structure);

    let sql_files = classification::collect_sql_files(&absolute_target);
    let mapper_files = mapper::collect_mapper_files(&absolute_target, &config.mapper);
    let files_checked = sql_files.len() + mapper_files.len();

    // 构建一次 Rhai 引擎，全文件/全片段复用（mapper 模式可能产生大量片段，
    // 每片段重建引擎会导致性能问题）
    let engine_instance = engine::build_engine();

    let mut all_violations: Vec<Violation> = Vec::new();

    // ===== 脚本模式：.sql/.ddl/.dml 文件，每个文件作为单 SQL 单元 =====
    for file_path in &sql_files {
        let classification_result = classification::classify_file(file_path, &config.classification)?;

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
    println!("  - config/rules/dml/no_select_all.rhai");
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
        ],
        output: crate::config::OutputConfig {
            formats: vec!["plain".to_string()],
            output_dir: None,
        },
        mapper: crate::config::MapperConfig::default(),
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

[output]
formats = ["plain", "json", "html"]

# MyBatis Mapper 模式：扫描 XML 中的 <select>/<insert>/<update>/<delete>。
# 缺省或 enabled = false 时完全保持现有行为（仅扫描 .sql/.ddl/.dml）。
# [mapper]
# enabled = true
# paths = ["src/main/resources/mapper"]
# patterns = ["**/*Mapper.xml", "**/*.xml"]
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
    r#"// no_select_all.rhai - Disallow SELECT * in DML scripts
// 基于 AST：识别 SELECT 语句，检查投影列表是否包含通配符（* 或 table.*）。
// 相比字符串匹配，AST 不会误判注释中的 SELECT * 或字符串字面量。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
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
