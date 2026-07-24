// no_drop_table.rhai (DDL001)
// 禁止在 DDL 脚本中使用 DROP TABLE，防止误删生产表。
// 基于 AST 精确识别，不会误匹配注释或字符串字面量。

if guard_parse_error(context) {
    violations.push(#{"message": "SQL parse error: " + context["ast"].parse_error(), "line": 1});
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(violation("DROP TABLE is not allowed in DDL scripts, use a dedicated migration instead: " + d.name(), s.line(), s.column()));
    }
}
