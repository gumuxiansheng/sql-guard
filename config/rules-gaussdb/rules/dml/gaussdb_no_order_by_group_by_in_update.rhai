// gaussdb_no_order_by_group_by_in_update.rhai (GDML002)
// GaussDB 规范 G-DML-08：UPDATE 语句中禁止使用 ORDER BY、GROUP BY 子句，
// 避免不必要的排序。
//
// 判定口径：UpdateInfo.has_order_by() / has_group_by()。
// ⚠️ 实现背景：sqlparser 0.60 的 Update 结构没有 order_by/group_by 字段，
// 这两个标志由引擎做"语句原文顶层子句扫描"得出（跳过括号内子查询、
// 字符串与注释），规则侧直接使用即可。
//
// 反面案例（会被拦截）：
//   UPDATE t SET a = 1 ORDER BY b;
//   UPDATE t SET a = 1 GROUP BY b;
//
// 正面案例（放行）：
//   UPDATE t SET a = 1 WHERE id = 2;
//   UPDATE t SET a = (SELECT x FROM y ORDER BY x LIMIT 1) WHERE b = 2;  -- 子查询内不算
//
// 边界提示：`UPDATE ... ORDER BY` 在 sqlparser 里属"残缺解析"，需要方言回退链
// 才能拿到 UPDATE 节点。dialect=generic 且未配置 dialect_fallback 时会退化为
// PARSE_ERROR（本规则拿不到节点，但会由 PARSE 警告暴露，不会静默）。

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_update() {
        let u = s.update();
        if u.has_order_by() {
            violations.push(violation(
                "UPDATE on '" + u.table_name() + "' must not use ORDER BY",
                s.line(), s.column()));
        }
        if u.has_group_by() {
            violations.push(violation(
                "UPDATE on '" + u.table_name() + "' must not use GROUP BY",
                s.line(), s.column()));
        }
    }
}
