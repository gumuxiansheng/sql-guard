// gaussdb_no_system_column.rhai (GTYP002)
// GaussDB 规范 G-TYP-03：禁止使用 CTID / CID / OID / XID / TID / XMIN / CMIN /
// XMAX / CMAX 作为业务字段（这些是系统列，占用会导致语义冲突与升级风险）。
//
// 判定口径：对 CREATE TABLE 的每一列做大小写不敏感比对（不剥离引号：
// 带引号的 "ctid" 同样属于违规命名，只是会先被 GNAM002 拦下）。
//
// 反面案例（会被拦截）：
//   CREATE TABLE t (id INT, ctid INT, oid BIGINT);
//
// 正面案例（放行）：
//   CREATE TABLE t (id INT, row_no INT, xid_ref VARCHAR(32));

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let system_columns = ["CTID", "CID", "OID", "XID", "TID", "XMIN", "CMIN", "XMAX", "CMAX"];

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        for c in ct.columns() {
            if c.name().to_upper() in system_columns {
                violations.push(violation(
                    "column '" + c.name() + "' of table '" + ct.table_name() +
                    "' uses a reserved system column name",
                    s.line(), s.column()));
            }
        }
    }
}
