//! `<include refid="..."/>` 引用解析。
//!
//! MyBatis 中 `<sql id="xxx">` 定义可复用片段，`<include refid="xxx"/>` 引用它。
//! 本模块把引用替换为片段实际内容，递归处理片段内嵌套的 include，
//! 最大递归深度 `MAX_INCLUDE_DEPTH` 防止循环引用。
//!
//! v1 限制：仅支持同文件内、按 refid 完整字符串匹配（不解析跨 namespace 引用）。

use std::collections::HashMap;

use crate::error::SqlGuardError;

const MAX_INCLUDE_DEPTH: usize = 10;

/// 把 `sql` 中所有 `<include refid="..."/>` 替换为 `fragments` 中对应内容。
pub fn resolve_includes(
    sql: &str,
    fragments: &HashMap<String, String>,
) -> Result<String, SqlGuardError> {
    resolve_inner(sql, fragments, 0)
}

fn resolve_inner(
    sql: &str,
    fragments: &HashMap<String, String>,
    depth: usize,
) -> Result<String, SqlGuardError> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(SqlGuardError::MapperError(format!(
            "Exceeded max include depth ({}) — possible circular reference",
            MAX_INCLUDE_DEPTH
        )));
    }

    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < chars.len() {
        // 检测 <include
        if chars[i] == '<'
            && i + 7 < chars.len()
            && chars[i + 1] == 'i'
            && chars[i + 2] == 'n'
            && chars[i + 3] == 'c'
            && chars[i + 4] == 'l'
            && chars[i + 5] == 'u'
            && chars[i + 6] == 'd'
            && chars[i + 7] == 'e'
        {
            // 找到标签结束的 >
            let mut j = i + 7;
            while j < chars.len() && chars[j] != '>' {
                j += 1;
            }
            if j >= chars.len() {
                // 未闭合，原样保留
                out.push_str(&chars[i..].iter().collect::<String>());
                break;
            }
            let tag: String = chars[i..=j].iter().collect();
            if let Some(refid) = extract_refid(&tag) {
                if let Some(frag) = fragments.get(&refid) {
                    // 递归处理片段
                    let resolved = resolve_inner(frag, fragments, depth + 1)?;
                    out.push_str(&resolved);
                } else {
                    // 找不到片段：保留占位文本，便于规则上报
                    out.push_str(&format!("/* missing include: {} */", refid));
                }
            }
            i = j + 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// 从 `<include refid="xxx" .../>` 标签中提取 refid 属性值。
fn extract_refid(tag: &str) -> Option<String> {
    let key = "refid";
    let mut idx = tag.find(key)?;
    // 确认匹配的不是子串（如 "myrefid"），需前导字符为非字母
    if idx > 0 {
        let prev = tag.as_bytes()[idx - 1];
        if prev.is_ascii_alphabetic() {
            // 不是真正的 refid，简单跳过——实际中极少出现
            idx = tag[idx + key.len()..].find(key)? + idx + key.len();
        }
    }
    let mut j = idx + key.len();
    let bytes = tag.as_bytes();
    // 跳过空白和 =
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'=') {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    let quote = bytes[j];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let rest = &tag[j + 1..];
    let end = rest.find(quote as char)?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frags() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("cols".to_string(), "id, name".to_string());
        m.insert("where_clause".to_string(), "WHERE id = ?".to_string());
        m
    }

    #[test]
    fn simple_include() {
        let frags = frags();
        let sql = "SELECT <include refid=\"cols\"/> FROM users";
        assert_eq!(
            resolve_includes(sql, &frags).unwrap(),
            "SELECT id, name FROM users"
        );
    }

    #[test]
    fn multiple_includes() {
        let frags = frags();
        let sql = "SELECT <include refid=\"cols\"/> FROM users <include refid=\"where_clause\"/>";
        assert_eq!(
            resolve_includes(sql, &frags).unwrap(),
            "SELECT id, name FROM users WHERE id = ?"
        );
    }

    #[test]
    fn missing_include_keeps_marker() {
        let frags = frags();
        let sql = "<include refid=\"nonexistent\"/>";
        assert_eq!(
            resolve_includes(sql, &frags).unwrap(),
            "/* missing include: nonexistent */"
        );
    }

    #[test]
    fn single_quotes_supported() {
        let frags = frags();
        let sql = "<include refid='cols'/>";
        assert_eq!(resolve_includes(sql, &frags).unwrap(), "id, name");
    }

    #[test]
    fn circular_reference_errors() {
        let mut frags = HashMap::new();
        frags.insert("a".to_string(), "<include refid=\"a\"/>".to_string());
        let sql = "<include refid=\"a\"/>";
        let result = resolve_includes(sql, &frags);
        assert!(result.is_err());
    }
}
