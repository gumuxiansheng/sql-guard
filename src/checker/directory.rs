use std::collections::HashSet;
use std::path::Path;

use crate::config::StructureConfig;
use crate::error::DirectoryIssue;

/// 检查目录结构是否符合 `structure.paths` 约定。
///
/// 递归扫描时跳过 `exclude_dirs` 列出的目录名（任意层级，按名称匹配），
/// 避免 strict 模式下把 `.git`、`target` 等目录误报为 Unexpected。
///
/// `allow_extra` 的匹配规则（两种形式）：
/// - 以 `/` 结尾的字符串（如 `"sql/migrations/"`）：前缀匹配，匹配所有以该串开头的相对路径
/// - 不以 `/` 结尾的字符串（如 `"README.md"`、`"sql"`）：精确匹配相对路径名
///
/// 该规则同时适用于"严格模式缺失文件检测"与"递归收集相对路径"两个分支。
pub fn check_directory_structure(
    root: &Path,
    structure: &StructureConfig,
    exclude_dirs: &[String],
) -> (Vec<DirectoryIssue>, Vec<DirectoryIssue>) {
    let mut missing = Vec::new();
    let mut unexpected = Vec::new();

    let allow_extra_set: HashSet<&String> = structure.allow_extra.iter().collect();
    let exclude_set: HashSet<&str> = exclude_dirs.iter().map(|s| s.as_str()).collect();

    let mut found_set: HashSet<String> = HashSet::new();
    let mut found_all: Vec<String> = Vec::new();

    if root.exists() {
        collect_relative_paths(root, root, &mut found_all, &allow_extra_set, &exclude_set);
    }

    for path_str in &found_all {
        found_set.insert(path_str.clone());
    }

    for required in &structure.paths {
        if !found_set.contains(required) {
            missing.push(DirectoryIssue {
                path: Path::new(required).to_path_buf(),
                issue_type: crate::error::DirectoryIssueType::Missing,
            });
        }
    }

    if structure.strict {
        let required_set_owned: HashSet<String> = structure.paths.iter().cloned().collect();
        for found in &found_all {
            if !required_set_owned.contains(found) {
                let is_prefix_of_required = structure.paths.iter().any(|r| {
                    r.starts_with(found)
                        && (r.len() == found.len() || r.as_bytes().get(found.len()) == Some(&b'/'))
                });
                let should_skip = is_prefix_of_required
                    || allow_extra_set.contains(found)
                    || allow_extra_set.iter().any(|a| {
                        if a.ends_with('/') {
                            found.starts_with(a.as_str())
                        } else {
                            false
                        }
                    });
                if !should_skip {
                    unexpected.push(DirectoryIssue {
                        path: Path::new(found).to_path_buf(),
                        issue_type: crate::error::DirectoryIssueType::Unexpected,
                    });
                }
            }
        }
    }

    (missing, unexpected)
}

fn collect_relative_paths(
    root: &Path,
    current: &Path,
    paths: &mut Vec<String>,
    allow_extra: &HashSet<&String>,
    exclude_set: &HashSet<&str>,
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
                if let Ok(rel) = path.strip_prefix(root) {
                    // 统一为正斜杠，与配置中 paths 的风格一致，避免 Windows 反斜杠导致不匹配
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    let should_skip = allow_extra.iter().any(|a| {
                        if a.ends_with('/') {
                            rel_str.starts_with(a.as_str()) || rel_str == a.trim_end_matches('/')
                        } else {
                            false
                        }
                    });
                    if !should_skip {
                        paths.push(rel_str.clone());
                    }
                }
                collect_relative_paths(root, &path, paths, allow_extra, exclude_set);
            }
        }
    }
}

/// ★ D2：directory 模块对外格式化 API。当前 main.rs 用内部格式化逻辑，
/// 此函数保留作为库 API 供外部调用方（如 IDE 插件）使用。
#[allow(dead_code)]
pub fn format_directory_issues(
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
) -> String {
    use colored::Colorize;
    let mut output = String::new();

    if !missing.is_empty() {
        output.push_str(&format!("{}:\n", "Missing Required Paths".red().bold()));
        for issue in missing {
            output.push_str(&format!("  {}\n", issue.path.display()));
        }
    }

    if !unexpected.is_empty() {
        output.push_str(&format!("{}:\n", "Unexpected Paths".yellow().bold()));
        for issue in unexpected {
            output.push_str(&format!("  {}\n", issue.path.display()));
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::config::StructureConfig;
    use crate::error::{DirectoryIssue, DirectoryIssueType};

    use super::{check_directory_structure, format_directory_issues};

    fn make_structure(paths: Vec<&str>, strict: bool, allow_extra: Vec<&str>) -> StructureConfig {
        StructureConfig {
            paths: paths.iter().map(|s| s.to_string()).collect(),
            strict,
            allow_extra: allow_extra.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ===== check_directory_structure =====

    #[test]
    fn check_directory_structure_required_exists_no_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql")).expect("mkdir");

        let structure = make_structure(vec!["sql"], false, vec![]);
        let (missing, _unexpected) = check_directory_structure(root, &structure, &[]);
        assert!(missing.is_empty());
    }

    #[test]
    fn check_directory_structure_required_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let structure = make_structure(vec!["sql"], false, vec![]);
        let (missing, _unexpected) = check_directory_structure(root, &structure, &[]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].path, Path::new("sql"));
        assert!(matches!(missing[0].issue_type, DirectoryIssueType::Missing));
    }

    #[test]
    fn check_directory_structure_strict_reports_unexpected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql")).expect("mkdir");
        fs::create_dir_all(root.join("extra")).expect("mkdir");

        let structure = make_structure(vec!["sql"], true, vec![]);
        let (_missing, unexpected) = check_directory_structure(root, &structure, &[]);
        assert_eq!(unexpected.len(), 1);
        assert_eq!(unexpected[0].path, Path::new("extra"));
        assert!(matches!(
            unexpected[0].issue_type,
            DirectoryIssueType::Unexpected
        ));
    }

    #[test]
    fn check_directory_structure_non_strict_no_unexpected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql")).expect("mkdir");
        fs::create_dir_all(root.join("extra")).expect("mkdir");

        let structure = make_structure(vec!["sql"], false, vec![]);
        let (_missing, unexpected) = check_directory_structure(root, &structure, &[]);
        assert!(unexpected.is_empty());
    }

    #[test]
    fn check_directory_structure_allow_extra_prefix_suppresses_unexpected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql")).expect("mkdir");
        fs::create_dir_all(root.join("migrations/sub")).expect("mkdir");

        // allow_extra entry ending with '/' matches as a prefix → suppresses
        // unexpected issues for "migrations" and everything beneath it
        let structure = make_structure(vec!["sql"], true, vec!["migrations/"]);
        let (_missing, unexpected) = check_directory_structure(root, &structure, &[]);
        assert!(unexpected.is_empty());
    }

    #[test]
    fn check_directory_structure_excludes_dirs_during_scan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sql")).expect("mkdir");
        fs::create_dir_all(root.join("target/sub")).expect("mkdir");

        let structure = make_structure(vec!["sql"], true, vec![]);
        let (_missing, unexpected) =
            check_directory_structure(root, &structure, &["target".to_string()]);
        assert!(unexpected.is_empty());
    }

    // ===== format_directory_issues =====

    #[test]
    fn format_directory_issues_with_missing() {
        let missing = vec![DirectoryIssue {
            path: PathBuf::from("sql"),
            issue_type: DirectoryIssueType::Missing,
        }];
        let output = format_directory_issues(&missing, &[]);
        assert!(output.contains("Missing Required Paths"));
    }

    #[test]
    fn format_directory_issues_with_unexpected() {
        let unexpected = vec![DirectoryIssue {
            path: PathBuf::from("extra"),
            issue_type: DirectoryIssueType::Unexpected,
        }];
        let output = format_directory_issues(&[], &unexpected);
        assert!(output.contains("Unexpected Paths"));
    }

    #[test]
    fn format_directory_issues_empty() {
        let output = format_directory_issues(&[], &[]);
        assert!(output.is_empty());
    }
}
