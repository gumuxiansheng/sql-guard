// no_join_without_condition.rhai (DML006)
// JOIN 必须带 ON 条件，防止产生无意的笛卡儿积。
// 基于 AST 检查 JOIN 子句是否包含 ON 或 USING 条件（显式 CROSS JOIN 豁免）。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users JOIN orders;                  -- 无 ON/USING，产生笛卡尔积
//
// 正面案例（放行）：
//   SELECT * FROM users u JOIN orders o ON u.id = o.user_id;
//   SELECT * FROM users CROSS JOIN orders;            -- 显式 CROSS JOIN 豁免

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_join_without_condition() {
            violations.push(violation("JOIN without ON or USING condition causes a Cartesian product. Add an explicit join condition.", s.line(), s.column()));
        }
    }
}
