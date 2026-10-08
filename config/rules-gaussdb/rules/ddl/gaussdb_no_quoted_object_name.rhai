// gaussdb_no_quoted_object_name.rhai (GNAM002)
// GaussDB 规范 G-NAM-02：禁止使用双引号括起来的字符串来定义数据库对象名称。
//
// 判定口径：sqlparser 的 ObjectName Display 会**保留**引号样式，
// 因此 `CREATE TABLE "Users"` 的 table_name() 返回 `"Users"`（带引号），
// is_quoted_ident 直接可判，无需读原文。
//
// 反面案例（会被拦截）：
//   CREATE TABLE "Users" (id INT);
//   CREATE INDEX "idx_users_id" ON "Users" (id);
//   CREATE TABLE t (id INT, "name" VARCHAR(20));
//
// 正面案例（放行）：
//   CREATE TABLE users (id INT, name VARCHAR(20));

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let n = ct.table_name();
        if is_quoted_ident(n) {
            violations.push(violation(
                "object name '" + n + "' must not be defined with quotes",
                s.line(), s.column()));
        }
        for c in ct.columns() {
            if is_quoted_ident(c.name()) {
                violations.push(violation(
                    "column name '" + c.name() + "' must not be defined with quotes",
                    s.line(), s.column()));
            }
        }
    }
    if s.has_create_index() {
        let ci = s.create_index();
        let n = ci.name();
        if n.len() > 0 && is_quoted_ident(n) {
            violations.push(violation(
                "index name '" + n + "' must not be defined with quotes",
                s.line(), s.column()));
        }
        if is_quoted_ident(ci.table_name()) {
            violations.push(violation(
                "index target table '" + ci.table_name() + "' must not be defined with quotes",
                s.line(), s.column()));
        }
    }
    if s.has_create_view() {
        let n = s.create_view().name();
        if is_quoted_ident(n) {
            violations.push(violation(
                "view name '" + n + "' must not be defined with quotes",
                s.line(), s.column()));
        }
    }
}
