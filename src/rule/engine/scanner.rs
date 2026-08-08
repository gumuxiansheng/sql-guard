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

#[cfg(test)]
mod tests {
    use super::*;

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
}
