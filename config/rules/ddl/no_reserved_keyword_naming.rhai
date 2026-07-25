// no_reserved_keyword_naming.rhai (DDL003)
// 禁止使用 SQL 关键字进行数据库对象命名（表名、列名、索引名、视图名等）。
// 使用关键字作为标识符会导致歧义、可移植性问题，并在某些数据库中需要转义。
//
// 反面案例（会被拦截）：
//   -- SELECT / ORDER 等都是 SQL 关键字，作标识符需转义且易出错
//   CREATE TABLE SELECT (id INT, name VARCHAR(100));
//   CREATE TABLE users (order INT, group VARCHAR(100));
//   CREATE INDEX INDEX ON users (id);
//
// 正面案例（放行）：
//   CREATE TABLE user_orders (order_id INT, group_no VARCHAR(100));
//   CREATE TABLE users (order_no INT, group_name VARCHAR(100));
//   CREATE INDEX idx_users_id ON users (id);

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// SQL 保留关键字集合（大写），用于大小写不敏感匹配
let keywords = #{
    "SELECT": true, "INSERT": true, "UPDATE": true, "DELETE": true,
    "FROM": true, "WHERE": true, "INTO": true, "VALUES": true, "SET": true,
    "JOIN": true, "INNER": true, "LEFT": true, "RIGHT": true, "FULL": true,
    "OUTER": true, "CROSS": true, "ON": true, "USING": true,
    "UNION": true, "ALL": true, "INTERSECT": true, "EXCEPT": true,
    "ORDER": true, "BY": true, "GROUP": true, "HAVING": true,
    "LIMIT": true, "OFFSET": true, "FETCH": true, "DISTINCT": true,
    "AS": true, "WITH": true, "RECURSIVE": true,
    "CREATE": true, "TABLE": true, "INDEX": true, "VIEW": true,
    "DROP": true, "ALTER": true, "ADD": true, "MODIFY": true, "CHANGE": true,
    "RENAME": true, "TRUNCATE": true, "SCHEMA": true, "DATABASE": true,
    "SEQUENCE": true, "CONSTRAINT": true, "PRIMARY": true, "FOREIGN": true,
    "REFERENCES": true, "DEFAULT": true, "CHECK": true, "UNIQUE": true,
    "KEY": true, "COLUMN": true,
    "AND": true, "OR": true, "NOT": true, "IN": true, "EXISTS": true,
    "BETWEEN": true, "LIKE": true, "IS": true, "NULL": true,
    "TRUE": true, "FALSE": true, "CASE": true, "WHEN": true, "THEN": true,
    "ELSE": true, "END": true, "IF": true, "BEGIN": true,
    "COMMIT": true, "ROLLBACK": true, "SAVEPOINT": true,
    "GRANT": true, "REVOKE": true, "RETURN": true,
    "CASCADE": true, "RESTRICT": true,
    "USER": true, "ROLE": true, "PUBLIC": true, "SESSION": true,
    "TRANSACTION": true, "LOCK": true,
    "CAST": true, "CONVERT": true, "NATURAL": true, "ESCAPE": true,
    "TEMPORARY": true, "TEMP": true,
    "INT": true, "INTEGER": true, "SMALLINT": true, "BIGINT": true,
    "DECIMAL": true, "NUMERIC": true, "FLOAT": true, "REAL": true,
    "DOUBLE": true, "CHAR": true, "VARCHAR": true, "TEXT": true,
    "DATE": true, "TIME": true, "TIMESTAMP": true, "INTERVAL": true,
    "BOOLEAN": true, "BLOB": true, "BINARY": true,
    "CURRENT_DATE": true, "CURRENT_TIME": true, "CURRENT_TIMESTAMP": true,
    "CURRENT_USER": true,
};

/// 从可能包含 schema 前缀和引号的名字中提取最后一个标识符部分，并去除引号。
fn extract_identifier(name) {
    let parts = name.split(".");
    let last = parts[parts.len() - 1];
    let n = last.len();
    if n < 2 { return last; }
    if last.starts_with("\"") && last.ends_with("\"") {
        return last.sub_string(1, n - 2);
    }
    if last.starts_with("`") && last.ends_with("`") {
        return last.sub_string(1, n - 2);
    }
    if last.starts_with("[") && last.ends_with("]") {
        return last.sub_string(1, n - 2);
    }
    return last;
}

/// 检查名字是否使用了 SQL 保留关键字（大小写不敏感）。
/// 关键字集合通过参数传入，因为 Rhai 函数无法访问外层 scope 变量。
fn is_reserved_keyword(name, kw_map) {
    let ident = extract_identifier(name);
    let upper = ident.to_upper();
    return upper in kw_map;
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let tname = ct.table_name();
        if is_reserved_keyword(tname, keywords) {
            violations.push(violation("Table name '" + tname + "' uses a SQL reserved keyword, use a different name", s.line(), s.column()));
        }
        for c in ct.columns() {
            if is_reserved_keyword(c.name(), keywords) {
                violations.push(violation("Column name '" + c.name() + "' in table '" + tname + "' uses a SQL reserved keyword", s.line(), s.column()));
            }
        }
        for idx in ct.indexes() {
            if idx.name().len() > 0 && is_reserved_keyword(idx.name(), keywords) {
                violations.push(violation("Index name '" + idx.name() + "' uses a SQL reserved keyword", s.line(), s.column()));
            }
        }
        for u in ct.uniques() {
            if u.name().len() > 0 && is_reserved_keyword(u.name(), keywords) {
                violations.push(violation("Unique constraint name '" + u.name() + "' uses a SQL reserved keyword", s.line(), s.column()));
            }
        }
    }
    if s.has_create_index() {
        let ci = s.create_index();
        if ci.name().len() > 0 && is_reserved_keyword(ci.name(), keywords) {
            violations.push(violation("Index name '" + ci.name() + "' uses a SQL reserved keyword", s.line(), s.column()));
        }
    }
    if s.has_create_view() {
        let v = s.create_view();
        if is_reserved_keyword(v.name(), keywords) {
            violations.push(violation("View name '" + v.name() + "' uses a SQL reserved keyword", s.line(), s.column()));
        }
    }
}
