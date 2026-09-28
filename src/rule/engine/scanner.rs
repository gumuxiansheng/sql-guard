use super::ast::*;

/// 扫描 SQL 源文本，检测是否出现 `FROM ... , ...` 形式的隐式逗号 JOIN。
///
/// 跟踪：字符串字面量（`'...'`、`"..."` 标识符）、行注释（`-- ...`）、
/// 块注释（`/* ... */`）、paren depth。从 FROM 关键字开始（不区分大小写、词边界），
/// 到 WHERE/GROUP/HAVING/ORDER/LIMIT/OFFSET/UNION/EXCEPT/INTERSECT/FOR/FETCH/RETURNING
/// 等子句终止关键字或语句结尾结束。跟踪期间 paren_depth=0 处的逗号 → 视为隐式 JOIN。
pub(crate) fn detect_comma_join_in_sql(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    let bytes = upper.as_bytes();
    let raw = sql.as_bytes();
    let n = bytes.len();

    let mut i = 0;
    // 状态机：是否在 FROM 子句内（顶层 paren_depth=0 处的 FROM）
    let mut in_from = false;
    let mut paren_depth: i32 = 0;

    while i < n {
        let b = bytes[i];

        // 行注释 --...\n
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 块注释 /* ... */（可能跨行，支持嵌套一层的简单实现）
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // 单引号字符串
        if b == b'\'' {
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    // 转义 '' 跳过
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 双引号字符串（标识符或字符串，统一跳过）
        if b == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'"' {
                    if i + 1 < n && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 反引号（MySQL 标识符）
        if b == b'`' {
            i += 1;
            while i < n && bytes[i] != b'`' {
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }

        if b == b'(' {
            paren_depth += 1;
            i += 1;
            continue;
        }
        if b == b')' {
            if paren_depth > 0 {
                paren_depth -= 1;
            }
            i += 1;
            continue;
        }

        // 在 FROM 子句内、paren_depth=0 处的逗号 = 隐式 JOIN
        if in_from && paren_depth == 0 && b == b',' {
            return true;
        }

        // 关键字识别（词边界）
        if b.is_ascii_alphabetic() {
            // 找到词尾
            let start = i;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &raw[start..i];
            let word_str = std::str::from_utf8(word).unwrap_or("").to_uppercase();

            // 词边界检查（前后非字母数字下划线）
            let prev_ok = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            let next_ok = i == n || !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_');
            if !prev_ok || !next_ok {
                continue;
            }

            if !in_from && word_str == "FROM" {
                in_from = true;
            } else if in_from {
                // FROM 子句终止关键字
                if matches!(
                    word_str.as_str(),
                    "WHERE"
                        | "GROUP"
                        | "HAVING"
                        | "ORDER"
                        | "LIMIT"
                        | "OFFSET"
                        | "UNION"
                        | "EXCEPT"
                        | "INTERSECT"
                        | "FOR"
                        | "FETCH"
                        | "RETURNING"
                        | "QUALIFY"
                        | "WINDOW"
                        | "INTO"
                ) {
                    in_from = false;
                }
            }
            continue;
        }

        // 分号重置 FROM 状态
        if b == b';' {
            in_from = false;
            paren_depth = 0;
        }
        i += 1;
    }
    false
}

/// 扫描 SQL 源文本，收集所有注释（行注释与块注释）。
///
/// - 行注释：`-- ...` 直到行尾
/// - 块注释：`/* ... */` 可能跨行
///
/// 行号通过计数 `\n` 累计（从 1 开始）。
pub(crate) fn collect_comments(sql: &str) -> Vec<CommentInfo> {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut comments = Vec::new();
    let mut line: i64 = 1;
    let mut i = 0;

    while i < n {
        let b = bytes[i];

        // 行注释 --...\n
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            let start = i;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            let text = std::str::from_utf8(&bytes[start..i])
                .unwrap_or("")
                .to_string();
            comments.push(CommentInfo {
                text,
                line,
                kind: "LINE".to_string(),
            });
            continue;
        }
        // 块注释 /* ... */
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let start = i;
            let start_line = line;
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            // 跳过结束 */
            if i + 1 < n {
                i += 2;
            } else {
                i = n;
            }
            let text = std::str::from_utf8(&bytes[start..i.min(n)])
                .unwrap_or("")
                .to_string();
            comments.push(CommentInfo {
                text,
                line: start_line,
                kind: "BLOCK".to_string(),
            });
            continue;
        }
        // 单引号字符串（避免字符串内的 -- 或 /* 被误识别）
        if b == b'\'' {
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            continue;
        }
        // 双引号
        if b == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'"' {
                    if i + 1 < n && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            continue;
        }
        // 反引号
        if b == b'`' {
            i += 1;
            while i < n && bytes[i] != b'`' {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }

        if b == b'\n' {
            line += 1;
        }
        i += 1;
    }

    comments
}

/// 提取 SQL 文本中**顶层**（括号深度 0）的标识符词序列（统一 ASCII 大写）。
///
/// 跳过：单/双引号/反引号字符串、行注释、块注释；丢弃非标识符字符
/// （标点、数字开头的串只保留其字母数字下划线部分）。
///
/// 用途：为 [`has_top_level_clause`] 提供"不受括号内子查询干扰"的词序列。
/// 之所以按**词序列**而非直接子串匹配，是为了容忍关键字之间的任意空白与换行
/// （`ORDER\n  BY`、`ORDER /* c */ BY` 都能命中）。
pub(crate) fn top_level_words(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut words = Vec::new();

    while i < n {
        let b = bytes[i];

        // 行注释 --...
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 块注释 /* ... */
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // 单引号字符串（含 '' 转义）
        if b == b'\'' {
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 双引号 / 反引号（标识符或字符串，整体跳过）
        if b == b'"' || b == b'`' {
            let q = b;
            i += 1;
            while i < n {
                if bytes[i] == q {
                    if i + 1 < n && bytes[i + 1] == q {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        if b == b'(' {
            depth += 1;
            i += 1;
            continue;
        }
        if b == b')' {
            depth -= 1;
            i += 1;
            continue;
        }

        // 词边界：仅收集括号深度 0 处的词，且词的**首字符**必须是字母或下划线，
        // 避免把 `1.5` / `123` 这类数字字面量当成关键字候选。
        if depth == 0 && (b.is_ascii_alphabetic() || b == b'_') {
            let prev_ok = i == 0
                || !(bytes[i - 1].is_ascii_alphanumeric()
                    || bytes[i - 1] == b'_'
                    || bytes[i - 1] == b'.');
            if prev_ok {
                let start = i;
                while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                words.push(
                    std::str::from_utf8(&bytes[start..i])
                        .unwrap_or("")
                        .to_ascii_uppercase(),
                );
                continue;
            }
        }
        i += 1;
    }

    words
}

/// 判断语句原文中是否存在**顶层**的子句关键字（如 `"ORDER BY"`、`"GROUP BY"`）。
///
/// 用于 sqlparser 未建模的子句（UPDATE 的 `ORDER BY`/`GROUP BY`、
/// DELETE 的 `GROUP BY`）做原文兜底判定。跟踪括号深度，`OVER (ORDER BY ...)`
/// 这类子查询/窗口定义内部的同名子句不会被误判为顶层。
///
/// 关键字之间允许任意空白、换行与注释（基于词序列匹配）。
pub(crate) fn has_top_level_clause(sql: &str, keyword: &str) -> bool {
    let want: Vec<String> = keyword
        .split_whitespace()
        .map(|w| w.to_ascii_uppercase())
        .collect();
    if want.is_empty() {
        return false;
    }
    let words = top_level_words(sql);
    if words.len() < want.len() {
        return false;
    }
    words.windows(want.len()).any(|w| w == want.as_slice())
}

/// 按行区间 `[start_line, end_line]`（**1-based、闭区间**）切出整行文本，以 `\n` 连接。
///
/// 与 `SqlAst::slice` 的差别：本函数不看列号，整行取，专供"按语句行区间取原文"使用。
/// 兼容 LF/CRLF（`\r` 会保留在行尾）。越界按"取到边界为止"处理，不 panic。
pub(crate) fn line_range_text(sql: &str, start_line: i64, end_line: i64) -> String {
    if sql.is_empty() {
        return String::new();
    }
    let l1 = start_line.max(1) as usize;
    let l2 = end_line.max(1) as usize;
    if l2 < l1 {
        return String::new();
    }
    let mut out = String::new();
    for (idx, line_text) in sql.split('\n').enumerate() {
        let line_no = idx + 1;
        if line_no < l1 {
            continue;
        }
        if line_no > l2 {
            break;
        }
        if line_no > l1 {
            out.push('\n');
        }
        out.push_str(line_text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== top_level_words / has_top_level_clause =====

    // ===== detect_comma_join_in_sql =====

    #[test]
    fn test_no_comma_join_simple_select() {
        assert!(!detect_comma_join_in_sql("SELECT 1"));
        assert!(!detect_comma_join_in_sql("SELECT id FROM users"));
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users WHERE id = 1"
        ));
    }

    #[test]
    fn test_detect_basic_comma_join() {
        assert!(detect_comma_join_in_sql("SELECT id FROM users, orders"));
        assert!(detect_comma_join_in_sql(
            "SELECT id FROM users u, orders o WHERE u.id = o.user_id"
        ));
    }

    #[test]
    fn test_comma_inside_string_literal_ignored() {
        // 字符串里的逗号不应被识别为 JOIN
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users WHERE name = 'a, b, c'"
        ));
        // 双引号字符串内的逗号也跳过
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users WHERE name = \"a, b\""
        ));
    }

    #[test]
    fn test_comma_inside_subquery_ignored() {
        // 子查询括号内的逗号（paren_depth > 0）不算隐式 JOIN
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM (SELECT a, b FROM t) sub"
        ));
        // 函数参数列表里的逗号也不算
        assert!(!detect_comma_join_in_sql("SELECT CONCAT(a, b) FROM users"));
    }

    #[test]
    fn test_comma_after_clause_terminator_ignored() {
        // WHERE 子句后的逗号（如 IN 列表）不应触发
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users WHERE id IN (1, 2, 3)"
        ));
        // GROUP BY 后的多个列用逗号分隔，也不应触发
        assert!(!detect_comma_join_in_sql(
            "SELECT dept, COUNT(*) FROM users GROUP BY dept, status"
        ));
    }

    #[test]
    fn test_semicolon_resets_from_state() {
        // 多条语句：第一条有 FROM，分号后第二条的逗号不应继承 FROM 状态
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users; SELECT 1, 2"
        ));
        // 但第二条若也有 FROM，则正常检测
        assert!(detect_comma_join_in_sql("SELECT 1; SELECT id FROM a, b"));
    }

    #[test]
    fn test_comma_in_comments_ignored() {
        // 行注释里的逗号
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users -- a, b\nWHERE 1=1"
        ));
        // 块注释里的逗号
        assert!(!detect_comma_join_in_sql("SELECT id FROM /* a, b */ users"));
    }

    #[test]
    fn test_backtick_identifier_comma_ignored() {
        // MySQL 反引号标识符里的逗号
        assert!(!detect_comma_join_in_sql(
            "SELECT id FROM users WHERE `a,b` = 1"
        ));
    }

    // ===== collect_comments =====

    #[test]
    fn test_collect_line_comments() {
        let sql = "-- first comment\nSELECT 1\n-- second\n";
        let comments = collect_comments(sql);
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].kind, "LINE");
        assert_eq!(comments[0].line, 1);
        assert_eq!(comments[0].text, "-- first comment");
        assert_eq!(comments[1].line, 3);
        assert_eq!(comments[1].text, "-- second");
    }

    #[test]
    fn test_collect_block_comment_single_line() {
        let sql = "SELECT /* hint */ 1";
        let comments = collect_comments(sql);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, "BLOCK");
        assert_eq!(comments[0].line, 1);
        assert_eq!(comments[0].text, "/* hint */");
    }

    #[test]
    fn test_collect_block_comment_multiline() {
        let sql = "SELECT /* multi\nline\ncomment */ 1";
        let comments = collect_comments(sql);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, "BLOCK");
        assert_eq!(
            comments[0].line, 1,
            "block comment line should be start line"
        );
        assert!(comments[0].text.contains("multi"));
        assert!(comments[0].text.contains("comment"));
    }

    #[test]
    fn test_collect_comments_skips_string_literals() {
        // 字符串字面量里的 -- 不应被识别为行注释
        let sql = "SELECT 'a -- not a comment' FROM t";
        let comments = collect_comments(sql);
        assert!(
            comments.is_empty(),
            "no comment should be collected from string literal"
        );
    }

    #[test]
    fn test_collect_comments_skips_string_with_block_marker() {
        // 字符串里的 /* 不应被识别为块注释
        let sql = "SELECT 'a /* not a comment' FROM t";
        let comments = collect_comments(sql);
        assert!(comments.is_empty());
    }

    #[test]
    fn test_collect_comments_unterminated_block() {
        // 未闭合的块注释：扫描到 EOF，仍应作为 BLOCK 注释收集
        let sql = "SELECT /* never closed";
        let comments = collect_comments(sql);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, "BLOCK");
        assert_eq!(comments[0].text, "/* never closed");
    }

    #[test]
    fn test_collect_comments_mixed() {
        let sql = "-- line one\n/* block */\nSELECT 'x' -- tail\n";
        let comments = collect_comments(sql);
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].kind, "LINE");
        assert_eq!(comments[0].line, 1);
        assert_eq!(comments[1].kind, "BLOCK");
        assert_eq!(comments[1].line, 2);
        assert_eq!(comments[2].kind, "LINE");
        assert_eq!(comments[2].line, 3);
    }

    #[test]
    fn test_top_level_words_skips_strings_and_nested_parens() {
        let words = top_level_words("UPDATE t SET a = 'ORDER BY' WHERE b IN (SELECT ORDER FROM x)");
        assert_eq!(words[0], "UPDATE");
        assert!(words.contains(&"SET".to_string()));
        assert!(words.contains(&"WHERE".to_string()));
        // 括号内的 ORDER 被丢弃（深度 1）
        assert!(!words.contains(&"ORDER".to_string()));
        // 字符串里的 ORDER 被丢弃
        assert_eq!(words.iter().filter(|w| *w == "ORDER").count(), 0);
    }

    #[test]
    fn test_top_level_words_drops_numeric_literals() {
        let words = top_level_words("UPDATE t SET a = 1.5, b = 2 WHERE c = 3");
        assert_eq!(words, vec!["UPDATE", "T", "SET", "A", "B", "WHERE", "C"]);
    }

    #[test]
    fn test_has_top_level_clause_basic() {
        assert!(has_top_level_clause(
            "UPDATE t SET a = 1 WHERE b = 2 ORDER BY c",
            "ORDER BY"
        ));
        assert!(has_top_level_clause(
            "UPDATE t SET a = 1 WHERE b = 2 GROUP BY c",
            "GROUP BY"
        ));
        assert!(!has_top_level_clause(
            "UPDATE t SET a = 1 WHERE b = 2",
            "ORDER BY"
        ));
        assert!(!has_top_level_clause(
            "DELETE FROM t WHERE b = 2",
            "GROUP BY"
        ));
    }

    #[test]
    fn test_has_top_level_clause_tolerates_whitespace_and_comments() {
        // 关键字被换行 / 块注释分隔，仍应命中
        assert!(has_top_level_clause(
            "UPDATE t SET a=1 WHERE b=2 ORDER\n  BY c",
            "ORDER BY"
        ));
        assert!(has_top_level_clause(
            "UPDATE t SET a=1 WHERE b=2 ORDER /* c */ BY c",
            "ORDER BY"
        ));
    }

    #[test]
    fn test_has_top_level_clause_ignores_nested_and_quoted() {
        // 子查询 / 窗口定义里的 ORDER BY 不算顶层
        assert!(!has_top_level_clause(
            "UPDATE t SET a = (SELECT x FROM y ORDER BY x LIMIT 1) WHERE b = 2",
            "ORDER BY"
        ));
        assert!(!has_top_level_clause(
            "SELECT ROW_NUMBER() OVER (ORDER BY c) FROM t",
            "ORDER BY"
        ));
        // 字符串与注释里的 ORDER BY 不算
        assert!(!has_top_level_clause(
            "UPDATE t SET a = 'ORDER BY' WHERE b = 2",
            "ORDER BY"
        ));
        assert!(!has_top_level_clause(
            "UPDATE t SET a = 1 /* ORDER BY c */ WHERE b = 2",
            "ORDER BY"
        ));
    }

    #[test]
    fn test_has_top_level_clause_word_boundary() {
        // 不应把 `reorder by_x` 当作 `ORDER BY`
        assert!(!has_top_level_clause(
            "UPDATE t SET a = 1 WHERE reorder by_x = 2",
            "ORDER BY"
        ));
        // 三段关键字也能匹配
        assert!(has_top_level_clause(
            "UPDATE t SET a = 1 ORDER BY x",
            "ORDER BY X"
        ));
    }

    // ===== line_range_text =====

    #[test]
    fn test_line_range_text_basic_inclusive() {
        let sql = "SELECT 1;\nUPDATE t SET a = 1;\nDELETE FROM t;";
        assert_eq!(line_range_text(sql, 1, 1), "SELECT 1;");
        assert_eq!(line_range_text(sql, 2, 2), "UPDATE t SET a = 1;");
        assert_eq!(line_range_text(sql, 1, 2), "SELECT 1;\nUPDATE t SET a = 1;");
        assert_eq!(line_range_text(sql, 3, 3), "DELETE FROM t;");
    }

    #[test]
    fn test_line_range_text_out_of_bounds_and_inverted() {
        let sql = "a\nb";
        assert_eq!(line_range_text(sql, 1, 99), "a\nb", "上界越界取到文件尾");
        assert_eq!(line_range_text(sql, 3, 5), "", "起始行越界返回空串");
        assert_eq!(line_range_text(sql, 2, 1), "", "区间倒置返回空串");
        assert_eq!(line_range_text("", 1, 1), "");
    }

    #[test]
    fn test_line_range_text_keeps_crlf_carriage_return() {
        let sql = "SELECT 1;\r\nSELECT 2;";
        // `\r` 保留在行尾，与 sqlparser 把 `\r` 计入列号的语义一致
        assert_eq!(line_range_text(sql, 1, 1), "SELECT 1;\r");
        assert_eq!(line_range_text(sql, 2, 2), "SELECT 2;");
    }

    #[test]
    fn test_line_range_text_multibyte_safe() {
        let sql = "CREATE TABLE 用户表 (\n  名称 VARCHAR(20)\n);";
        assert_eq!(line_range_text(sql, 2, 2), "  名称 VARCHAR(20)");
    }
}
