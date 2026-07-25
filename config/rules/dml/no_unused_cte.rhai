// no_unused_cte.rhai (DML102)
// 检测未使用的 CTE（WITH 子句）。
// 基于 AST：通过 sel.ctes() 提取所有 CTE 名称，再检查名称是否在
// 主查询的 projection / where_clause / from_table / from_table_alias / joins
// 中被引用。相比早期字符串 split(" AS (") 启发式，AST 方案能精确处理
// 多 CTE、递归 CTE、列别名等场景。SQLFluff ST03 等效规则。
//
// 反面案例（会被拦截）：
//   WITH unused_cte AS (
//       SELECT * FROM orders WHERE total > 100
//   )
//   SELECT id, name FROM users;            -- unused_cte 定义后从未被引用
//
// 正面案例（放行）：
//   WITH active_users AS (
//       SELECT id, name FROM users WHERE status = 'active'
//   )
//   SELECT * FROM active_users;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_cte() {
            let visible = "";
            for col in sel.projection() {
                visible += " " + col;
            }
            visible += " " + sel.where_clause();
            visible += " " + sel.from_table();
            visible += " " + sel.from_table_alias();
            for j in sel.joins() {
                visible += " " + j.table_name();
                visible += " " + j.alias();
                visible += " " + j.condition_text();
            }
            for sub in sel.subqueries() {
                for col in sub.projection() {
                    visible += " " + col;
                }
                visible += " " + sub.where_clause();
                visible += " " + sub.from_table();
                visible += " " + sub.from_table_alias();
                for j in sub.joins() {
                    visible += " " + j.table_name();
                    visible += " " + j.alias();
                }
            }
            let upper_visible = visible.to_upper();

            for cte in sel.ctes() {
                let cte_name = cte.name().to_upper();
                if cte_name == "" {
                    continue;
                }
                if !upper_visible.contains(cte_name) {
                    violations.push(violation("CTE '" + cte.name() + "' is defined but not referenced in the main query or its subqueries. Remove unused CTE.", s.line(), s.column()));
                }
            }
        }
    }
}
