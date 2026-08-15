//! Git diff 解析：调用 `git diff --unified=0` 并解析 hunk 行范围。
//!
//! 输出每个改动文件及其**新侧**行范围（1-indexed, 含两端）供 `check-diff`
//! 做语句级交集过滤；同时输出**旧侧**行范围（删除行）供 `replay-export --base`
//! 识别被删除的语句。
//!
//! 只关心"新增/修改"行与"删除"行，`--diff-filter=d` 排除纯删除的文件；
//! 新增文件（`is_new = true`）整文件算改动。

/// ★ D3：`Path` 仅在 cfg(test) 测试模块中使用，生产代码只用 `PathBuf`。
/// 编译器 dead_code 分析不看测试模块故报 unused，此处显式允许。
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::SqlGuardError;

/// 单个文件的改动信息。
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// 文件路径（相对当前工作目录，与 git diff 输出一致）。
    pub path: PathBuf,
    /// 每个改动 hunk 的 [start_line, end_line]（含两端，1-indexed，**新文件**行号）。
    /// 纯删除 hunk（无新增行）不包含对应项。
    pub hunks: Vec<(usize, usize)>,
    /// 每个改动 hunk 对应的**旧文件**行范围（含两端，1-indexed）。
    /// 新增文件 / 纯新增 hunk 不包含对应项。供增量导出识别被删除的语句。
    pub old_hunks: Vec<(usize, usize)>,
    /// true = 新增文件（整文件算改动，过滤时全保留）。
    pub is_new: bool,
}

/// 调用 `git diff` 计算基线到**工作区**的改动并解析（含未提交内容）。
///
/// 语义 = 三点 diff `base...HEAD` 再叠加未提交改动：
/// 1. `git merge-base <base> HEAD` 求公共祖先（三点 diff 的起点）；
/// 2. `git diff --unified=0 <merge-base>` 对比工作区——暂存 + 未暂存的改动全部覆盖；
/// 3. `git ls-files --others --exclude-standard` 补充未跟踪的新文件（整文件算改动）。
///
/// CI 干净检出时工作区 == HEAD，行为与 `base...HEAD` 完全一致；
/// 本地开发时未提交（含未跟踪）的 SQL 改动也会被检查。
///
/// `path_patterns` 用于限制 diff 范围（如 `*.sql`、`src/main/resources/mapper/**/*.xml`）。
///
/// 显式指定 `--src-prefix=a/ --dst-prefix=b/`，强制 `+++ b/path` 前缀格式，
/// 不受用户 `diff.noprefix` 等配置影响——解析器依赖此前缀提取文件路径。
pub fn get_diff(base: &str, path_patterns: &[&str]) -> Result<Vec<FileDiff>, SqlGuardError> {
    // 三点语义 base...HEAD = merge-base(base, HEAD) → HEAD；
    // 把对比终点从 HEAD 换成工作区，即得到"已提交 + 未提交"的全部改动。
    let merge_base = merge_base(base)?;

    let mut cmd = Command::new("git");
    cmd.args([
        "diff",
        "--unified=0",
        "--diff-filter=d",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        &merge_base,
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
    let mut result = parse_diff_output(&stdout)?;

    // 未跟踪的新文件不出现在 git diff 输出中，单独列出并标记 is_new（整文件算改动）。
    for path in list_untracked(path_patterns)? {
        result.push(FileDiff {
            path: PathBuf::from(path),
            hunks: Vec::new(),
            old_hunks: Vec::new(),
            is_new: true,
        });
    }

    Ok(result)
}

/// `git merge-base <base> HEAD`：三点 diff 的起点。
fn merge_base(base: &str) -> Result<String, SqlGuardError> {
    let output = Command::new("git")
        .args(["merge-base", base, "HEAD"])
        .output()
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to run git: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SqlGuardError::CheckError(format!(
            "git merge-base failed for base '{}' ({}): {}",
            base,
            output.status,
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `git ls-files --others --exclude-standard --full-name`：未跟踪文件列表（尊重 .gitignore）。
///
/// `-z` 用 NUL 分隔，避免路径含引号/特殊字符时被 C 转义；
/// `--full-name` 强制输出仓库根相对路径，与 git diff 的路径风格一致。
fn list_untracked(path_patterns: &[&str]) -> Result<Vec<String>, SqlGuardError> {
    let mut cmd = Command::new("git");
    cmd.args([
        "ls-files",
        "--others",
        "--exclude-standard",
        "--full-name",
        "-z",
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
            "git ls-files failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

/// 读取 `git show <base>:<path>` 的旧文件内容。
///
/// 返回 `Ok(None)` 表示 base 中不存在该文件（如本次新增的文件）。
/// 供 `replay-export --base` 对旧文件内容解析，识别被删除的语句。
pub fn git_show(base: &str, path: &Path) -> Result<Option<String>, SqlGuardError> {
    let mut cmd = Command::new("git");
    cmd.args(["show", &format!("{}:{}", base, path.to_string_lossy())]);

    let output = cmd
        .output()
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to run git: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 文件在 base 中不存在（新增文件）：git 报 `exists on disk, but not in` /
        // `does not exist in` 等，视为 Ok(None)。
        if stderr.contains("exists on disk") || stderr.contains("does not exist") {
            return Ok(None);
        }
        return Err(SqlGuardError::CheckError(format!(
            "git show failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
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
                old_hunks: Vec::new(),
                is_new: is_new_file,
            });
        }
        // hunk 头：`@@ -old_start,old_len +new_start,new_len @@`
        else if line.starts_with("@@ ") {
            if let Some(ref mut c) = current {
                if let Some((old_range, new_range)) = parse_hunk_header(line) {
                    if let Some(r) = new_range {
                        c.hunks.push(r);
                    }
                    if let Some(r) = old_range {
                        c.old_hunks.push(r);
                    }
                }
            }
        }
    }

    if let Some(c) = current {
        result.push(c);
    }

    Ok(result)
}

/// hunk 头中解析出的行范围对：(旧侧范围, 新侧范围)。某侧无行（纯新增/纯删除）为 None。
type HunkRanges = (Option<(usize, usize)>, Option<(usize, usize)>);

/// 解析 hunk 头 `@@ -10,3 +12,5 @@ context` 中的两个范围：
/// 旧侧 `-10,3` → (10, 12)，新侧 `+12,5` → (12, 16)。
///
/// 某侧 len = 0（纯新增 / 纯删除）或解析失败时该侧返回 None。
fn parse_hunk_header(line: &str) -> Option<HunkRanges> {
    let body = line
        .find("@@ ")
        .map(|i| &line[i + 3..])?
        .split(" @@")
        .next()?;
    // body 形如 `-10,3 +12,5`：分隔出新侧 `+` 与旧侧 `-`
    let minus_idx = body.find('-')?;
    let plus_idx = body[minus_idx + 1..].find('+')? + minus_idx + 1;
    let old_part = &body[minus_idx + 1..plus_idx];
    let new_part = &body[plus_idx + 1..];

    let parse_side = |s: &str| -> Option<(usize, usize)> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (start_str, len_str) = s.split_once(',').unwrap_or((s, "1"));
        let start: usize = start_str.parse().ok()?;
        let len: usize = len_str.parse().ok()?;
        if len == 0 {
            // 纯删除 / 纯新增侧：无行范围
            return None;
        }
        Some((start, start + len - 1))
    };

    Some((parse_side(old_part), parse_side(new_part)))
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
        assert_eq!(result[0].old_hunks, vec![(10, 12)]);
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
        assert!(result[0].old_hunks.is_empty());
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
        assert_eq!(result[0].old_hunks, vec![(5, 6), (20, 20)]);
    }

    #[test]
    fn parse_pure_delete_hunk() {
        // 新侧 len=0：只记录旧侧范围（供增量导出识别被删语句）
        let diff = "\
diff --git a/sql/d.sql b/sql/d.sql
index 1234567..abcdefg 100644
--- a/sql/d.sql
+++ b/sql/d.sql
@@ -10,3 +10,0 @@ context
-DELETE FROM t WHERE id = 1;
-DELETE FROM t WHERE id = 2;
-DELETE FROM t WHERE id = 3;
";
        let result = parse_diff_output(diff).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, Path::new("sql/d.sql"));
        assert!(result[0].hunks.is_empty());
        assert_eq!(result[0].old_hunks, vec![(10, 12)]);
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
        assert_eq!(result[0].hunks, vec![(1, 1)]);
        assert_eq!(result[0].old_hunks, vec![(1, 1)]);
        assert_eq!(result[1].path, Path::new("sql/b.sql"));
        assert!(result[1].is_new);
        assert_eq!(result[1].hunks, vec![(1, 2)]);
        assert!(result[1].old_hunks.is_empty());
    }

    #[test]
    fn parse_hunk_header_single_line() {
        // 新旧侧同为 `-10,1 +5,1` → 旧 (10,10)，新 (5,5)
        let r = parse_hunk_header("@@ -10,1 +5,1 @@ ctx");
        assert_eq!(r, Some((Some((10, 10)), Some((5, 5)))));
    }

    #[test]
    fn parse_hunk_header_no_len() {
        // 省略 len 默认为 1
        let r = parse_hunk_header("@@ -10 +5 @@ ctx");
        assert_eq!(r, Some((Some((10, 10)), Some((5, 5)))));
    }

    #[test]
    fn parse_hunk_header_pure_delete() {
        // new_len = 0：新侧 None，旧侧保留
        let r = parse_hunk_header("@@ -10,3 +10,0 @@ ctx");
        assert_eq!(r, Some((Some((10, 12)), None)));
    }

    #[test]
    fn parse_hunk_header_pure_add() {
        // old_len = 0：旧侧 None，新侧保留
        let r = parse_hunk_header("@@ -0,0 +1,3 @@ ctx");
        assert_eq!(r, Some((None, Some((1, 3)))));
    }

    #[test]
    fn parse_hunk_header_context_contains_plus() {
        // 上下文行内含 `-`/`+`（文件内容的一部分），不影响 hunk 范围解析
        let r = parse_hunk_header("@@ -5,2 +6,3 @@ +ctx -line");
        assert_eq!(r, Some((Some((5, 6)), Some((6, 8)))));
    }

    #[test]
    fn parse_empty_diff() {
        let result = parse_diff_output("").unwrap();
        assert!(result.is_empty());
    }
}
