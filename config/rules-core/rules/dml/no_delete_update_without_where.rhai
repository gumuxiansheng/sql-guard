// no_delete_update_without_where.rhai (DML002)
// DELETE 和 UPDATE 语句必须带 WHERE 条件，防止全表误操作。
// 基于 AST 精确判断 WHERE 子句是否存在。
//
// 反面案例（会被拦截）：
//   DELETE FROM users;                       -- 删除全表
//   UPDATE users SET status = 'inactive';    -- 更新全表
//
// 正面案例（放行）：
//   DELETE FROM users WHERE id = 100;
//   UPDATE users SET status = 'inactive' WHERE last_login IS NULL;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_delete() {
        let d = s.delete();
        if !d.has_where() {
            violations.push(violation("DELETE without WHERE clause will remove all rows from table: " + d.table_name() + ". Add a WHERE condition or use TRUNCATE if intentional.", s.line(), s.column()));
        }
    }
    if s.has_update() {
        let u = s.update();
        if !u.has_where() {
            violations.push(violation("UPDATE without WHERE clause will modify all rows in table: " + u.table_name() + ". Add a WHERE condition.", s.line(), s.column()));
        }
    }
}
