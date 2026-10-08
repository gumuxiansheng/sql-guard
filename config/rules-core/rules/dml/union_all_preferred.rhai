// union_all_preferred.rhai (DML106)
// 除非需要去重，否则优先使用 UNION ALL 而非 UNION。
// UNION 会执行隐式 DISTINCT（排序去重），性能低于 UNION ALL。
//
// 反面案例（会被拦截）：
//   SELECT id FROM users UNION SELECT user_id FROM orders;     -- 隐式去重，性能差
//
// 正面案例（放行）：
//   SELECT id FROM users UNION ALL SELECT user_id FROM orders;  -- 不去重，性能更好
//   SELECT id FROM users UNION DISTINCT SELECT user_id FROM orders;  -- 确需去重时显式写明

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.is_union() && !sel.is_union_all() {
            violations.push(violation("UNION performs implicit DISTINCT. Use UNION ALL if duplicate elimination is not required, or explicitly use UNION DISTINCT for clarity.", s.line(), s.column()));
        }
    }
}
