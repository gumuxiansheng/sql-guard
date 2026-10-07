// max_join_tables.rhai (DML110)
// 单条查询（FROM 主表 + 各 JOIN 表 + 隐式逗号 JOIN 表）最多关联 5 张表，
// 防止出现难以优化和阅读的超大 JOIN。
// 基于 AST 的 table_count()，对每个查询层（含递归子查询）单独计数。
//
// 可配参数（[rules.params]）：
//   max_join_tables = 5      # 默认 5；GaussDB 规范建议联机场景 ≤3、批量场景 ≤5
//                            # （GaussDB 项目建议配 3，见 docs/gaussdb-rule-gap-analysis.md）
//
// 反面案例（会被拦截）：
//   SELECT * FROM t1
//   JOIN t2 ON ... JOIN t3 ON ... JOIN t4 ON ... JOIN t5 ON ... JOIN t6 ON ...;  -- 6 张表
//
// 正面案例（放行）：
//   SELECT * FROM t1 JOIN t2 ON ... JOIN t3 ON ... JOIN t4 ON ... JOIN t5 ON ...;  -- 恰好 5 张表

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let p = context["params"];

let limit = 5;
if "max_join_tables" in p {
    limit = p["max_join_tables"];
}

// 递归检查 select 及其子查询的表数是否超过上限。
// ⚠️ Rhai 的 fn 无法访问外层 scope 变量（实测报 "Variable not found"），
// 因此阈值必须作为参数显式传入。
fn exceeds_table_limit(sel, limit) {
    if sel.table_count() > limit {
        return true;
    }
    for sub in sel.subqueries() {
        if exceeds_table_limit(sub, limit) {
            return true;
        }
    }
    return false;
}

for s in ast.statements() {
    if s.has_select() {
        if exceeds_table_limit(s.select(), limit) {
            violations.push(violation(
                "Query joins more than " + limit.to_string() + " tables. Consider splitting the query or using a CTE/view.",
                s.line(), s.column()));
        }
    }
}
