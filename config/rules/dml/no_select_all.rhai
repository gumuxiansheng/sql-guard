// no_select_all.rhai (DML001)
// 禁止使用 SELECT *，必须显式列出所需列。
// 基于 AST 精确识别通配符，不会误判注释或字符串字面量。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users;
//   SELECT u.* FROM users u JOIN orders o ON u.id = o.user_id;
//
// 正面案例（放行）：
//   SELECT id, name, email FROM users;
//   SELECT u.id, u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_wildcard() {
            violations.push(violation("SELECT * is not allowed. Specify columns explicitly.", s.line(), s.column()));
        }
    }
}
