// no_unused_join.rhai (DML101)
// 检测可能未使用的 JOIN 关系。
// 基于 AST：优先用 JOIN 别名（j.alias()）去匹配投影列表，无别名时回退到表名。
// 修复了旧版"JOIN orders o"但投影是 o.id 时的误报（旧版用 orders 匹配失败）。
// SQLFluff ST11 等效规则。

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
        if sel.has_joins() {
            // 汇总投影文本 + WHERE 子句（JOIN 表常出现在 WHERE 条件里）
            let projection_text = "";
            for col in sel.projection() {
                projection_text += " " + col;
            }
            projection_text += " " + sel.where_clause();
            let upper_proj = projection_text.to_upper();

            for j in sel.joins() {
                // 优先用别名匹配（别名出现在投影列前缀中），无别名再用表名
                let needle = j.table_name().to_upper();
                if j.has_alias() {
                    needle = j.alias().to_upper();
                }
                // CROSS JOIN 与无 needle 的不报
                if needle == "" {
                    continue;
                }
                if j.join_type() == "CROSS" {
                    continue;
                }
                // 也把表名作为补充 needle：用户可能用 orders.id 而非 o.id
                let table_upper = j.table_name().to_upper();
                if !upper_proj.contains(needle) && !upper_proj.contains(table_upper) {
                    violations.push(#{
                        "message": "Possible unused JOIN to table '" + j.table_name() + "' (alias '" + j.alias() + "'). The table columns are not referenced in SELECT list or WHERE clause.",
                        "line": s.line(),
                        "column": s.column()
                    });
                }
            }
        }
    }
}
