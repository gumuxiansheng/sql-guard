// gaussdb_recommended_data_types.rhai (GTYP003)
// GaussDB 规范 G-TYP-04：优先使用附录推荐的基础数据类型。
//
// 推荐类型（规范附录）：
//   整数 TINYINT / SMALLINT / INTEGER / BIGINT；金额等精度敏感 NUMERIC / DECIMAL；
//   布尔 BOOLEAN；定长字符 CHAR(n)；变长字符 VARCHAR(n) / NVARCHAR2(n) / NCHAR(n)；
//   时间 DATE / TIME / TIMESTAMP / SMALLDATETIME / INTERVAL / REALTIME；
//   二进制 BYTEA。
//
// 判定口径：取类型文本的「基础名」（第一个 '(' 之前的部分）比对白名单。
// 未列出的类型（如 SERIAL、MONEY、UUID、INET、自定义域等）会给出 warning。
//
// 负面案例（会提示）：
//   CREATE TABLE t (id SERIAL, ip INET, amount MONEY);
//
// 正面案例（放行）：
//   CREATE TABLE t (id BIGINT, ip VARCHAR(45), amount NUMERIC(18,2));

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let recommended = [
    "TINYINT", "SMALLINT", "INTEGER", "BIGINT", "INT", "INT2", "INT4", "INT8",
    "NUMERIC", "DECIMAL", "BOOLEAN", "BOOL",
    "CHAR", "NCHAR", "VARCHAR", "NVARCHAR2", "VARCHAR2", "TEXT",
    "DATE", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "SMALLDATETIME", "INTERVAL", "REALTIME",
    "BYTEA",
];

/// 取类型基础名：TIMESTAMP(6) → TIMESTAMP
/// ⚠️ 本项目的 Rhai 构建里 String.trim() 返回 unit（不可用），故不做 trim。
fn base_type(t) {
    let lp = t.index_of("(");
    if lp >= 0 {
        return t.sub_string(0, lp).to_upper();
    }
    return t.to_upper();
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        for c in ct.columns() {
            let b = base_type(c.data_type());
            if b.len() > 0 && !(b in recommended) {
                violations.push(violation(
                    "column '" + c.name() + "' uses non-recommended type '" + c.data_type() +
                    "'; prefer a GaussDB recommended basic type",
                    s.line(), s.column()));
            }
        }
    }
}
