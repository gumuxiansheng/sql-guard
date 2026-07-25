// no_constant_where.rhai (DML108)
// 避免 WHERE 子句中使用常量条件（WHERE 1=1, WHERE true 等）。
// 原因：WHERE 1=1 通常是动态 SQL 拼接的产物，虽无害但非必要；
// WHERE false 或 WHERE 1=0 可能是调试遗留代码。
// SQLFluff ST10 等效规则。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users WHERE 1=1;
//   SELECT * FROM users WHERE true;
//   SELECT * FROM users WHERE 1=0;     -- 调试遗留的常量条件
//
// 正面案例（放行）：
//   SELECT * FROM users WHERE status = 'active';
//   -- 动态 SQL 拼接时 1=1 作为占位"无害"，但建议重构为明确条件
//   SELECT * FROM users WHERE 1=1 AND status = 'active';

let sql = context["sql_content"];

let patterns = [
    "WHERE 1=1",
    "WHERE 1 = 1",
    "WHERE 1=0",
    "WHERE 1 = 0",
    "WHERE TRUE",
    "WHERE TRUE ",
    "WHERE FALSE",
    "WHERE FALSE ",
    "WHERE 1<>1",
    "WHERE 1 <> 1"
];

let lines = sql.split("\n");
let line_num = 1;
for line in lines {
    let uline = line.to_upper();
    for pat in patterns {
        if uline.contains(pat) {
            violations.push(violation_line("Constant condition '" + pat + "' found in WHERE clause. This is likely dead code or a debugging artifact. Remove it.", line_num));
        }
    }
    line_num += 1;
}
