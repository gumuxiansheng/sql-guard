// gaussdb_no_reserved_prefix.rhai (GNAM003)
// GaussDB 规范 G-NAM-03：禁止创建以 pg_ / gs_ / adm_ / my_ / db_ 为前缀的对象名称
// （这些是数据库系统预留前缀）。
//
// 判定口径：内置 has_reserved_prefix 对**叶子段**判定（大小写不敏感），
// 因此 `ofsm.pg_class` 也能命中。
//
// 反面案例（会被拦截）：
//   CREATE TABLE pg_legacy (id INT);
//   CREATE INDEX gs_idx ON users (id);
//   CREATE TABLE users (db_id INT);
//
// 正面案例（放行）：
//   CREATE TABLE legacy_orders (id INT);
//   CREATE TABLE users (id INT);

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let n = ct.table_name();
        if has_reserved_prefix(n) {
            violations.push(violation(
                "object name '" + n + "' uses a system-reserved prefix (pg_/gs_/adm_/my_/db_)",
                s.line(), s.column()));
        }
        for c in ct.columns() {
            if has_reserved_prefix(c.name()) {
                violations.push(violation(
                    "column name '" + c.name() + "' uses a system-reserved prefix (pg_/gs_/adm_/my_/db_)",
                    s.line(), s.column()));
            }
        }
    }
    if s.has_create_index() {
        let n = s.create_index().name();
        if n.len() > 0 && has_reserved_prefix(n) {
            violations.push(violation(
                "index name '" + n + "' uses a system-reserved prefix (pg_/gs_/adm_/my_/db_)",
                s.line(), s.column()));
        }
    }
    if s.has_create_view() {
        let n = s.create_view().name();
        if has_reserved_prefix(n) {
            violations.push(violation(
                "view name '" + n + "' uses a system-reserved prefix (pg_/gs_/adm_/my_/db_)",
                s.line(), s.column()));
        }
    }
}
