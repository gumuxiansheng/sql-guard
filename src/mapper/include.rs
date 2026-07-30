//! `<include refid="..."/>` 引用解析。
//!
//! MyBatis 中 `<sql id="xxx">` 定义可复用片段，`<include refid="xxx"/>` 引用它。
//! 本模块把引用替换为片段实际内容，递归处理片段内嵌套的 include，
//! 最大递归深度 `MAX_INCLUDE_DEPTH` 防止循环引用。
//!
//! 跨 namespace 引用规则（v2）：
//! - `<include refid="cols"/>`（无点）→ 仅查当前文件本地片段表 `fragments`
//! - `<include refid="com.example.UserMapper.cols"/>`（含点）→ 查全局表 `global`
//!   （由所有 mapper XML 的 `namespace.id` 组合而成）
//!
//! 单文件场景调用方可传 `None` 作为 `global`，行为与 v1 完全一致。

use std::collections::HashMap;

use crate::error::SqlGuardError;

const MAX_INCLUDE_DEPTH: usize = 10;

/// 把 `sql` 中所有 `<include refid="..."/>` 替换为片段实际内容。
///
/// - `fragments`：当前文件的本地片段表（`<sql id="xxx">` 内容，按 id 索引）。
/// - `global`：跨 namespace 全局片段表，key 形如 `namespace.id`。
///   `None` 时退化为单文件模式（仅查 `fragments`）。
///
/// 查找顺序：先查 `fragments`（本地优先，向后兼容），未命中且 refid 含点时再查 `global`。
pub fn resolve_includes(
    sql: &str,
    fragments: &HashMap<String, String>,
    global: Option<&HashMap<String, String>>,
) -> Result<String, SqlGuardError> {
    resolve_inner(sql, fragments, global, 0)
}

fn resolve_inner(
    sql: &str,
    fragments: &HashMap<String, String>,
    global: Option<&HashMap<String, String>>,
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
                // 查找顺序：本地 fragments 优先（短 id），未命中且含点时查 global
                let found = fragments.get(&refid).or_else(|| {
                    if refid.contains('.') {
                        global.and_then(|g| g.get(&refid))
                    } else {
                        None
                    }
                });
                if let Some(frag) = found {
                    // 递归处理片段
                    let resolved = resolve_inner(frag, fragments, global, depth + 1)?;
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
    while j < bytes.len()
        && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'=')
    {
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
            resolve_includes(sql, &frags, None).unwrap(),
            "SELECT id, name FROM users"
        );
    }

    #[test]
    fn multiple_includes() {
        let frags = frags();
        let sql = "SELECT <include refid=\"cols\"/> FROM users <include refid=\"where_clause\"/>";
        assert_eq!(
            resolve_includes(sql, &frags, None).unwrap(),
            "SELECT id, name FROM users WHERE id = ?"
        );
    }

    #[test]
    fn missing_include_keeps_marker() {
        let frags = frags();
        let sql = "<include refid=\"nonexistent\"/>";
        assert_eq!(
            resolve_includes(sql, &frags, None).unwrap(),
            "/* missing include: nonexistent */"
        );
    }

    #[test]
    fn single_quotes_supported() {
        let frags = frags();
        let sql = "<include refid='cols'/>";
        assert_eq!(resolve_includes(sql, &frags, None).unwrap(), "id, name");
    }

    #[test]
    fn circular_reference_errors() {
        let mut frags = HashMap::new();
        frags.insert("a".to_string(), "<include refid=\"a\"/>".to_string());
        let sql = "<include refid=\"a\"/>";
        let result = resolve_includes(sql, &frags, None);
        assert!(result.is_err());
    }

    // ===== 跨 namespace 引用 =====

    fn global_frags() -> HashMap<String, String> {
        // 模拟另一个 mapper XML（namespace=com.example.OtherMapper）注册的全局片段
        let mut m = HashMap::new();
        m.insert(
            "com.example.OtherMapper.sharedCols".to_string(),
            "id, name, email".to_string(),
        );
        m.insert(
            "com.example.OtherMapper.sharedWhere".to_string(),
            "WHERE deleted = 0".to_string(),
        );
        m
    }

    #[test]
    fn cross_namespace_include_resolved() {
        // refid 含点 → 查全局表
        let local = HashMap::new(); // 当前文件无本地片段
        let global = global_frags();
        let sql = "SELECT <include refid=\"com.example.OtherMapper.sharedCols\"/> FROM users";
        assert_eq!(
            resolve_includes(sql, &local, Some(&global)).unwrap(),
            "SELECT id, name, email FROM users"
        );
    }

    #[test]
    fn cross_namespace_include_missing() {
        // 全局表也找不到的 namespace.id → 保留占位文本
        let local = HashMap::new();
        let global = global_frags();
        let sql = "<include refid=\"com.example.Ns.nonexistent\"/>";
        assert_eq!(
            resolve_includes(sql, &local, Some(&global)).unwrap(),
            "/* missing include: com.example.Ns.nonexistent */"
        );
    }

    #[test]
    fn local_takes_priority_over_global() {
        // refid 短 id（无点）只查本地，不查全局
        let mut local = HashMap::new();
        local.insert("cols".to_string(), "local_id".to_string());
        let mut global = HashMap::new();
        // 即使全局也有 "cols"（虽然实际不会，因全局 key 都是 namespace.id 形式），本地优先
        global.insert("cols".to_string(), "global_id".to_string());
        let sql = "<include refid=\"cols\"/>";
        assert_eq!(
            resolve_includes(sql, &local, Some(&global)).unwrap(),
            "local_id"
        );
    }

    #[test]
    fn short_id_not_looked_up_in_global() {
        // 短 id 即使在 global 中存在（理论上不会，因全局 key 都含点）也不查 global
        let local = HashMap::new();
        let global = global_frags();
        let sql = "<include refid=\"sharedCols\"/>";
        // sharedCols 无点 → 不查 global → 视为 missing
        assert_eq!(
            resolve_includes(sql, &local, Some(&global)).unwrap(),
            "/* missing include: sharedCols */"
        );
    }

    #[test]
    fn cross_namespace_recursive_include() {
        // 全局片段内含本地短 id 引用 → 仍能在本地解析
        let mut local = HashMap::new();
        local.insert("localCol".to_string(), "id".to_string());
        let mut global = HashMap::new();
        global.insert(
            "ns.outer".to_string(),
            "<include refid=\"localCol\"/> FROM users".to_string(),
        );
        let sql = "SELECT <include refid=\"ns.outer\"/>";
        assert_eq!(
            resolve_includes(sql, &local, Some(&global)).unwrap(),
            "SELECT id FROM users"
        );
    }
}
