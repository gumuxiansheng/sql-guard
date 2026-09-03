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
//
// 检测基于 AST：逐条语句统计其文本中 `CASE` 关键字出现次数（仅限该语句自身的
// 行范围，不跨语句累计），出现 1 次以上即判定为嵌套 CASE，并标注语句起始行号。
// 相比早期"全文件字符串匹配"（无法定位行号、且多条语句各含一个简单 CASE 会误报），
// 本方案可输出精确行号，且不会把跨语句的多个简单 CASE 误判为嵌套。

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let sql = context["sql_content"];
let lines = sql.split("\n");
let line_count = lines.len();

for s in ast.statements() {
    // 提取语句文本：从 s.line() 到 s.end_line()（含），行号与 sql_content 同坐标系
    let start = s.line();
    let end = s.end_line();
    if start < 1 { continue; }
    let stmt_text = "";
    let i = start;
    while i <= end && i <= line_count {
        if stmt_text != "" { stmt_text += "\n"; }
        stmt_text += lines[i - 1];
        i = i + 1;
    }

    // 按 "CASE" 分割，统计片段数减 1 即为该语句内 CASE 出现次数
    let parts = stmt_text.to_upper().split("CASE");
    if parts.len() - 1 > 1 {
        violations.push(violation("Nested CASE expressions reduce readability. Consider rewriting with simple CASE or extracting into subqueries.", s.line(), s.column()));
    }
}
