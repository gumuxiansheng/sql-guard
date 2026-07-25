// backup_table_naming.rhai (DDL004)
// 通过 CREATE TABLE AS SELECT 创建的备份表表名必须以 bks_ 开头。
// 如确有该语句生成的非备份表，必须在语句同行或上一行加显式注释 NOT_BACKUP 方可豁免
// （仅写普通注释不足以豁免，防止随意绕过校验）。
//
// 反面案例（会被拦截）：
//   -- 非 bks_ 前缀，且无 NOT_BACKUP 豁免注释
//   CREATE TABLE tmp_users AS SELECT * FROM users;
//   -- 附近有普通注释但非 NOT_BACKUP，仍拦截
//   -- 这是物化中间结果
//   CREATE TABLE report_snapshot AS SELECT * FROM users;
//
// 正面案例（放行）：
//   -- 以 bks_ 开头的备份表
//   CREATE TABLE bks_users_20240101 AS SELECT * FROM users;
//   -- 同行/上一行显式 NOT_BACKUP 注释豁免
//   -- NOT_BACKUP: 物化中间结果，非备份表
//   CREATE TABLE report_snapshot AS SELECT * FROM users;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let comments = ast.comments();

/// 检查指定行号附近（同行或上一行）是否存在显式豁免注释（含 NOT_BACKUP，大小写不敏感）。
fn has_nearby_not_backup_comment(stmt_line, comment_list) {
    for c in comment_list {
        if c.line() == stmt_line || c.line() == stmt_line - 1 {
            if c.text().to_upper().contains("NOT_BACKUP") {
                return true;
            }
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
                if !has_nearby_not_backup_comment(s.line(), comments) {
                    violations.push(violation("Backup table created via CREATE TABLE AS SELECT must be named with 'bks_' prefix, or add a comment containing 'NOT_BACKUP' on the same or previous line to mark it as not a backup table: " + tname, s.line(), s.column()));
                }
            }
        }
    }
}
