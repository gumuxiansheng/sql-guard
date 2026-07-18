// no_drop_table.rhai - Disallow DROP TABLE in DDL scripts
// 基于 AST：遍历语句列表，对 DROP_TABLE 类型语句上报违规。
// 相比字符串匹配，AST 能精确识别语句类型，不会误匹配注释或子串。

let ast = context["ast"];

// 解析失败时也上报，避免漏检
if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(#{
            "message": "DROP TABLE is not allowed in DDL scripts: " + d.name(),
            "line": s.line(),
            "column": s.column()
        });
    }
}
