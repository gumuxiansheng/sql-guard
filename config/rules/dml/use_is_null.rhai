// use_is_null.rhai (DML103)
// 使用 IS NULL / IS NOT NULL 而非 = NULL / <> NULL。
// 原因：SQL 中 NULL = NULL 的结果是 NULL（不是 TRUE），
// WHERE name = NULL 永远不会返回任何行，这是常见编码错误。
// SQLFluff CV05 等效规则。

let ast = context["ast"];

if ast.has_parse_error() {
    violations.push(#{
        "message": "SQL parse error: " + ast.parse_error(),
        "line": 1
    });
}

// 基于字符串匹配检查 "= NULL" / "<> NULL" / "!= NULL"
let sql = context["sql_content"];
let upper = sql.to_upper();

// 检查 = NULL 但排除 IS NULL 和 IS NOT NULL
let lines = sql.split("\n");
let line_num = 1;
for line in lines {
    let uline = line.to_upper();
    // 避免匹配 "IS NULL" 或 "IS NOT NULL"
    if (uline.contains("= NULL") || uline.contains("=NULL")) && !uline.contains("IS NULL") && !uline.contains("IS NOT NULL") {
        violations.push(#{
            "message": "Use 'IS NULL' instead of '= NULL'. NULL comparisons always return FALSE with '='.",
            "line": line_num
        });
    }
    if (uline.contains("<> NULL") || uline.contains("<>NULL") || uline.contains("!= NULL") || uline.contains("!=NULL")) && !uline.contains("IS NOT NULL") {
        violations.push(#{
            "message": "Use 'IS NOT NULL' instead of '<> NULL' or '!= NULL'. NULL comparisons always return FALSE with '<>'.",
            "line": line_num
        });
    }
    line_num += 1;
}
