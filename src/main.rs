mod config;
mod cli;
mod error;
mod checker;
mod rule;
mod reporter;

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
        Commands::Check { path, config: config_path, format, output_dir } => {
            run_check(&path, &config_path, &format, output_dir.as_deref())?;
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

    let absolute_target = if target_dir.is_absolute() {
        target_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(target_dir))
            .unwrap_or_else(|_| target_dir.to_path_buf())
    };

    let (missing, unexpected) = directory::check_directory_structure(&absolute_target, &config.structure);

    let sql_files = classification::collect_sql_files(&absolute_target);
    let files_checked = sql_files.len();

    let mut all_violations: Vec<Violation> = Vec::new();

    for file_path in &sql_files {
        let classification_result = classification::classify_file(file_path, &config.classification)?;

        let sql_content = fs::read_to_string(file_path)
            .map_err(|e| SqlGuardError::CheckError(format!("Failed to read '{}': {}", file_path.display(), e)))?;

        let violations = engine::run_rules_for_file(
            file_path,
            &sql_content,
            &classification_result.script_type,
            &config,
            &config_dir,
        )?;

        all_violations.extend(violations);
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
        rules: vec![],
        output: crate::config::OutputConfig {
            formats: vec!["plain".to_string()],
            output_dir: None,
        },
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
name = "no_drop_table"
description = "Disallow DROP TABLE in DDL scripts"
enabled = true
script_path = "config/rules/ddl/no_drop_table.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
name = "primary_key_required"
description = "CREATE TABLE must have a PRIMARY KEY"
enabled = true
script_path = "config/rules/ddl/primary_key_required.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
name = "no_select_all"
description = "Disallow SELECT * in DML scripts"
enabled = true
script_path = "config/rules/dml/no_select_all.rhai"
applies_to = ["dml"]
severity = "error"

[output]
formats = ["plain", "json", "html"]
"#
}

fn get_no_drop_table_script() -> &'static str {
    r#"// no_drop_table.rhai - Disallow DROP TABLE in DDL scripts
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("DROP TABLE") || upper.contains("DROP TABLE IF EXISTS") {
    violations.push("DROP TABLE is not allowed in DDL scripts");
}
"#
}

fn get_primary_key_script() -> &'static str {
    r#"// primary_key_required.rhai - CREATE TABLE must have a PRIMARY KEY
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("CREATE TABLE") && !upper.contains("PRIMARY KEY") {
    violations.push("CREATE TABLE statement should include a PRIMARY KEY");
}
"#
}

fn get_no_select_all_script() -> &'static str {
    r#"// no_select_all.rhai - Disallow SELECT * in DML scripts
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("SELECT *") {
    violations.push("SELECT * is not allowed. Specify columns explicitly.");
}
"#
}
