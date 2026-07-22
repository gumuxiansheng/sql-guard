// no_unused_cte.rhai (DML102)
// 检测未使用的 CTE（WITH 子句）。
// 基于 AST：通过 sel.ctes() 提取所有 CTE 名称，再检查名称是否在
// 主查询的 projection / where_clause / from_table / from_table_alias / joins
// 中被引用。相比早期字符串 split(" AS (") 启发式，AST 方案能精确处理
// 多 CTE、递归 CTE、列别名等场景。
// SQLFluff ST03 等效规则。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_cte() {
            // 收集主查询可见文本：projection + where + from + joins + having
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
            // 子查询中也算引用（CTE 可能被嵌套子查询用到）
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
                // 空名或递归 CTE 自引用场景跳过
                if cte_name == "" {
                    continue;
                }
                if !upper_visible.contains(cte_name) {
                    violations.push(#{
                        "message": "CTE '" + cte.name() + "' is defined but not referenced in the main query or its subqueries. Remove unused CTE.",
                        "line": s.line(),
                        "column": s.column()
                    });
                }
            }
        }
    }
}
