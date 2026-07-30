//! MyBatis Mapper 模式入口。
//!
//! 职责：
//! - 在配置的 mapper 路径下收集 XML 文件（按 patterns 过滤）
//! - 把 `<select>/<insert>/<update>/<delete>` 的标签类型映射到 SqlGuard 的 `script_type`
//!   （P0 硬编码全映射到 `"dml"`；后续可配置化）
//!
//! XML 解析与 SQL 提取见 [`parser`]，占位符标准化见 [`placeholder`]，
//! `<include>` 解析见 [`include`]。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};

use crate::config::MapperConfig;

pub mod dynamic;
pub mod include;
pub mod parser;
pub mod placeholder;

pub use parser::extract_sql_from_xml;

/// 在 mapper.paths 下收集所有匹配 patterns 的 XML 文件。
///
/// `root` 用于解析相对路径；若 `mapper.enabled == false` 直接返回空。
/// 递归时跳过 `exclude_dirs` 列出的目录名（任意层级，按名称匹配）。
pub fn collect_mapper_files(
    root: &Path,
    mapper: &MapperConfig,
    exclude_dirs: &[String],
) -> Vec<PathBuf> {
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

    let exclude_set: HashSet<&str> = exclude_dirs.iter().map(|s| s.as_str()).collect();

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
        collect_xml_files(&abs_path, &abs_path, &glob_set, &exclude_set, &mut files);
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
    exclude_set: &HashSet<&str>,
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
                collect_xml_files(root, &path, glob_set, exclude_set, files);
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
/// 默认映射（向后兼容）：select/insert/update/delete → `"dml"`。
/// 可通过 `[mapper.statement_type_mapping]` 配置覆盖，例如把 select 映射到 `"query"`
/// 让 SELECT 走 query 类型规则，与 DML 分别治理。
///
/// 未在 mapping 中配置的标签回退到 `"other"`。
pub fn map_statement_type<'a>(
    stmt_type: &str,
    mapping: &'a std::collections::HashMap<String, String>,
) -> &'a str {
    // 优先查配置映射（大小写不敏感：标签名转小写后匹配）
    let lower = stmt_type.to_lowercase();
    if let Some(t) = mapping.get(&lower) {
        return t.as_str();
    }
    // 兼容旧调用方：未传 mapping 或 mapping 为空时，回退到硬编码默认
    match lower.as_str() {
        "select" | "insert" | "update" | "delete" => "dml",
        _ => "other",
    }
}
