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
/// M1 阶段为骨架：按 operation_type 逐条生成反向语句（保守实现）。
/// M3 阶段会补全 RENAME back 的"原名"提取（需脚本内 CREATE TABLE 上下文）。
pub fn gen_alter_metadata(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    r: &dyn DialectRenderer,
) -> BackupRollbackPair {
    let mut rollback_parts: Vec<String> = vec![];
    let mut warnings: Vec<String> = vec![];
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
                "ADD_INDEX" | "ADD_CONSTRAINT" => {
                    if !op.constraint_name.is_empty() {
                        rollback_parts.push(format!(
                            "ALTER TABLE {} DROP INDEX {};",
                            r.quote_ident(tbl),
                            r.quote_ident(&op.constraint_name)
                        ));
                    }
                }
                "ADD_PRIMARY_KEY" => {
                    rollback_parts.push(format!("{};", r.drop_primary_key(tbl, None)));
                }
                "RENAME_COLUMN" => {
                    // 反向 RENAME 需要原名，M3 阶段从 CREATE TABLE 上下文提取
                    warnings.push("RENAME COLUMN rollback needs original name from CREATE TABLE context (M3)".to_string());
                }
                "RENAME_TABLE" => {
                    warnings.push("RENAME TABLE rollback needs original name from CREATE TABLE context (M3)".to_string());
                }
                _ => {
                    warnings.push(format!("Unsupported ALTER op: {}", op.operation_type));
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
        safety: SafetyClass::default(),
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::engine::ast::{StmtInfo, CreateInfo, CreateIndexInfo, ViewInfo};
    use crate::rollback::dialect::MySqlRenderer;

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
}
