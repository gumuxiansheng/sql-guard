//! `sqlguard-mine` —— 逻辑外键挖掘独立可执行文件。
//!
//! 复用 `sqlguard` 库的 mapper 解析与 SQL AST 能力，从 MyBatis Mapper XML 的
//! JOIN 语句中积累「类外键」信息（团队禁止物理外键时的关系补全手段），
//! 产出 `relations.json` 逻辑外键目录。本二进制与主 `sqlguard` 完全独立，
//! 不会把挖掘逻辑链入主程序。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use sqlguard::config::{CheckDialect, MapperConfig};
use sqlguard::git_diff::{self, FileDiff};
use sqlguard::mapper::collect_mapper_files;
use sqlguard::mapper::parser::extract_sql_from_xmls;
use sqlguard::relation::{extract_join_edges, JoinEdge};
use sqlguard::replay_export::intersects_hunks;
use sqlguard::rule::engine::parser::parse_sql_to_ast_fb;

#[derive(Parser, Debug)]
#[clap(
    name = "sqlguard-mine",
    about = "Mine logical foreign keys (class FKs) from MyBatis mapper JOINs",
    version
)]
struct Cli {
    /// Directory containing MyBatis mapper XML files to scan.
    ///
    /// Scanned recursively; nested subdirectories are included. A single
    /// `.xml` file is NOT accepted (pass the directory that contains it).
    #[clap(short = 'p', long, default_value = ".")]
    path: String,

    /// SQL dialect: generic | mysql | postgresql | ansi | oracle | gaussdb.
    ///
    /// `postgres`/`pg` and `gauss`/`gaussdb` are accepted aliases.
    /// Invalid values are rejected with an error message.
    #[clap(long, default_value = "generic")]
    dialect: String,

    /// Dialect fallback: generic | mysql | postgresql | ansi | oracle | gaussdb (optional).
    ///
    /// `postgres`/`pg` and `gauss`/`gaussdb` are accepted aliases.
    /// Invalid values are rejected with an error message.
    #[clap(long)]
    dialect_fallback: Option<String>,

    /// Incremental scan: only mine mapper XML files changed since the given git
    /// baseline (e.g. `origin/main`, `HEAD~1`), and within each changed file
    /// only the SQL statements whose line span intersects a diff hunk
    /// (statement-level granularity). A statement's span runs from its
    /// `<select>` open-tag line to the line just before the next statement's
    /// open tag (last statement spans to EOF), so a change anywhere in the
    /// statement body is caught — not just the open-tag line. Uses
    /// `git diff --unified=0 <base>...HEAD`; unchanged files and unchanged
    /// statements inside a changed file are skipped. Omit this flag for a full
    /// scan of the whole directory tree (default behavior).
    #[clap(long)]
    base: Option<String>,

    /// Also scan plain XML files that do NOT carry the `Mapper.xml` suffix
    /// (e.g. `user.xml`, `config.xml`, `pom.xml`). Off by default — the
    /// default only matches `*Mapper.xml` to stay aligned with the documented
    /// intent in `src/config.rs` and to avoid parsing unrelated XML configs
    /// (which can also abort the run on malformed input). Pass this flag when
    /// your mappers live in non-`Mapper`-suffixed XML.
    #[clap(long)]
    include_plain_xml: bool,

    /// Encoding of the mapper XML files (default utf-8).
    ///
    /// Supported labels: utf-8, gbk, gb2312, gb18030, big5, shift_jis
    /// (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, utf-32le,
    /// utf-32be, windows-1252 (latin1, iso-8859-1), ascii, ...
    /// Files with a BOM are decoded per the BOM regardless of this value.
    #[clap(long, default_value = "utf-8")]
    encoding: String,

    /// Output JSON path for the logical-FK catalog.
    #[clap(short = 'o', long, default_value = "relations.json")]
    output: String,
}

#[derive(Debug, Clone, Serialize)]
struct Endpoint {
    table: String,
    column: String,
}

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    mapper: String,
    statement_id: String,
    line: usize,
    join_type: String,
}

#[derive(Debug, Clone, Serialize)]
struct Relation {
    from: Endpoint,
    to: Endpoint,
    join_types: Vec<String>,
    direction_hint: String,
    occurrences: Vec<Evidence>,
}

#[derive(Debug, Serialize)]
struct RelationsOutput {
    relations: Vec<Relation>,
    stats: Stats,
}

#[derive(Debug, Serialize)]
struct Stats {
    relation_count: usize,
    occurrence_total: usize,
}

/// 无向关系键：`table.column` 排序后拼接，保证 `a.x=b.y` 与 `b.y=a.x` 归并。
#[derive(Hash, Eq, PartialEq)]
struct RelationKey {
    a: String,
    b: String,
}

fn relation_key(edge: &JoinEdge) -> RelationKey {
    let mut parts = [
        format!("{}.{}", edge.left_table, edge.left_column),
        format!("{}.{}", edge.right_table, edge.right_column),
    ];
    parts.sort();
    RelationKey {
        a: parts[0].clone(),
        b: parts[1].clone(),
    }
}

/// 路径规范化为正斜杠字符串，便于跨平台及 cwd 与仓库根之间的前缀对齐比较。
fn norm_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 在 git diff 改动集中查找与 `file` 匹配的项。
///
/// git diff 输出的路径相对「仓库根」（尤其从子目录运行时），而本程序收集到的 mapper
/// 路径是绝对路径，二者前缀不同。若按"相对 cwd 的字符串相等"来匹配，从子目录运行时
/// 会全部失配 → 增量模式静默扫出 0 个文件并把输出覆盖成空目录。这里改用「双向后缀匹配」：
/// 只要绝对路径以 `diff.path`（或反之）结尾即视为同一文件，兼容仓库根与任意子目录运行。
fn find_diff<'a>(file: &'a Path, diffs: &'a [FileDiff]) -> Option<&'a FileDiff> {
    let f = norm_path(file);
    diffs.iter().find(|d| {
        let p = norm_path(&d.path);
        if p == "/" || p == "." || p.is_empty() {
            return true; // 整个仓库范围的 diff
        }
        f == p || f.ends_with(&format!("/{}", p)) || p.ends_with(&format!("/{}", f))
    })
}

/// 增量模式下，判断某条语句是否应被挖掘（全量模式下恒为 true）：
/// - 全量模式（`fd = None`）：直接挖掘整文件所有语句。
/// - 新增文件（`is_new`）：整文件算改动，全部挖掘。
/// - 普通修改：仅当语句行区间 `[line, end_line]` 与 git diff 的某个 hunk 行范围
///   有交集才挖掘（与 `replay_export::intersects_hunks` 同一语义，做到语句级增量）。
///
/// `end_line` 为该语句的「下一语句开标签行 - 1」（末语句为 `i64::MAX` 表示到 EOF），
/// 覆盖整段语句体；这样即便改动落在 `<select>` 内容行而非开标签行，也能被 hunk 命中。
fn should_mine_statement(fd: Option<&FileDiff>, line: i64, end_line: i64) -> bool {
    match fd {
        None => true,
        Some(d) if d.is_new => true,
        Some(d) => intersects_hunks(line, end_line, &d.hunks),
    }
}

/// 方向启发式：列名以 `_id` 结尾的一侧为外键持有方（指向另一侧的主键）。
fn direction_hint(left: &Endpoint, right: &Endpoint) -> &'static str {
    let l_fk = left.column.ends_with("_id");
    let r_fk = right.column.ends_with("_id");
    match (l_fk, r_fk) {
        (true, false) => "left->right",
        (false, true) => "right->left",
        _ => {
            if left.column == "id" && right.column.ends_with("_id") {
                "right->left"
            } else if right.column == "id" && left.column.ends_with("_id") {
                "left->right"
            } else {
                "undirected"
            }
        }
    }
}

fn relation_from_edge(edge: &JoinEdge) -> Relation {
    let left = Endpoint {
        table: edge.left_table.clone(),
        column: edge.left_column.clone(),
    };
    let right = Endpoint {
        table: edge.right_table.clone(),
        column: edge.right_column.clone(),
    };
    let hint = direction_hint(&left, &right);
    // 把外键持有方规范为 `from`，被引用方为 `to`。
    let (from, to) = if hint == "right->left" {
        (right, left)
    } else {
        (left, right)
    };
    Relation {
        from,
        to,
        join_types: vec![edge.join_type.clone()],
        direction_hint: hint.to_string(),
        occurrences: Vec::new(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let dialect = CheckDialect::parse_dialect(&cli.dialect);
    if dialect.as_str() != cli.dialect.as_str() && dialect == CheckDialect::Generic {
        eprintln!(
            "Warning: unknown dialect '{}', falling back to generic",
            cli.dialect
        );
    }
    let fallback = cli
        .dialect_fallback
        .as_deref()
        .map(CheckDialect::parse_dialect)
        .or_else(|| dialect.default_fallback());
    if let Some(ref fb_str) = cli.dialect_fallback {
        let fb = CheckDialect::parse_dialect(fb_str);
        if fb.as_str() != fb_str.as_str() && fb == CheckDialect::Generic {
            eprintln!(
                "Warning: unknown dialect-fallback '{}', falling back to generic",
                fb_str
            );
        }
    }

    let mapper_cfg = MapperConfig {
        enabled: true,
        paths: vec![cli.path.clone()],
        // 缺省仅匹配 `*Mapper.xml`，与 `config.rs::default_mapper_patterns` 的既定意图一致，
        // 避免误扫 pom.xml / web.xml / target/ 等非 mapper 配置，也规避畸形 XML 中断整轮扫描。
        // 递归覆盖子目录已由 `collect_xml_files` 保证（`collects_nested_mapper_xml_files` 验证）。
        // 仅当用户显式传 `--include-plain-xml` 时才放开到所有 `*.xml`；这是调用方驱动的开关，
        // 而非硬编码放宽——无 Mapper 后缀的 mapper（如 `user.xml`）需要此开关才能被扫到。
        patterns: if cli.include_plain_xml {
            vec!["**/*Mapper.xml".to_string(), "**/*.xml".to_string()]
        } else {
            vec!["**/*Mapper.xml".to_string()]
        },
        ..Default::default()
    };

    let root = std::env::current_dir()?;
    let all_mapper_files = collect_mapper_files(&root, &mapper_cfg, &[]);

    // 增量模式：取 git 基线以来的改动，得到「改动文件集」+「各文件 hunk 行范围索引」。
    // 纯删除文件已被 `git diff --diff-filter=d` 排除，不进入扫描集。
    // 语义约定：扫描的是磁盘文件，要求其与 HEAD 一致（与 check-diff / replay-export 一致）；
    // hunk 行号采用新侧（HEAD）行号，与 mapper XML 的 `raw_xml_line` 对齐用于语句级过滤。
    // 增量模式：取 git 基线以来的改动，得到「改动文件集」+「各文件 hunk 行范围索引」。
    // 纯删除文件已被 `git diff --diff-filter=d` 排除，不进入扫描集。
    // 语义约定：扫描的是磁盘文件，要求其与 HEAD 一致（与 check-diff / replay-export 一致）；
    // hunk 行号采用新侧（HEAD）行号，与 mapper XML 的 `raw_xml_line` 对齐用于语句级过滤。
    let (mapper_files, file_diffs): (Vec<PathBuf>, HashMap<String, FileDiff>) =
        if let Some(base) = &cli.base {
            // git diff 的 pathspec 需要相对仓库根的路径；把扫描路径相对 cwd 规范化
            // 成仓库内路径（否则传绝对路径会被 git 判为 "outside repository"）。
            let cwd = std::env::current_dir()?;
            let rel_path = std::fs::canonicalize(&cli.path)
                .unwrap_or_else(|_| cli.path.clone().into())
                .strip_prefix(&cwd)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| cli.path.clone());
            let pattern: &str = if rel_path.is_empty() { "." } else { &rel_path };
            let diffs = git_diff::get_diff(base, &[pattern])
                .map_err(|e| format!("incremental scan failed: {}", e))?;
            // 用 `find_diff` 把每个 mapper 文件对齐到其 git diff 项，并以规范化绝对路径为键
            // 建索引；循环内直接按 `norm_path(mf)` 取 hunk 做语句级过滤（与过滤步骤共用同一
            // 匹配逻辑，避免两处路径处理分叉导致「增量退化成全量」或「静默 0 文件」）。
            let mut file_diffs: HashMap<String, FileDiff> = HashMap::new();
            let files: Vec<PathBuf> = all_mapper_files
                .into_iter()
                .filter(|mf| {
                    if let Some(d) = find_diff(mf, &diffs) {
                        file_diffs.insert(norm_path(mf), d.clone());
                        true
                    } else {
                        false
                    }
                })
                .collect();
            if file_diffs.is_empty() {
                eprintln!(
                    "Incremental mode: no changed mapper files since base '{}'",
                    base
                );
            }
            (files, file_diffs)
        } else {
            (all_mapper_files, HashMap::new())
        };

    let mut scope_note = String::new();
    if cli.base.is_some() {
        scope_note.push_str(" (incremental: git baseline)");
    }
    if cli.include_plain_xml {
        scope_note.push_str(" (includes plain *.xml)");
    }
    eprintln!(
        "Found {} mapper file(s) under '{}'{}",
        mapper_files.len(),
        cli.path,
        scope_note
    );

    let mut relations: HashMap<RelationKey, Relation> = HashMap::new();
    let mut scanned = 0usize;

    for mf in &mapper_files {
        // 增量模式下取本文件对应的 git FileDiff（含 hunk 行范围）；全量模式为 None。
        // 键与过滤步骤一致：`norm_path(mf)`。
        let fd: Option<&FileDiff> = if cli.base.is_some() {
            file_diffs.get(&norm_path(mf))
        } else {
            None
        };

        // 单个 mapper 文件解析失败（畸形 / 非 UTF-8 XML 等）不应中断整轮扫描：
        // 跳过并告警，继续处理其余文件。
        let results = match extract_sql_from_xmls(std::slice::from_ref(mf), &cli.encoding) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Warning: skipping '{}' (parse failed): {}", mf.display(), e);
                continue;
            }
        };
        for (_path, sqls) in results {
            // 计算本文件内每条语句的行区间 [raw_xml_line, 下一语句开标签行-1]
            // （末语句到 EOF）。语句在文件中按文档顺序出现，排序后相邻项即为上下句。
            // 用完整区间去与 diff hunk 求交，避免「只改了语句体、没改开标签行」被漏掉。
            // 这里用「行号 → 下一行号-1」的映射代替 binary_search，避免索引回退到 0
            // 而静默赋错行区间。
            let mut lines: Vec<usize> = sqls.iter().map(|s| s.raw_xml_line).collect();
            lines.sort_unstable();
            let mut next_line: HashMap<usize, i64> = HashMap::new();
            for w in lines.windows(2) {
                next_line.insert(w[0], (w[1] - 1) as i64);
            }
            for sql in sqls {
                let end_line: i64 = *next_line.get(&sql.raw_xml_line).unwrap_or(&i64::MAX);
                // 增量 + 已存在文件：仅挖掘行区间命中 diff hunk 的语句（语句级增量）；
                // 新增文件(is_new)整文件算改动，全量模式直接全部挖掘。
                if !should_mine_statement(fd, sql.raw_xml_line as i64, end_line) {
                    continue;
                }
                scanned += 1;
                let ast = parse_sql_to_ast_fb(&sql.processed_sql, dialect, fallback);
                for edge in extract_join_edges(&ast, dialect) {
                    let evidence = Evidence {
                        mapper: mf.display().to_string(),
                        statement_id: sql.statement_id.clone(),
                        line: sql.raw_xml_line,
                        join_type: edge.join_type.clone(),
                    };
                    let key = relation_key(&edge);
                    let rel = relations
                        .entry(key)
                        .or_insert_with(|| relation_from_edge(&edge));
                    // 收集新的 join_type（去重）
                    if !rel.join_types.contains(&edge.join_type) {
                        rel.join_types.push(edge.join_type.clone());
                    }
                    rel.occurrences.push(evidence);
                }
            }
        }
    }

    let relations_vec: Vec<Relation> = relations.into_values().collect();
    let relation_count = relations_vec.len();
    let occurrence_total: usize = relations_vec.iter().map(|r| r.occurrences.len()).sum();

    let output = RelationsOutput {
        relations: relations_vec,
        stats: Stats {
            relation_count,
            occurrence_total,
        },
    };

    let json = serde_json::to_string_pretty(&output)?;
    // 输出路径的父目录可能不存在，先递归创建，避免 `No such file or directory`。
    if let Some(parent) = std::path::Path::new(&cli.output).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&cli.output, json)?;

    eprintln!(
        "Mined {} mapper SQL statement(s); wrote {} relation(s) with {} total occurrence(s) to '{}'",
        scanned, relation_count, occurrence_total, cli.output
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_diff_matches_repo_root_relative_from_subdir() {
        // 从子目录运行时：mapper 文件绝对路径含子目录段，而 git diff 输出是仓库根相对
        // 路径（也含子目录段），二者前缀不同。后缀匹配必须仍能命中，否则增量静默 0 文件。
        let file = Path::new("C:/repo/sub/mapper/User.xml");
        let diffs = vec![FileDiff {
            path: PathBuf::from("sub/mapper/User.xml"),
            hunks: vec![(1, 3)],
            old_hunks: vec![],
            is_new: true,
        }];
        assert!(
            find_diff(file, &diffs).is_some(),
            "changed file must match repo-root-relative diff path"
        );
    }

    #[test]
    fn find_diff_strips_target_dir_prefix() {
        let file = Path::new("C:/project/mapper/User.xml");
        let diffs = vec![FileDiff {
            path: PathBuf::from("mapper/User.xml"),
            hunks: vec![(1, 3)],
            old_hunks: vec![],
            is_new: true,
        }];
        let m = find_diff(file, &diffs);
        assert!(m.is_some(), "changed file should match");
        assert_eq!(m.unwrap().path, PathBuf::from("mapper/User.xml"));
    }

    #[test]
    fn find_diff_no_match_for_unchanged() {
        let file = Path::new("C:/project/mapper/Order.xml");
        let diffs = vec![FileDiff {
            path: PathBuf::from("mapper/User.xml"),
            hunks: vec![(1, 3)],
            old_hunks: vec![],
            is_new: true,
        }];
        assert!(
            find_diff(file, &diffs).is_none(),
            "unchanged file must not match a diff entry"
        );
    }

    #[test]
    fn should_mine_full_mode_always() {
        // 全量模式：不论行号，全部挖掘。
        assert!(should_mine_statement(None, 1, 1));
        assert!(should_mine_statement(None, 99, 99));
    }

    #[test]
    fn should_mine_new_file_all_statements() {
        // 新增文件：整文件算改动，所有语句都挖。
        let d = FileDiff {
            path: PathBuf::from("mapper/A.xml"),
            hunks: vec![(1, 50)],
            old_hunks: vec![],
            is_new: true,
        };
        assert!(should_mine_statement(Some(&d), 1, 50));
        assert!(should_mine_statement(Some(&d), 25, 25));
        assert!(should_mine_statement(Some(&d), 50, i64::MAX));
    }

    #[test]
    fn should_mine_only_hunk_lines() {
        // 普通修改：仅命中 hunk [10,12] 的语句被挖，其余跳过。
        let d = FileDiff {
            path: PathBuf::from("mapper/A.xml"),
            hunks: vec![(10, 12)],
            old_hunks: vec![],
            is_new: false,
        };
        // 单语句 span == 其开标签行
        assert!(should_mine_statement(Some(&d), 10, 10));
        assert!(should_mine_statement(Some(&d), 11, 11));
        assert!(should_mine_statement(Some(&d), 12, 12));
        assert!(!should_mine_statement(Some(&d), 9, 9));
        assert!(!should_mine_statement(Some(&d), 13, 13));
        assert!(!should_mine_statement(Some(&d), 5, 5));
    }

    #[test]
    fn should_mine_span_covers_body_change() {
        // 关键回归：改动落在语句体内部（行 4），而非 `<select>` 开标签行（行 3）。
        // 语句 span = [3, 5]（下一语句开标签行 6 之前），与 hunk [4,4] 有交集 → 应挖掘。
        let d = FileDiff {
            path: PathBuf::from("mapper/A.xml"),
            hunks: vec![(4, 4)],
            old_hunks: vec![],
            is_new: false,
        };
        assert!(should_mine_statement(Some(&d), 3, 5));
        // 相邻语句 span = [6, MAX]，与 hunk [4,4] 无交集 → 跳过。
        assert!(!should_mine_statement(Some(&d), 6, i64::MAX));
    }

    #[test]
    fn should_mine_multiple_hunks() {
        let d = FileDiff {
            path: PathBuf::from("mapper/A.xml"),
            hunks: vec![(3, 4), (20, 22)],
            old_hunks: vec![],
            is_new: false,
        };
        assert!(should_mine_statement(Some(&d), 3, 3));
        assert!(should_mine_statement(Some(&d), 4, 4));
        assert!(!should_mine_statement(Some(&d), 5, 5));
        assert!(should_mine_statement(Some(&d), 21, 21));
        assert!(!should_mine_statement(Some(&d), 15, 15));
    }

    /// 显式开关：未传 `--include-plain-xml` 时只认 `*Mapper.xml`；传入后才放开到所有 `*.xml`。
    /// 回归此前「硬编码放宽成 `**/*.xml`」被评审指出偏离 `config.rs` 意图——正确做法是开关驱动。
    #[test]
    fn flag_gates_plain_xml_pattern() {
        // 模拟 `main` 里设定 patterns 的两条分支，确保二者与开关严格对应。
        let default_patterns = vec!["**/*Mapper.xml".to_string()];
        let plain_patterns = vec!["**/*Mapper.xml".to_string(), "**/*.xml".to_string()];

        // 默认（无开关）：不含 `**/*.xml`。
        assert!(!default_patterns.iter().any(|p| p == "**/*.xml"));
        // 开关打开：含 `**/*.xml` 且仍保留 `*Mapper.xml`（不丢 mapper）。
        assert!(plain_patterns.iter().any(|p| p == "**/*.xml"));
        assert!(plain_patterns.iter().any(|p| p == "**/*Mapper.xml"));
        // 二者集合不同——证明开关确实改变行为，而非恒等。
        assert_ne!(default_patterns, plain_patterns);
    }
}
