// gaussdb_require_commit_in_transaction.rhai (GDDL001)
// GaussDB 规范 G-DDL-02：自动提交关闭或在事务中时，需要显式使用 COMMIT 提交
// （这种场景下 DDL 操作不会自动提交，事务悬挂会长期持锁）。
//
// 判定口径：遍历同一文件内的语句，维护一个极简状态机——
//   遇到 START_TRANSACTION/BEGIN 记为"进入事务"，遇到 COMMIT/ROLLBACK 记为"结束事务"；
//   事务区间内若出现 DDL（CREATE/ALTER/DROP/TRUNCATE），且到文件末尾仍未结束事务 → 违规。
//
// 已知局限：**仅同文件**。跨文件（BEGIN 在 A 文件、COMMIT 在 B 文件）无法判定，
// 需要后续 C9 项目级聚合能力。
//
// 反面案例（会被拦截，报在 BEGIN 所在行）：
//   BEGIN;
//   CREATE TABLE t (id INT);      -- 事务内 DDL，文件末尾没有 COMMIT
//
// 正面案例（放行）：
//   BEGIN;
//   CREATE TABLE t (id INT);
//   COMMIT;

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

let in_txn = false;
let txn_line = 0;
let txn_col = 0;
let ddl_in_txn = false;

for s in ast.statements() {
    if s.has_transaction() {
        let t = s.transaction();
        if t.kind() == "START_TRANSACTION" {
            in_txn = true;
            txn_line = s.line();
            txn_col = s.column();
            ddl_in_txn = false;
        } else {
            // COMMIT / ROLLBACK：事务正常结束
            in_txn = false;
        }
    } else if in_txn {
        if s.has_create_table() || s.has_alter_table() || s.has_drop_object() || s.has_truncate()
            || s.has_create_index() || s.has_create_view()
        {
            ddl_in_txn = true;
        }
    }
}

if in_txn && ddl_in_txn {
    violations.push(violation(
        "transaction contains DDL but has no explicit COMMIT/ROLLBACK before end of file",
        txn_line, txn_col));
}
