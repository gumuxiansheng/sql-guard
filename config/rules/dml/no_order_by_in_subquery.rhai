// no_order_by_in_subquery.rhai (DML105)
// 子查询中的 ORDER BY 通常无效（除非使用 LIMIT/OFFSET）。
// 原因：SQL 标准中，子查询是逻辑无序的，ORDER BY 在内层子查询
// 通常被优化器忽略，只有在配合 LIMIT 时才有效。
// SQLFluff AM03 等效规则。

let sql = context["sql_content"];
let upper = sql.to_upper();

// 启发式：检查子查询模式 (...) ORDER BY ... 且不是最外层
// 简化实现：检查是否包含 ") ORDER BY" 模式
let lines = sql.split("\n");
let mut in_subquery = false;
let mut paren_depth = 0;
let mut found_issue = false;

for line in lines {
    let uline = line.to_upper();
    for ch in uline.chars() {
        if ch == '(' { paren_depth += 1; }
        if ch == ')' { paren_depth -= 1; }
    }
    // 如果在子查询内（有未闭合的括号）且包含 ORDER BY，且不包含 LIMIT
    if paren_depth > 0 && uline.contains("ORDER BY") && !uline.contains("LIMIT") && !found_issue {
        violations.push(#{
            "message": "ORDER BY in subquery is typically ignored unless combined with LIMIT/OFFSET. Consider moving ORDER BY to the outer query.",
            "line": 1
        });
        found_issue = true;
    }
}
