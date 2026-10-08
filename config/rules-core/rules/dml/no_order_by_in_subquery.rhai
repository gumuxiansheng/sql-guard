// no_order_by_in_subquery.rhai (DML105)
// 子查询中的 ORDER BY 通常无效（除非配合 LIMIT/OFFSET/FETCH）。
// 基于 AST：递归遍历 sel.subqueries()，对每个子查询检查 has_order_by() &&
// !has_limit() && !has_offset() && !has_fetch()。
// 修复了旧版字符串扫描的两个误报：
//   1. OVER(ORDER BY ...) 窗口函数 —— AST 中窗口函数的 ORDER BY 不在子查询层
//   2. ") ORDER BY" 模式在括号外（最外层查询，本来就允许）
// SQLFluff AM03 等效规则。
//
// 反面案例（会被拦截）：
//   SELECT * FROM (SELECT * FROM users ORDER BY name) AS u;   -- 内层 ORDER BY 无效
//
// 正面案例（放行）：
//   SELECT * FROM (SELECT * FROM users ORDER BY name LIMIT 10) AS u;  -- 配合 LIMIT 有效
//   SELECT * FROM (SELECT * FROM users) AS u ORDER BY name;            -- 移到外层
//   SELECT id, ROW_NUMBER() OVER (ORDER BY created_at) AS rn FROM users;  -- 窗口函数不算

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// 递归检查单个 SelectInfo 及其子查询
fn check_subquery(sel) {
    for sub in sel.subqueries() {
        if sub.has_order_by() && !sub.has_limit() && !sub.has_offset() && !sub.has_fetch() {
            return true;
        }
        if check_subquery(sub) {
            return true;
        }
    }
    return false;
}

for s in ast.statements() {
    if s.has_select() {
        let found = check_subquery(s.select());
        if found {
            violations.push(violation_line("ORDER BY in subquery is typically ignored unless combined with LIMIT/OFFSET. Consider moving ORDER BY to the outer query.", 1));
        }
    }
}
