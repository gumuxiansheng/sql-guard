// no_nested_case.rhai (DML107)
// 避免嵌套 CASE 表达式，优先使用简单 CASE。
// 原因：嵌套 CASE（CASE WHEN ... CASE WHEN ...）可读性差且易出错，
// 简单 CASE（CASE expr WHEN ...）通常可表达相同逻辑。
// SQLFluff ST04 等效规则。

let sql = context["sql_content"];
let upper = sql.to_upper();

// 按 "CASE" 分割，统计片段数减 1 即为 CASE 出现次数
let parts = upper.split("CASE");
let case_count = parts.len() - 1;

if case_count > 1 {
    violations.push("Nested CASE expressions reduce readability. Consider rewriting with simple CASE or extracting into subqueries.");
}
