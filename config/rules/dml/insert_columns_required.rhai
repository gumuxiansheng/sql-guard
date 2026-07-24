// insert_columns_required.rhai (DML003)
// INSERT 语句必须显式指定列名，禁止 INSERT INTO t VALUES(...)。
// 显式指定列名使代码自文档化，且对表结构变更更鲁棒。

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_insert() {
        let ins = s.insert();
        if !ins.has_columns() {
            violations.push(violation("INSERT must specify target columns explicitly: INSERT INTO " + ins.table_name() + " (col1, col2, ...) VALUES (...)", s.line(), s.column()));
        }
    }
}
