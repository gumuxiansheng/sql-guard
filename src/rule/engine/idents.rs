//! 标识符与字节级内置工具函数（C5）。
//!
//! 规则脚本中大量"命名规范"类检查需要三样 Rhai 标准库不提供的能力：
//! 1. **字节长度**：Rhai `String.len()` 返回**字符**数，而规范多以"字节"表述
//!    （如 GaussDB「对象名长度禁止超过 63 个字节」）。中文/多字节场景下两者不等。
//! 2. **标识符字符合法性**：Rhai 遍历 `String` 的字符成本高，且规则脚本重复实现易分叉。
//! 3. **引号 / 前缀 / 保留字判定**：需要"取最后一段标识符 + 去引号 + 大小写折叠"的
//!    一致语义，否则各规则会各写一套。
//!
//! 本模块把这些判定收敛为纯函数，由 `runner::build_engine` 注册为 Rhai 全局函数。
//!
//! ## 与规则脚本的关系
//!
//! [`RESERVED_WORDS`] 与 `config/rules/ddl/no_reserved_keyword_naming.rhai`（DDL003）
//! 内联的关键字表**语义一致**。DDL003 目前仍用脚本内联表（为兼容旧二进制），
//! 二者修改时必须同步；后续 DDL003 迁移到 `is_reserved_word()` 后，
//! 本模块即为唯一事实源。
//!
//! ## 大小写与引号语义（对齐 PostgreSQL / GaussDB）
//!
//! - **不带引号**的标识符在 PG 系数据库中被折叠为**小写**；
//! - **双引号**包裹的标识符保留原大小写（也因此"用双引号定义对象名"本身是违规项，
//!   见 GaussDB 规范 G-NAM-02，由 [`is_quoted_ident`] 判定）；
//! - MySQL 反引号、SQL Server 方括号在本模块中被统一视作"带引号"。

/// SQL 保留关键字集合（大写，大小写不敏感匹配）。
///
/// 来源与 `config/rules/ddl/no_reserved_keyword_naming.rhai`（DDL003）内联表一致。
/// 修改此表时必须同步该规则脚本（与其 `examples/` 副本）。
///
/// `#[rustfmt::skip]`：按语义词组分行排布（117 项），比 rustfmt 默认的「一项一行」
/// 更利于人工核对与 diff。
#[rustfmt::skip]
pub const RESERVED_WORDS: &[&str] = &[
    // DML / 查询
    "SELECT", "INSERT", "UPDATE", "DELETE", "FROM", "WHERE", "INTO", "VALUES", "SET", "JOIN",
    "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "ON", "USING", "UNION", "ALL", "INTERSECT",
    "EXCEPT", "ORDER", "BY", "GROUP", "HAVING", "LIMIT", "OFFSET", "FETCH", "DISTINCT", "AS", "WITH",
    "RECURSIVE",
    // DDL
    "CREATE", "TABLE", "INDEX", "VIEW", "DROP", "ALTER", "ADD", "MODIFY", "CHANGE", "RENAME",
    "TRUNCATE", "SCHEMA", "DATABASE", "SEQUENCE", "CONSTRAINT", "PRIMARY", "FOREIGN", "REFERENCES",
    "DEFAULT", "CHECK", "UNIQUE", "KEY", "COLUMN",
    // 表达式 / 谓词
    "AND", "OR", "NOT", "IN", "EXISTS", "BETWEEN", "LIKE", "IS", "NULL", "TRUE", "FALSE", "CASE",
    "WHEN", "THEN", "ELSE", "END", "IF", "BEGIN",
    // 事务 / 权限
    "COMMIT", "ROLLBACK", "SAVEPOINT", "GRANT", "REVOKE", "RETURN", "CASCADE", "RESTRICT", "USER",
    "ROLE", "PUBLIC", "SESSION", "TRANSACTION", "LOCK",
    // 类型 / 转换
    "CAST", "CONVERT", "NATURAL", "ESCAPE", "TEMPORARY", "TEMP", "INT", "INTEGER", "SMALLINT",
    "BIGINT", "DECIMAL", "NUMERIC", "FLOAT", "REAL", "DOUBLE", "CHAR", "VARCHAR", "TEXT", "DATE",
    "TIME", "TIMESTAMP", "INTERVAL", "BOOLEAN", "BLOB", "BINARY", "CURRENT_DATE", "CURRENT_TIME",
    "CURRENT_TIMESTAMP", "CURRENT_USER",
];

/// GaussDB 规范（G-NAM-03）禁止的对象名前缀。
pub const RESERVED_PREFIXES: &[&str] = &["pg_", "gs_", "adm_", "my_", "db_"];

/// 标识符的**字节**长度（UTF-8 编码后）。
///
/// Rhai 的 `String.len()` 返回字符数，多字节字符场景下会低估。
/// GaussDB 的 63 字节对象名限制必须用本函数判定。
pub fn len_bytes(s: &str) -> i64 {
    s.len() as i64
}

/// 判断 `s` 是否为"合法的裸标识符"：非空、仅含 `[A-Za-z0-9_]`、且不以数字开头。
///
/// 对应 GaussDB 规范 G-NAM-01「对象名只能使用字母、数字和下划线的组合」。
/// 判定基于**原始入参**（不做去引号），因此带引号的名字会返回 `false` ——
/// 这正是期望行为：规范要求名字本身不得包含引号。
pub fn is_valid_ident(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    let mut chars = t.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 判断 `s` 是否被引号包裹（`"x"` / `` `x` `` / `[x]`）。
///
/// 对应 GaussDB 规范 G-NAM-02「禁止使用双引号括起来的字符串来定义数据库对象名称」。
pub fn is_quoted_ident(s: &str) -> bool {
    let t = s.trim();
    let b = t.as_bytes();
    if b.len() < 2 {
        return false;
    }
    let (f, l) = (b[0], b[b.len() - 1]);
    (f == b'"' && l == b'"') || (f == b'`' && l == b'`') || (f == b'[' && l == b']')
}

/// 去掉首尾的成对引号（`"x"` / `` `x` `` / `[x]` → `x`）。
///
/// 引号不配对时原样返回（不做"尽力而为"的猜测，避免规则误判）。
/// ASCII 字节判断，不会破坏多字节字符。
pub fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if is_quoted_ident(t) {
        // 引号是 ASCII 单字节，按字节位置切片安全（不会切断 UTF-8 序列）
        return t[1..t.len() - 1].to_string();
    }
    t.to_string()
}

/// 取名字的"叶子标识符"：去掉 schema 前缀后取最后一段，并去除引号。
///
/// 例：`ofsm.cdeorg` → `cdeorg`；`"Users"` → `Users`；`users` → `users`。
/// 与 DDL003 脚本内 `extract_identifier` 的语义一致。
pub fn ident_leaf(s: &str) -> String {
    let last = s.trim().rsplit('.').next().unwrap_or("");
    strip_quotes(last)
}

/// 归一化标识符：取叶子名 + 按 PG 折叠规则统一大小写。
///
/// - 带引号（`"Users"`）→ 保留原大小写，返回 `Users`
/// - 不带引号（`Users`）→ PG 系折叠为小写，返回 `users`
/// - 带 schema 前缀时**只看叶子段**：`ofsm."Cdeorg"` → `Cdeorg`，`OFSM.CDEORG` → `cdeorg`
///
/// 用于把"SQL 原文中的名字"与"元数据中的实际对象名"对齐比较。
pub fn normalize_ident(s: &str) -> String {
    let t = s.trim();
    // 引号判定必须作用在**叶子段**上：`ofsm."CDEORG"` 的叶子是带引号的，
    // 若按整串判定（首字节 'o'、尾字节 '"'）会误判为"未加引号"从而错误折叠大小写。
    let last = t.rsplit('.').next().unwrap_or("").trim();
    if is_quoted_ident(last) {
        strip_quotes(last)
    } else {
        last.to_lowercase()
    }
}

/// 判断叶子标识符是否为 SQL 保留关键字（大小写不敏感）。
pub fn is_reserved_word(s: &str) -> bool {
    let leaf = ident_leaf(s);
    if leaf.is_empty() {
        return false;
    }
    let upper = leaf.to_uppercase();
    RESERVED_WORDS.iter().any(|k| *k == upper)
}

/// 判断叶子标识符是否使用了系统预留前缀（`pg_` / `gs_` / `adm_` / `my_` / `db_`）。
///
/// 对应 GaussDB 规范 G-NAM-03。大小写不敏感。
pub fn has_reserved_prefix(s: &str) -> bool {
    let leaf = ident_leaf(s).to_lowercase();
    !leaf.is_empty() && RESERVED_PREFIXES.iter().any(|p| leaf.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_len_bytes_counts_utf8_bytes_not_chars() {
        assert_eq!(len_bytes("abc"), 3);
        assert_eq!(len_bytes("中文"), 6, "每个汉字 3 字节");
        assert_eq!(len_bytes(""), 0);
        assert_eq!(len_bytes("a中"), 4);
        // Rhai 的 len() 是字符数，这里必须不同，否则规则会漏判
        assert_ne!(len_bytes("中文") as usize, "中文".chars().count());
    }

    #[test]
    fn test_is_valid_ident_accepts_charset_and_rejects_others() {
        assert!(is_valid_ident("users"));
        assert!(is_valid_ident("user_orders_2"));
        assert!(is_valid_ident("_private"));
        assert!(is_valid_ident("A1"));
        assert!(!is_valid_ident(""));
        assert!(!is_valid_ident("  "));
        assert!(!is_valid_ident("2users"), "不得以数字开头");
        assert!(!is_valid_ident("user-orders"), "短横线非法");
        assert!(!is_valid_ident("用户表"), "非 ASCII 非法");
        assert!(!is_valid_ident("\"users\""), "带引号非法");
        assert!(!is_valid_ident("user.name"), "点号非法");
        assert!(!is_valid_ident("user name"), "空格非法");
    }

    #[test]
    fn test_quoted_ident_and_strip_quotes() {
        assert!(is_quoted_ident("\"Users\""));
        assert!(is_quoted_ident("`Users`"));
        assert!(is_quoted_ident("[Users]"));
        assert!(!is_quoted_ident("Users"));
        assert!(!is_quoted_ident("\"Users"));
        assert!(!is_quoted_ident("U"));
        assert!(!is_quoted_ident(""));

        assert_eq!(strip_quotes("\"Users\""), "Users");
        assert_eq!(strip_quotes("`Users`"), "Users");
        assert_eq!(strip_quotes("[Users]"), "Users");
        assert_eq!(strip_quotes("Users"), "Users");
        assert_eq!(strip_quotes("\"不配对"), "\"不配对");
        // 多字节安全：切片按 ASCII 引号字节位置进行
        assert_eq!(strip_quotes("\"用户表\""), "用户表");
    }

    #[test]
    fn test_ident_leaf_handles_schema_prefix_and_quotes() {
        assert_eq!(ident_leaf("ofsm.cdeorg"), "cdeorg");
        assert_eq!(ident_leaf("a.b.c"), "c");
        assert_eq!(ident_leaf("\"Users\""), "Users");
        assert_eq!(ident_leaf("ofsm.\"Users\""), "Users");
        assert_eq!(ident_leaf("users"), "users");
        assert_eq!(ident_leaf(""), "");
    }

    #[test]
    fn test_normalize_ident_folds_unquoted_to_lowercase() {
        assert_eq!(normalize_ident("Users"), "users");
        assert_eq!(normalize_ident("\"Users\""), "Users");
        assert_eq!(normalize_ident("OFSM.CDEORG"), "cdeorg");
        assert_eq!(normalize_ident("ofsm.\"CDEORG\""), "CDEORG");
    }

    #[test]
    fn test_reserved_words_match_and_are_unique() {
        assert!(is_reserved_word("select"));
        assert!(is_reserved_word("SELECT"));
        assert!(is_reserved_word("ofsm.select"), "带 schema 前缀也要命中");
        assert!(is_reserved_word("\"ORDER\""), "带引号也要命中");
        assert!(!is_reserved_word("user_orders"));
        assert!(!is_reserved_word("order_no"));
        assert!(!is_reserved_word(""));
        assert!(!is_reserved_word("用户表"));

        // 表内不得有重复项，且条目数与 DDL003 内联表一致（117 项）
        let mut sorted = RESERVED_WORDS.to_vec();
        sorted.sort_unstable();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(sorted, deduped, "RESERVED_WORDS 存在重复项");
        assert_eq!(RESERVED_WORDS.len(), 117, "与 DDL003 内联表项数不一致");
    }

    #[test]
    fn test_has_reserved_prefix() {
        assert!(has_reserved_prefix("pg_class"));
        assert!(has_reserved_prefix("GS_TABLE"));
        assert!(has_reserved_prefix("adm_user"));
        assert!(has_reserved_prefix("my_tab"));
        assert!(has_reserved_prefix("db_x"));
        assert!(
            has_reserved_prefix("ofsm.pg_class"),
            "带 schema 前缀也要命中"
        );
        assert!(!has_reserved_prefix("users"));
        assert!(!has_reserved_prefix("mypg_x"));
        assert!(!has_reserved_prefix(""));
    }
}
