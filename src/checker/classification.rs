use std::path::Path;

use globset::{Glob, GlobSetBuilder};

use crate::config::ClassificationConfig;
use crate::error::SqlGuardError;

#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub file_path: String,
    pub script_type: String,
}

pub fn classify_file(
    file_path: &Path,
    config: &ClassificationConfig,
) -> Result<ClassificationResult, SqlGuardError> {
    let file_str = file_path.to_string_lossy().replace('\\', "/");

    let mut sorted_rules = config.rules.clone();
    sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    let mut builder = GlobSetBuilder::new();
    for rule in &sorted_rules {
        let glob = Glob::new(&rule.pattern)
            .map_err(|e| SqlGuardError::ConfigError(format!("Invalid glob pattern '{}': {}", rule.pattern, e)))?;
        builder.add(glob);
    }
    let glob_set = builder
        .build()
        .map_err(|e| SqlGuardError::ConfigError(format!("Failed to build glob set: {}", e)))?;

    for (i, rule) in sorted_rules.iter().enumerate() {
        if glob_set.matches(&file_str).contains(&i) {
            return Ok(ClassificationResult {
                file_path: file_str,
                script_type: rule.script_type.clone(),
            });
        }
    }

    Ok(ClassificationResult {
        file_path: file_str,
        script_type: config.default_type.clone(),
    })
}

pub fn collect_sql_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if root.exists() {
        collect_files_recursive(root, root, &mut files);
    }
    files
}

fn collect_files_recursive(root: &Path, current: &Path, files: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files_recursive(root, &path, files);
            } else if path.is_file() {
                if let Some(ext) = path.extension() {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if ext_lower == "sql" || ext_lower == "ddl" || ext_lower == "dml" {
                        files.push(path);
                    }
                }
            }
        }
    }
}
