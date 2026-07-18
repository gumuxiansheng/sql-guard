// union_all_preferred.rhai (DML106)
// 除非需要去重，否则优先使用 UNION ALL 而非 UNION。
// 原因：UNION 会执行隐式 DISTINCT（排序去重），性能低于 UNION ALL。
// 如果业务不需要去重，UNION ALL 更快且语义更清晰。
// SQLFluff AM02 等效规则。

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
        if sel.is_union() && !sel.is_union_all() {
            violations.push(#{
                "message": "UNION performs implicit DISTINCT. Use UNION ALL if duplicate elimination is not required, or explicitly use UNION DISTINCT for clarity.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
