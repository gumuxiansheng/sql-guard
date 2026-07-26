use std::path::{Path, PathBuf};
use std::collections::HashSet;

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
