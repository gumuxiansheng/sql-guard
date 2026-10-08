// no_unused_join.rhai (DML101)
// 检测可能未使用的 JOIN 关系。
// 基于 AST：用 sel.is_qualifier_referenced() 精确判断 JOIN 的表（优先别名，回退表名）
// 是否被本查询体引用。引用范围 = 投影（含 t.*） + WHERE + GROUP BY + HAVING + QUALIFY
// + ORDER BY；JOIN 的 ON 条件刻意不计入（右表通常只在 ON 出现一次，那样仍属"未被使用"）。
//
// 匹配方式已从早期的"投影文本子串 contains"升级为"限定符精确匹配"，修复三类误报：
//   1. SELECT t1.* FROM a t2 LEFT JOIN b t1 ON ...      -- 限定通配符 t1.* 此前没进投影列表，
//                                                         导致投影为空、JOIN 表被误判未使用
//   2. SELECT * FROM a JOIN b ON ...                     -- 裸星号引用了所有表，但别名/表名
//                                                         不在文本里，此前同样误报
//   3. SELECT u.id FROM users u JOIN orders o ... ORDER BY o.created_at
//                                                       -- ORDER BY / GROUP BY 引用此前不看
// SQLFluff ST11 等效规则。
//
// 反面案例（会被拦截）：
//   SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id;
//   -- o 只在 ON 条件出现，投影 / WHERE / ORDER BY 都没用它的列
//
// 正面案例（放行）：
//   SELECT t1.* FROM ofsm.cdeusr t2 LEFT JOIN ofsm.cdeorg t1
//       ON t2.ibkcde = t1.orgno WHERE t2.usr_uid = :usrUid;   -- t1.* 已使用 t1
//   SELECT * FROM users u JOIN orders o ON u.id = o.user_id;  -- 裸 * 使用了所有表
//   SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id ORDER BY o.created_at;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_joins() {
            // 裸 `SELECT *`：一次引用了所有表，无法判断哪张 JOIN 表没用，整体跳过。
            // （`t1.*` 这类限定通配符不算，仍需逐个判断。）
            if sel.has_bare_wildcard() {
                continue;
            }

            for j in sel.joins() {
                if j.join_type() == "CROSS" {
                    continue;
                }
                // 优先用别名匹配；无别名时回退到表名（含 schema 全名 + 去掉 schema 的表名，
                // 以兼容 `ofsm.cdeorg.col` 与 `cdeorg.col` 两种写法）。
                let needle = j.table_name();
                if j.has_alias() {
                    needle = j.alias();
                }
                if needle == "" {
                    continue;
                }
                if !sel.is_qualifier_referenced(needle)
                    && !sel.is_qualifier_referenced(j.table_name())
                    && !sel.is_qualifier_referenced(j.table_name_leaf())
                {
                    violations.push(violation("Possible unused JOIN to table '" + j.table_name() + "' (alias '" + j.alias() + "'). The table columns are not referenced in SELECT list, WHERE, GROUP BY, HAVING or ORDER BY clause.", s.line(), s.column()));
                }
            }
        }
    }
}
