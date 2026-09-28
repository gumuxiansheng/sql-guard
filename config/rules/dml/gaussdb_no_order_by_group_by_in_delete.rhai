// gaussdb_no_order_by_group_by_in_delete.rhai (GDML003)
// GaussDB 规范 G-DML-09：DELETE 语句中禁止使用 ORDER BY、GROUP BY 子句，
// 避免不必要的排序。
//
// 判定口径：DeleteInfo.has_order_by()（sqlparser 的 Delete 有 order_by 字段，AST 可得）
// 与 has_group_by()（sqlparser 无该字段，由原文顶层子句扫描兜底）。
//
// 反面案例（会被拦截）：
//   DELETE FROM t WHERE a = 1 ORDER BY b;
//   DELETE FROM t WHERE a = 1 GROUP BY b;
//
// 正面案例（放行）：
//   DELETE FROM t WHERE id = 2;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_delete() {
        let d = s.delete();
        if d.has_order_by() {
            violations.push(violation(
                "DELETE on '" + d.table_name() + "' must not use ORDER BY",
                s.line(), s.column()));
        }
        if d.has_group_by() {
            violations.push(violation(
                "DELETE on '" + d.table_name() + "' must not use GROUP BY",
                s.line(), s.column()));
        }
    }
}
