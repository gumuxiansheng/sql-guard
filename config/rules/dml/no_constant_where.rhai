// no_constant_where.rhai (DML108)
// 禁止 WHERE 子句中出现"恒真 / 恒假"的常量条件，例如：
//   WHERE 1=1 / WHERE 2=2 / WHERE 'x' = 'x'   —— 字面量自比较（恒真，即"全匹配"筛选）
//   WHERE 1=0 / WHERE 1<>1 / WHERE 'a' = 'b'  —— 字面量互比较（恒假，调试残留）
//   WHERE TRUE / WHERE FALSE                  —— 裸布尔常量
// 这类条件通常是动态 SQL 拼接占位或调试遗留，在静态 SQL 中无实际意义，应删除或改为真实条件。
//
// 检测基于 AST 的 WHERE 子句文本，逐 token 判断是否构成"字面量 比较 字面量"。
// 仅当两侧都是字面量（数字 / 字符串 / 布尔）时才判定为常量条件，
// 因此不会误报 col = 1 / a.id = b.id / status = 'active' 等真实条件。
// SQLFluff ST10 等效规则，重点覆盖 WHERE 1=1 / WHERE 2=2 这类全匹配筛选语句。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users WHERE 1=1;
//   SELECT * FROM users WHERE 2=2;
//   SELECT * FROM users WHERE 'x' = 'x';
//   SELECT * FROM users WHERE status = 'active' AND 1=1;
//   UPDATE users SET flag = 1 WHERE 1=0;
//   DELETE FROM logs WHERE 'a' = 'b';
//   SELECT * FROM t WHERE true;
//
// 正面案例（放行）：
//   SELECT * FROM users WHERE status = 'active';
//   SELECT * FROM users WHERE id = 1;
//   SELECT a.id, b.name FROM a JOIN b ON a.id = b.id;   -- 列自比较（非字面量），不误报

fn is_string_lit(tok) {
    let n = tok.len();
    if n < 2 { return false; }
    return tok.sub_string(0, 1) == "'" && tok.sub_string(n - 1, 1) == "'";
}

fn is_numeric(tok) {
    if tok == "" { return false; }
    let has_dot = false;
    let i = 0;
    let n = tok.len();
    if n > 0 && tok.sub_string(0, 1) == "-" { i = 1; }
    if i >= n { return false; }
    while i < n {
        let c = tok.sub_string(i, 1);
        if c == "." {
            if has_dot { return false; }
            has_dot = true;
        } else if c >= "0" && c <= "9" {
            // 数字，忽略
        } else {
            return false;
        }
        i = i + 1;
    }
    return true;
}

fn is_bool_lit(tok) {
    let u = tok.to_upper();
    return u == "TRUE" || u == "FALSE";
}

fn is_literal(tok) {
    if is_string_lit(tok) { return true; }
    if is_numeric(tok) { return true; }
    if is_bool_lit(tok) { return true; }
    return false;
}

fn is_compare_op(tok) {
    return tok == "=" || tok == "<>" || tok == "!=" || tok == "<" || tok == ">" || tok == "<=" || tok == ">=";
}

// 将 WHERE 子句文本直接拆成 token 数组：运算符 / 括号各自成 token，
// 字符串字面量原样保留为单个 token，空白（含换行 / 制表符）作为分隔符。
// 不依赖 Rhai 的 replace / split（1.25 未提供），全部手写。
fn tokenize(s) {
    let toks = [];
    let cur = "";
    let i = 0;
    let n = s.len();
    while i < n {
        let c = s.sub_string(i, 1);
        if c == "'" {
            if cur != "" { toks.push(cur); cur = ""; }
            let lit = "'";
            i = i + 1;
            while i < n {
                let d = s.sub_string(i, 1);
                lit = lit + d;
                i = i + 1;
                if d == "'" {
                    if i < n && s.sub_string(i, 1) == "'" {
                        lit = lit + "'";
                        i = i + 1;
                    } else {
                        break;
                    }
                }
            }
            toks.push(lit);
            continue;
        }
        if c == " " || c == "\t" || c == "\n" || c == "\r" {
            if cur != "" { toks.push(cur); cur = ""; }
            i = i + 1;
            continue;
        }
        if c == "(" || c == ")" {
            if cur != "" { toks.push(cur); cur = ""; }
            toks.push(c);
            i = i + 1;
            continue;
        }
        if c == "=" {
            if cur != "" { toks.push(cur); cur = ""; }
            toks.push("=");
            i = i + 1;
            continue;
        }
        if c == "<" || c == ">" {
            if cur != "" { toks.push(cur); cur = ""; }
            if i + 1 < n {
                let nxt = s.sub_string(i + 1, 1);
                if (c == "<" && (nxt == "=" || nxt == ">")) || (c == ">" && nxt == "=") {
                    toks.push(c + nxt);
                    i = i + 2;
                    continue;
                }
            }
            toks.push(c);
            i = i + 1;
            continue;
        }
        if c == "!" {
            if cur != "" { toks.push(cur); cur = ""; }
            if i + 1 < n && s.sub_string(i + 1, 1) == "=" {
                toks.push("!=");
                i = i + 2;
                continue;
            }
            toks.push("!");
            i = i + 1;
            continue;
        }
        cur = cur + c;
        i = i + 1;
    }
    if cur != "" { toks.push(cur); }
    return toks;
}

// 判断单个 WHERE 条件文本是否含有常量 / 恒真条件。
fn has_constant_condition(clause) {
    let toks = tokenize(clause);
    // 裸常量：整条条件就是一个字面量（如 WHERE TRUE / WHERE 1）
    if toks.len() == 1 && is_literal(toks[0]) {
        return true;
    }
    // 字面量 比较 字面量（恒真或恒假）
    let i = 0;
    while i + 2 < toks.len() {
        if is_literal(toks[i]) && is_compare_op(toks[i + 1]) && is_literal(toks[i + 2]) {
            return true;
        }
        i = i + 1;
    }
    return false;
}

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    let clause = "";
    if s.has_select() {
        let sel = s.select();
        if sel.has_where() { clause = sel.where_clause(); }
    } else if s.has_update() {
        let u = s.update();
        if u.has_where() { clause = u.where_clause(); }
    } else if s.has_delete() {
        let d = s.delete();
        if d.has_where() { clause = d.where_clause(); }
    }
    if clause != "" && has_constant_condition(clause) {
        violations.push(violation(
            "Constant/tautology condition in WHERE clause ('" + clause + "'). This always matches or never matches and is usually dead code or a dynamic-SQL placeholder. Remove it or replace with a real condition.",
            s.line(), s.column()
        ));
    }
}
