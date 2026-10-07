// gaussdb_statement_max_bytes.rhai (GPERF001)
// GaussDB 规范 G-PERF-07：SQL 语句最佳长度 < 5KB（附录最佳实践阈值）。
//
// 判定口径：len_bytes(ast.stmt_text(s))。
//   - 必须用内置 len_bytes（UTF-8 字节数）；Rhai 的 String.len() 是字符数，
//     中文场景下会低估。
//   - 语句原文基于语句行区间切片，同一行写两条语句时会连带取到同行后续内容
//     （已知局限，长语句恰恰通常独占多行，影响有限）。
//
// 可配参数（[rules.params]）：
//   stmt_max_bytes = 5120     # 默认 5KB
//
// 会提示：
//   单条语句超过阈值的超长 SQL（通常是拼接出来的大 IN 列表或超宽投影）。

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
let p = context["params"];

let limit = 5120;
if "stmt_max_bytes" in p {
    limit = p["stmt_max_bytes"];
}

for s in ast.statements() {
    let t = ast.stmt_text(s);
    let b = len_bytes(t);
    if b > limit {
        violations.push(violation(
            "statement is " + b.to_string() + " bytes, exceeding the recommended " +
            limit.to_string() + " bytes; consider splitting it",
            s.line(), s.column()));
    }
}
