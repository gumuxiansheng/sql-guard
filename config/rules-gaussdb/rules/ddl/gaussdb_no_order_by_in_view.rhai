// gaussdb_no_order_by_in_view.rhai (GOBJ002)
// GaussDB 规范 G-OBJ-02：禁止在视图中进行排序操作。
//
// 判定口径：ViewInfo.definition() 返回视图定义体（AS SELECT ...）的 SelectInfo，
// 直接看 has_order_by()。这是 M1 新增 ViewInfo.definition 的直接成果——
// 在此之前"视图内排序"完全不可判。
//
// 反面案例（会被拦截）：
//   CREATE VIEW v_orders AS SELECT id FROM orders ORDER BY id;
//
// 正面案例（放行）：
//   CREATE VIEW v_orders AS SELECT id FROM orders;
//   -- 需要有序时由查询方写 ORDER BY

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_view() {
        let v = s.create_view();
        if v.has_definition() && v.definition().has_order_by() {
            violations.push(violation(
                "view '" + v.name() + "' must not contain ORDER BY; let the caller sort instead",
                s.line(), s.column()));
        }
    }
}
