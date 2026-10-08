// gaussdb_update_subquery_to_join.rhai (GDML004)
// GaussDB 规范 G-DML-10：针对 UPDATE ... SET ... WHERE 含 SUBQUERY（子查询）
// 的 SQL 语句，宜将 SUBQUERY 调整为 JOIN，并根据执行计划对比分析优化。
//
// 这是**建议级**（warning）：只提示"存在子查询"，不做等价改写，也不判定
// 改写后一定更优——最终以执行计划为准。
//
// 判定口径：UpdateInfo.has_subquery()（对 WHERE 表达式递归判断
// Subquery / InSubquery / Exists）。
//
// 会提示：
//   UPDATE t SET a = 1 WHERE id IN (SELECT id FROM y);
//   UPDATE t SET a = 1 WHERE EXISTS (SELECT 1 FROM y WHERE y.id = t.id);
//
// 不提示：
//   UPDATE t SET a = 1 WHERE id = 2;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_update() {
        let u = s.update();
        if u.has_subquery() {
            violations.push(violation(
                "UPDATE on '" + u.table_name() + "' has a subquery in WHERE; consider rewriting as JOIN and compare execution plans",
                s.line(), s.column()));
        }
    }
}
