// no_delete_update_without_where.rhai (DML002)
// DELETE 和 UPDATE 语句必须带 WHERE 条件，防止全表误操作。
// 基于 AST 精确判断 WHERE 子句是否存在。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_delete() {
        let d = s.delete();
        if !d.has_where() {
            violations.push(#{
                "message": "DELETE without WHERE clause will remove all rows from table: " + d.table_name() + ". Add a WHERE condition or use TRUNCATE if intentional.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
    if s.has_update() {
        let u = s.update();
        if !u.has_where() {
            violations.push(#{
                "message": "UPDATE without WHERE clause will modify all rows in table: " + u.table_name() + ". Add a WHERE condition.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
