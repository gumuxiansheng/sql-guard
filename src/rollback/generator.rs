//! 主调度：按 `StmtInfo.kind` 分发到 ddl / ddl_like / dml。
//!
//! 对应设计文档 §4.4。

use crate::config::{Config, RollbackConfig};
use crate::rule::engine::ast::StmtInfo;
use super::dialect::DialectRenderer;
use super::naming::NamingAllocator;
use super::pk::PrimaryKeyResolver;
use super::{BackupRollbackPair, SourceRef, SafetyClass, BackupStrategy};

pub struct RollbackGenerator<'a> {
    pub(crate) config: &'a Config,
    pub(crate) rollback_config: &'a RollbackConfig,
    pub(crate) renderer: &'a dyn DialectRenderer,
    pub(crate) naming: NamingAllocator<'a>,
    pub(crate) pk_resolver: PrimaryKeyResolver,
    pub(crate) seq: u64,
}

impl<'a> RollbackGenerator<'a> {
    pub fn new(config: &'a Config, rc: &'a RollbackConfig, renderer: &'a dyn DialectRenderer) -> Self {
        RollbackGenerator {
            config,
            rollback_config: rc,
            renderer,
            naming: NamingAllocator::new(rc),
            pk_resolver: PrimaryKeyResolver::new(rc),
            seq: 0,
        }
    }

    /// 对单条语句生成 backup/rollback。stmt_kind 决定走 ddl / ddl_like / dml 分支。
    pub fn generate(&mut self, stmt: &StmtInfo, source: SourceRef, original: &str) -> BackupRollbackPair {
        self.seq += 1;
        let seq = self.seq;
        match stmt.kind.as_str() {
            // 纯反向 DDL（无需原表定义）
            "CREATE_TABLE" => super::ddl::gen_create_table(stmt, seq, source, original, self.renderer),
            "CREATE_INDEX" => super::ddl::gen_create_index(stmt, seq, source, original, self.renderer),
            "CREATE_VIEW" => super::ddl::gen_create_view(stmt, seq, source, original, self.renderer),
            // ALTER 中只需反向元数据的子操作（ADD COLUMN 仅当无约束时走此路径，见 is_metadata_only_alter）
            "ALTER_TABLE" if is_metadata_only_alter(stmt) => super::ddl::gen_alter_metadata(stmt, seq, source, original, self.renderer),
            // ★ 需要原表定义的 DDL → 统一走 CREATE TABLE LIKE 模式（含 ADD COLUMN 带约束的降级，见 F17）
            "DROP_TABLE" | "DROP_INDEX" | "DROP_VIEW"
            | "ALTER_TABLE"  // DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT、DROP PRIMARY KEY、带约束的 ADD COLUMN 等
                => super::ddl_like::gen_with_full_backup(stmt, seq, source, original, self),
            // DML
            "INSERT" => super::dml::gen_insert(stmt, seq, source, original, self),
            "UPDATE" => super::dml::gen_update(stmt, seq, source, original, self),
            "DELETE" => super::dml::gen_delete(stmt, seq, source, original, self),
            // ★ TRUNCATE 走 DML 路径：表结构保留，仅需 INSERT FROM bks_ 重灌数据
            // （与 ddl_like 的原子 RENAME 切换不同，TRUNCATE 不涉及表结构变更）
            "TRUNCATE" => super::dml::gen_truncate(stmt, seq, source, original, self),
            // ★ REPLACE INTO 部分支持（标 partial，见 §3.4 #6）：备份被覆盖旧行，无法 DELETE 新行
            "REPLACE" => super::dml::gen_replace(stmt, seq, source, original, self),
            // 不支持
            "MERGE" | "UPSERT" => unsupported(seq, source, original, "MERGE/UPSERT not supported, split into INSERT+UPDATE"),
            // 跳过
            "START_TRANSACTION" | "COMMIT" | "ROLLBACK" | "SET_VARIABLE" | "USE" => skip(seq, source, original),
            _ => unsupported(seq, source, original, &format!("Unsupported statement kind: {}", stmt.kind)),
        }
    }
}

/// 判断 ALTER TABLE 是否仅涉及纯元数据反向操作（RENAME COLUMN/TABLE、ADD INDEX/CONSTRAINT/PK、
/// **无约束的 ADD COLUMN**）。
///
/// ★ F17 修正：ADD COLUMN 仅当**不含** NOT NULL/DEFAULT/COMMENT/ON UPDATE 等列级约束时才视为
/// metadata-only（反向 DROP COLUMN 可行）；含约束时返回 false，走 ddl_like 全表 LIKE 路径，
/// 否则 DROP 后再 ADD 回去会丢失约束。
///
/// DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT/PK 等需要原表定义的子操作返回 false，走 ddl_like 路径。
pub fn is_metadata_only_alter(stmt: &StmtInfo) -> bool {
    if let Some(alter) = &stmt.alter_table {
        alter.operations.iter().all(|op| {
            match op.operation_type.as_str() {
                // ★ ADD COLUMN 必须检查列约束子句（F17）
                "ADD_COLUMN" => !alter_op_has_constraints(op),
                "ADD_CONSTRAINT" | "ADD_INDEX" | "RENAME_COLUMN"
                | "RENAME_TABLE" | "ADD_PRIMARY_KEY" => true,
                _ => false,  // DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT/PK 等
            }
        })
    } else {
        false
    }
}

/// 判断 ALTER 操作的 detail 字段是否含列级约束（NOT NULL/DEFAULT/COMMENT/ON UPDATE）。
///
/// AlterOpInfo 当前没有专门的约束字段，由 parser 写入 `detail` 文本，
/// 这里通过子串匹配判断（M1 阶段保守判断，M3 parser 完善后可改为结构化字段）。
pub fn alter_op_has_constraints(op: &crate::rule::engine::ast::AlterOpInfo) -> bool {
    let detail_upper = op.detail.to_uppercase();
    detail_upper.contains("NOT NULL")
        || detail_upper.contains("DEFAULT")
        || detail_upper.contains("COMMENT")
        || detail_upper.contains("ON UPDATE")
        || detail_upper.contains("AUTO_INCREMENT")
        || detail_upper.contains("UNIQUE")
        || detail_upper.contains("PRIMARY KEY")
        || detail_upper.contains("REFERENCES")
        || detail_upper.contains("CHECK")
}

/// 跳过事务控制 / SET 变量等语句：不生成 backup/rollback，manifest 仅记录原文。
pub fn skip(seq: u64, source: SourceRef, original: &str) -> BackupRollbackPair {
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: None,
        safety: SafetyClass::default(),
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec![],
    }
}

/// 不支持的语句：标记 `irreversible=true`，CI 阻断发布。
pub fn unsupported(seq: u64, source: SourceRef, original: &str, reason: &str) -> BackupRollbackPair {
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: None,
        safety: SafetyClass {
            reliable: false,
            partial: false,
            irreversible: true,
            ..Default::default()
        },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec![reason.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::engine::ast::{StmtInfo, AlterTableInfo, AlterOpInfo};

    fn make_alter_stmt(ops: Vec<AlterOpInfo>) -> StmtInfo {
        StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, truncate: None, create_view: None,
            create_index: None, transaction: None,
            alter_table: Some(AlterTableInfo {
                table_name: "t".to_string(),
                operations: ops,
                adds_primary_key: false,
                drops_primary_key: false,
                added_primary_key_columns: vec![],
            }),
        }
    }

    fn op(op_type: &str, detail: &str) -> AlterOpInfo {
        AlterOpInfo {
            operation_type: op_type.to_string(),
            column_name: String::new(),
            table_name: String::new(),
            constraint_name: String::new(),
            detail: detail.to_string(),
        }
    }

    #[test]
    fn metadata_only_add_column_no_constraint() {
        let stmt = make_alter_stmt(vec![op("ADD_COLUMN", "")]);
        assert!(is_metadata_only_alter(&stmt));
    }

    #[test]
    fn not_metadata_only_add_column_with_not_null() {
        let stmt = make_alter_stmt(vec![op("ADD_COLUMN", "INT NOT NULL")]);
        assert!(!is_metadata_only_alter(&stmt));
    }

    #[test]
    fn not_metadata_only_add_column_with_default() {
        let stmt = make_alter_stmt(vec![op("ADD_COLUMN", "VARCHAR(32) DEFAULT 'x'")]);
        assert!(!is_metadata_only_alter(&stmt));
    }

    #[test]
    fn not_metadata_only_drop_column() {
        let stmt = make_alter_stmt(vec![op("DROP_COLUMN", "")]);
        assert!(!is_metadata_only_alter(&stmt));
    }

    #[test]
    fn metadata_only_add_index() {
        let stmt = make_alter_stmt(vec![op("ADD_INDEX", "")]);
        assert!(is_metadata_only_alter(&stmt));
    }

    #[test]
    fn metadata_only_rename_column() {
        let stmt = make_alter_stmt(vec![op("RENAME_COLUMN", "")]);
        assert!(is_metadata_only_alter(&stmt));
    }

    #[test]
    fn not_metadata_only_when_no_alter_info() {
        let stmt = StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, truncate: None, create_view: None,
            create_index: None, transaction: None, alter_table: None,
        };
        assert!(!is_metadata_only_alter(&stmt));
    }

    #[test]
    fn unsupported_marks_irreversible() {
        let pair = unsupported(1, SourceRef::placeholder(), "MERGE INTO ...", "MERGE not supported");
        assert!(pair.safety.irreversible);
        assert!(!pair.safety.reliable);
        assert!(pair.rollback.is_none());
        assert!(pair.warnings.iter().any(|w| w.contains("MERGE")));
    }

    #[test]
    fn skip_returns_none_backup_rollback() {
        let pair = skip(1, SourceRef::placeholder(), "COMMIT");
        assert!(pair.backup.is_none());
        assert!(pair.rollback.is_none());
    }
}
