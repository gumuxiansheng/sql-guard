// use_is_null.rhai (DML103)
// 使用 IS NULL / IS NOT NULL 而非 = NULL / <> NULL。
// 原因：SQL 中 NULL = NULL 的结果是 NULL（不是 TRUE），
// WHERE name = NULL 永远不会返回任何行，这是常见编码错误。
// SQLFluff CV05 等效规则。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users WHERE name = NULL;     -- 永远返回空结果
//   SELECT * FROM users WHERE name <> NULL;    -- 永远返回空结果
//
// 正面案例（放行）：
//   SELECT * FROM users WHERE name IS NULL;
//   SELECT * FROM users WHERE name IS NOT NULL;

// 基于字符串匹配检查 "= NULL" / "<> NULL" / "!= NULL"
let sql = context["sql_content"];
let lines = sql.split("\n");
let line_num = 1;
for line in lines {
    let uline = line.to_upper();
    if (uline.contains("= NULL") || uline.contains("=NULL")) && !uline.contains("IS NULL") && !uline.contains("IS NOT NULL") {
        violations.push(violation_line("Use 'IS NULL' instead of '= NULL'. NULL comparisons always return FALSE with '='.", line_num));
    }
    if (uline.contains("<> NULL") || uline.contains("<>NULL") || uline.contains("!= NULL") || uline.contains("!=NULL")) && !uline.contains("IS NOT NULL") {
        violations.push(violation_line("Use 'IS NOT NULL' instead of '<> NULL' or '!= NULL'. NULL comparisons always return FALSE with '<>'.", line_num));
    }
    line_num += 1;
}
