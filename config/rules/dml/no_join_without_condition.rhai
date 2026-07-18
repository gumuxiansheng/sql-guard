// no_join_without_condition.rhai (DML006)
// JOIN 必须带 ON 条件，防止产生无意的笛卡尔积。
// 原因：无条件的 JOIN（CROSS JOIN 除外）通常是编码错误，
// 会导致大量无用数据行。SQLFluff AM05 等效规则。
// 基于 AST 检查 JOIN 子句是否包含 ON 或 USING 条件。

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
        if sel.has_join_without_condition() {
            violations.push(#{
                "message": "JOIN without ON or USING condition causes a Cartesian product. Add an explicit join condition.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
