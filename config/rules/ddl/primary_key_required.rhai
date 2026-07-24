// primary_key_required.rhai (DDL002)
// 每张表最终必须定义主键。主键可出现在 CREATE TABLE 内（列级/表级约束），
// 也可在后续 ALTER TABLE ... ADD PRIMARY KEY 中补建。
// 基于 AST 跨语句判断：
//   - 避免把 CREATE 无内联 PK + ALTER ADD PRIMARY KEY 补建"误判为违规（此前漏测）；
//   - 同时识别"CREATE 有 PK 但 ALTER DROP PRIMARY KEY"导致最终无主键的情况。

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// 第一遍：统计 ALTER TABLE 对主键的增删，按表名记录。
let pk_added = #{};
let pk_dropped = #{};
for s in ast.statements() {
    if s.kind() == "ALTER_TABLE" && s.has_alter_table() {
        let at = s.alter_table();
        let tname = at.table_name();
        if at.adds_primary_key() {
            pk_added[tname] = true;
        }
        if at.drops_primary_key() {
            pk_dropped[tname] = true;
        }
    }
}

// 第二遍：逐表判断最终是否含主键。
for s in ast.statements() {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        let tname = ct.table_name();
        if ct.has_primary_key() {
            if tname in pk_dropped {
                violations.push(violation("CREATE TABLE " + tname + " declares a PRIMARY KEY but it is removed by ALTER TABLE ... DROP PRIMARY KEY, leaving the table without a primary key", s.line(), s.column()));
            }
        } else {
            if !(tname in pk_added) {
                violations.push(violation("CREATE TABLE " + tname + " must include a PRIMARY KEY (column constraint, table constraint, or ALTER TABLE ... ADD PRIMARY KEY)", s.line(), s.column()));
            }
        }
    }
}
