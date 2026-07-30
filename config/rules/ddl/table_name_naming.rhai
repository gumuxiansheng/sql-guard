// table_name_naming.rhai (DDL007)
// 表名命名规范：只能包含小写字母、数字、下划线，且不能以数字开头。
// 覆盖 CREATE TABLE 和 ALTER TABLE ... RENAME TO 两种定义/重命名表名的场景。
//
// 反面案例（会被拦截）：
//   CREATE TABLE Users (id INT PRIMARY KEY);          -- 含大写
//   CREATE TABLE 2fa_codes (id INT PRIMARY KEY);      -- 以数字开头
//   CREATE TABLE order-items (id INT PRIMARY KEY);    -- 含连字符
//   CREATE TABLE `order` (id INT PRIMARY KEY);        -- 去引号后仍为 order（合法），但如果叫 Order 则拦截
//   ALTER TABLE old_t RENAME TO NewTable;             -- 含大写
//
// 正面案例（放行）：
//   CREATE TABLE user_orders (id INT PRIMARY KEY);
//   CREATE TABLE t1 (id INT PRIMARY KEY);
//   ALTER TABLE old_t RENAME TO new_table;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// 合法字符集：a-z / 0-9 / _
let valid_chars = "abcdefghijklmnopqrstuvwxyz0123456789_";
let digits = "0123456789";

/// 去除标识符两侧的引号（双引号 / 反引号 / 方括号）。
fn strip_quotes(name) {
    let n = name.len();
    if n < 2 { return name; }
    if name.starts_with("\"") && name.ends_with("\"") {
        return name.sub_string(1, n - 2);
    }
    if name.starts_with("`") && name.ends_with("`") {
        return name.sub_string(1, n - 2);
    }
    if name.starts_with("[") && name.ends_with("]") {
        return name.sub_string(1, n - 2);
    }
    return name;
}

/// 从可能含 schema 前缀的名字中提取最后一个标识符部分并去引号。
fn extract_identifier(name) {
    let parts = name.split(".");
    let last = parts[parts.len() - 1];
    return strip_quotes(last);
}

/// 校验表名：只能包含小写字母、数字、下划线，且不能以数字开头。
/// 返回空字符串表示合法；返回非空字符串描述具体违规原因。
fn validate_table_name(name) {
    if name.len() == 0 { return ""; }

    // 不能以数字开头
    let first = name.sub_string(0, 1);
    if digits.contains(first) {
        return "starts with a digit";
    }

    // 只能包含小写字母、数字、下划线
    for ch in name {
        if !valid_chars.contains(ch) {
            return "contains invalid character '" + ch + "' (only lowercase letters, digits, and underscores are allowed)";
        }
    }
    return "";
}

for s in ast.statements() {
    // CREATE TABLE — 校验新表名
    if s.has_create_table() {
        let ct = s.create_table();
        let tname = ct.table_name();
        let clean = extract_identifier(tname);
        let reason = validate_table_name(clean);
        if reason != "" {
            violations.push(violation("Table name '" + clean + "' " + reason + "; table names must contain only lowercase letters, digits, and underscores, and must not start with a digit", s.line(), s.column()));
        }
    }

    // ALTER TABLE ... RENAME TO — 校验新表名
    if s.has_alter_table() {
        let at = s.alter_table();
        for op in at.operations() {
            if op.operation_type() == "RENAME_TABLE" {
                let new_name = op.table_name();
                let clean = extract_identifier(new_name);
                let reason = validate_table_name(clean);
                if reason != "" {
                    violations.push(violation("Renamed table name '" + clean + "' " + reason + "; table names must contain only lowercase letters, digits, and underscores, and must not start with a digit", s.line(), s.column()));
                }
            }
        }
    }
}
