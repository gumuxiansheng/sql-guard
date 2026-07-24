// order_by_required_for_pagination.rhai (DML007)
// 分页查询必须带 ORDER BY 排序条件，否则每次返回的结果集不确定。
// 若业务方明确需要随机排序展示数据，可在 SQL 中添加注释豁免：
//   -- RANDOM_ORDER
// 或
//   /* RANDOM_ORDER */

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let sql_upper = context["sql_content"].to_upper();
if sql_upper.contains("RANDOM_ORDER") {
    return;
}

fn check_subquery(sel) {
    let is_pagination = sel.has_limit() || sel.has_offset() || sel.has_fetch();
    if is_pagination && !sel.has_order_by() {
        return true;
    }
    for sub in sel.subqueries() {
        if check_subquery(sub) {
            return true;
        }
    }
    return false;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        if check_subquery(s.select()) {
            violations.push(violation("Pagination query (LIMIT/OFFSET/FETCH) without ORDER BY produces non-deterministic results. Add an ORDER BY clause, or if random ordering is intentional, add '-- RANDOM_ORDER' comment to suppress.", s.line(), s.column()));
        }
    }
}
