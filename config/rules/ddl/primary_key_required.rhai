// primary_key_required.rhai - CREATE TABLE must have a PRIMARY KEY
// 基于 AST：识别 CREATE TABLE 语句，检查列定义或表级约束中是否包含 PRIMARY KEY。
// 相比字符串匹配，AST 能正确识别 CREATE TABLE 结构，不会误判 ALTER TABLE 或注释。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

let stmts = ast.statements();
for s in stmts {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        if !ct.has_primary_key() {
            violations.push(#{
                "message": "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
