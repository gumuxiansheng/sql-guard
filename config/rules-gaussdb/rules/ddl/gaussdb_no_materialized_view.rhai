// gaussdb_no_materialized_view.rhai (GOBJ001)
// GaussDB 规范 G-OBJ-01：禁止使用物化视图。
//
// 判定口径：ViewInfo.materialized()（CREATE MATERIALIZED VIEW 时为 true）。
//
// 反面案例（会被拦截）：
//   CREATE MATERIALIZED VIEW mv_orders AS SELECT id FROM orders;
//
// 正面案例（放行）：
//   CREATE VIEW v_orders AS SELECT id FROM orders;   -- 仍会被 GOBJ003 提示"不建议使用视图"

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_view() {
        let v = s.create_view();
        if v.materialized() {
            violations.push(violation(
                "materialized view '" + v.name() + "' is not allowed; use a regular table or view instead",
                s.line(), s.column()));
        }
    }
}
