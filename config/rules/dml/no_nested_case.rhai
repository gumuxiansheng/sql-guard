// no_nested_case.rhai (DML107)
// 避免嵌套 CASE 表达式，优先使用简单 CASE。
// 嵌套 CASE（CASE WHEN ... CASE WHEN ...）可读性差且易出错。
//
// 反面案例（会被拦截）：
//   SELECT CASE WHEN status = 1 THEN
//       CASE WHEN type = 'A' THEN 'Type A Active' ELSE 'Active' END
//   ELSE 'Inactive' END FROM orders;
//
// 正面案例（放行）：
//   SELECT CASE status
//       WHEN 1 THEN 'Active'
//       WHEN 0 THEN 'Inactive'
//       ELSE 'Unknown'
//   END FROM orders;

let sql = context["sql_content"];
let upper = sql.to_upper();

// 按 "CASE" 分割，统计片段数减 1 即为 CASE 出现次数
let parts = upper.split("CASE");
let case_count = parts.len() - 1;

if case_count > 1 {
    violations.push(violation_msg("Nested CASE expressions reduce readability. Consider rewriting with simple CASE or extracting into subqueries."));
}
