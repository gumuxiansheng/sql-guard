// no_select_all.rhai (DML001)
// 禁止使用 SELECT *，必须显式列出所需列。
// 原因：SELECT * 会返回所有列，可能导致不必要的数据传输、
// 隐式依赖表结构、并破坏视图/物化视图的兼容性。
// 基于 AST 精确识别通配符，不会误判注释或字符串字面量。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
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
