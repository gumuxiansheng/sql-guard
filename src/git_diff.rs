//! Git diff 解析：调用 `git diff --unified=0` 并解析 hunk 行范围。
//!
//! 输出每个改动文件及其新增/修改侧行范围（1-indexed, 含两端），
//! 供 `check-diff` 子命令做语句级范围交集过滤。
//!
//! 只关心"新增/修改"行，不关心删除行（删除的 SQL 已不在文件中）。
//! `--diff-filter=d` 排除纯删除的文件；新增文件（`is_new = true`）整文件算改动。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::SqlGuardError;

/// 单个文件的改动信息。
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// 文件路径（相对当前工作目录，与 git diff 输出一致）。
    pub path: PathBuf,
    /// 每个改动 hunk 的 [start_line, end_line]（含两端，1-indexed）。
    pub hunks: Vec<(usize, usize)>,
    /// true = 新增文件（整文件算改动，过滤时全保留）。
    pub is_new: bool,
}

/// 调用 `git diff --unified=0 base...HEAD` 并解析。
///
/// `path_patterns` 用于限制 diff 范围（如 `*.sql`、`src/main/resources/mapper/**/*.xml`）。
///
/// 显式指定 `--src-prefix=a/ --dst-prefix=b/`，强制 `+++ b/path` 前缀格式，
/// 不受用户 `diff.noprefix` 等配置影响——解析器依赖此前缀提取文件路径。
pub fn get_diff(base: &str, path_patterns: &[&str]) -> Result<Vec<FileDiff>, SqlGuardError> {
    let mut cmd = Command::new("git");
    cmd.args([
        "diff",
        "--unified=0",
        "--diff-filter=d",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        &format!("{}...HEAD", base),
    ]);
    if !path_patterns.is_empty() {
        cmd.arg("--");
        for p in path_patterns {
            cmd.arg(p);
        }
    }

    let output = cmd
        .output()
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to run git: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SqlGuardError::CheckError(format!(
            "git diff failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_diff_output(&stdout)
}

/// 解析 `git diff --unified=0` 的输出。
pub fn parse_diff_output(stdout: &str) -> Result<Vec<FileDiff>, SqlGuardError> {
    let mut result: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut is_new_file = false;

    for line in stdout.lines() {
        // 检测新文件块开始：`diff --git a/x b/x`
        if line.starts_with("diff --git ") {
            // 保存上一个文件
            if let Some(c) = current.take() {
                result.push(c);
            }
            is_new_file = false;
        }
        // 新增文件标记：`new file mode 100644`
        else if line.starts_with("new file mode") {
            is_new_file = true;
        }
        // 文件路径：`+++ b/path`（修改文件）或 `+++ /dev/null`（删除，已被 filter 排除）
        else if let Some(path) = line.strip_prefix("+++ b/") {
            current = Some(FileDiff {
                path: PathBuf::from(path),
                hunks: Vec::new(),
                is_new: is_new_file,
            });
        }
        // hunk 头：`@@ -old_start,old_len +new_start,new_len @@`
        else if line.starts_with("@@ ") {
            if let Some(ref mut c) = current {
                if let Some((start, end)) = parse_hunk_header(line) {
                    c.hunks.push((start, end));
                }
            }
        }
    }

    if let Some(c) = current {
        result.push(c);
    }

    Ok(result)
}

/// 解析 hunk 头 `@@ -10,3 +12,5 @@ context` 中的 `+12,5` → (12, 16)。
/// `+new_start,new_len`：从 new_start 开始，共 new_len 行。
/// 若 new_len = 0，表示纯删除，没有新增行——返回 None。
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    // 找到 `+` 开始的部分
    let plus_idx = line.find('+')?;
    let after_plus = &line[plus_idx + 1..];

    // 截取到下一个空格或 `@@`
    let end = after_plus
        .find(|c: char| c == ' ' || c == '@')
        .unwrap_or(after_plus.len());
    let range_str = &after_plus[..end];

    let (start_str, len_str) = range_str.split_once(',').unwrap_or((range_str, "1"));

    let start: usize = start_str.parse().ok()?;
    let len: usize = len_str.parse().ok()?;

    if len == 0 {
        // 纯删除 hunk，没有新增行
        return None;
    }

    Some((start, start + len - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_modification() {
        let diff = "\
diff --git a/sql/001.sql b/sql/001.sql
index 1234567..abcdefg 100644
--- a/sql/001.sql
+++ b/sql/001.sql
@@ -10,3 +12,5 @@ context
+INSERT INTO users VALUES (1);
+INSERT INTO users VALUES (2);
";
        let result = parse_diff_output(diff).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, Path::new("sql/001.sql"));
        assert!(!result[0].is_new);
        assert_eq!(result[0].hunks, vec![(12, 16)]);
    }

    #[test]
    fn parse_new_file() {
        let diff = "\
diff --git a/sql/new.sql b/sql/new.sql
new file mode 100644
index 0000000..1234567
--- /dev/null
+++ b/sql/new.sql
@@ -0,0 +1,3 @@
+SELECT 1;
+SELECT 2;
+SELECT 3;
";
        let result = parse_diff_output(diff).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, Path::new("sql/new.sql"));
        assert!(result[0].is_new);
        assert_eq!(result[0].hunks, vec![(1, 3)]);
    }

    #[test]
    fn parse_multiple_hunks_same_file() {
        let diff = "\
diff --git a/sql/001.sql b/sql/001.sql
index 1234567..abcdefg 100644
--- a/sql/001.sql
+++ b/sql/001.sql
@@ -5,2 +5,2 @@ context
-old;
+new1;
+new2;
@@ -20,1 +22,1 @@ context
-old20;
+new20;
";
        let result = parse_diff_output(diff).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hunks, vec![(5, 6), (22, 22)]);
    }

    #[test]
    fn parse_multiple_files() {
        let diff = "\
diff --git a/sql/a.sql b/sql/a.sql
index 123..abc 100644
--- a/sql/a.sql
+++ b/sql/a.sql
@@ -1,1 +1,1 @@
-old;
+new;
diff --git a/sql/b.sql b/sql/b.sql
new file mode 100644
index 000..abc
--- /dev/null
+++ b/sql/b.sql
@@ -0,0 +1,2 @@
+SELECT 1;
+SELECT 2;
";
        let result = parse_diff_output(diff).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, Path::new("sql/a.sql"));
        assert!(!result[0].is_new);
        assert_eq!(result[1].path, Path::new("sql/b.sql"));
        assert!(result[1].is_new);
    }

    #[test]
    fn parse_hunk_header_single_line() {
        // new_len = 1: `+5,1` → (5, 5)
        let r = parse_hunk_header("@@ -10,1 +5,1 @@ ctx");
        assert_eq!(r, Some((5, 5)));
    }

    #[test]
    fn parse_hunk_header_no_len() {
        // 省略 len 默认为 1
        let r = parse_hunk_header("@@ -10 +5 @@ ctx");
        assert_eq!(r, Some((5, 5)));
    }

    #[test]
    fn parse_hunk_header_pure_delete() {
        // new_len = 0：纯删除 hunk，应返回 None
        let r = parse_hunk_header("@@ -10,3 +10,0 @@ ctx");
        assert_eq!(r, None);
    }

    #[test]
    fn parse_empty_diff() {
        let result = parse_diff_output("").unwrap();
        assert!(result.is_empty());
    }
}
