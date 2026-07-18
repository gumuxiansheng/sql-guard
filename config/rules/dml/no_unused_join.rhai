// no_unused_join.rhai (DML101)
// 检测可能未使用的 JOIN 关系。
// 启发式：如果 JOIN 的表没有列在 SELECT 投影列表中（通过表名判断），可能未使用。
// 注意：此规则基于字符串匹配，可能存在误报，需人工复核。
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
            let projection_text = "";
            for col in sel.projection() {
                projection_text += col;
            }
            let upper_proj = projection_text.to_upper();

            for j in sel.joins() {
                let jt = j.table_name().to_upper();
                // 如果 JOIN 表名没有在投影列表中出现（忽略投影 * 的模糊情况），
                // 且不是 CROSS JOIN，可能未使用
                if !upper_proj.contains(jt) && j.join_type() != "CROSS" && j.join_type() != "" {
                    violations.push(#{
                        "message": "Possible unused JOIN to table '" + j.table_name() + "'. The table columns are not referenced in SELECT list.",
                        "line": s.line(),
                        "column": s.column()
                    });
                }
            }
        }
    }
}
