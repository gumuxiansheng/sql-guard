// gaussdb_view_usage_warning.rhai (GOBJ003)
// GaussDB 规范 G-OBJ-09：不建议使用视图，不建议在视图中嵌套视图。
//
// 这是**建议级**（warning）：只要出现普通视图就提示，提醒评估是否可以用
// 基表 + 应用侧查询替代。物化视图由 GOBJ001 直接拦截（不走本规则）。
//
// 已知局限：视图嵌套的精确判定需要"表↔视图"映射，本规则只提示"存在视图"；
// 需要更严格检查时可配合 GOBJ001 / 手工评审。
//
// 会提示：
//   CREATE VIEW v_orders AS SELECT id FROM orders;
//
// 不会提示：
//   CREATE MATERIALIZED VIEW ...      -- 交给 GOBJ001 报 error
//   CREATE TABLE ...                  -- 与视图无关

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_view() {
        let v = s.create_view();
        if !v.materialized() {
            violations.push(violation(
                "view '" + v.name() + "' is discouraged; prefer querying base tables (and avoid nested views)",
                s.line(), s.column()));
        }
    }
}
