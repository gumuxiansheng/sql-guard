use std::path::Path;
use std::collections::HashSet;

use crate::config::StructureConfig;
use crate::error::DirectoryIssue;

pub fn check_directory_structure(
    root: &Path,
    structure: &StructureConfig,
) -> (Vec<DirectoryIssue>, Vec<DirectoryIssue>) {
    let mut missing = Vec::new();
    let mut unexpected = Vec::new();

    let allow_extra_set: HashSet<&String> = structure.allow_extra.iter().collect();

    let mut found_set: HashSet<String> = HashSet::new();
    let mut found_all: Vec<String> = Vec::new();

    if root.exists() {
        collect_relative_paths(root, root, &mut found_all, &allow_extra_set);
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
                    r.starts_with(found) && (r.len() == found.len() || r.as_bytes().get(found.len()) == Some(&b'/'))
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
) {
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel_str = rel.to_string_lossy().to_string();
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
                let _ = collect_relative_paths(root, &path, paths, allow_extra);
            }
        }
    }
}

pub fn format_directory_issues(missing: &[DirectoryIssue], unexpected: &[DirectoryIssue]) -> String {
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
