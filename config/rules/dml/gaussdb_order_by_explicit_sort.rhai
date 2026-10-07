// gaussdb_order_by_explicit_sort.rhai (GDML005)
// GaussDB 规范 G-DML-17：ORDER BY 子句必须显式指定排序方式（ASC/DESC）、
// NULL 的排序方式（NULL FIRST / NULL LAST）。
//
// 判定口径：SelectInfo.has_order_by_without_direction() /
// has_order_by_without_nulls_spec()。这两者是 M1 新增的"三态"判定——
// 能区分「原文未写」与「显式写为 ASC」，而"未写"正是本规范的违规点。
//
// 递归：对子查询内的 ORDER BY 同样检查（先扁平化所有查询层，再在顶层上报）。
//
// 反面案例（会被拦截）：
//   SELECT id FROM t ORDER BY id;              -- 未指定 ASC/DESC，也未指定 NULLS
//   SELECT id FROM t ORDER BY id ASC;          -- 未指定 NULLS FIRST/LAST
//
// 正面案例（放行）：
//   SELECT id FROM t ORDER BY id ASC NULLS LAST;
//   SELECT id FROM t;                          -- 无 ORDER BY，不检查

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

/// 扁平化一个查询及其全部子查询层，返回 Array<SelectInfo>。
/// ⚠️ Rhai 的 fn 无法访问外层 scope 变量（如 violations），
/// 所以这里只做收集，上报统一在顶层进行。
fn collect_selects(sel) {
    let out = [sel];
    for sub in sel.subqueries() {
        for deeper in collect_selects(sub) {
            out.push(deeper);
        }
    }
    return out;
}

for s in ast.statements() {
    if s.has_select() {
        for sel in collect_selects(s.select()) {
            if !sel.has_order_by() {
                continue;
            }
            if sel.has_order_by_without_direction() {
                violations.push(violation(
                    "ORDER BY must specify ASC or DESC explicitly (" + sel.order_by_text() + ")",
                    s.line(), s.column()));
            }
            if sel.has_order_by_without_nulls_spec() {
                violations.push(violation(
                    "ORDER BY must specify NULLS FIRST or NULLS LAST explicitly (" + sel.order_by_text() + ")",
                    s.line(), s.column()));
            }
        }
    }
}
