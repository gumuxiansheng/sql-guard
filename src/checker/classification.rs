use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};

use crate::config::ClassificationConfig;
use crate::error::SqlGuardError;

#[derive(Debug, Clone)]
pub struct ClassificationResult {
    /// ★ D2：分类结果来源文件路径。当前 main.rs 仅读取 script_type，
    /// file_path 保留用于未来按文件聚合分类结果的场景及作为公开契约的一部分。
    #[allow(dead_code)]
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
        let glob = Glob::new(&rule.pattern).map_err(|e| {
            SqlGuardError::ConfigError(format!("Invalid glob pattern '{}': {}", rule.pattern, e))
        })?;
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

/// 收集 SQL 脚本文件（.sql / .ddl / .dml）。
///
/// 扫描策略：
/// - `scan_paths` 非空：仅扫描这些白名单目录（相对 `root` 或绝对路径）。
/// - `scan_paths` 为空：兜底扫描整个 `root`（保持向后兼容）。
/// - 递归时跳过 `exclude_dirs` 列出的目录名（任意层级，按名称匹配）。
pub fn collect_sql_files(
    root: &Path,
    scan_paths: &[String],
    exclude_dirs: &[String],
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let exclude_set: HashSet<String> = exclude_dirs.iter().cloned().collect();

    let scan_roots: Vec<PathBuf> = if scan_paths.is_empty() {
        // 兜底：未配置白名单时扫描整个 root
        if root.exists() {
            vec![root.to_path_buf()]
        } else {
            Vec::new()
        }
    } else {
        scan_paths
            .iter()
            .map(|p| {
                if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    root.join(p)
                }
            })
            .collect()
    };

    for scan_root in &scan_roots {
        if scan_root.exists() {
            collect_files_recursive(scan_root, scan_root, &exclude_set, &mut files);
        }
    }
    files
}

fn collect_files_recursive(
    root: &Path,
    current: &Path,
    exclude_set: &HashSet<String>,
    files: &mut Vec<PathBuf>,
) {
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // 跳过黑名单目录（按目录名匹配，任意层级）
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if exclude_set.contains(file_name) {
                        continue;
                    }
                }
                collect_files_recursive(root, &path, exclude_set, files);
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use crate::config::{ClassificationConfig, ClassificationRule};

    use super::{classify_file, collect_sql_files};

    fn make_rule(
        name: &str,
        pattern: &str,
        script_type: &str,
        priority: i32,
    ) -> ClassificationRule {
        ClassificationRule {
            name: name.to_string(),
            pattern: pattern.to_string(),
            script_type: script_type.to_string(),
            priority,
        }
    }

    fn make_config(rules: Vec<ClassificationRule>, default_type: &str) -> ClassificationConfig {
        ClassificationConfig {
            rules,
            default_type: default_type.to_string(),
        }
    }

    // ===== classify_file =====

    #[test]
    fn classify_file_matches_glob_pattern_with_priority() {
        let config = make_config(vec![make_rule("ddl", "**/ddl/**", "ddl", 10)], "other");
        let result = classify_file(Path::new("sql/ddl/a.sql"), &config).expect("classify");
        assert_eq!(result.script_type, "ddl");
    }

    #[test]
    fn classify_file_returns_default_when_no_match() {
        let config = make_config(vec![make_rule("ddl", "**/ddl/**", "ddl", 10)], "other");
        let result = classify_file(Path::new("sql/dml/a.sql"), &config).expect("classify");
        assert_eq!(result.script_type, "other");
    }

    #[test]
    fn classify_file_higher_priority_wins() {
        let config = make_config(
            vec![
                make_rule("sql-by-ext", "**/*.sql", "sql", 1),
                make_rule("ddl", "**/ddl/**", "ddl", 10),
            ],
            "other",
        );
        // sql/ddl/a.sql matches both rules; higher priority (ddl, 10) wins
        let result = classify_file(Path::new("sql/ddl/a.sql"), &config).expect("classify");
        assert_eq!(result.script_type, "ddl");
    }

    #[test]
    fn classify_file_normalizes_windows_backslashes() {
        let config = make_config(vec![make_rule("ddl", "**/ddl/**", "ddl", 10)], "other");
        // Backslashes are normalized to forward slashes before glob matching
        let result = classify_file(Path::new("sql\\ddl\\a.sql"), &config).expect("classify");
        assert_eq!(result.script_type, "ddl");
    }

    // ===== collect_sql_files =====

    #[test]
    fn collect_sql_files_only_sql_not_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql/ddl")).expect("mkdir");
        fs::create_dir_all(root.join("sql/dml")).expect("mkdir");
        fs::write(root.join("sql/ddl/a.sql"), "").expect("write");
        fs::write(root.join("sql/dml/b.txt"), "").expect("write");

        let files = collect_sql_files(root, &["sql".to_string()], &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.sql"));
    }

    #[test]
    fn collect_sql_files_collects_ddl_and_dml_extensions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql")).expect("mkdir");
        fs::write(root.join("sql/a.sql"), "").expect("write");
        fs::write(root.join("sql/b.ddl"), "").expect("write");
        fs::write(root.join("sql/c.dml"), "").expect("write");
        fs::write(root.join("sql/d.txt"), "").expect("write");

        let files = collect_sql_files(root, &["sql".to_string()], &[]);
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn collect_sql_files_excludes_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql/excluded")).expect("mkdir");
        fs::write(root.join("sql/a.sql"), "").expect("write");
        fs::write(root.join("sql/excluded/b.sql"), "").expect("write");

        let files = collect_sql_files(root, &["sql".to_string()], &["excluded".to_string()]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.sql"));
    }

    #[test]
    fn collect_sql_files_empty_scan_paths_scans_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).expect("mkdir");
        fs::write(root.join("a.sql"), "").expect("write");
        fs::write(root.join("sub/b.sql"), "").expect("write");

        let files = collect_sql_files(root, &[], &[]);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn collect_sql_files_nonexistent_path_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let files = collect_sql_files(root, &["nonexistent".to_string()], &[]);
        assert!(files.is_empty());
    }
}
