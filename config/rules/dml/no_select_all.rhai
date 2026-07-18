// no_select_all.rhai - Disallow SELECT * in DML scripts
// 基于 AST：识别 SELECT 语句，检查投影列表是否包含通配符（* 或 table.*）。
// 相比字符串匹配，AST 不会误判注释中的 SELECT * 或字符串字面量。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "SELECT" {
        let sel = s.select();
        if sel.has_wildcard() {
            violations.push(#{
                "message": "SELECT * is not allowed. Specify columns explicitly.",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
