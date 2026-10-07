// gaussdb_create_index_concurrently.rhai (GIDX001)
// GaussDB 规范 G-IDX-01：创建索引时，如果有联机事务，必须添加 CONCURRENTLY 参数。
//
// 判定口径：CreateIndexInfo.concurrently()。
//
// ⚠️ 误报提示：本规则按"联机库"假设，凡 CREATE INDEX 未带 CONCURRENTLY 一律报。
// 若某些 DDL 只在停服窗口执行，请用行内豁免：
//   -- sqlguard-disable-next-line GIDX001
//   CREATE INDEX idx_t_a ON t (a);
//
// 反面案例（会被拦截）：
//   CREATE INDEX idx_t_a ON t (a);
//
// 正面案例（放行）：
//   CREATE INDEX CONCURRENTLY idx_t_a ON t (a);

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

for s in ast.statements() {
    if s.has_create_index() {
        let ci = s.create_index();
        if !ci.concurrently() {
            violations.push(violation(
                "CREATE INDEX '" + ci.name() + "' on '" + ci.table_name() +
                "' must use CONCURRENTLY when the system has online transactions",
                s.line(), s.column()));
        }
    }
}
