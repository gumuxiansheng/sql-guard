// subquery_alias_required.rhai (DML004)
// FROM 子句中的子查询必须起别名，否则所有数据库都会报错。
// 基于 AST 检查 FROM 子句中的派生表（子查询）是否有别名。

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
        if sel.has_subquery_in_from() && !sel.from_subquery_has_alias() {
            violations.push(#{
                "message": "Subquery in FROM clause must have an alias: SELECT ... FROM (SELECT ...) AS alias",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
