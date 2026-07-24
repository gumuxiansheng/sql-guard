// no_constant_where.rhai (DML108)
// 避免 WHERE 子句中使用常量条件（WHERE 1=1, WHERE true 等）。
// 原因：WHERE 1=1 通常是动态 SQL 拼接的产物，虽无害但非必要；
// WHERE false 或 WHERE 1=0 可能是调试遗留代码。
// SQLFluff ST10 等效规则。

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
