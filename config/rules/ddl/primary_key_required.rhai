// primary_key_required.rhai (DDL002)
// CREATE TABLE 必须定义 PRIMARY KEY，确保每张表有唯一标识。
// 基于 AST 识别列级和表级主键约束，不会误判 ALTER TABLE 或注释。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        if !ct.has_primary_key() {
            violations.push(#{
                "message": "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY (column constraint or table constraint)",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
