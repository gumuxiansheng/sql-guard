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
    pub(crate) renderer: &'a dyn DialectRenderer,
    pub(crate) naming: NamingAllocator<'a>,
    pub(crate) pk_resolver: PrimaryKeyResolver,
    pub(crate) seq: u64,
    /// ★ P1-1：备份模式配置（auto/full/incremental），来自 RollbackConfig.backup_mode。
    /// - auto（默认）：保持现有分发逻辑
    /// - full：强制全表 LIKE（DML 行为不变，仅影响 ALTER 升级增量路径——当前未实现，故无效果）
    /// - incremental：ALTER DROP/MODIFY 缺 CREATE TABLE 上下文时报错（当前升级增量未实现，故所有 ALTER DROP/MODIFY 均报错）
    pub(crate) backup_mode: String,
    /// ★ BUG#2 修复：是否将表名统一转小写（对应 MySQL lower_case_table_names=1）。
    /// 启用后所有表名（含 bks_ 备份表名、_rb_ 影子表名）在生成脚本时统一小写。
    pub(crate) lower_case_table_names: bool,
}

impl<'a> RollbackGenerator<'a> {
    pub fn new(_config: &'a Config, rc: &'a RollbackConfig, renderer: &'a dyn DialectRenderer) -> Self {
        RollbackGenerator {
            renderer,
            naming: NamingAllocator::new(rc),
            pk_resolver: PrimaryKeyResolver::new(rc),
            seq: 0,
            backup_mode: rc.backup_mode.clone(),
            lower_case_table_names: rc.lower_case_table_names,
        }
    }

    /// 对单条语句生成 backup/rollback。stmt_kind 决定走 ddl / ddl_like / dml 分支。
    pub fn generate(&mut self, stmt: &StmtInfo, source: SourceRef, original: &str) -> BackupRollbackPair {
        self.seq += 1;
        let seq = self.seq;

        // ★ BUG#2 修复：按配置规范化表名大小写（lower_case_table_names=true 时统一小写）。
        // 在分发前规范化，所有子函数（ddl/dml/ddl_like）和 extract_target_table_from_stmt
        // 自动用上规范化后的表名，无需逐个修改子函数。
        let normalized;
        let stmt: &StmtInfo = if self.lower_case_table_names {
            normalized = self.normalize_stmt_table_names(stmt);
            &normalized
        } else {
            stmt
        };

        let mut pair = match stmt.kind.as_str() {
            // 纯反向 DDL（无需原表定义）
            "CREATE_TABLE" => super::ddl::gen_create_table(stmt, seq, source, original, self.renderer),
            "CREATE_INDEX" => super::ddl::gen_create_index(stmt, seq, source, original, self.renderer),
            "CREATE_VIEW" => super::ddl::gen_create_view(stmt, seq, source, original, self.renderer),
            // ALTER 中只需反向元数据的子操作（ADD COLUMN 仅当无约束时走此路径，见 is_metadata_only_alter）
            "ALTER_TABLE" if is_metadata_only_alter(stmt) => super::ddl::gen_alter_metadata(stmt, seq, source, original, self.renderer),
            // ★ 需要原表定义的 DDL → 统一走 CREATE TABLE LIKE 模式（含 ADD COLUMN 带约束的降级，见 F17）
            "DROP_TABLE" | "DROP_INDEX" | "DROP_VIEW"
            | "ALTER_TABLE"  // DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT、DROP PRIMARY KEY、带约束的 ADD COLUMN 等
                => {
                // ★ P1-1：backup_mode=incremental 时，ALTER DROP/MODIFY 缺 CREATE TABLE 上下文应报错。
                // 当前升级增量路径未实现（所有 ALTER DROP/MODIFY 都走全表 LIKE），
                // 故 incremental 模式下这些语句一律视为"缺上下文"，标 irreversible 阻断发布。
                // DROP_TABLE/DROP_INDEX/DROP_VIEW 不受 backup_mode 影响（本就无增量路径）。
                if self.backup_mode == "incremental" && stmt.kind == "ALTER_TABLE" {
                    unsupported(
                        seq, source, original,
                        "backup_mode=incremental requires CREATE TABLE context for ALTER DROP/MODIFY, \
                         which is not yet implemented; use backup_mode=auto or full",
                    )
                } else {
                    super::ddl_like::gen_with_full_backup(stmt, seq, source, original, self)
                }
            }
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
        };
        pair.stmt_kind = stmt.kind.clone();
        pair
    }

    /// ★ BUG#2 修复：规范化 StmtInfo 中所有表名字段为小写。
    ///
    /// 覆盖所有含表名的字段（create_table/drop_object/alter_table/insert/update/delete/
    /// truncate/create_view/create_index），确保后续 extract_target_table_from_stmt
    /// 及各 generate 子函数统一使用小写表名，适配 lower_case_table_names=1 的目标库。
    ///
    /// 注意：仅规范化表名，不规范化列名（列名大小写敏感性由 lower_case_file_system 控制，
    /// 与表名机制不同，不在本修复范围）。
    fn normalize_stmt_table_names(&self, stmt: &StmtInfo) -> StmtInfo {
        let mut s = stmt.clone();
        if let Some(ci) = &mut s.create_table {
            ci.table_name = ci.table_name.to_lowercase();
        }
        if let Some(di) = &mut s.drop_object {
            di.name = di.name.to_lowercase();
        }
        if let Some(ai) = &mut s.alter_table {
            ai.table_name = ai.table_name.to_lowercase();
        }
        if let Some(ii) = &mut s.insert {
            ii.table_name = ii.table_name.to_lowercase();
        }
        if let Some(ui) = &mut s.update {
            ui.table_name = ui.table_name.to_lowercase();
        }
        if let Some(di) = &mut s.delete {
            di.table_name = di.table_name.to_lowercase();
        }
        if let Some(ti) = &mut s.truncate {
            ti.table_name = ti.table_name.to_lowercase();
        }
        if let Some(vi) = &mut s.create_view {
            vi.name = vi.name.to_lowercase();
        }
        if let Some(cix) = &mut s.create_index {
            cix.table_name = cix.table_name.to_lowercase();
        }
        s
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
        stmt_kind: String::new(),
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
        stmt_kind: String::new(),
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

    // ===== P1-1: backup_mode 配置接入测试 =====

    fn make_cfg_with_backup_mode(backup_mode: &str) -> (Config, RollbackConfig) {
        let cfg = Config {
            structure: crate::config::StructureConfig { paths: vec![], strict: false, allow_extra: vec![] },
            classification: crate::config::ClassificationConfig { rules: vec![], default_type: "other".to_string() },
            rules: vec![], rules_file: None, rules_dir: std::path::PathBuf::new(),
            output: crate::config::OutputConfig::default(),
            mapper: crate::config::MapperConfig::default(),
            scan: crate::config::ScanConfig::default(),
            file_check: crate::config::FileCheckConfig::default(),
            rollback: RollbackConfig { backup_mode: backup_mode.to_string(), ..RollbackConfig::default() },
            cache: crate::config::CacheConfig::default(),
            dialect: crate::config::CheckDialect::default(),
        };
        let rc = cfg.rollback.clone();
        (cfg, rc)
    }

    #[test]
    fn backup_mode_incremental_blocks_alter_drop_column() {
        // P1-1：backup_mode=incremental 时，ALTER DROP COLUMN 应标 irreversible 并报错
        let (cfg, rc) = make_cfg_with_backup_mode("incremental");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_alter_stmt(vec![op("DROP_COLUMN", "")]);
        let pair = gen.generate(&stmt, SourceRef::placeholder(), "ALTER TABLE t DROP COLUMN x");
        assert!(pair.safety.irreversible, "incremental 模式下 ALTER DROP COLUMN 应标 irreversible");
        assert!(pair.rollback.is_none());
        assert!(pair.warnings.iter().any(|w| w.contains("backup_mode=incremental")));
    }

    #[test]
    fn backup_mode_auto_allows_alter_drop_column() {
        // P1-1：auto 模式（默认）下，ALTER DROP COLUMN 走全表 LIKE 路径，不报错
        let (cfg, rc) = make_cfg_with_backup_mode("auto");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_alter_stmt(vec![op("DROP_COLUMN", "")]);
        let pair = gen.generate(&stmt, SourceRef::placeholder(), "ALTER TABLE t DROP COLUMN x");
        assert!(!pair.safety.irreversible, "auto 模式下 ALTER DROP COLUMN 不应标 irreversible");
        assert!(pair.rollback.is_some());
    }

    #[test]
    fn backup_mode_full_allows_alter_drop_column() {
        // P1-1：full 模式下，ALTER DROP COLUMN 走全表 LIKE 路径（与 auto 一致）
        let (cfg, rc) = make_cfg_with_backup_mode("full");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_alter_stmt(vec![op("DROP_COLUMN", "")]);
        let pair = gen.generate(&stmt, SourceRef::placeholder(), "ALTER TABLE t DROP COLUMN x");
        assert!(!pair.safety.irreversible);
        assert!(pair.rollback.is_some());
    }

    // ===== BUG#2: lower_case_table_names 表名小写化测试 =====

    use crate::rule::engine::ast::{InsertInfo, DeleteInfo, TruncateInfo, DropInfo};

    fn make_cfg_with_lower_case(lower: bool) -> (Config, RollbackConfig) {
        let cfg = Config {
            structure: crate::config::StructureConfig { paths: vec![], strict: false, allow_extra: vec![] },
            classification: crate::config::ClassificationConfig { rules: vec![], default_type: "other".to_string() },
            rules: vec![], rules_file: None, rules_dir: std::path::PathBuf::new(),
            output: crate::config::OutputConfig::default(),
            mapper: crate::config::MapperConfig::default(),
            scan: crate::config::ScanConfig::default(),
            file_check: crate::config::FileCheckConfig::default(),
            rollback: RollbackConfig { lower_case_table_names: lower, ..RollbackConfig::default() },
            cache: crate::config::CacheConfig::default(),
            dialect: crate::config::CheckDialect::default(),
        };
        let rc = cfg.rollback.clone();
        (cfg, rc)
    }

    fn make_insert_stmt_mixed_case(table: &str, columns: &[&str]) -> StmtInfo {
        StmtInfo {
            kind: "INSERT".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None,
            insert: Some(InsertInfo {
                table_name: table.to_string(),
                columns: columns.iter().map(|s| s.to_string()).collect(),
            }),
            update: None, delete: None, alter_table: None,
            truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_delete_stmt_mixed_case(table: &str, where_clause: Option<&str>) -> StmtInfo {
        StmtInfo {
            kind: "DELETE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None,
            delete: Some(DeleteInfo {
                table_name: table.to_string(),
                where_clause: where_clause.map(|s| s.to_string()),
            }),
            alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_truncate_stmt_mixed_case(table: &str) -> StmtInfo {
        StmtInfo {
            kind: "TRUNCATE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, alter_table: None,
            truncate: Some(TruncateInfo {
                table_name: table.to_string(),
                has_table_keyword: true,
            }),
            create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_drop_table_stmt_mixed_case(table: &str) -> StmtInfo {
        StmtInfo {
            kind: "DROP_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None,
            drop_object: Some(DropInfo {
                object_type: "TABLE".to_string(),
                name: table.to_string(),
                if_exists: false,
            }),
            select: None, insert: None, update: None, delete: None, alter_table: None,
            truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    #[test]
    fn normalize_lowercases_all_table_name_fields() {
        // ★ BUG#2 单元测试：normalize_stmt_table_names 应把所有表名字段转为小写
        let (cfg, rc) = make_cfg_with_lower_case(true);
        let gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = StmtInfo {
            kind: "INSERT".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None,
            insert: Some(InsertInfo {
                table_name: "Users".to_string(),
                columns: vec!["ID".to_string(), "Name".to_string()],
            }),
            update: None, delete: None, alter_table: None,
            truncate: None, create_view: None, create_index: None, transaction: None,
        };
        let normalized = gen.normalize_stmt_table_names(&stmt);
        let insert = normalized.insert.expect("insert should exist");
        assert_eq!(insert.table_name, "users", "table name should be lowercased");
        // 列名不规范化（仅表名）
        assert_eq!(insert.columns, vec!["ID", "Name"], "column names should NOT be lowercased");
    }

    #[test]
    fn normalize_lowercases_drop_object_name() {
        // ★ BUG#2：DROP_TABLE 的 drop_object.name 也应被小写化
        let (cfg, rc) = make_cfg_with_lower_case(true);
        let gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_drop_table_stmt_mixed_case("Orders");
        let normalized = gen.normalize_stmt_table_names(&stmt);
        let drop = normalized.drop_object.expect("drop_object should exist");
        assert_eq!(drop.name, "orders", "drop object name should be lowercased");
    }

    #[test]
    fn normalize_lowercases_alter_table_name() {
        // ★ BUG#2：ALTER_TABLE 的 alter_table.table_name 应被小写化
        let (cfg, rc) = make_cfg_with_lower_case(true);
        let gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let mut stmt = make_alter_stmt(vec![op("ADD_COLUMN", "INT")]);
        if let Some(ai) = &mut stmt.alter_table {
            ai.table_name = "UserProfiles".to_string();
        }
        let normalized = gen.normalize_stmt_table_names(&stmt);
        let alter = normalized.alter_table.expect("alter_table should exist");
        assert_eq!(alter.table_name, "userprofiles", "alter table name should be lowercased");
    }

    #[test]
    fn lower_case_table_names_enabled_insert_rollback_uses_lowercase() {
        // ★ BUG#2 端到端：lower_case_table_names=true 时，INSERT 大写表名生成的 DELETE 应用小写表名
        let (cfg, rc) = make_cfg_with_lower_case(true);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        // 无 PK → 全列匹配，rollback 必然包含表名
        let stmt = make_insert_stmt_mixed_case("Users", &["id", "name"]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "INSERT INTO Users (id, name) VALUES (1, 'alice')",
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(
            rollback.contains("`users`"),
            "rollback should use lowercase table name `users`, got: {}",
            rollback
        );
        assert!(
            !rollback.contains("`Users`"),
            "rollback should NOT contain uppercase `Users`, got: {}",
            rollback
        );
    }

    #[test]
    fn lower_case_table_names_disabled_preserves_original_case() {
        // ★ BUG#2 回归：lower_case_table_names=false（默认）时，表名大小写保持原样
        let (cfg, rc) = make_cfg_with_lower_case(false);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_insert_stmt_mixed_case("Users", &["id", "name"]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "INSERT INTO Users (id, name) VALUES (1, 'alice')",
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(
            rollback.contains("`Users`"),
            "rollback should preserve original case `Users` when lower_case_table_names=false, got: {}",
            rollback
        );
    }

    #[test]
    fn lower_case_table_names_enabled_delete_backup_uses_lowercase() {
        // ★ BUG#2 端到端：lower_case_table_names=true 时，DELETE 生成的 backup 表名（bks_）也用小写
        let (cfg, rc) = make_cfg_with_lower_case(true);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_delete_stmt_mixed_case("Orders", Some("id = 1"));
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "DELETE FROM Orders WHERE id = 1",
        );
        let backup = pair.backup.expect("backup should exist");
        assert!(
            backup.to_lowercase().contains("bks_orders"),
            "backup should use lowercase bks_orders, got: {}",
            backup
        );
        assert!(
            !backup.contains("bks_Orders"),
            "backup should NOT contain mixed-case bks_Orders, got: {}",
            backup
        );
        // 原表名也应小写
        assert!(
            backup.contains("`orders`"),
            "backup should reference lowercase `orders`, got: {}",
            backup
        );
    }

    #[test]
    fn lower_case_table_names_enabled_truncate_uses_lowercase() {
        // ★ BUG#2 端到端：TRUNCATE 的 backup/rollback 表名都应小写
        let (cfg, rc) = make_cfg_with_lower_case(true);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_truncate_stmt_mixed_case("Sessions");
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "TRUNCATE TABLE Sessions",
        );
        let backup = pair.backup.expect("backup should exist");
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(backup.contains("`sessions`"), "backup should use `sessions`, got: {}", backup);
        assert!(rollback.contains("`sessions`"), "rollback should use `sessions`, got: {}", rollback);
        assert!(backup.to_lowercase().contains("bks_sessions"), "backup table name should be bks_sessions, got: {}", backup);
    }
}
