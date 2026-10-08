// gaussdb_no_quoted_column_in_ddl.rhai (GDCL001)
// GaussDB 规范 G-DCL-02：DDL 脚本要确保执行后数据库字段的名称，
// 禁止使用引号、反引号等各种符号括起来。
//
// 判定口径：sqlparser 丢弃引号样式信息，因此必须读**语句原文**
// （ast.stmt_text）——这是 M1 新增 C4 原文切片的直接成果。
//
// 作用域：只检查 ALTER TABLE。CREATE TABLE 的对象名/列名引号由
// GNAM002（禁止引号定义对象名）覆盖，避免同一处问题重复告警。
//
// 已知局限：基于子串匹配，字符串字面量或注释里的引号会误报 → 用行内豁免：
//   -- sqlguard-disable-next-line GDCL001
//
// 反面案例（会被拦截）：
//   ALTER TABLE t ADD COLUMN "name" VARCHAR(20);
//   ALTER TABLE t RENAME COLUMN a TO `b`;
//
// 正面案例（放行）：
//   ALTER TABLE t ADD COLUMN name VARCHAR(20);

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_alter_table() {
        let t = ast.stmt_text(s);
        if t.contains("\"") || t.contains("`") {
            violations.push(violation(
                "DDL must not quote column names (found quote or backtick in statement text)",
                s.line(), s.column()));
        }
    }
}
