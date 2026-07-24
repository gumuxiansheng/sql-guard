// helpers.rhai — 规则脚本公共辅助函数
// 由 engine::runner 在每条规则脚本执行前自动注入，规则脚本无需 import 即可直接调用。
//
// ⚠️ Rhai 值语义限制：函数内对 Array 参数的 push 不会写回调用者变量。
// 因此所有 helper 函数只返回值，push 操作在脚本顶层用 violations.push(...) 执行。
// context 变量同理——函数内无法访问 scope 变量，必须通过参数传入。

/// 检查 AST 是否存在解析错误。返回 true 表示有错误。
/// 调用方需自行 push violation 后 return。
///
/// 用法：
///   if guard_parse_error(context) {
///       violations.push(parse_error_violation(context));
///       return;
///   }
fn guard_parse_error(context) {
    let ast = context["ast"];
    return ast.has_parse_error();
}

/// 构建 parse error violation Map。
fn parse_error_violation(context) {
    #{
        "message": "SQL parse error: " + context["ast"].parse_error(),
        "line": 1
    }
}

/// 构建一条带行号和列号的 violation Map。
/// 用法：violations.push(violation("msg", line, column));
fn violation(message, line, column) {
    #{
        "message": message,
        "line": line,
        "column": column
    }
}

/// 构建一条仅带行号的 violation Map。
/// 用法：violations.push(violation_line("msg", 3));
fn violation_line(message, line) {
    #{
        "message": message,
        "line": line
    }
}

/// 构建一条无行号的 violation 消息字符串。
/// 用法：violations.push(violation_msg("msg"));
fn violation_msg(message) {
    message
}

/// 用分隔符把字符串数组拼接成单个字符串。
/// Rhai 标准库的 Array 没有 `.join()` 方法，规则脚本统一调用本函数。
/// 用法：join_strs(["a", "b"], "_") == "a_b"
fn join_strs(arr, sep) {
    let s = "";
    let i = 0;
    while i < arr.len() {
        if i > 0 { s = s + sep; }
        s = s + arr[i];
        i = i + 1;
    }
    return s;
}
