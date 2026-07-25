// no_unused_join.rhai (DML101)
// 检测可能未使用的 JOIN 关系。
// 基于 AST：优先用 JOIN 别名（j.alias()）去匹配投影列列表，无别名时回退到表名。
// 修复了旧版 "JOIN orders o" 但投影是 o.id 时的误报（旧版用 orders 匹配失败）。
// SQLFluff ST11 等效规则。
//
// 反面案例（会被拦截）：
//   SELECT u.id, u.name FROM users u JOIN unused_table t ON u.id = t.user_id;
//   -- JOIN 的表（别名 t / 表名 unused_table）未出现在 SELECT 与 WHERE 中
//
// 正面案例（放行）：
//   SELECT u.id, u.name FROM users u;                              -- 无多余 JOIN
//   SELECT o.id FROM orders o JOIN users u ON o.user_id = u.id;    -- u 经 ON 条件引用

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_joins() {
            let projection_text = "";
            for col in sel.projection() {
                projection_text += " " + col;
            }
            projection_text += " " + sel.where_clause();
            let upper_proj = projection_text.to_upper();

            for j in sel.joins() {
                let needle = j.table_name().to_upper();
                if j.has_alias() {
                    needle = j.alias().to_upper();
                }
                if needle == "" {
                    continue;
                }
                if j.join_type() == "CROSS" {
                    continue;
                }
                let table_upper = j.table_name().to_upper();
                if !upper_proj.contains(needle) && !upper_proj.contains(table_upper) {
                    violations.push(violation("Possible unused JOIN to table '" + j.table_name() + "' (alias '" + j.alias() + "'). The table columns are not referenced in SELECT list or WHERE clause.", s.line(), s.column()));
                }
            }
        }
    }
}
