//! MyBatis Mapper XML 解析与 SQL 提取。
//!
//! 流程：
//! 1. 第一遍：扫描所有 `<sql id="...">` 片段，存入 `HashMap<id, content>`
//! 2. 第二遍：扫描 `<select>/<insert>/<update>/<delete>` 标签：
//!    - 收集标签内文本内容（动态 SQL 标签如 `<if>/<where>/<foreach>` 剥离，
//!      仅保留其内部文本，由事件流自然实现，不用 regex）
//!    - 解析 `<include refid="..."/>`（同文件内）
//!    - 标准化 `#{}` / `${}` 占位符
//!    - 记录 `<select>` 标签在 XML 中的起始行号

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::SqlGuardError;

use super::include::resolve_includes;
use super::placeholder::normalize_placeholders;

/// 四类 SQL 语句标签。
const SQL_TAGS: &[&str] = &["select", "insert", "update", "delete"];

/// 提取出的一条 SQL 语句。
#[derive(Debug, Clone)]
#[allow(dead_code)] // statement_id / raw_sql 留给 P1 mapper 上下文注入
pub struct ExtractedSql {
    /// `<select id="xxx">` 的 id 属性值。
    pub statement_id: String,
    /// `"select"` / `"insert"` / `"update"` / `"delete"`。
    pub statement_type: String,
    /// `<select>` 标签在 XML 文件中的起始行号（1-indexed）。
    pub raw_xml_line: usize,
    /// 标签内原始文本（含占位符和已剥离的动态标签文本）。
    pub raw_sql: String,
    /// 经过 include 解析 + 占位符标准化后，可直接交给 sqlparser 的 SQL。
    pub processed_sql: String,
}

/// 从 Mapper XML 文件提取所有 SQL 语句。
pub fn extract_sql_from_xml(xml_path: &Path) -> Result<Vec<ExtractedSql>, SqlGuardError> {
    let content = std::fs::read_to_string(xml_path).map_err(|e| {
        SqlGuardError::MapperError(format!(
            "Failed to read mapper XML '{}': {}",
            xml_path.display(),
            e
        ))
    })?;

    let fragments = collect_sql_fragments(&content)?;
    extract_statements(&content, &fragments)
}

// ===== 内部实现 =====

/// 第一遍：收集所有 `<sql id="...">` 片段。
fn collect_sql_fragments(content: &str) -> Result<HashMap<String, String>, SqlGuardError> {
    let mut scanner = XmlScanner::new(content);
    let mut fragments = HashMap::new();

    while let Some(result) = scanner.next_event() {
        let (event, _line) = result?;
        if let Event::Start(e) = event {
            let name = lowercased_name(&e);
            if name == "sql" {
                if let Some(id) = extract_attr(e.attributes(), "id")? {
                    // 收集到匹配的 </sql>
                    let inner = scanner.collect_until_end("sql")?;
                    fragments.insert(id, inner);
                }
            }
        }
    }
    Ok(fragments)
}

/// 第二遍：扫描 SQL 标签并提取每条语句。
fn extract_statements(
    content: &str,
    fragments: &HashMap<String, String>,
) -> Result<Vec<ExtractedSql>, SqlGuardError> {
    let mut scanner = XmlScanner::new(content);
    let mut results = Vec::new();

    while let Some(result) = scanner.next_event() {
        let (event, line_before) = result?;
        if let Event::Start(e) = event {
            let name = lowercased_name(&e);
            if SQL_TAGS.contains(&name.as_str()) {
                let stmt_id = extract_attr(e.attributes(), "id")?.unwrap_or_default();
                let raw_sql = scanner.collect_until_end(&name)?;
                if raw_sql.trim().is_empty() {
                    continue;
                }
                let with_includes = resolve_includes(&raw_sql, fragments)?;
                let with_where = process_where_markers(&with_includes);
                let processed = normalize_placeholders(&with_where);
                results.push(ExtractedSql {
                    statement_id: stmt_id,
                    statement_type: name,
                    raw_xml_line: line_before,
                    raw_sql,
                    processed_sql: processed,
                });
            }
        }
    }
    Ok(results)
}

// ===== quick-xml 事件扫描器 =====
// 维护累计行号：每次 read_event 后按消耗的字节范围数 `\n`，
// next_event 返回 (Event, event 开始时的行号)。

struct XmlScanner<'a> {
    reader: Reader<&'a [u8]>,
    content: &'a str,
    buf: Vec<u8>,
    last_pos: usize,
    current_line: usize,
}

impl<'a> XmlScanner<'a> {
    fn new(content: &'a str) -> Self {
        let reader = Reader::from_str(content);
        // 注：quick-xml 0.31 默认 trim_text(false)，保留原始 text 事件，
        // 否则会丢失行号信息。
        Self {
            reader,
            content,
            buf: Vec::new(),
            last_pos: 0,
            current_line: 1,
        }
    }

    /// 读取下一个事件，返回 (event, event 开始时的行号)。Eof 返回 None。
    ///
    /// 使用 `into_owned()` 把 Event 转为 `'static`，避免借用 `self.buf`
    /// 导致后续无法 clear buf（这是 quick-xml `read_event_into` 的常见陷阱）。
    /// 单文件 mapper XML 性能可接受。
    fn next_event(&mut self) -> Option<Result<(quick_xml::events::Event<'static>, usize), SqlGuardError>> {
        let line_before = self.current_line;
        let event = self.reader.read_event_into(&mut self.buf);
        let cur_pos = self.reader.buffer_position().min(self.content.len());
        let bytes = self.content.as_bytes();
        for b in &bytes[self.last_pos..cur_pos] {
            if *b == b'\n' {
                self.current_line += 1;
            }
        }
        self.last_pos = cur_pos;
        match event {
            Ok(Event::Eof) => None,
            Ok(e) => {
                // 先把 Event 转为 owned（消耗 e，结束对 self.buf 的借用），
                // 然后 clear buf
                let owned = e.into_owned();
                self.buf.clear();
                Some(Ok((owned, line_before)))
            }
            Err(e) => Some(Err(SqlGuardError::MapperError(format!(
                "XML parse error: {}",
                e
            )))),
        }
    }

    /// 在当前 reader 位置（刚读完 Start 标签）开始，收集文本内容直到匹配的 End。
    /// - `<include refid="..."/>` 保留为原始标签文本，交给后续 [`resolve_includes`]
    /// - 其他子标签（动态 SQL 标签 `<if>/<where>/<foreach>` 等）剥离，仅保留其内部文本
    fn collect_until_end(&mut self, end_tag: &str) -> Result<String, SqlGuardError> {
        let end_bytes = end_tag.as_bytes();
        let mut out = String::new();
        while let Some(result) = self.next_event() {
            let (event, _line) = result?;
            match event {
                Event::Text(t) => {
                    let unescaped = t
                        .unescape()
                        .map_err(|e| SqlGuardError::MapperError(format!("XML text unescape error: {}", e)))?;
                    out.push_str(&unescaped);
                }
                Event::Empty(e) => {
                    // 保留 <include refid="..."/> 给后续 resolve_includes 处理；
                    // 其他空标签（如 <bind .../>）剥离
                    let name = lowercased_name(&e);
                    if name == "include" {
                        if let Some(refid) = extract_attr(e.attributes(), "refid")? {
                            out.push_str(&format!("<include refid=\"{}\"/>", refid));
                        }
                    }
                }
                Event::End(e) => {
                    if e.name().as_ref().eq_ignore_ascii_case(end_bytes) {
                        return Ok(out);
                    }
                }
                Event::Start(e) => {
                    // <where> 标签：MyBatis 会自动插入 WHERE 关键字并去掉首个 AND/OR，
                    // 这里先埋 marker，后续 process_where_markers 统一处理
                    if lowercased_name(&e) == "where" {
                        out.push_str("__WHERE__");
                    }
                    // 其他动态标签（if/foreach/set/trim 等）：剥离标签本身，内部文本保留
                }
                _ => {}
            }
        }
        Err(SqlGuardError::MapperError(format!(
            "Unexpected EOF while collecting <{}> content",
            end_tag
        )))
    }
}

fn lowercased_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    let name = e.name();
    let name_bytes = name.as_ref();
    String::from_utf8_lossy(name_bytes).to_ascii_lowercase()
}

fn extract_attr(
    attrs: quick_xml::events::attributes::Attributes<'_>,
    key: &str,
) -> Result<Option<String>, SqlGuardError> {
    for attr in attrs {
        let attr = attr.map_err(|e| {
            SqlGuardError::MapperError(format!("XML attribute parse error: {}", e))
        })?;
        if attr.key.as_ref().eq_ignore_ascii_case(key.as_bytes()) {
            let v = attr
                .unescape_value()
                .map_err(|e| SqlGuardError::MapperError(format!("XML attribute unescape error: {}", e)))?;
            return Ok(Some(v.into_owned()));
        }
    }
    Ok(None)
}

/// 将 `__WHERE__` marker 替换为 `WHERE `，并去除紧跟在后的第一个 `AND`/`OR`。
///
/// MyBatis 的 `<where>` 标签行为：
/// - 若内部内容非空，插入 `WHERE` 关键字
/// - 去除内容开头的 `AND` 或 `OR`（忽略前导空白）
fn process_where_markers(s: &str) -> String {
    const MARKER: &str = "__WHERE__";
    let mut result = String::with_capacity(s.len() + 16);
    let mut remaining = s;
    loop {
        match remaining.find(MARKER) {
            None => {
                result.push_str(remaining);
                break;
            }
            Some(pos) => {
                result.push_str(&remaining[..pos]);
                result.push_str("WHERE ");
                let after_marker = &remaining[pos + MARKER.len()..];
                let after_ws = after_marker.trim_start();
                // Strip leading AND or OR (case-insensitive, after optional whitespace)
                let upper_4: String = after_ws.chars().take(4).flat_map(|c| c.to_uppercase()).collect();
                let stripped = if upper_4.starts_with("AND ") {
                    Some(4)
                } else if upper_4.starts_with("OR ") {
                    Some(3)
                } else if after_ws.len() <= 3 && upper_4.trim_end().eq_ignore_ascii_case("AND") {
                    Some(after_ws.len())
                } else if after_ws.len() <= 2 && upper_4.trim_end().eq_ignore_ascii_case("OR") {
                    Some(after_ws.len())
                } else {
                    None
                };
                if let Some(skip) = stripped {
                    // Strip whitespace AND the AND/OR keyword
                    remaining = &after_ws[skip..];
                } else {
                    // No AND/OR to strip; whitespace already consumed by "WHERE " suffix
                    remaining = after_ws;
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_simple_select() {
        let xml = r#"<?xml version="1.0"?>
<mapper namespace="com.example.UserMapper">
  <select id="findById" resultType="User">
    SELECT id, name FROM users WHERE id = #{id}
  </select>
</mapper>"#;
        let dir = std::env::temp_dir().join("sqlguard_parser_test_1.xml");
        std::fs::write(&dir, xml).unwrap();
        let result = extract_sql_from_xml(&dir).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].statement_id, "findById");
        assert_eq!(result[0].statement_type, "select");
        assert_eq!(result[0].raw_xml_line, 3);
        assert!(result[0].processed_sql.contains("?"));
        assert!(!result[0].processed_sql.contains("#{"));
    }

    #[test]
    fn extract_all_statement_types() {
        let xml = r#"<mapper namespace="x">
  <select id="s">SELECT 1</select>
  <insert id="i">INSERT INTO t (a) VALUES (1)</insert>
  <update id="u">UPDATE t SET a = 1</update>
  <delete id="d">DELETE FROM t</delete>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_2.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].statement_type, "select");
        assert_eq!(result[1].statement_type, "insert");
        assert_eq!(result[2].statement_type, "update");
        assert_eq!(result[3].statement_type, "delete");
    }

    #[test]
    fn dynamic_tags_stripped() {
        let xml = r#"<mapper>
  <select id="findByCond">
    SELECT * FROM users
    <where>
      <if test="name != null">AND name LIKE #{name}</if>
    </where>
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_3.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 1);
        // 动态标签应被剥离，文本保留；<where> 会插入 WHERE 并去除首个 AND
        assert!(result[0].processed_sql.contains("WHERE"));
        assert!(result[0].processed_sql.contains("name LIKE ?"));
        assert!(!result[0].processed_sql.contains("AND name LIKE ?"));
        assert!(!result[0].processed_sql.contains("<if"));
        assert!(!result[0].processed_sql.contains("<where"));
    }

    #[test]
    fn include_resolved() {
        let xml = r#"<mapper>
  <sql id="BaseColumns">id, name, email</sql>
  <select id="findAll">
    SELECT <include refid="BaseColumns"/> FROM users
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_4.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].processed_sql.contains("id, name, email"));
        assert!(!result[0].processed_sql.contains("<include"));
    }

    #[test]
    fn where_tag_inserts_where_keyword() {
        let xml = r#"<mapper>
  <delete id="deleteByCond">
    DELETE FROM users
    <where>
      <if test="name != null">AND name LIKE #{name}</if>
    </where>
  </delete>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_where1.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 1);
        let sql = result[0].processed_sql.trim();
        // Must contain WHERE keyword (inserted by the fix)
        assert!(sql.contains("WHERE"), "processed_sql should contain WHERE: {}", sql);
        // First AND after WHERE should be stripped
        assert!(!sql.contains("WHERE AND"), "WHERE AND should not appear: {}", sql);
        assert!(!sql.contains("WHERE\nAND"), "WHERE\\nAND should not appear: {}", sql);
    }

    #[test]
    fn where_tag_with_include_preserves_where() {
        let xml = r#"<mapper>
  <sql id="cond">AND status = #{status}</sql>
  <delete id="deleteByInclude">
    DELETE FROM users
    <where>
      <include refid="cond"/>
    </where>
  </delete>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_where2.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 1);
        let sql = result[0].processed_sql.trim();
        assert!(sql.contains("WHERE"), "processed_sql should contain WHERE: {}", sql);
        // The AND from the include should be stripped
        assert!(!sql.contains("WHERE AND"), "WHERE AND should not appear: {}", sql);
        assert!(sql.contains("WHERE status"), "should have WHERE status: {}", sql);
    }

    #[test]
    fn where_tag_no_and_or() {
        let xml = r#"<mapper>
  <select id="findByStatus">
    SELECT * FROM users
    <where>
      status = #{status}
    </where>
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_where3.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 1);
        let sql = result[0].processed_sql.trim();
        assert!(sql.contains("WHERE"), "processed_sql should contain WHERE: {}", sql);
        assert!(sql.contains("status = ?"), "should have status = ?: {}", sql);
    }

    #[test]
    fn process_where_markers_strips_first_and() {
        assert_eq!(process_where_markers("__WHERE__ AND name = ?"), "WHERE name = ?");
        assert_eq!(process_where_markers("__WHERE__AND name = ?"), "WHERE name = ?");
        assert_eq!(process_where_markers("__WHERE__\n  AND name = ?"), "WHERE name = ?");
        assert_eq!(process_where_markers("__WHERE__ OR name = ?"), "WHERE name = ?");
        assert_eq!(process_where_markers("__WHERE__ name = ?"), "WHERE name = ?");
        assert_eq!(process_where_markers("__WHERE__ and name = ?"), "WHERE name = ?");
        assert_eq!(process_where_markers("__WHERE__"), "WHERE ");
    }

    #[test]
    fn dollar_placeholder_to_identifier() {
        let xml = r#"<mapper>
  <select id="dynamicTable">
    SELECT * FROM ${tableName} WHERE id = #{id}
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_5.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].processed_sql.contains("_var_tableName"));
        assert!(result[0].processed_sql.contains("?"));
    }
}
