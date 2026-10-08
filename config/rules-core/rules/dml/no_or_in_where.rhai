// no_or_in_where.rhai (DML111)
// 禁止在 WHERE 子句中使用 OR 连接多个条件。
// 原因：OR 条件会让优化器难以（或无法）对单个列走索引路径——
//   - 同一列的 `col = 1 OR col = 2` 应改写为 `col IN (1, 2)`（索引利用更佳）；
//   - 不同列的 `a = 1 OR b = 2` 属于"析取扫描"（Disjunctive Scan），
//     优化器通常需要分别扫描两列对应的索引再合并，或直接放弃索引退化为全表扫描；
//   - 可改写为 UNION ALL（不同列）或拆分查询（业务层合并结果）。
// 详细替代写法与索引/执行效率对比见 docs/sql-guidelines/avoid-or-in-where.md。
//
// 检测基于 AST 的 WHERE 子句文本：按 token 扫描（字符串字面量整体保留为单个
// token），命中独立 `OR` 关键字即报。因此不会误报：
//   - 字符串值内的 OR（WHERE note = 'pending or done'）
//   - 列名/标识符中的 or（WHERE normal_flag = 1 / WHERE or_id = 5）
// 对 SELECT 及其子查询、UPDATE、DELETE 的 WHERE 统一检查。
//
// 反面案例（会被拦截）：
//   SELECT * FROM users WHERE status = 'active' OR status = 'pending';
//   SELECT * FROM users WHERE (a = 1 OR b = 2) AND c = 3;
//   SELECT * FROM (SELECT id FROM users WHERE x = 1 OR y = 2) t;   -- 子查询 WHERE
//   UPDATE users SET flag = 1 WHERE id = 1 OR id = 2;
//   DELETE FROM logs WHERE created_at < '2024-01-01' OR type = 'debug';
//
// 正面案例（放行）：
//   SELECT * FROM users WHERE status IN ('active', 'pending');    -- 已用 IN
//   SELECT * FROM users WHERE a = 1 AND b = 2;                    -- 纯 AND
//   SELECT * FROM users WHERE note = 'pending or done';           -- 字符串内含 OR
//   SELECT * FROM users WHERE normal_flag = 1;                    -- 标识符含 or

// 将 WHERE 子句文本拆成 token 数组：运算符 / 括号各自成 token，
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
        cur = cur + c;
        i = i + 1;
    }
    if cur != "" { toks.push(cur); }
    return toks;
}

// 判断 WHERE 子句文本中是否出现独立的 OR 关键字（字符串字面量内的 or 不算）。
fn has_or_keyword(clause) {
    let toks = tokenize(clause);
    let i = 0;
    while i < toks.len() {
        if toks[i].to_upper() == "OR" {
            return true;
        }
        i = i + 1;
    }
    return false;
}

// 递归检查 select 及其子查询的 WHERE。
fn select_where_has_or(sel) {
    if sel.has_where() && has_or_keyword(sel.where_clause()) {
        return true;
    }
    for sub in sel.subqueries() {
        if select_where_has_or(sub) {
            return true;
        }
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
        if select_where_has_or(s.select()) {
            violations.push(violation(
                "WHERE clause combines conditions with OR ('" + s.select().where_clause() + "'). OR usually prevents efficient index utilization; prefer IN (...) for same-column values, UNION ALL for cross-column disjunctions, or splitting the query. See docs/sql-guidelines/avoid-or-in-where.md.",
                s.line(), s.column()
            ));
        }
    } else if s.has_update() {
        let u = s.update();
        if u.has_where() && has_or_keyword(u.where_clause()) {
            violations.push(violation(
                "WHERE clause combines conditions with OR ('" + u.where_clause() + "'). OR usually prevents efficient index utilization; prefer IN (...) for same-column values, UNION ALL for cross-column disjunctions, or splitting the query.",
                s.line(), s.column()
            ));
        }
    } else if s.has_delete() {
        let d = s.delete();
        if d.has_where() && has_or_keyword(d.where_clause()) {
            violations.push(violation(
                "WHERE clause combines conditions with OR ('" + d.where_clause() + "'). OR usually prevents efficient index utilization; prefer IN (...) for same-column values, UNION ALL for cross-column disjunctions, or splitting the query.",
                s.line(), s.column()
            ));
        }
    }
}
