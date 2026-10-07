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

/// 取字符串第 i 个字符（返回单字符字符串）；越界或负索引返回空串。
/// Rhai 的 `s[i]` 索引语义不稳定，统一用本函数做字符级访问。
fn char_at(s, i) {
    if i < 0 { return ""; }
    if i >= s.len() { return ""; }
    return s.sub_string(i, 1);
}

/// 把十进制数字字符串解析为整数（安全版）。
///
/// 为什么不用 Rhai 内建的 `parse_int`：内建版本遇到非数字输入会 **抛错**
/// （"invalid digit found in string"），把整条规则的脚本执行打断；规则里读的是
/// 用户写的类型文本（`VARCHAR(n)` 的 n），不能假设一定合法。
///
/// 典型用途：读类型声明中的长度，如 `VARCHAR(1000)` → 1000，
/// 供「大字段」判定（GaussDB 规范：字符串长度超过 2000 字节即大字段）使用。
///
/// 语义：跳过前导空白；遇到第一个非数字字符即停止；全无数字时返回 0。
/// 例：parse_int_safe("1000") == 1000；parse_int_safe(" 42x") == 42；
///     parse_int_safe("abc") == 0
fn parse_int_safe(s) {
    let digits = "0123456789";
    let n = 0;
    let i = 0;
    let started = false;
    while i < s.len() {
        let c = char_at(s, i);
        let d = -1;
        let j = 0;
        while j < 10 {
            if char_at(digits, j) == c { d = j; }
            j = j + 1;
        }
        if d >= 0 {
            n = n * 10 + d;
            started = true;
        } else {
            if started { break; }
            if c != " " && c != "\t" { break; }
        }
        i = i + 1;
    }
    return n;
}
