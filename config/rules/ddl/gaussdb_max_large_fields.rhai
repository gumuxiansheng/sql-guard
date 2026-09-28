// gaussdb_max_large_fields.rhai (GTYP001)
// GaussDB 规范 G-TYP-02：单表大字段（例如 VARCHAR(1000)）宜不超过 8 个。
//
// 「大字段」定义（规范术语）：
//   1) 存在数据与原表分离存储的字段类型：TEXT / CLOB / BLOB / BYTEA / RAW / JSON / XML 等；
//   2) 或者字符串类型声明长度超过 2000 字节（GaussDB Oracle 兼容模式下 VARCHAR(n) 的 n 即字节）。
//
// 实现说明：长度解析用 helpers.rhai 的 parse_int_safe（Rhai 内建 parse_int
// 遇到非数字输入会抛错，不适合读用户书写的类型文本）。
//
// 可配参数（[rules.params]）：
//   max_large_fields  = 8      # 单表大字段个数上限
//   large_field_bytes = 2000   # 字符串类型超过该字节数即视为大字段
//
// 反面案例（会被拦截）：
//   CREATE TABLE t (a VARCHAR(4000), b VARCHAR(4000), c TEXT, d TEXT, e TEXT,
//                   f TEXT, g TEXT, h TEXT, i TEXT);   -- 9 个大字段
//
// 正面案例（放行）：
//   CREATE TABLE t (id INT, name VARCHAR(64), note VARCHAR(512));

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let p = context["params"];

let max_large_fields = 8;
if "max_large_fields" in p {
    max_large_fields = p["max_large_fields"];
}
let large_field_bytes = 2000;
if "large_field_bytes" in p {
    large_field_bytes = p["large_field_bytes"];
}

/// 取类型声明中的长度（VARCHAR(1000) → 1000）；未声明长度或非纯数字返回 -1。
fn declared_length(t) {
    let lp = t.index_of("(");
    let rp = t.index_of(")");
    if lp >= 0 && rp > lp {
        // ⚠️ 本项目的 Rhai 构建里 String.trim() 返回 unit（不可用），
        //    这里改用 index_of 判定而非先 trim。
        let inner = t.sub_string(lp + 1, rp - lp - 1);
        // 只接受纯数字：Rhai 内建 parse_int 对非数字会抛错，先自行校验
        let digits = "0123456789";
        let ok = inner.len() > 0;
        let i = 0;
        while i < inner.len() {
            if digits.index_of(char_at(inner, i)) < 0 { ok = false; break; }
            i = i + 1;
        }
        if !ok { return -1; }
        return parse_int_safe(inner);
    }
    return -1;
}

/// 是否为大字段。threshold 为字符串类型的字节阈值。
fn is_large_field(t, threshold) {
    let u = t.to_upper();
    // 1) 数据与原表分离存储的类型
    if u.contains("TEXT") { return true; }
    if u.contains("CLOB") { return true; }
    if u.contains("BLOB") { return true; }
    if u.contains("BYTEA") { return true; }
    if u.contains("RAW") { return true; }
    if u.contains("JSON") { return true; }
    if u.contains("XML") { return true; }
    if u.contains("GEOMETRY") { return true; }
    // 2) 字符串类型声明长度超过阈值
    let n = declared_length(u);
    if n > threshold { return true; }
    return false;
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let big = [];
        for c in ct.columns() {
            if is_large_field(c.data_type(), large_field_bytes) {
                big.push(c.name() + " " + c.data_type());
            }
        }
        if big.len() > max_large_fields {
            violations.push(violation(
                "table '" + ct.table_name() + "' has " + big.len().to_string() +
                " large fields (max " + max_large_fields.to_string() + "): " + join_strs(big, ", "),
                s.line(), s.column()));
        }
    }
}
