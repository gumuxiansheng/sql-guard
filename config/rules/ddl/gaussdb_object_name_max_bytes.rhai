// gaussdb_object_name_max_bytes.rhai (GNAM004)
// GaussDB 规范 G-NAM-04：数据库对象的名称长度禁止超过 63 个字节。
//
// 判定口径：必须用内置 len_bytes（UTF-8 字节数）。Rhai 的 String.len() 返回
// **字符数**，中文/多字节场景下会低估，不能替代。
//
// 可配参数（[rules.params]）：
//   name_max_bytes = 63      # 默认 63
//
// 反面案例（会被拦截）：
//   CREATE TABLE t_aaaa...(超过 63 字节) (id INT);
//   CREATE TABLE 用户订单明细汇总表（中文名按字节计）(id INT);
//
// 正面案例（放行）：
//   CREATE TABLE user_orders (id INT);

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let p = context["params"];

let limit = 63;
if "name_max_bytes" in p {
    limit = p["name_max_bytes"];
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let n = ct.table_name();
        let b = len_bytes(n);
        if b > limit {
            violations.push(violation(
                "object name '" + n + "' is " + b.to_string() + " bytes, exceeding the " + limit.to_string() + "-byte limit",
                s.line(), s.column()));
        }
        for c in ct.columns() {
            let cb = len_bytes(c.name());
            if cb > limit {
                violations.push(violation(
                    "column name '" + c.name() + "' is " + cb.to_string() + " bytes, exceeding the " + limit.to_string() + "-byte limit",
                    s.line(), s.column()));
            }
        }
    }
    if s.has_create_index() {
        let n = s.create_index().name();
        if n.len() > 0 && len_bytes(n) > limit {
            violations.push(violation(
                "index name '" + n + "' is " + len_bytes(n).to_string() + " bytes, exceeding the " + limit.to_string() + "-byte limit",
                s.line(), s.column()));
        }
    }
    if s.has_create_view() {
        let n = s.create_view().name();
        if len_bytes(n) > limit {
            violations.push(violation(
                "view name '" + n + "' is " + len_bytes(n).to_string() + " bytes, exceeding the " + limit.to_string() + "-byte limit",
                s.line(), s.column()));
        }
    }
}
