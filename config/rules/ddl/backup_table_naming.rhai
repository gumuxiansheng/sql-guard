// backup_table_naming.rhai (DDL004)
// 通过 CREATE TABLE AS SELECT 创建的备份表表名必须以 bks_ 开头。
// 如确有该语句生成的非备份表，需在该语句附近（同行或上一行）显式注释说明。
//
// 反面案例（会被拦截）：
//   -- 备份表却未以 bks_ 开头，且附近无豁免注释
//   CREATE TABLE tmp_users AS SELECT * FROM users;
//   CREATE TABLE users_20240101 AS SELECT * FROM users;
//
// 正面案例（放行）：
//   -- 以 bks_ 开头的备份表
//   CREATE TABLE bks_users_20240101 AS SELECT * FROM users;
//   -- 非备份用途：同行/上一行加注释说明即可豁免
//   -- 物化中间结果，非备份表
//   CREATE TABLE report_snapshot AS SELECT * FROM users;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let comments = ast.comments();

/// 检查指定行号附近（同行或上一行）是否存在注释。
fn has_nearby_comment(stmt_line, comment_list) {
    for c in comment_list {
        if c.line() == stmt_line || c.line() == stmt_line - 1 {
            return true;
        }
    }
    return false;
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        if ct.is_create_as() {
            let tname = ct.table_name();
            // 提取最后一个标识符部分（去掉 schema 前缀和引号）
            let parts = tname.split(".");
            let last = parts[parts.len() - 1];
            let name = last;
            let n = name.len();
            if n >= 2 {
                if (name.starts_with("\"") && name.ends_with("\""))
                    || (name.starts_with("`") && name.ends_with("`"))
                    || (name.starts_with("[") && name.ends_with("]")) {
                    name = name.sub_string(1, n - 2);
                }
            }
            let lower = name.to_lower();
            if !lower.starts_with("bks_") {
                if !has_nearby_comment(s.line(), comments) {
                    violations.push(violation("Backup table created via CREATE TABLE AS SELECT must be named with 'bks_' prefix, or add a comment explaining it is not a backup table: " + tname, s.line(), s.column()));
                }
            }
        }
    }
}
