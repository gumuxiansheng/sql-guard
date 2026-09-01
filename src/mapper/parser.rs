//! MyBatis Mapper XML 解析与 SQL 提取（静态检查侧）。
//!
//! ## 与 [`crate::mapper::dynamic`] 的关系
//! 本模块**不再自己剥离动态标签**，而是复用 [`dynamic`] 解析出的 [`DynNode`] 树，
//! 再用 [`dynamic::render_canonical`] 压成**一条**代表性 SQL 交给规则引擎。
//! 这样 `<where>`/`<set>`/`<trim>`/`<foreach>`/`<choose>` 的 MyBatis 语义
//! （补 WHERE/SET 关键字、去首个 AND/OR、去尾逗号、空体不输出 prefix、
//! `IN (...)` 括号）只有一份实现，静态检查与 `replay-export` 行为一致。
//!
//! 历史实现「剥离所有标签、只保留文本」会造成两类系统性误判：
//! - `<trim prefix="WHERE">` / `<trim prefix="set">` 被整体剥离 → UPDATE 看起来
//!   既没有 SET 也没有 WHERE → 误报 DML002（无 where 的 update）
//! - `<if>` 体里独立成行的 `and` 无法被剥离 → 渲染出 `WHERE and X = ?` → 解析失败
//!
//! ## 双渲染兜底
//! 主 SQL 用 [`RenderMode::AllTrue`]（所有 `<if>` 取真，覆盖面最大）。
//! 少数 mapper 把「外层 if 提供 `and`、内层多个 if 互斥提供操作数」写在一起，
//! 全取真会拼出语法错误，此时用 [`RenderMode::ExclusiveNested`] 的
//! [`ExtractedSql::processed_sql_alt`] 兜底（由调用方在主 SQL 解析失败时启用）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::SqlGuardError;

use super::dynamic::{self, DynNode, DynamicStatement, RenderMode};
use super::placeholder::normalize_placeholders;

/// 提取出的一条 SQL 语句。
#[derive(Debug, Clone)]
#[allow(dead_code)] // statement_id / raw_sql 留给 P1 mapper 上下文注入
pub struct ExtractedSql {
    /// `<select id="xxx">` 的 id 属性值。
    pub statement_id: String,
    /// `"select"` / `"insert"` / `"update"` / `"delete"`。
    pub statement_type: String,
    /// `<select>` 标签在 XML 文件中的起始行号（1-indexed）。
    ///
    /// **近似值说明**：违规行号由「渲染后 SQL 的行号 + `raw_xml_line - 1`」映射而来。
    /// 渲染会把 `<if>`/`<where>` 等动态标签本身占用的行删除、把分支文本拼入，
    /// 因此含**多行**动态标签的语句，其违规行号可能与 XML 实际行有 ± 几行偏差；
    /// 单个标签起始行（即本字段所指位置）附近是准确的。此误差只影响展示行号，
    /// 不影响违规判定本身。
    pub raw_xml_line: usize,
    /// 标签内原始文本（含占位符和已剥离的动态标签文本）。
    pub raw_sql: String,
    /// 经过 include 解析 + 动态标签渲染 + 占位符标准化后，可直接交给 sqlparser 的 SQL。
    pub processed_sql: String,
    /// 备用渲染（[`RenderMode::ExclusiveNested`]）。与主 SQL 相同时为 `None`。
    ///
    /// 仅在 [`Self::processed_sql`] 解析失败时启用，避免「互斥内层 if」写法被误报。
    pub processed_sql_alt: Option<String>,
    /// 第三兜底渲染（[`RenderMode::FirstBranch`]）：每个同层兄弟 `<if>` 组只取第一个。
    ///
    /// 接在 `processed_sql_alt` 之后作为最终兜底，专门吃掉「同层/嵌套的**互斥** `<if>`
    /// 被 AllTrue 全拼导致条件间缺 AND」的写法（如 `ibkcdeflg==0` 与 `ibkcdeflg==1`、
    /// `orgLv==0` 与 `orgLv!='0'`）。与主 SQL 相同时为 `None`。
    pub processed_sql_alt2: Option<String>,
    /// 是否包含 MyBatis `${}` 动态替换（运行时才确定内容，静态期不可解析）。
    ///
    /// 用于「信任跳过」：含此标志的语句解析失败时，不报误导性的 `PARSE` 错误，
    /// 改报诚实的 `DYN`（动态 substitution 未静态校验）警告，避免报告里出现一批
    /// "假"解析错误。检测基于渲染后的原文 `raw_sql` 是否含 `${`（与
    /// [`crate::mapper::placeholder::normalize_placeholders`] 的占位符判定一致）。
    pub has_dynamic: bool,
}

/// 把一条动态语句渲染为 [`ExtractedSql`]；渲染结果为空白时返回 `None`。
fn render_extracted(stmt: &DynamicStatement) -> Option<ExtractedSql> {
    let raw_sql = dynamic::render_canonical(&stmt.root_nodes, RenderMode::AllTrue);
    if raw_sql.trim().is_empty() {
        return None;
    }
    let processed_sql = normalize_placeholders(&raw_sql);
    let alt_raw = dynamic::render_canonical(&stmt.root_nodes, RenderMode::ExclusiveNested);
    let alt = normalize_placeholders(&alt_raw);
    let processed_sql_alt = if alt.trim().is_empty() || alt == processed_sql {
        None
    } else {
        Some(alt)
    };
    let alt2_raw = dynamic::render_canonical(&stmt.root_nodes, RenderMode::FirstBranch);
    let alt2 = normalize_placeholders(&alt2_raw);
    let processed_sql_alt2 = if alt2.trim().is_empty()
        || alt2 == processed_sql
        || Some(&alt2) == processed_sql_alt.as_ref()
    {
        None
    } else {
        Some(alt2)
    };
    // `${}` 动态替换在 `raw_sql` 中仍以字面 `${...}` 存在（占位符标准化尚未执行），
    // 与 `normalize_placeholders` 的判定口径一致。
    let has_dynamic = raw_sql.contains("${");
    Some(ExtractedSql {
        statement_id: stmt.statement_id.clone(),
        statement_type: stmt.statement_type.clone(),
        raw_xml_line: stmt.raw_xml_line,
        raw_sql,
        processed_sql,
        processed_sql_alt,
        processed_sql_alt2,
        has_dynamic,
    })
}

/// 从单个 Mapper XML 文件提取所有 SQL 语句（仅同文件内 include 解析）。
///
/// 跨 namespace 引用需使用 [`extract_sql_from_xmls`] 批量提取，构建全局片段表。
/// `encoding` 是读取 XML 文件所用的编码标签（`[scan] encoding` / `--encoding`）。
pub fn extract_sql_from_xml(
    xml_path: &Path,
    encoding: &str,
) -> Result<Vec<ExtractedSql>, SqlGuardError> {
    let content = crate::encoding::read_to_string(xml_path, encoding)
        .map_err(|e| SqlGuardError::MapperError(format!("Failed to read mapper XML: {}", e)))?;

    let stmts = dynamic::parse_dynamic_statements_from_content(&content, None)?;
    Ok(stmts.iter().filter_map(render_extracted).collect())
}

/// 从多个 Mapper XML 文件批量提取 SQL，构建跨 namespace 全局片段表。
///
/// 两阶段流程：
/// 1. **收集阶段**：扫描所有文件的 `<mapper namespace="...">` 与 `<sql id="...">`，
///    把每个片段注册到全局表，key 为 `namespace.id`（如 `com.example.UserMapper.cols`）。
/// 2. **提取阶段**：逐文件提取 `<select>/<insert>/<update>/<delete>`，
///    解析 `<include refid="..."/>` 时优先查本地片段，未命中且 refid 含点时查全局表。
///
/// 返回 `Vec<(PathBuf, Vec<ExtractedSql>)>`，保持输入顺序。
///
/// # 跨 namespace 引用规则
/// - `<include refid="cols"/>`（无点）→ 仅查当前文件本地片段
/// - `<include refid="com.example.UserMapper.cols"/>`（含点）→ 查全局表
///
/// ★ D2：当前 main.rs 仅用单文件版 `extract_sql_from_xml`，此批量版是跨 namespace
/// include 解析的核心扩展能力，保留用于未来多文件批量处理场景。
/// `encoding` 是读取 XML 文件所用的编码标签（`[scan] encoding` / `--encoding`）。
#[allow(dead_code)]
pub fn extract_sql_from_xmls(
    xml_paths: &[PathBuf],
    encoding: &str,
) -> Result<Vec<(PathBuf, Vec<ExtractedSql>)>, SqlGuardError> {
    // 阶段 1：收集所有文件的 namespace + 本地片段，注册到全局表
    let mut global: HashMap<String, Vec<DynNode>> = HashMap::new();
    let mut per_file: Vec<(PathBuf, String)> = Vec::new();

    for path in xml_paths {
        let content = crate::encoding::read_to_string(path, encoding)
            .map_err(|e| SqlGuardError::MapperError(format!("Failed to read mapper XML: {}", e)))?;

        let namespace = extract_mapper_namespace(&content);
        let local_frags = dynamic::collect_fragments_from_content(&content)?;

        // 注册到全局表：每个片段同时按 `namespace.id` 注册
        // （namespace 缺失时跳过全局注册，仍可作为本地片段使用）
        if let Some(ref ns) = namespace {
            for (id, nodes) in &local_frags {
                global.insert(format!("{}.{}", ns, id), nodes.clone());
            }
        }

        per_file.push((path.clone(), content));
    }

    // 阶段 2：逐文件提取，传入全局片段表
    let mut results = Vec::with_capacity(per_file.len());
    for (path, content) in per_file {
        let stmts = dynamic::parse_dynamic_statements_from_content(&content, Some(&global))?;
        results.push((path, stmts.iter().filter_map(render_extracted).collect()));
    }
    Ok(results)
}

/// 提取 `<mapper namespace="...">` 的 namespace 属性值。
///
/// 扫描第一个 `mapper` 开始标签的 `namespace` 属性。
/// 缺失或解析失败时返回 `None`（向后兼容无 namespace 的 mapper）。
fn extract_mapper_namespace(content: &str) -> Option<String> {
    let mut scanner = XmlScanner::new(content);
    while let Some(result) = scanner.next_event() {
        let (event, _line) = match result {
            Ok(r) => r,
            Err(_) => return None,
        };
        if let Event::Start(e) = event {
            let name = lowercased_name(&e);
            if name == "mapper" {
                if let Ok(Some(ns)) = extract_attr(e.attributes(), "namespace") {
                    return Some(ns);
                }
                return None;
            }
        }
    }
    None
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
    fn next_event(
        &mut self,
    ) -> Option<Result<(quick_xml::events::Event<'static>, usize), SqlGuardError>> {
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
        let attr = attr
            .map_err(|e| SqlGuardError::MapperError(format!("XML attribute parse error: {}", e)))?;
        if attr.key.as_ref().eq_ignore_ascii_case(key.as_bytes()) {
            let v = attr.unescape_value().map_err(|e| {
                SqlGuardError::MapperError(format!("XML attribute unescape error: {}", e))
            })?;
            return Ok(Some(v.into_owned()));
        }
    }
    Ok(None)
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
        let result = extract_sql_from_xml(&dir, "utf-8").unwrap();
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        assert_eq!(result.len(), 1);
        let sql = result[0].processed_sql.trim();
        // Must contain WHERE keyword (inserted by the fix)
        assert!(
            sql.contains("WHERE"),
            "processed_sql should contain WHERE: {}",
            sql
        );
        // First AND after WHERE should be stripped
        assert!(
            !sql.contains("WHERE AND"),
            "WHERE AND should not appear: {}",
            sql
        );
        assert!(
            !sql.contains("WHERE\nAND"),
            "WHERE\\nAND should not appear: {}",
            sql
        );
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        assert_eq!(result.len(), 1);
        let sql = result[0].processed_sql.trim();
        assert!(
            sql.contains("WHERE"),
            "processed_sql should contain WHERE: {}",
            sql
        );
        // The AND from the include should be stripped
        assert!(
            !sql.contains("WHERE AND"),
            "WHERE AND should not appear: {}",
            sql
        );
        assert!(
            sql.contains("WHERE status"),
            "should have WHERE status: {}",
            sql
        );
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        assert_eq!(result.len(), 1);
        let sql = result[0].processed_sql.trim();
        assert!(
            sql.contains("WHERE"),
            "processed_sql should contain WHERE: {}",
            sql
        );
        assert!(
            sql.contains("status = ?"),
            "should have status = ?: {}",
            sql
        );
    }

    /// 提取并压成单行，便于断言。
    fn one_line(xml: &str, name: &str) -> String {
        let path = std::env::temp_dir().join(format!("sqlguard_parser_{}.xml", name));
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        assert_eq!(result.len(), 1, "expect exactly 1 statement");
        result[0]
            .processed_sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 回归：`<if>` 体里 `and` 独占一行时也必须被 `<where>` 剥掉。
    /// 旧实现只认 `"AND "`（严格跟空格），`and\n` 残留会渲染出 `WHERE and X = ?`。
    #[test]
    fn where_strips_and_on_its_own_line() {
        let sql = one_line(
            r#"<mapper>
  <select id="q">
    SELECT * FROM t
    <where>
      <if test="a != null">
        and
        A = #{a}
      </if>
    </where>
  </select>
</mapper>"#,
            "and_newline",
        );
        assert_eq!(sql, "SELECT * FROM t WHERE A = ?", "got: {}", sql);
    }

    /// 回归（用户问题 1/2）：`<trim prefix="WHERE" prefixOverrides="AND |OR ">`
    /// 等价于 `<where>`，旧实现整体剥离导致 UPDATE 看起来没有 WHERE → 误报 DML002。
    #[test]
    fn trim_acts_as_where_and_set() {
        let sql = one_line(
            r#"<mapper>
  <update id="u">
    UPDATE t
    <trim prefix="set" suffixOverrides=",">
      <if test="a != null">A = #{a},</if>
      <if test="b != null">B = #{b},</if>
    </trim>
    <trim prefix="WHERE" prefixOverrides="AND |OR ">
      <if test="id != null">AND ID = #{id}</if>
    </trim>
  </update>
</mapper>"#,
            "trim_where_set",
        );
        assert_eq!(
            sql, "UPDATE t set A = ?, B = ? WHERE ID = ?",
            "got: {}",
            sql
        );
    }

    /// `<trim>` 内容为空时整个标签不输出（含 prefix），否则会渲染出 `UPDATE t set WHERE`。
    #[test]
    fn empty_trim_emits_nothing() {
        let sql = one_line(
            r#"<mapper>
  <delete id="d">
    DELETE FROM t
    <trim prefix="WHERE" prefixOverrides="AND |OR "></trim>
  </delete>
</mapper>"#,
            "trim_empty",
        );
        assert_eq!(sql, "DELETE FROM t", "got: {}", sql);
    }

    /// `<foreach>` 必须渲染出 open/close，否则 `IN` 后面空空如也。
    #[test]
    fn foreach_renders_in_list() {
        let sql = one_line(
            r#"<mapper>
  <select id="q">
    SELECT * FROM t WHERE id IN
    <foreach collection="ids" item="i" open="(" close=")" separator=",">#{i}</foreach>
  </select>
</mapper>"#,
            "foreach_in",
        );
        assert_eq!(sql, "SELECT * FROM t WHERE id IN (?)", "got: {}", sql);
    }

    /// `<![CDATA[ ]]>` 内容不能被丢弃（mapper 常用它包 `<=` / `>=`）。
    #[test]
    fn cdata_content_preserved() {
        let sql = one_line(
            r#"<mapper>
  <select id="q">
    SELECT * FROM t WHERE d <![CDATA[ >= ]]> #{d}
  </select>
</mapper>"#,
            "cdata",
        );
        assert_eq!(sql, "SELECT * FROM t WHERE d >= ?", "got: {}", sql);
    }

    /// `<choose>` 只命中一个分支，全拼会造出语法错误。
    #[test]
    fn choose_takes_first_when() {
        let sql = one_line(
            r#"<mapper>
  <select id="q">
    SELECT * FROM t ORDER BY
    <choose>
      <when test="x == 1">A</when>
      <when test="x == 2">B</when>
      <otherwise>C</otherwise>
    </choose>
  </select>
</mapper>"#,
            "choose",
        );
        assert_eq!(sql, "SELECT * FROM t ORDER BY A", "got: {}", sql);
    }

    /// `<selectKey>` 是独立语句，不能被拼进外层 INSERT。
    #[test]
    fn select_key_is_dropped() {
        let sql = one_line(
            r#"<mapper>
  <insert id="i">
    <selectKey keyProperty="id" resultType="long" order="BEFORE">
      SELECT SEQ_T.NEXTVAL FROM DUAL
    </selectKey>
    INSERT INTO t (id, a) VALUES (#{id}, #{a})
  </insert>
</mapper>"#,
            "select_key",
        );
        assert_eq!(sql, "INSERT INTO t (id, a) VALUES (?, ?)", "got: {}", sql);
    }

    /// 「外层 if 提供 and，内层多个 if 互斥提供操作数」→ 需要 alt 渲染兜底。
    #[test]
    fn exclusive_nested_alt_is_provided() {
        let xml = r#"<mapper>
  <select id="q">
    SELECT * FROM t
    <where>
      <if test="orgno != null"> and
        <if test="c != '1'"> SUPORGNO = #{orgno} </if>
        <if test="c != '0'"> ORGNO = #{orgno} </if>
      </if>
    </where>
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_excl_nested.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        let alt = result[0]
            .processed_sql_alt
            .as_ref()
            .expect("alt render should exist")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(alt, "SELECT * FROM t WHERE SUPORGNO = ?", "got: {}", alt);
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
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].processed_sql.contains("_var_tableName"));
        assert!(result[0].processed_sql.contains("?"));
    }

    #[test]
    fn has_dynamic_flag_set_for_dollar_substitution() {
        // 含 `${}` 动态替换的语句应被标记 `has_dynamic = true`（信任跳过依据）。
        let xml = r#"<mapper>
  <select id="dynamicTable">
    SELECT * FROM ${tableName} WHERE id = #{id}
  </select>
  <select id="staticSql">
    SELECT id, name FROM users WHERE id = #{id}
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_test_6.xml");
        std::fs::write(&path, xml).unwrap();
        let result = extract_sql_from_xml(&path, "utf-8").unwrap();
        assert_eq!(result.len(), 2);
        let dynamic = result
            .iter()
            .find(|s| s.statement_id == "dynamicTable")
            .unwrap();
        let static_sql = result
            .iter()
            .find(|s| s.statement_id == "staticSql")
            .unwrap();
        assert!(
            dynamic.has_dynamic,
            "statement with ${{}} must be flagged dynamic"
        );
        assert!(
            !static_sql.has_dynamic,
            "static statement must not be flagged dynamic"
        );
    }

    #[test]
    fn debug_finwhitelist_insert() {
        // 临时调试测试：FinwhitelistEntity.xml 的 INSERT 列列表含子查询
        // `(select t.name from G_SYS_trade t ...) AS INDUSTRY_CODE_NAME`，
        // sqlparser 不支持 INSERT 列列表中嵌套子查询表达式 → PARSE_ERROR。
        // 这是 mapper XML 本身的非标准 SQL 写法，不是 sqlguard 的 bug。
        // 保留此测试作为回归验证：确保该语句始终走 PARSE 路径而非崩溃。
        let content = match std::fs::read_to_string(
            "examples/mapper/mapper-full/mapper/FinwhitelistEntity.xml",
        ) {
            Ok(c) => c,
            Err(_) => {
                return;
            } // 文件不存在时静默跳过
        };
        let fragments = crate::mapper::dynamic::collect_fragments_from_content(&content).unwrap();
        let stmts = crate::mapper::dynamic::parse_dynamic_statements_from_content(
            &content,
            Some(&fragments),
        )
        .unwrap();
        let insert = stmts.iter().find(|s| s.statement_id == "insert");
        assert!(insert.is_some(), "insert statement should exist");
        if let Some(s) = insert {
            let rendered = crate::mapper::dynamic::render_canonical(
                &s.root_nodes,
                crate::mapper::dynamic::RenderMode::AllTrue,
            );
            let normalized = crate::mapper::placeholder::normalize_placeholders(&rendered);
            // INSERT 列列表中的子查询会导致 PG 解析失败，这是预期行为
            let ast = crate::rule::engine::parser::parse_sql_to_ast_fb(
                &normalized,
                crate::config::CheckDialect::GaussDB,
                Some(crate::config::CheckDialect::Oracle),
            );
            assert!(
                ast.statements.iter().any(|st| st.kind == "PARSE_ERROR"),
                "INSERT with subquery in column list should be PARSE_ERROR"
            );
        }
    }

    /// 安全回归（P3）：DTD / 外部实体不得被解析或展开。
    ///
    /// quick-xml 是非验证解析器，不解析 DTD 内部子集、不加载外部实体——
    /// `<!ENTITY xxe SYSTEM "file:///...">` 只会作为 DocType 事件被忽略，
    /// 文本中的 `&xxe;` 引用不会有任何文件被读取。
    ///
    /// 断言语义：
    /// - DOCTYPE（含 SYSTEM 外部实体声明）不应中断正常语句提取；
    /// - 「DTD 声明实体 + 文本引用该实体」的经典 XXE 攻击输入，结果要么
    ///   解析报错（攻击失败），要么内容中绝不含外部文件特征（未展开）。
    #[test]
    fn xxe_doctype_and_entity_are_safe() {
        // 1) 简单外部 DTD 引用：被忽略，SQL 正常提取
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE mapper SYSTEM "file:///etc/evil.dtd">
<mapper namespace="com.example.UserMapper">
  <select id="q">
    SELECT 1
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_xxe_doctype.xml");
        std::fs::write(&path, xml).unwrap();
        let stmts = extract_sql_from_xml(&path, "utf-8")
            .expect("DOCTYPE must be ignored, SQL should still extract");
        assert_eq!(stmts.len(), 1);
        let _ = std::fs::remove_file(&path);

        // 2) 经典 XXE：DOCTYPE 内部子集声明实体 + SQL 文本中引用该实体。
        //    无论 quick-xml 对内部子集支持程度如何，攻击都必须失败：
        //    Err（解析报错）或 Ok 但内容不含外部文件特征。
        let attack = r#"<?xml version="1.0"?>
<!DOCTYPE mapper [
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<mapper>
  <select id="q">
    &xxe; SELECT 1
  </select>
</mapper>"#;
        let path = std::env::temp_dir().join("sqlguard_parser_xxe_attack.xml");
        std::fs::write(&path, attack).unwrap();
        match extract_sql_from_xml(&path, "utf-8") {
            Err(_) => {} // 解析报错 = 攻击失败，可接受
            Ok(stmts) => {
                for s in &stmts {
                    assert!(
                        !s.processed_sql.contains("root:")
                            && !s.processed_sql.contains("/etc/passwd"),
                        "external entity must never expand to file content: {}",
                        s.processed_sql
                    );
                }
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}
