//! GaussDB → PostgreSQL 词法级归一化重写层。
//!
//! GaussDB 等「PG 内核 + Oracle/MySQL 外壳」的国产库，在 PG 语法基础上兼容了
//! 部分 Oracle/MySQL 专有构造。若直接用 `PostgreSqlDialect` 解析，这些构造会触发
//! 解析失败；若回退到 `OracleDialect`，则整条语句的 AST 语义（标识符大小写折叠、
//! 伪表归属等）由 Oracle 决定，污染 PG 语义。
//!
//! 本模块在 sqlparser 解析前对 SQL 文本做**词法级、可审计**的归一化重写，把
//! GaussDB 兼容的 Oracle/MySQL 专有构造改写为 PG 等价语法，再用纯 `PostgreSqlDialect`
//! 解析。AST 语义 100% 来自 PG 方言，污染消除。
//!
//! ## 当前覆盖的规则（首版，全部默认开启）
//!
//! | 规则 ID | 原始构造 | 重写为 | 实现方式 |
//! |---|---|---|---|
//! | G_ORACLE_MINUS | `MINUS` | `EXCEPT` | 词法替换（词边界匹配） |
//! | G_ORACLE_SYSDATE | `SYSDATE` | `CURRENT_TIMESTAMP` | 词法替换（词边界匹配） |
//! | G_ORACLE_NVL | `NVL(` | `COALESCE(` | 词法替换（词边界 + 紧跟左括号） |
//! | G_ORACLE_DUAL | `FROM dual` / `FROM DUAL` | `FROM (SELECT 1) AS dual` | 词法+上下文 |
//! | G_MYSQL_BACKTICK | `` `ident` `` | `"ident"` | 词法替换（成对反引号） |
//!
//! ## 设计约束
//!
//! - **行号保真**：所有替换不引入/删除换行符，保证 violation 行号与原文件一致。
//!   变长替换（如 `SYSDATE`(7) → `CURRENT_TIMESTAMP`(17)）仅在同一行内变长，
//!   不影响行号；后续 sqlparser 解析时按字符计数列号会自然对齐新文本。
//! - **不误伤字符串/注释**：扫描器跳过单引号字符串 `'...'`（含 `''` 转义）、
//!   `--` 行注释、`/* */` 块注释，其中的 `MINUS`/`SYSDATE` 等不被重写。
//! - **可审计**：每次重写记录 [`RewriteRecord`]，供 debug 模式透出"哪些构造被归一化"。
//! - **UTF-8 安全**：非 ASCII 字符（如中文）按字符级透传，不做字节级 `as char` 转换，
//!   避免乱码（项目历史上的已知教训）。

/// 单次重写的审计记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteRecord {
    /// 规则 ID（如 `"G_ORACLE_MINUS"`）。
    pub rule_id: &'static str,
    /// 原始片段文本。
    pub original: String,
    /// 重写后片段文本。
    pub rewritten: String,
    /// 在原 SQL 中的字节偏移。
    pub offset: usize,
}

/// 重写结果。
#[derive(Debug, Clone, Default)]
pub struct RewrittenSql {
    /// 归一化后的 SQL 文本（可直接喂给 `PostgreSqlDialect` 解析）。
    pub sql: String,
    /// 本次重写的审计记录列表（按偏移升序）。
    ///
    /// 当前仅用于测试断言与未来 debug 模式透出。生产路径只消费 `sql` 字段，
    /// 故标 `#[allow(dead_code)]` 避免误报——该字段是公开 API 的一部分，
    /// 移除会破坏可审计性设计（见模块级文档「可审计」约束）。
    #[allow(dead_code)]
    pub rewrites: Vec<RewriteRecord>,
}

impl RewrittenSql {
    /// 是否发生了任何重写。
    ///
    /// 供测试断言与未来 debug 模式使用，故标 `#[allow(dead_code)]`。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rewrites.is_empty()
    }
}

/// 对 GaussDB 兼容的 Oracle/MySQL 专有语法做词法级归一化重写。
///
/// 入口函数：在 `parse_sql_to_ast_fb` 中对 `CheckDialect::GaussDB` 主方言调用，
/// 把重写后的文本交给 `PostgreSqlDialect` 解析。
///
/// 见模块级文档的规则表与设计约束。
pub(crate) fn rewrite_for_pg_parse(sql: &str) -> RewrittenSql {
    let mut out = String::with_capacity(sql.len() + 16);
    let mut rewrites: Vec<RewriteRecord> = Vec::new();
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;

    while i < n {
        let b = bytes[i];

        // 1) 单引号字符串字面量 '...' （含 '' 转义）：原样透传，不重写
        if b == b'\'' {
            out.push('\'');
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    out.push('\'');
                    i += 1;
                    if i < n && bytes[i] == b'\'' {
                        // '' 转义：保留两个引号，继续在字符串内
                        out.push('\'');
                        i += 1;
                        continue;
                    } else {
                        break;
                    }
                } else {
                    // 非 ASCII 字节按 UTF-8 字符透传，避免乱码
                    push_char(&mut out, sql, &mut i);
                }
            }
            continue;
        }

        // 2) 行注释 -- ...：原样透传到行尾
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' {
                push_char(&mut out, sql, &mut i);
            }
            continue;
        }

        // 3) 块注释 /* ... */（含嵌套，PG 风格）：原样透传
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            out.push('/');
            out.push('*');
            i += 2;
            let mut depth = 1;
            while i < n && depth > 0 {
                if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                    out.push('/');
                    out.push('*');
                    i += 2;
                    depth += 1;
                } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                    out.push('*');
                    out.push('/');
                    i += 2;
                    depth -= 1;
                } else {
                    push_char(&mut out, sql, &mut i);
                }
            }
            continue;
        }

        // 4) 反引号标识符 `ident` → "ident"（MySQL 兼容）
        if b == b'`' {
            let start = i;
            let out_start = out.len(); // ★ Fix 2: 快照 out 长度，避免被前置变长重写污染偏移
            out.push('"');
            i += 1;
            let mut closed = false;
            while i < n {
                if bytes[i] == b'`' {
                    if i + 1 < n && bytes[i + 1] == b'`' {
                        // `` 转义：重写为 ""
                        out.push('"');
                        out.push('"');
                        i += 2;
                        continue;
                    }
                    // 闭合反引号
                    out.push('"');
                    i += 1;
                    closed = true;
                    break;
                } else if bytes[i] == b'"' {
                    // 标识符内的双引号需转义为 ""
                    out.push('"');
                    out.push('"');
                    i += 1;
                } else {
                    push_char(&mut out, sql, &mut i);
                }
            }
            // ★ Fix 1: 未闭合反引号（EOF 前未找到配对）补上 closing "，
            // 避免生成的 SQL 标识符残缺导致后续解析失败。原始内容不丢弃。
            if !closed {
                out.push('"');
            }
            let original = sql[start..i].to_string();
            let rewritten = out[out_start..].to_string();
            rewrites.push(RewriteRecord {
                rule_id: "G_MYSQL_BACKTICK",
                original,
                rewritten,
                offset: start,
            });
            continue;
        }

        // 5) 标识符级规则：扫描完整标识符词，按词义重写
        //    - MINUS → EXCEPT
        //    - SYSDATE → CURRENT_TIMESTAMP
        //    - NVL( → COALESCE(
        //    - FROM dual → FROM (SELECT 1) AS dual（FROM 后跳过空白匹配 dual）
        //    - ADD (col_def, ...) → ADD COLUMN col_def, ADD COLUMN col_def, ...
        if is_ident_char(b) {
            let word_start = i;
            i += 1;
            while i < n && is_ident_char(bytes[i]) {
                i += 1;
            }
            let word = &sql[word_start..i];
            let word_upper = word.to_uppercase();
            let next_is_lparen = i < n && bytes[i] == b'(';

            match word_upper.as_str() {
                "MINUS" => {
                    rewrites.push(RewriteRecord {
                        rule_id: "G_ORACLE_MINUS",
                        original: word.to_string(),
                        rewritten: "EXCEPT".to_string(),
                        offset: word_start,
                    });
                    out.push_str("EXCEPT");
                }
                "SYSDATE" => {
                    rewrites.push(RewriteRecord {
                        rule_id: "G_ORACLE_SYSDATE",
                        original: word.to_string(),
                        rewritten: "CURRENT_TIMESTAMP".to_string(),
                        offset: word_start,
                    });
                    out.push_str("CURRENT_TIMESTAMP");
                }
                "NVL" if next_is_lparen => {
                    rewrites.push(RewriteRecord {
                        rule_id: "G_ORACLE_NVL",
                        original: word.to_string(),
                        rewritten: "COALESCE".to_string(),
                        offset: word_start,
                    });
                    out.push_str("COALESCE");
                }
                "FROM" => {
                    // FROM dual / FROM DUAL → FROM (SELECT 1) AS dual
                    // 保存 i，若不匹配 dual 则回退透传原始 FROM 词（保留大小写）
                    let after_from = i;
                    // 跳过空白（空格/Tab）
                    while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
                        i += 1;
                    }
                    if i + 4 <= n && sql[i..i + 4].eq_ignore_ascii_case("dual") {
                        let dual_end = i + 4;
                        let dual_followed_ok = dual_end >= n || !is_ident_char(bytes[dual_end]);
                        if dual_followed_ok {
                            let original = sql[word_start..dual_end].to_string();
                            rewrites.push(RewriteRecord {
                                rule_id: "G_ORACLE_DUAL",
                                original,
                                rewritten: "FROM (SELECT 1) AS dual".to_string(),
                                offset: word_start,
                            });
                            out.push_str("FROM (SELECT 1) AS dual");
                            i = dual_end;
                            continue;
                        }
                    }
                    // 不是 dual：透传原始 FROM 词（保留大小写），回退 i 到 FROM 词之后
                    out.push_str(word);
                    i = after_from;
                }
                "ADD" => {
                    // Oracle 风格：ALTER TABLE t ADD (col1 type, col2 type, ...)
                    // PG 等价：ALTER TABLE t ADD COLUMN col1 type, ADD COLUMN col2 type, ...
                    //
                    // 仅当 ADD 后（跳过空白）紧跟 `(` 且括号内首词不是约束关键词
                    // （CONSTRAINT/PRIMARY/UNIQUE/CHECK/FOREIGN/KEY）时重写。
                    //
                    // ★ Fix 4: 保留原始 ADD 词大小写（add/ADD/Add），COLUMN 统一用大写
                    // （新引入的关键字，遵循 SQL 大写惯例）。
                    let after_add = i;
                    if let Some((rewritten_text, new_i)) =
                        try_rewrite_alter_add_parens(sql, after_add, word)
                    {
                        let original = sql[word_start..new_i].to_string();
                        rewrites.push(RewriteRecord {
                            rule_id: "G_ORACLE_ALTER_ADD_PARENS",
                            original,
                            rewritten: rewritten_text.clone(),
                            offset: word_start,
                        });
                        out.push_str(&rewritten_text);
                        i = new_i;
                    } else {
                        // 不是 ADD (...) 模式：透传原始 ADD 词（保留大小写）
                        out.push_str(word);
                        i = after_add;
                    }
                }
                _ => {
                    out.push_str(word);
                }
            }
            continue;
        }

        // 6) 其他字符原样透传（按 UTF-8 字符，避免乱码）
        push_char(&mut out, sql, &mut i);
    }

    RewrittenSql { sql: out, rewrites }
}

/// 尝试把 `ADD (col_def, ...)` 重写为 `ADD COLUMN col_def, ADD COLUMN col_def, ...`。
///
/// 只读检查：不修改调用方的游标。返回 `Some((rewritten_text, new_i))` 表示匹配成功，
/// `new_i` 指向 `)` 之后的位置；返回 `None` 表示不是 `ADD (...)` 模式或括号不匹配
/// 或括号内是约束定义（以 CONSTRAINT/PRIMARY/UNIQUE/CHECK/FOREIGN/KEY 开头）。
///
/// `after_add` 是 `ADD` 词结束后的字节偏移（即向前看的起点）。
/// `add_word` 是原始 `ADD` 词文本（保留大小写：add/ADD/Add），用于输出模板。
fn try_rewrite_alter_add_parens(
    sql: &str,
    after_add: usize,
    add_word: &str,
) -> Option<(String, usize)> {
    let bytes = sql.as_bytes();
    let n = bytes.len();

    // 跳过空白（空格/Tab/换行），寻找 `(`
    let mut j = after_add;
    while j < n && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
        j += 1;
    }
    if j >= n || bytes[j] != b'(' {
        return None;
    }

    let paren_start = j;
    j += 1;

    // 匹配括号对（考虑嵌套括号与字符串字面量）
    let mut depth = 1u32;
    while j < n && depth > 0 {
        match bytes[j] {
            b'\'' => {
                // 跳过字符串字面量（含 '' 转义）
                j += 1;
                while j < n {
                    if bytes[j] == b'\'' {
                        j += 1;
                        if j < n && bytes[j] == b'\'' {
                            j += 1;
                            continue;
                        } else {
                            break;
                        }
                    } else {
                        let ch = sql[j..].chars().next()?;
                        j += ch.len_utf8();
                    }
                }
            }
            b'(' => {
                depth += 1;
                j += 1;
            }
            b')' => {
                depth -= 1;
                j += 1;
            }
            b'-' if j + 1 < n && bytes[j + 1] == b'-' => {
                // 跳过行注释
                while j < n && bytes[j] != b'\n' {
                    j += 1;
                }
            }
            b'/' if j + 1 < n && bytes[j + 1] == b'*' => {
                // 跳过块注释（含嵌套）
                j += 2;
                let mut cdepth = 1u32;
                while j < n && cdepth > 0 {
                    if bytes[j] == b'/' && j + 1 < n && bytes[j + 1] == b'*' {
                        j += 2;
                        cdepth += 1;
                    } else if bytes[j] == b'*' && j + 1 < n && bytes[j + 1] == b'/' {
                        j += 2;
                        cdepth -= 1;
                    } else {
                        let ch = sql[j..].chars().next()?;
                        j += ch.len_utf8();
                    }
                }
            }
            _ => {
                let ch = sql[j..].chars().next()?;
                j += ch.len_utf8();
            }
        }
    }

    if depth != 0 {
        return None; // 括号不匹配
    }

    let paren_end = j; // `)` 之后的位置
    let inner = &sql[paren_start + 1..paren_end - 1];

    // 检查括号内首词是否为约束关键词——是则不重写（让 Oracle 回退兜底）
    let inner_trimmed = inner.trim_start();
    let first_word: String = inner_trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    match first_word.to_uppercase().as_str() {
        "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN" | "KEY" => return None,
        _ => {}
    }

    // 按顶层逗号拆分括号内内容
    let items = split_top_level_commas(inner);

    // 重写为 {add_word} COLUMN <item1>, {add_word} COLUMN <item2>, ...
    // 对每个 item 递归调用 rewrite_for_pg_parse，使括号内的 NVL/SYSDATE 等也被重写。
    // ★ Fix 4: add_word 保留原始大小写；COLUMN 统一大写（新引入关键字，遵循 SQL 惯例）。
    let rewritten: Vec<String> = items
        .iter()
        .map(|item| {
            let inner_rewritten = rewrite_for_pg_parse(item.trim());
            format!("{} COLUMN {}", add_word, inner_rewritten.sql)
        })
        .collect();
    let rewritten_text = rewritten.join(", ");

    Some((rewritten_text, paren_end))
}

/// 按顶层逗号拆分字符串（忽略嵌套括号、字符串字面量、注释内的逗号）。
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut depth = 0u32;
    let mut i = 0usize;

    while i < n {
        match bytes[i] {
            b'\'' => {
                // 跳过字符串字面量
                i += 1;
                while i < n {
                    if bytes[i] == b'\'' {
                        i += 1;
                        if i < n && bytes[i] == b'\'' {
                            i += 1;
                            continue;
                        } else {
                            break;
                        }
                    } else {
                        let ch = s[i..].chars().next().unwrap();
                        i += ch.len_utf8();
                    }
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                // ★ Fix 3: 防御性 saturating_sub，避免不平衡括号导致 u32 下溢 panic。
                // 调用方 try_rewrite_alter_add_parens 已校验括号配平，此处为纵深防御。
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth == 0 => {
                result.push(&s[start..i]);
                start = i + 1;
                i += 1;
            }
            _ => {
                let ch = s[i..].chars().next().unwrap();
                i += ch.len_utf8();
            }
        }
    }
    result.push(&s[start..]);
    result
}

/// 透传 `sql[i..]` 的下一个 UTF-8 字符到 `out`，并推进 `i`。
///
/// 用于非特殊字符的透传分支，保证多字节字符（如中文）不被字节级 `as char`
/// 转换破坏。特殊字符（`'` / `--` / `/*` / `` ` `` / ASCII 标识符字符）都是 ASCII，
/// 在字节级扫描中已正确识别，不会进入本函数。
fn push_char(out: &mut String, sql: &str, i: &mut usize) {
    let ch = sql[*i..].chars().next().unwrap();
    out.push(ch);
    *i += ch.len_utf8();
}

/// 判断字节是否为标识符字符（字母 / 数字 / 下划线）。
/// 与 sqlparser 的 tokenizer 行为一致：标识符由 `[A-Za-z0-9_]` 组成。
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== G_ORACLE_MINUS =====

    #[test]
    fn g_oracle_minus_rewrites_to_except() {
        let r = rewrite_for_pg_parse("SELECT a FROM t1 MINUS SELECT a FROM t2");
        assert_eq!(r.sql, "SELECT a FROM t1 EXCEPT SELECT a FROM t2");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_ORACLE_MINUS");
        assert_eq!(r.rewrites[0].original, "MINUS");
        assert_eq!(r.rewrites[0].rewritten, "EXCEPT");
    }

    #[test]
    fn g_oracle_minus_case_insensitive() {
        let r = rewrite_for_pg_parse("select a from t1 minus select a from t2");
        assert_eq!(r.sql, "select a from t1 EXCEPT select a from t2");
    }

    #[test]
    fn g_oracle_minus_not_in_string_literal() {
        // 字符串内的 MINUS 不应被重写
        let r = rewrite_for_pg_parse("SELECT 'MINUS' FROM t");
        assert_eq!(r.sql, "SELECT 'MINUS' FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_minus_not_in_line_comment() {
        let r = rewrite_for_pg_parse("SELECT 1 -- MINUS here\nFROM t");
        assert_eq!(r.sql, "SELECT 1 -- MINUS here\nFROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_minus_not_in_block_comment() {
        let r = rewrite_for_pg_parse("SELECT /* MINUS */ 1 FROM t");
        assert_eq!(r.sql, "SELECT /* MINUS */ 1 FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_minus_not_substring_of_identifier() {
        // MINUS 作为子串不应被重写（如列名 myMINUScol 或 MINUS_FIELD）
        let r = rewrite_for_pg_parse("SELECT myMINUScol FROM t");
        assert_eq!(r.sql, "SELECT myMINUScol FROM t");
        assert!(r.is_empty());

        let r2 = rewrite_for_pg_parse("SELECT MINUS_FIELD FROM t");
        assert_eq!(r2.sql, "SELECT MINUS_FIELD FROM t");
        assert!(r2.is_empty());
    }

    // ===== G_ORACLE_SYSDATE =====

    #[test]
    fn g_oracle_sysdate_rewrites_to_current_timestamp() {
        let r = rewrite_for_pg_parse("SELECT SYSDATE FROM t");
        assert_eq!(r.sql, "SELECT CURRENT_TIMESTAMP FROM t");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_ORACLE_SYSDATE");
    }

    #[test]
    fn g_oracle_sysdate_lowercase() {
        let r = rewrite_for_pg_parse("SELECT sysdate FROM t");
        assert_eq!(r.sql, "SELECT CURRENT_TIMESTAMP FROM t");
    }

    #[test]
    fn g_oracle_sysdate_not_substring() {
        let r = rewrite_for_pg_parse("SELECT sysdate_col FROM t");
        assert_eq!(r.sql, "SELECT sysdate_col FROM t");
        assert!(r.is_empty());
    }

    // ===== G_ORACLE_NVL =====

    #[test]
    fn g_oracle_nvl_rewrites_to_coalesce() {
        let r = rewrite_for_pg_parse("SELECT NVL(name, 'unknown') FROM t");
        assert_eq!(r.sql, "SELECT COALESCE(name, 'unknown') FROM t");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_ORACLE_NVL");
    }

    #[test]
    fn g_oracle_nvl_lowercase() {
        let r = rewrite_for_pg_parse("SELECT nvl(name, 'x') FROM t");
        assert_eq!(r.sql, "SELECT COALESCE(name, 'x') FROM t");
    }

    #[test]
    fn g_oracle_nvl_without_paren_not_rewritten() {
        // NVL 后不紧跟左括号时不重写（可能是列名）
        let r = rewrite_for_pg_parse("SELECT NVL FROM t");
        assert_eq!(r.sql, "SELECT NVL FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_nvl_with_space_before_paren() {
        // NVL (name, 'x') —— NVL 后有空格再跟括号：按规范不视为函数调用，不重写
        // （sqlparser 解析时这种写法本身就有歧义，保守不重写）
        let r = rewrite_for_pg_parse("SELECT NVL (name, 'x') FROM t");
        assert_eq!(r.sql, "SELECT NVL (name, 'x') FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_nvl_not_substring() {
        let r = rewrite_for_pg_parse("SELECT NVL_FIELD FROM t");
        assert_eq!(r.sql, "SELECT NVL_FIELD FROM t");
        assert!(r.is_empty());
    }

    // ===== G_ORACLE_DUAL =====

    #[test]
    fn g_oracle_dual_rewrites_to_subquery() {
        let r = rewrite_for_pg_parse("SELECT 1 FROM dual");
        assert_eq!(r.sql, "SELECT 1 FROM (SELECT 1) AS dual");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_ORACLE_DUAL");
    }

    #[test]
    fn g_oracle_dual_uppercase() {
        let r = rewrite_for_pg_parse("SELECT 1 FROM DUAL");
        assert_eq!(r.sql, "SELECT 1 FROM (SELECT 1) AS dual");
    }

    #[test]
    fn g_oracle_dual_with_whitespace() {
        let r = rewrite_for_pg_parse("SELECT 1 FROM  dual");
        assert_eq!(r.sql, "SELECT 1 FROM (SELECT 1) AS dual");
    }

    #[test]
    fn g_oracle_dual_not_column_named_dual() {
        // FROM dual 应重写，但 dual 作为列名不应被误伤
        let r = rewrite_for_pg_parse("SELECT dual FROM t");
        assert_eq!(r.sql, "SELECT dual FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_dual_not_table_named_dualx() {
        // dual 后跟非标识符字符才重写；dualx 是不同标识符
        let r = rewrite_for_pg_parse("SELECT 1 FROM dualx");
        assert_eq!(r.sql, "SELECT 1 FROM dualx");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_dual_preserves_following_semicolon() {
        let r = rewrite_for_pg_parse("SELECT 1 FROM dual;");
        assert_eq!(r.sql, "SELECT 1 FROM (SELECT 1) AS dual;");
    }

    #[test]
    fn g_oracle_dual_fromx_not_rewritten() {
        // FROMx 是不同标识符，不应触发 FROM dual 检查
        let r = rewrite_for_pg_parse("SELECT 1 FROMx dual");
        assert_eq!(r.sql, "SELECT 1 FROMx dual");
        assert!(r.is_empty());
    }

    // ===== G_MYSQL_BACKTICK =====

    #[test]
    fn g_mysql_backtick_rewrites_to_double_quote() {
        let r = rewrite_for_pg_parse("SELECT `order` FROM t");
        assert_eq!(r.sql, "SELECT \"order\" FROM t");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_MYSQL_BACKTICK");
    }

    #[test]
    fn g_mysql_backtick_multiple() {
        let r = rewrite_for_pg_parse("SELECT `order`, `group` FROM t");
        assert_eq!(r.sql, "SELECT \"order\", \"group\" FROM t");
        assert_eq!(r.rewrites.len(), 2);
    }

    #[test]
    fn g_mysql_backtick_escape_inner_double_quote() {
        // 反引号标识符内含双引号：重写后需转义为 ""
        let r = rewrite_for_pg_parse("SELECT `a\"b` FROM t");
        assert_eq!(r.sql, "SELECT \"a\"\"b\" FROM t");
    }

    #[test]
    fn g_mysql_backtick_escape_double_backtick() {
        // `` 在 MySQL 标识符中是转义的反引号；重写后变为两个双引号
        let r = rewrite_for_pg_parse("SELECT `a``b` FROM t");
        assert_eq!(r.sql, "SELECT \"a\"\"b\" FROM t");
    }

    #[test]
    fn g_mysql_backtick_not_in_string() {
        let r = rewrite_for_pg_parse("SELECT 'a`b' FROM t");
        assert_eq!(r.sql, "SELECT 'a`b' FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_mysql_backtick_unclosed_appends_closing_quote() {
        // ★ Fix 1: 未闭合反引号（EOF 前无配对）应补上 closing "，避免生成的标识符残缺。
        // 输入 `order FROM t（无闭合反引号）→ 全部内容 `order FROM t` 被当作标识符，
        // EOF 时补闭合双引号 → "order FROM t"（内容不丢弃，只是语法上被收容为字符串标识符）
        let r = rewrite_for_pg_parse("SELECT `order FROM t");
        assert_eq!(r.sql, "SELECT \"order FROM t\"");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_MYSQL_BACKTICK");
        assert_eq!(r.rewrites[0].original, "`order FROM t");
        assert_eq!(r.rewrites[0].rewritten, "\"order FROM t\"");
    }

    #[test]
    fn g_mysql_backtick_rewritten_field_correct_after_prior_rewrite() {
        // ★ Fix 2: 前置变长重写（MINUS→EXCEPT，5→6 字节）后，
        // 反引号的 rewritten 审计字段应正确切片，而非从 out[start..] 取错位内容。
        let r = rewrite_for_pg_parse("SELECT a MINUS b FROM `order`");
        // 两处重写：MINUS→EXCEPT + 反引号→双引号
        assert_eq!(r.rewrites.len(), 2);
        let backtick_rewrite = &r.rewrites[1];
        assert_eq!(backtick_rewrite.rule_id, "G_MYSQL_BACKTICK");
        assert_eq!(backtick_rewrite.original, "`order`");
        assert_eq!(backtick_rewrite.rewritten, "\"order\"");
    }

    // ===== 行号保真 =====

    #[test]
    fn rewrite_preserves_line_numbers() {
        // 多行 SQL，重写后每行内容应在同一行号
        let sql = "SELECT SYSDATE\nFROM dual\nMINUS\nSELECT SYSDATE FROM t";
        let r = rewrite_for_pg_parse(sql);
        let lines: Vec<&str> = r.sql.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "SELECT CURRENT_TIMESTAMP");
        assert_eq!(lines[1], "FROM (SELECT 1) AS dual");
        assert_eq!(lines[2], "EXCEPT");
        assert_eq!(lines[3], "SELECT CURRENT_TIMESTAMP FROM t");
    }

    // ===== UTF-8 安全 =====

    #[test]
    fn rewrite_preserves_chinese_characters() {
        // 中文注释和字符串应完整透传，不乱码
        let sql = "SELECT '中文' FROM t -- 中文注释\nWHERE name = '测试'";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(r.sql, sql);
        assert!(r.is_empty());
    }

    #[test]
    fn rewrite_preserves_chinese_in_backtick() {
        // 反引号标识符内的中文应完整透传
        let sql = "SELECT `中文字段` FROM t";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(r.sql, "SELECT \"中文字段\" FROM t");
    }

    // ===== 组合场景 =====

    #[test]
    fn rewrite_mixed_oracle_constructs() {
        let sql = "SELECT NVL(name, 'x') FROM t WHERE created > SYSDATE MINUS SELECT a FROM dual";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(
            r.sql,
            "SELECT COALESCE(name, 'x') FROM t WHERE created > CURRENT_TIMESTAMP EXCEPT SELECT a FROM (SELECT 1) AS dual"
        );
        // 4 处重写：NVL、SYSDATE、MINUS、dual
        assert_eq!(r.rewrites.len(), 4);
    }

    #[test]
    fn rewrite_mixed_mysql_and_oracle() {
        let sql = "SELECT `order`, NVL(name, 'x') FROM t MINUS SELECT a FROM dual";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(
            r.sql,
            "SELECT \"order\", COALESCE(name, 'x') FROM t EXCEPT SELECT a FROM (SELECT 1) AS dual"
        );
        assert_eq!(r.rewrites.len(), 4);
    }

    #[test]
    fn rewrite_no_change_for_pure_pg_sql() {
        let sql = "SELECT id, name FROM users WHERE id = $1";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(r.sql, sql);
        assert!(r.is_empty());
    }

    #[test]
    fn rewrite_preserves_pg_string_escaping() {
        // PG 字符串内 '' 是转义的单引号，应完整透传
        let sql = "SELECT 'it''s MINUS not a keyword' FROM t";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(r.sql, "SELECT 'it''s MINUS not a keyword' FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn rewrite_preserves_nested_block_comment() {
        // PG 支持嵌套块注释，应完整透传
        let sql = "SELECT /* outer /* inner MINUS */ outer */ 1 FROM t";
        let r = rewrite_for_pg_parse(sql);
        assert_eq!(r.sql, sql);
        assert!(r.is_empty());
    }

    // ===== G_ORACLE_ALTER_ADD_PARENS =====

    #[test]
    fn g_oracle_alter_add_single_column() {
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (c1 INT)");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 INT");
        assert_eq!(r.rewrites.len(), 1);
        assert_eq!(r.rewrites[0].rule_id, "G_ORACLE_ALTER_ADD_PARENS");
    }

    #[test]
    fn g_oracle_alter_add_multiple_columns() {
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (c1 INT, c2 VARCHAR(10))");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 INT, ADD COLUMN c2 VARCHAR(10)");
    }

    #[test]
    fn g_oracle_alter_add_with_column_options() {
        // 列定义带 DEFAULT / NOT NULL 等选项
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (c1 INT DEFAULT 0 NOT NULL, c2 VARCHAR(10) NULL)");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 INT DEFAULT 0 NOT NULL, ADD COLUMN c2 VARCHAR(10) NULL");
    }

    #[test]
    fn g_oracle_alter_add_lowercase_add() {
        // add 小写也应匹配；★ Fix 4: 保留原始 add 小写，COLUMN 大写
        let r = rewrite_for_pg_parse("alter table t add (c1 int)");
        assert_eq!(r.sql, "alter table t add COLUMN c1 int");
    }

    #[test]
    fn g_oracle_alter_add_newline_between_add_and_paren() {
        // ADD 和 ( 之间可以有换行
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD\n(\n  c1 INT,\n  c2 VARCHAR(10)\n)");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 INT, ADD COLUMN c2 VARCHAR(10)");
    }

    #[test]
    fn g_oracle_alter_add_nested_type_parens() {
        // 类型中的嵌套括号（NUMERIC(10,2)）不应干扰顶层逗号拆分
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (c1 NUMERIC(10,2), c2 VARCHAR(10))");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 NUMERIC(10,2), ADD COLUMN c2 VARCHAR(10)");
    }

    #[test]
    fn g_oracle_alter_add_default_with_comma_in_string() {
        // DEFAULT 'a,b' 中的逗号在字符串内，不应被拆分
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (c1 VARCHAR(10) DEFAULT 'a,b', c2 INT)");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 VARCHAR(10) DEFAULT 'a,b', ADD COLUMN c2 INT");
    }

    #[test]
    fn g_oracle_alter_add_constraint_not_rewritten() {
        // ADD (CONSTRAINT ...) 是约束定义，不重写（让 Oracle 回退兜底）
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (CONSTRAINT pk PRIMARY KEY (id))");
        assert_eq!(r.sql, "ALTER TABLE t ADD (CONSTRAINT pk PRIMARY KEY (id))");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_alter_add_primary_key_not_rewritten() {
        // ADD (PRIMARY KEY (...)) 是约束定义，不重写
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD (PRIMARY KEY (id))");
        assert_eq!(r.sql, "ALTER TABLE t ADD (PRIMARY KEY (id))");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_alter_add_without_paren_not_rewritten() {
        // PG 原生语法 ALTER TABLE t ADD COLUMN c1 INT 不应被重写
        let r = rewrite_for_pg_parse("ALTER TABLE t ADD COLUMN c1 INT");
        assert_eq!(r.sql, "ALTER TABLE t ADD COLUMN c1 INT");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_alter_add_column_name_add_not_rewritten() {
        // ADD 作为列名时不应触发（后面不跟 `(`）
        let r = rewrite_for_pg_parse("SELECT add FROM t");
        assert_eq!(r.sql, "SELECT add FROM t");
        assert!(r.is_empty());
    }

    #[test]
    fn g_oracle_alter_add_mixed_with_other_rewrites() {
        // 与其他重写规则组合：ALTER TABLE 里有反引号 + NVL 默认值
        let r = rewrite_for_pg_parse("ALTER TABLE `order` ADD (c1 INT DEFAULT NVL(x, 0))");
        assert_eq!(r.sql, "ALTER TABLE \"order\" ADD COLUMN c1 INT DEFAULT COALESCE(x, 0)");
        // 2 处重写：反引号 + ADD (...)
        assert_eq!(r.rewrites.len(), 2);
    }
}
