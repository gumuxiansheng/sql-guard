//! MyBatis 占位符标准化。
//!
//! - `#{name, jdbcType=VARCHAR}` → `?`（值占位符，AST 可正常解析）
//! - `${tableName}` → `_var_table_name`（标识符位置，合成为合法标识符）
//!
//! 设计取舍：`${}` 通常出现在 `FROM ${t}` / `ORDER BY ${col}` 等标识符位置，
//! 直接替换为 `?` 会让 sqlparser 解析失败。这里用变量名做合成标识符，
//! 既保证 AST 可解析，又能让规则脚本识别"此处曾是动态标识符"。

/// 将 MyBatis 占位符标准化为 SQL 友好形式。详见模块文档。
pub fn normalize_placeholders(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '#' || c == '$') && i + 1 < chars.len() && chars[i + 1] == '{' {
            let is_hash = c == '#';
            // 寻找匹配的右大括号，支持嵌套（如 #{m["a"]}，虽然不常见）
            let mut depth = 1;
            let mut j = i + 2;
            while j < chars.len() {
                match chars[j] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if j >= chars.len() {
                // 未闭合，原样保留
                out.push(c);
                out.push('{');
                i += 2;
                continue;
            }
            let content: String = chars[i + 2..j].iter().collect();
            // `#{name, jdbcType=...}` 取第一个逗号前的部分作为变量名
            let var_name = content.split(',').next().unwrap_or("").trim();
            if is_hash {
                out.push('?');
            } else {
                let cleaned: String = var_name
                    .chars()
                    .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                    .collect();
                if cleaned.is_empty() {
                    out.push_str("_var");
                } else {
                    out.push_str("_var_");
                    out.push_str(&cleaned);
                }
            }
            i = j + 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_basic() {
        assert_eq!(normalize_placeholders("WHERE id = #{id}"), "WHERE id = ?");
    }

    #[test]
    fn hash_with_jdbc_type() {
        assert_eq!(
            normalize_placeholders("WHERE id = #{id, jdbcType=VARCHAR}"),
            "WHERE id = ?"
        );
    }

    #[test]
    fn dollar_to_identifier() {
        // 变量名原样保留（不做 camelCase → snake_case 转换），
        // 便于违规消息中显示原始变量名
        assert_eq!(
            normalize_placeholders("FROM ${tableName}"),
            "FROM _var_tableName"
        );
    }

    #[test]
    fn mixed() {
        assert_eq!(
            normalize_placeholders("SELECT * FROM ${t} WHERE id = #{id}"),
            "SELECT * FROM _var_t WHERE id = ?"
        );
    }

    #[test]
    fn unmatched_brace_preserved() {
        assert_eq!(normalize_placeholders("#{unterminated"), "#{unterminated");
    }

    #[test]
    fn unicode_content_preserved() {
        // 非 ASCII 内容不应触发 byte 层面的边界问题
        assert_eq!(
            normalize_placeholders("SELECT '中文' FROM ${t}"),
            "SELECT '中文' FROM _var_t"
        );
    }
}
