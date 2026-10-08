// join_type_required.rhai (DML109)
// JOIN 必须显式指定类型（LEFT / INNER / RIGHT / FULL / CROSS 等），不允许裸 JOIN。
// 裸 JOIN 的语义（默认 INNER）不明确，显式类型让查询意图更清晰、可移植性更好。
// 基于 AST：仅 sqlparser 的 JoinOperator::Join（裸 JOIN）会被识别，显式类型（含 NATURAL/老式内联等）放行。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users JOIN orders ON users.id = orders.user_id;   -- 裸 JOIN
//
// 正面案例（放行）：
//   SELECT * FROM users INNER JOIN orders ON users.id = orders.user_id;
//   SELECT * FROM users LEFT JOIN orders ON users.id = orders.user_id;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// 递归检查 select 及其子查询中是否存在裸 JOIN
fn has_bare_join(sel) {
    for j in sel.joins() {
        if !j.has_explicit_join_type() {
            return true;
        }
    }
    for sub in sel.subqueries() {
        if has_bare_join(sub) {
            return true;
        }
    }
    return false;
}

for s in ast.statements() {
    if s.has_select() {
        if has_bare_join(s.select()) {
            violations.push(violation_line("JOIN must specify an explicit join type (e.g. LEFT JOIN or INNER JOIN); bare JOIN is not allowed.", 1));
        }
    }
}
