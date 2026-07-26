//! 纯反向 DDL（无需原表定义）。
//!
//! 对应设计文档 §4.4 / §4.5.4。
//! - CREATE TABLE → rollback: DROP TABLE IF EXISTS
//! - CREATE INDEX → rollback: DROP INDEX IF EXISTS
//! - CREATE VIEW  → rollback: DROP VIEW IF EXISTS
//! - ALTER metadata-only（RENAME COLUMN/TABLE、ADD INDEX/CONSTRAINT/PK、无约束的 ADD COLUMN）
//!   → rollback: 反向元数据操作（DROP COLUMN / RENAME back / DROP INDEX / DROP CONSTRAINT / DROP PK）
//!
//! M1 阶段为骨架实现，M3 阶段补全 ALTER metadata-only 反向操作。

use crate::rule::engine::ast::StmtInfo;
use super::dialect::DialectRenderer;
use super::{BackupRollbackPair, SourceRef, SafetyClass, BackupStrategy};

/// CREATE TABLE → rollback: `DROP TABLE IF EXISTS <name>`
pub fn gen_create_table(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    r: &dyn DialectRenderer,
) -> BackupRollbackPair {
    let table = stmt.create_table.as_ref().map(|c| c.table_name.as_str()).unwrap_or("");
    let rollback = format!("DROP TABLE IF EXISTS {};", r.quote_ident(table));
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: Some(rollback),
        safety: SafetyClass::default(),
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec![],
    }
}

/// CREATE INDEX → rollback: `DROP INDEX IF EXISTS <name> [ON <table>]`
pub fn gen_create_index(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    r: &dyn DialectRenderer,
) -> BackupRollbackPair {
    let (idx, tbl) = stmt.create_index.as_ref()
        .map(|c| (c.name.as_str(), c.table_name.as_str()))
        .unwrap_or(("", ""));
    let rollback = format!("{};", r.drop_index(idx, tbl));
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: Some(rollback),
        safety: SafetyClass::default(),
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec![],
    }
}

/// CREATE VIEW → rollback: `DROP VIEW IF EXISTS <name>`
pub fn gen_create_view(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    r: &dyn DialectRenderer,
) -> BackupRollbackPair {
    let name = stmt.create_view.as_ref().map(|v| v.name.as_str()).unwrap_or("");
    let rollback = format!("DROP VIEW IF EXISTS {};", r.quote_ident(name));
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: Some(rollback),
        safety: SafetyClass::default(),
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec![],
    }
}

/// ALTER metadata-only：反向元数据操作。
///
/// M3 阶段补全：
/// - RENAME COLUMN：从 `op.detail`（"RENAME COLUMN <old> TO <new>"）提取新名，反向 `RENAME COLUMN <new> TO <old>`
/// - RENAME TABLE：`alter.table_name` 为原名，`op.table_name` 为新名，反向 `RENAME TABLE <new> TO <old>`
/// - ADD_CONSTRAINT：根据 detail 区分 PRIMARY KEY / UNIQUE / FOREIGN KEY / CHECK，生成对应 DROP 语句
///
/// ★ 局限（M5 接入 CREATE TABLE 上下文后补全）：
/// - RENAME COLUMN/TABLE 的"原名"取自 ALTER 语句本身（parser 已提取），不依赖 CREATE TABLE 上下文
/// - ADD_CONSTRAINT 的反向 DROP 对 FK/CHECK 走通用 `DROP CONSTRAINT`，MySQL 方言下 FK 应为
///   `DROP FOREIGN KEY`、CHECK 应为 `DROP CHECK`，M3 暂以 warning 提示 DBA 人工核对
pub fn gen_alter_metadata(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    r: &dyn DialectRenderer,
) -> BackupRollbackPair {
    let mut rollback_parts: Vec<String> = vec![];
    let mut warnings: Vec<String> = vec![];
    let mut reliable = true;
    if let Some(alter) = &stmt.alter_table {
        let tbl = &alter.table_name;
        for op in &alter.operations {
            match op.operation_type.as_str() {
                "ADD_COLUMN" => {
                    // 反向：DROP COLUMN（仅当无约束时走此路径，见 is_metadata_only_alter）
                    if !op.column_name.is_empty() {
                        rollback_parts.push(format!(
                            "ALTER TABLE {} DROP COLUMN {};",
                            r.quote_ident(tbl),
                            r.quote_ident(&op.column_name)
                        ));
                    }
                }
                "ADD_INDEX" => {
                    // ALTER TABLE ADD INDEX <name> → DROP INDEX <name>
                    if !op.constraint_name.is_empty() {
                        rollback_parts.push(format!(
                            "ALTER TABLE {} DROP INDEX {};",
                            r.quote_ident(tbl),
                            r.quote_ident(&op.constraint_name)
                        ));
                    }
                }
                "ADD_CONSTRAINT" => {
                    // 根据 detail 区分约束类型，生成对应 DROP
                    let detail_upper = op.detail.to_uppercase();
                    if detail_upper.contains("PRIMARY KEY") {
                        rollback_parts.push(format!("{};", r.drop_primary_key(tbl, Some(&op.constraint_name))));
                    } else if detail_upper.contains("UNIQUE") {
                        // UNIQUE 约束反向：MySQL 用 DROP INDEX，PG 用 DROP CONSTRAINT
                        rollback_parts.push(format!(
                            "ALTER TABLE {} DROP INDEX {};",
                            r.quote_ident(tbl),
                            r.quote_ident(&op.constraint_name)
                        ));
                    } else if detail_upper.contains("FOREIGN KEY") {
                        // ★ MySQL: ALTER TABLE t DROP FOREIGN KEY <name>
                        //   PG:    ALTER TABLE t DROP CONSTRAINT <name>
                        // M3 暂以通用 DROP CONSTRAINT，MySQL 方言下加 warning 提示 DBA 核对
                        rollback_parts.push(format!(
                            "ALTER TABLE {} DROP CONSTRAINT {};",
                            r.quote_ident(tbl),
                            r.quote_ident(&op.constraint_name)
                        ));
                        warnings.push(format!(
                            "ADD_CONSTRAINT (FOREIGN KEY) rollback uses DROP CONSTRAINT; MySQL dialect may need DROP FOREIGN KEY `{}`",
                            op.constraint_name
                        ));
                    } else if detail_upper.contains("CHECK") {
                        rollback_parts.push(format!(
                            "ALTER TABLE {} DROP CONSTRAINT {};",
                            r.quote_ident(tbl),
                            r.quote_ident(&op.constraint_name)
                        ));
                    } else {
                        // 兜底：未知约束类型，DROP INDEX 兜底 + warning
                        if !op.constraint_name.is_empty() {
                            rollback_parts.push(format!(
                                "ALTER TABLE {} DROP INDEX {};",
                                r.quote_ident(tbl),
                                r.quote_ident(&op.constraint_name)
                            ));
                        }
                        warnings.push(format!("Unknown ADD_CONSTRAINT subtype, fallback DROP INDEX: {}", op.detail));
                    }
                }
                "ADD_PRIMARY_KEY" => {
                    // parser 当前未单独产出此 operation_type（PK 走 ADD_CONSTRAINT），保留兜底
                    rollback_parts.push(format!("{};", r.drop_primary_key(tbl, None)));
                }
                "RENAME_COLUMN" => {
                    // op.column_name = old_column_name（parser 已提取）
                    // op.detail = "RENAME COLUMN <old> TO <new>"
                    let old_name = &op.column_name;
                    match extract_renamed_to(&op.detail) {
                        Some(new_name) => {
                            // 反向：RENAME COLUMN <new> TO <old>
                            rollback_parts.push(format!(
                                "ALTER TABLE {} RENAME COLUMN {} TO {};",
                                r.quote_ident(tbl),
                                r.quote_ident(&new_name),
                                r.quote_ident(old_name)
                            ));
                        }
                        None => {
                            warnings.push(format!(
                                "RENAME COLUMN rollback skipped: cannot extract new name from detail '{}'",
                                op.detail
                            ));
                            reliable = false;
                        }
                    }
                }
                "RENAME_TABLE" => {
                    // alter.table_name = 原表名（old），op.table_name = 新表名（new）
                    let old_table = tbl;
                    let new_table = &op.table_name;
                    if new_table.is_empty() {
                        warnings.push("RENAME TABLE rollback skipped: new table name empty".to_string());
                        reliable = false;
                    } else {
                        rollback_parts.push(format!(
                            "{};",
                            r.rename_table(new_table, old_table)
                        ));
                    }
                }
                _ => {
                    warnings.push(format!("Unsupported ALTER op: {}", op.operation_type));
                    reliable = false;
                }
            }
        }
    }
    let rollback = if rollback_parts.is_empty() {
        None
    } else {
        Some(rollback_parts.join("\n"))
    };
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback,
        safety: SafetyClass {
            reliable,
            ..Default::default()
        },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings,
    }
}

/// 从 `RENAME COLUMN <old> TO <new>` 或 `RENAME TO <new>` 文本中提取 ` TO ` 之后的新名。
/// 去除尾部 `;` 与首尾空白，支持带引号/反引号的标识符。
fn extract_renamed_to(detail: &str) -> Option<String> {
    let upper = detail.to_uppercase();
    let idx = upper.find(" TO ")?;
    let rest = detail[idx + 4..].trim();
    let cleaned = rest.trim_end_matches(';').trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(super::strip_ident_quotes(cleaned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::engine::ast::{StmtInfo, CreateInfo, CreateIndexInfo, ViewInfo, AlterTableInfo, AlterOpInfo};
    use crate::rollback::dialect::{MySqlRenderer, PostgreSqlRenderer};

    fn make_alter_stmt(table: &str, ops: Vec<AlterOpInfo>) -> StmtInfo {
        StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, truncate: None, create_view: None,
            create_index: None, transaction: None,
            alter_table: Some(AlterTableInfo {
                table_name: table.to_string(),
                operations: ops,
                adds_primary_key: false,
                drops_primary_key: false,
                added_primary_key_columns: vec![],
            }),
        }
    }

    fn alter_op(op_type: &str, col: &str, tbl: &str, con: &str, detail: &str) -> AlterOpInfo {
        AlterOpInfo {
            operation_type: op_type.to_string(),
            column_name: col.to_string(),
            table_name: tbl.to_string(),
            constraint_name: con.to_string(),
            detail: detail.to_string(),
        }
    }

    #[test]
    fn create_table_rollback_drops() {
        let stmt = StmtInfo {
            kind: "CREATE_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: Some(CreateInfo { table_name: "users".to_string(), ..Default::default() }),
            drop_object: None, select: None, insert: None, update: None, delete: None,
            alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
        };
        let pair = gen_create_table(&stmt, 1, SourceRef::placeholder(), "CREATE TABLE users (...)", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("DROP TABLE IF EXISTS `users`;"));
    }

    #[test]
    fn create_index_rollback_drops() {
        let stmt = StmtInfo {
            kind: "CREATE_INDEX".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None, update: None,
            delete: None, alter_table: None, truncate: None, create_view: None, transaction: None,
            create_index: Some(CreateIndexInfo {
                name: "idx_users_email".to_string(),
                table_name: "users".to_string(),
                columns: vec!["email".to_string()],
                is_unique: false,
            }),
        };
        let pair = gen_create_index(&stmt, 1, SourceRef::placeholder(), "CREATE INDEX ...", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("DROP INDEX IF EXISTS `idx_users_email` ON `users`;"));
    }

    #[test]
    fn create_view_rollback_drops() {
        let stmt = StmtInfo {
            kind: "CREATE_VIEW".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None, update: None,
            delete: None, alter_table: None, truncate: None, create_index: None, transaction: None,
            create_view: Some(ViewInfo { name: "v_orders".to_string(), materialized: false, is_replace: false, column_count: 0 }),
        };
        let pair = gen_create_view(&stmt, 1, SourceRef::placeholder(), "CREATE VIEW ...", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("DROP VIEW IF EXISTS `v_orders`;"));
    }

    // ===== M3 ALTER metadata-only 反向操作测试 =====

    #[test]
    fn rename_column_generates_reverse_rename() {
        // op.column_name = old (parser 已提取), detail = "RENAME COLUMN old TO new"
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_COLUMN", "name", "", "",
            "RENAME COLUMN name TO full_name",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME COLUMN name TO full_name", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` RENAME COLUMN `full_name` TO `name`;"));
        assert!(pair.safety.reliable);
    }

    #[test]
    fn rename_column_pg_uses_double_quotes() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_COLUMN", "name", "", "",
            "RENAME COLUMN name TO full_name",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME COLUMN name TO full_name", &PostgreSqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE \"users\" RENAME COLUMN \"full_name\" TO \"name\";"));
    }

    #[test]
    fn rename_column_strips_quoted_new_name() {
        // detail 中新名带反引号，提取后应剥离
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_COLUMN", "name", "", "",
            "RENAME COLUMN name TO `full_name`",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME COLUMN name TO `full_name`", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` RENAME COLUMN `full_name` TO `name`;"));
    }

    #[test]
    fn rename_column_missing_new_name_marks_unreliable() {
        // detail 中无 " TO "，提取失败 → 标 unreliable + warning
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_COLUMN", "name", "", "",
            "RENAME COLUMN name",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME COLUMN name", &MySqlRenderer);
        assert!(pair.rollback.is_none());
        assert!(!pair.safety.reliable);
        assert!(pair.warnings.iter().any(|w| w.contains("cannot extract new name")));
    }

    #[test]
    fn rename_table_mysql_generates_rename_back() {
        // alter.table_name = users (old), op.table_name = users_new (new)
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_TABLE", "", "users_new", "",
            "RENAME TO users_new",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME TO users_new", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("RENAME TABLE `users_new` TO `users`;"));
        assert!(pair.safety.reliable);
    }

    #[test]
    fn rename_table_pg_generates_alter_rename_back() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_TABLE", "", "users_new", "",
            "RENAME TO users_new",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME TO users_new", &PostgreSqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE \"users_new\" RENAME TO \"users\";"));
    }

    #[test]
    fn rename_table_empty_new_name_marks_unreliable() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "RENAME_TABLE", "", "", "",
            "RENAME TO",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users RENAME TO", &MySqlRenderer);
        assert!(pair.rollback.is_none());
        assert!(!pair.safety.reliable);
        assert!(pair.warnings.iter().any(|w| w.contains("new table name empty")));
    }

    #[test]
    fn add_constraint_pk_generates_drop_primary_key() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "ADD_CONSTRAINT", "", "", "pk_users",
            "ADD CONSTRAINT pk_users PRIMARY KEY (id)",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD CONSTRAINT pk_users PRIMARY KEY (id)", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` DROP PRIMARY KEY;"));
    }

    #[test]
    fn add_constraint_unique_generates_drop_index() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "ADD_CONSTRAINT", "", "", "uk_email",
            "ADD CONSTRAINT uk_email UNIQUE (email)",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD CONSTRAINT uk_email UNIQUE (email)", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` DROP INDEX `uk_email`;"));
    }

    #[test]
    fn add_constraint_fk_generates_drop_constraint_with_warning() {
        let stmt = make_alter_stmt("orders", vec![alter_op(
            "ADD_CONSTRAINT", "", "", "fk_user",
            "ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users(id)",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users(id)", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `orders` DROP CONSTRAINT `fk_user`;"));
        // MySQL 方言下 FK 应为 DROP FOREIGN KEY，加 warning 提示
        assert!(pair.warnings.iter().any(|w| w.contains("FOREIGN KEY") && w.contains("DROP FOREIGN KEY")));
    }

    #[test]
    fn add_constraint_check_generates_drop_constraint() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "ADD_CONSTRAINT", "", "", "ck_age",
            "ADD CONSTRAINT ck_age CHECK (age >= 0)",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD CONSTRAINT ck_age CHECK (age >= 0)", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` DROP CONSTRAINT `ck_age`;"));
    }

    #[test]
    fn add_column_no_constraint_generates_drop_column() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "ADD_COLUMN", "age", "", "",
            "ADD COLUMN age INT",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD COLUMN age INT", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` DROP COLUMN `age`;"));
        assert!(pair.safety.reliable);
    }

    #[test]
    fn add_index_generates_drop_index() {
        let stmt = make_alter_stmt("users", vec![alter_op(
            "ADD_INDEX", "", "", "idx_email",
            "ADD INDEX idx_email (email)",
        )]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD INDEX idx_email (email)", &MySqlRenderer);
        assert_eq!(pair.rollback.as_deref(), Some("ALTER TABLE `users` DROP INDEX `idx_email`;"));
    }

    #[test]
    fn multiple_alter_ops_generate_multi_line_rollback() {
        let stmt = make_alter_stmt("users", vec![
            alter_op("ADD_COLUMN", "age", "", "", "ADD COLUMN age INT"),
            alter_op("ADD_INDEX", "", "", "idx_email", "ADD INDEX idx_email (email)"),
        ]);
        let pair = gen_alter_metadata(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD COLUMN age INT, ADD INDEX idx_email (email)", &MySqlRenderer);
        let rollback = pair.rollback.unwrap();
        assert!(rollback.contains("ALTER TABLE `users` DROP COLUMN `age`;"));
        assert!(rollback.contains("ALTER TABLE `users` DROP INDEX `idx_email`;"));
        assert_eq!(rollback.lines().count(), 2);
    }

    #[test]
    fn extract_renamed_to_basic() {
        assert_eq!(extract_renamed_to("RENAME COLUMN name TO full_name"), Some("full_name".to_string()));
        assert_eq!(extract_renamed_to("RENAME COLUMN name TO `full_name`"), Some("full_name".to_string()));
        assert_eq!(extract_renamed_to("RENAME TO users_new"), Some("users_new".to_string()));
        assert_eq!(extract_renamed_to("RENAME COLUMN name"), None);
        assert_eq!(extract_renamed_to(""), None);
    }
}
