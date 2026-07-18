// insert_columns_required.rhai (DML003)
// INSERT 语句必须显式指定列名，禁止 INSERT INTO t VALUES(...)。
// 原因：不指定列名时，INSERT 隐式依赖表结构的列顺序，表结构变更会导致静默错误。
// 显式指定列名使代码自文档化，且对表结构变更更鲁棒。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

for s in ast.statements() {
    if s.has_insert() {
        let ins = s.insert();
        if !ins.has_columns() {
            violations.push(#{
                "message": "INSERT must specify target columns explicitly: INSERT INTO " + ins.table_name() + " (col1, col2, ...) VALUES (...)",
                "line": s.line(),
                "column": s.column()
            });
        }
    }
}
