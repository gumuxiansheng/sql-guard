// gaussdb_object_name_charset.rhai (GNAM001)
// GaussDB 规范 G-NAM-01：数据库对象名只能使用字母、数字和下划线的组合。
//
// 判定口径：先 strip_quotes 去掉成对引号，再用 is_valid_ident 校验字符集
// （非空、仅 [A-Za-z0-9_]、不以数字开头）。
//
// ⚠️ 与 GNAM002 是互补关系：未加引号的非法名字 sqlparser 通常直接解析失败
// （连 AST 都没有，规则看不到），所以本规则的实际作用集中在
// "校验带引号名字去掉引号之后的字符集" 上。
//
// 反面案例（会被拦截）：
//   CREATE TABLE "user-orders" (id INT);   -- 含短横线
//   CREATE TABLE "订单表" (id INT);         -- 非 ASCII
//
// 正面案例（放行）：
//   CREATE TABLE user_orders (id INT);
//   CREATE TABLE user_orders_v2 (id INT);

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let n = ct.table_name();
        if !is_valid_ident(strip_quotes(n)) {
            violations.push(violation(
                "object name '" + n + "' must contain only letters, digits and underscores",
                s.line(), s.column()));
        }
        for c in ct.columns() {
            if !is_valid_ident(strip_quotes(c.name())) {
                violations.push(violation(
                    "column name '" + c.name() + "' must contain only letters, digits and underscores",
                    s.line(), s.column()));
            }
        }
    }
    if s.has_create_index() {
        let n = s.create_index().name();
        if n.len() > 0 && !is_valid_ident(strip_quotes(n)) {
            violations.push(violation(
                "index name '" + n + "' must contain only letters, digits and underscores",
                s.line(), s.column()));
        }
    }
    if s.has_create_view() {
        let n = s.create_view().name();
        if !is_valid_ident(strip_quotes(n)) {
            violations.push(violation(
                "view name '" + n + "' must contain only letters, digits and underscores",
                s.line(), s.column()));
        }
    }
}
