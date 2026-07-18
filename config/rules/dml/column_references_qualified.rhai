// column_references_qualified.rhai (DML005)
// 多表查询中，列引用必须带表名限定（t.col），避免歧义。
// 原因：当查询涉及多张表时，未限定的列名可能导致歧义或意外绑定，
// 且降低可读性。SQLFluff RF02 等效规则。
// 基于 AST 检查多表 SELECT 中是否存在裸列引用。

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
        if sel.has_unqualified_column() {
            violations.push(#{
                "message": "Column reference should be qualified with table name in multi-table query. Use table.column syntax.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
