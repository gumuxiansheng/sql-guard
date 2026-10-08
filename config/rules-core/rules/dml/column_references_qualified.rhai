// column_references_qualified.rhai (DML005)
// 多表查询中，列引用必须带表名限定（t.col），避免歧义。
// 基于 AST 检查多表 SELECT 中是否存在裸列引用。
//
// 反面案例（会被拦截）：
//   SELECT id, name FROM users u JOIN orders o ON u.id = o.user_id;   -- 裸列 id/name
//
// 正面案例（放行）：
//   SELECT u.id, u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_unqualified_column() {
            violations.push(violation("Column reference should be qualified with table name in multi-table query. Use table.column syntax.", s.line(), s.column()));
        }
    }
}
