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
///
/// `mapper.paths` 条目支持通配符（含 `*` / `?` / `[` / `{` 即视为 glob）：
/// - 字面量条目：目录（相对 `root` 或绝对路径），递归收集其下 XML，保持历史语义。
/// - glob 条目：相对 `root` 匹配（正斜杠分隔，与 `patterns` 一致），命中目录时
///   递归收集其下 XML，命中 XML 文件时直接收录。例如 `paths = ["src/**/mapper"]`
///   可匹配任意层级嵌套的 mapper 目录。同文件被多个条目命中时去重（保序）。
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
        if has_glob_meta(path) {
            collect_mapper_glob(root, path, &glob_set, &exclude_set, &mut files);
        } else {
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
    }

    // 多个 paths 条目（字面量与 glob 混用）可能命中同一文件，去重保序
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert(f.clone()));
    files
}

/// 判断路径条目是否含 glob 元字符（含即按通配符处理）。
fn has_glob_meta(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

/// 按 glob 条目收集：递归遍历 `root`（跳过 `exclude_dirs`），
/// 目录相对路径命中则递归收集其下 XML（复用 `collect_xml_files`），
/// 文件相对路径命中且为 XML 则直接收录。
fn collect_mapper_glob(
    root: &Path,
    pattern: &str,
    glob_set: &globset::GlobSet,
    exclude_set: &HashSet<&str>,
    files: &mut Vec<PathBuf>,
) {
    let matcher = match Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            eprintln!("Warning: invalid mapper path glob '{}': {}", pattern, e);
            return;
        }
    };
    walk_mapper_glob(root, root, &matcher, glob_set, exclude_set, files);
}

fn walk_mapper_glob(
    root: &Path,
    current: &Path,
    matcher: &globset::GlobMatcher,
    glob_set: &globset::GlobSet,
    exclude_set: &HashSet<&str>,
    files: &mut Vec<PathBuf>,
) {
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                // 跳过黑名单目录（按目录名匹配，任意层级）
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if exclude_set.contains(file_name) {
                        continue;
                    }
                }
                if matcher.is_match(&rel) {
                    // 目录命中：按 patterns 收集其下 XML（以命中目录为根递归，与字面量一致）
                    collect_xml_files(&path, &path, glob_set, exclude_set, files);
                }
                walk_mapper_glob(root, &path, matcher, glob_set, exclude_set, files);
            } else if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "xml" && matcher.is_match(&rel) {
                        files.push(path);
                    }
                }
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 默认 `MapperConfig`（`patterns = ["**/*Mapper.xml"]`）必须递归收集子目录里的
    /// `*Mapper.xml` 文件。回归：递归扫描逻辑本身正确（不漏子目录）。
    #[test]
    fn collects_nested_mapper_xml_files() {
        let base = std::env::temp_dir().join("sqlguard_mine_nested_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub/deep")).unwrap();
        fs::write(base.join("UserMapper.xml"), "<xml/>").unwrap();
        fs::write(base.join("sub/OrderMapper.xml"), "<xml/>").unwrap();
        fs::write(base.join("sub/deep/ProductMapper.xml"), "<xml/>").unwrap();
        fs::write(base.join("ignore.txt"), "x").unwrap();

        let mut cfg = MapperConfig::default();
        cfg.enabled = true;
        cfg.paths = vec![base.to_string_lossy().to_string()];

        let mut files = collect_mapper_files(&base, &cfg, &[]);
        files.sort();
        assert_eq!(
            files.len(),
            3,
            "expected 3 *Mapper.xml files, got {:?}",
            files
        );
        assert!(files
            .iter()
            .all(|f| f.file_name().unwrap().to_string_lossy().ends_with("Mapper.xml")));

        let _ = fs::remove_dir_all(&base);
    }

    /// `sqlguard-mine` 显式把 `patterns` 设为包含 `**/*.xml`，因此子目录里**无 Mapper 后缀**
    /// 的普通 `*.xml` 也必须被递归收集（这正是「只扫第一层」bug 的修复点）。
    #[test]
    fn collects_nested_plain_xml_with_wildcard_pattern() {
        let base = std::env::temp_dir().join("sqlguard_mine_plain_xml_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub/deep")).unwrap();
        fs::write(base.join("a.xml"), "<xml/>").unwrap();
        fs::write(base.join("sub/b.xml"), "<xml/>").unwrap();
        fs::write(base.join("sub/deep/c.xml"), "<xml/>").unwrap();
        fs::write(base.join("UserMapper.xml"), "<xml/>").unwrap();

        let mut cfg = MapperConfig::default();
        cfg.enabled = true;
        cfg.paths = vec![base.to_string_lossy().to_string()];
        // 与 sqlguard-mine 的修复一致：同时匹配 *Mapper.xml 与任意 *.xml
        cfg.patterns = vec!["**/*Mapper.xml".to_string(), "**/*.xml".to_string()];

        let mut files = collect_mapper_files(&base, &cfg, &[]);
        files.sort();
        assert_eq!(files.len(), 4, "expected 4 xml files, got {:?}", files);

        let _ = fs::remove_dir_all(&base);
    }

    /// `paths` 支持 glob：`**/mapper` 命中任意层级的 mapper 目录并递归收集其下 XML。
    #[test]
    fn collects_glob_mapper_dir() {
        let base = std::env::temp_dir().join("sqlguard_mapper_glob_dir_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/main/resources/mapper/order")).unwrap();
        fs::create_dir_all(base.join("src/module/dao/resources/mapper")).unwrap();
        fs::create_dir_all(base.join("src/other/plain")).unwrap();
        fs::write(base.join("src/main/resources/mapper/UserMapper.xml"), "<xml/>").unwrap();
        fs::write(
            base.join("src/main/resources/mapper/order/OrderMapper.xml"),
            "<xml/>",
        )
        .unwrap();
        fs::write(
            base.join("src/module/dao/resources/mapper/ProductMapper.xml"),
            "<xml/>",
        )
        .unwrap();
        // 非 mapper 目录下的 XML 不应被 glob 命中
        fs::write(base.join("src/other/plain/plain.xml"), "<xml/>").unwrap();

        let cfg = MapperConfig {
            enabled: true,
            paths: vec!["**/mapper".to_string()],
            ..Default::default()
        };

        let mut files = collect_mapper_files(&base, &cfg, &[]);
        files.sort();
        assert_eq!(
            files.len(),
            3,
            "expected 3 xml files under **/mapper dirs, got {:?}",
            files
        );
        assert!(files
            .iter()
            .all(|f| f.file_name().unwrap().to_string_lossy().ends_with("Mapper.xml")));

        let _ = fs::remove_dir_all(&base);
    }

    /// `paths` glob 可命中具体 XML 文件（如 `**/resources/**/*Mapper.xml`），
    /// 与目录命中并行生效。
    #[test]
    fn collects_glob_mapper_file() {
        let base = std::env::temp_dir().join("sqlguard_mapper_glob_file_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/main/resources/mapper")).unwrap();
        fs::create_dir_all(base.join("src/other")).unwrap();
        fs::write(base.join("src/main/resources/mapper/UserMapper.xml"), "<xml/>").unwrap();
        fs::write(base.join("src/other/legacy_report.xml"), "<xml/>").unwrap();
        fs::write(base.join("src/other/note.txt"), "x").unwrap();

        // patterns 保留默认值，验证文件级命中不受 patterns 过滤（显式指定即收录）
        let cfg = MapperConfig {
            enabled: true,
            paths: vec!["**/other/legacy_report.xml".to_string()],
            patterns: vec!["**/*Mapper.xml".to_string()],
            ..Default::default()
        };

        let mut files = collect_mapper_files(&base, &cfg, &[]);
        files.sort();
        assert_eq!(files.len(), 1, "expected legacy_report.xml, got {:?}", files);
        assert_eq!(
            files[0].file_name().unwrap().to_string_lossy(),
            "legacy_report.xml"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// 字面量目录与 glob 混用 + 命中重叠时去重保序。
    #[test]
    fn glob_and_literal_mixed_dedupe() {
        let base = std::env::temp_dir().join("sqlguard_mapper_glob_mixed_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/main/resources/mapper")).unwrap();
        fs::write(base.join("src/main/resources/mapper/UserMapper.xml"), "<xml/>").unwrap();

        // 字面量目录与 glob 同时命中同一文件，最终只收录一次
        let cfg = MapperConfig {
            enabled: true,
            paths: vec![
                "src/main/resources/mapper".to_string(),
                "**/resources/mapper".to_string(),
                "**/main/**/UserMapper.xml".to_string(),
            ],
            ..Default::default()
        };

        let mut files = collect_mapper_files(&base, &cfg, &[]);
        files.sort();
        assert_eq!(files.len(), 1, "expected dedupe to 1 file, got {:?}", files);

        let _ = fs::remove_dir_all(&base);
    }

    /// glob 遍历同样跳过 exclude_dirs 黑名单目录。
    #[test]
    fn glob_respects_exclude_dirs() {
        let base = std::env::temp_dir().join("sqlguard_mapper_glob_exclude_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/mapper")).unwrap();
        fs::create_dir_all(base.join("target/generated/mapper")).unwrap();
        fs::write(base.join("src/mapper/UserMapper.xml"), "<xml/>").unwrap();
        fs::write(base.join("target/generated/mapper/GenMapper.xml"), "<xml/>").unwrap();

        let cfg = MapperConfig {
            enabled: true,
            paths: vec!["**/mapper".to_string()],
            ..Default::default()
        };

        let mut files = collect_mapper_files(&base, &cfg, &["target".to_string()]);
        files.sort();
        assert_eq!(files.len(), 1, "expected only src/mapper file, got {:?}", files);
        assert!(files[0].to_string_lossy().contains("src"));

        let _ = fs::remove_dir_all(&base);
    }

    /// 非法 glob 只打警告并跳过，不影响其他条目。
    #[test]
    fn invalid_glob_is_skipped_with_warning() {
        let base = std::env::temp_dir().join("sqlguard_mapper_glob_invalid_test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/mapper")).unwrap();
        fs::write(base.join("src/mapper/UserMapper.xml"), "<xml/>").unwrap();

        let cfg = MapperConfig {
            enabled: true,
            paths: vec!["src/mapper".to_string(), "**/[".to_string()],
            ..Default::default()
        };

        let mut files = collect_mapper_files(&base, &cfg, &[]);
        files.sort();
        assert_eq!(files.len(), 1, "expected valid entry only, got {:?}", files);

        let _ = fs::remove_dir_all(&base);
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
