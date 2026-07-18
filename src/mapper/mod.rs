//! MyBatis Mapper 模式入口。
//!
//! 职责：
//! - 在配置的 mapper 路径下收集 XML 文件（按 patterns 过滤）
//! - 把 `<select>/<insert>/<update>/<delete>` 的标签类型映射到 SqlGuard 的 `script_type`
//!   （P0 硬编码全映射到 `"dml"`；后续可配置化）
//!
//! XML 解析与 SQL 提取见 [`parser`]，占位符标准化见 [`placeholder`]，
//! `<include>` 解析见 [`include`]。

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};

use crate::config::MapperConfig;

pub mod include;
pub mod parser;
pub mod placeholder;

pub use parser::extract_sql_from_xml;

/// 在 mapper.paths 下收集所有匹配 patterns 的 XML 文件。
///
/// `root` 用于解析相对路径；若 `mapper.enabled == false` 直接返回空。
pub fn collect_mapper_files(root: &Path, mapper: &MapperConfig) -> Vec<PathBuf> {
    if !mapper.enabled {
        return Vec::new();
    }

    let patterns: Vec<&str> = if mapper.patterns.is_empty() {
        vec!["**/*Mapper.xml", "**/*.xml"]
    } else {
        mapper.patterns.iter().map(|s| s.as_str()).collect()
    };

    let glob_set = match build_glob_set(&patterns) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: invalid mapper patterns: {}", e);
            return Vec::new();
        }
    };

    let mut files = Vec::new();
    for path in &mapper.paths {
        let abs_path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            root.join(path)
        };
        if !abs_path.exists() {
            continue;
        }
        collect_xml_files(&abs_path, &abs_path, &glob_set, &mut files);
    }
    files
}

fn build_glob_set(patterns: &[&str]) -> Result<globset::GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p)?);
    }
    builder.build()
}

fn collect_xml_files(
    root: &Path,
    current: &Path,
    glob_set: &globset::GlobSet,
    files: &mut Vec<PathBuf>,
) {
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_xml_files(root, &path, glob_set, files);
            } else if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "xml" {
                        let rel = path.strip_prefix(root).unwrap_or(&path);
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if glob_set.is_match(&rel_str) {
                            files.push(path);
                        }
                    }
                }
            }
        }
    }
}

/// 把 MyBatis 语句标签映射到 SqlGuard 的 `script_type`。
///
/// P0 硬编码：select/insert/update/delete → `"dml"`。
/// 后续可通过 `[mapper.statement_type_mapping]` 配置化。
pub fn map_statement_type(stmt_type: &str) -> &str {
    match stmt_type {
        "select" | "insert" | "update" | "delete" => "dml",
        _ => "other",
    }
}
