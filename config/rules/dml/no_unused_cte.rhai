// no_unused_cte.rhai (DML102)
// 检测未使用的 CTE（WITH 子句）。
// 启发式：如果 CTE 的名称没有在主查询中被引用，则该 CTE 未使用。
// 注意：基于字符串匹配，可能误报含递归引用的情况。
// SQLFluff ST03 等效规则。

let sql = context["sql_content"];
let upper = sql.to_upper();

// 使用字符串匹配：检查 WITH ... AS (...) 定义的 CTE 名称是否在 WITH 块之后被引用
// 匹配 WITH name AS ( 或 WITH RECURSIVE name AS (
let with_pattern = "WITH ";
let cte_start = upper.find(with_pattern);
if cte_start != () {
    let after_with = upper.sub_string(cte_start.len());
    // 提取 CTE 名称：WITH name1 AS (...), name2 AS (...)
    // 简单方式：以 AS ( 分割
    let parts = after_with.split(" AS (");
    let first_cte_name = "";
    if parts.len() > 0 {
        let first_part = parts[0].trim();
        let name_end = first_part.find(" ");
        if name_end != () {
            first_cte_name = first_part.sub_string(0, name_end).trim();
        } else {
            first_cte_name = first_part;
        }
    }

    if first_cte_name != "" {
        // 检查 CTE 名称是否出现在主查询中（WITH ... AS (...) 之后的部分）
        let main_query_start = after_with.find(") ");
        if main_query_start != () {
            let main_query = after_with.sub_string(main_query_start);
            if !main_query.to_upper().contains(first_cte_name) {
                violations.push(#{
                    "message": "CTE '" + first_cte_name + "' is defined but not used in the main query. Remove unused CTE.",
                    "line": 1
                });
            }
        }
    }
}
