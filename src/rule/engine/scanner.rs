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
            let next_ok = i == n
                || !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_');
            if !prev_ok || !next_ok {
                continue;
            }

            if !in_from && word_str == "FROM" {
                in_from = true;
            } else if in_from {
                // FROM 子句终止关键字
                if matches!(
                    word_str.as_str(),
                    "WHERE" | "GROUP" | "HAVING" | "ORDER" | "LIMIT" | "OFFSET"
                        | "UNION" | "EXCEPT" | "INTERSECT" | "FOR" | "FETCH" | "RETURNING"
                        | "QUALIFY" | "WINDOW" | "INTO"
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
