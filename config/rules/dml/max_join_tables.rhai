// max_join_tables.rhai (DML110)
// 单条查询（FROM 主表 + 各 JOIN 表 + 隐式逗号 JOIN 表）最多关联 5 张表，
// 防止出现难以优化和阅读的超大 JOIN。
// 基于 AST 的 table_count()，对每个查询层（含递归子查询）单独计数。
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

// 递归检查 select 及其子查询的表数是否超过上限
fn exceeds_table_limit(sel) {
    if sel.table_count() > 5 {
        return true;
    }
    for sub in sel.subqueries() {
        if exceeds_table_limit(sub) {
            return true;
        }
    }
    return false;
}

for s in ast.statements() {
    if s.has_select() {
        if exceeds_table_limit(s.select()) {
            violations.push(violation_line("Query joins more than 5 tables (maximum is 5). Consider splitting the query or using a CTE/view.", 1));
        }
    }
}
