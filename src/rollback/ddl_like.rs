//! CREATE TABLE LIKE 统一模式：DROP/ALTER DROP/MODIFY/DROP INDEX/PK/TRUNCATE/DROP VIEW/
//! 带约束的 ADD COLUMN。
//!
//! 对应设计文档 §4.6.1。
//!
//! ★ 关键约束（必须落地，否则就是 P0/P1 翻车）：
//! - backup.sql 必须**幂等**（F9）：`DROP TABLE IF EXISTS bks_xxx; CREATE TABLE bks_xxx LIKE t; INSERT ...`
//! - backup.sql 每段必须**加锁**（F14）：MySQL 用全局 `FLUSH TABLES WITH READ LOCK`（★ 乙-2/N4 修正：
//!   per-table 形式会被隐式提交释放，是空锁）；PG 用 `LOCK TABLE t IN ACCESS SHARE MODE`
//! - rollback.sql 在 MySQL 方言下必须走 **F12 原子 RENAME 切换**（DROP+CREATE+INSERT 三步会因
//!   隐式提交导致 DROP 后 CREATE 失败原表丢失，P0-1）；PG 方言走事务内 DROP+CREATE LIKE+INSERT
//! - 每段必须生成 **F13 schema 校验语句**（只读 SELECT 形式）
//! - RENAME 切换前必须 `SET FOREIGN_KEY_CHECKS=0`（MySQL）/ `SET CONSTRAINTS ALL DEFERRED`（PG）
//! - 标记 `partial = true`（外键/触发器在 LIKE 中不保留）
//!
//! M1 阶段为骨架实现，M4 阶段补全 expected_schema.columns 推断、partitioned 回填、
//! has_auto_increment_or_serial 等辅助逻辑。

use crate::rule::engine::ast::StmtInfo;
use super::dialect::{DialectRenderer, AtomicStrategy};
use super::generator::RollbackGenerator;
use super::{BackupRollbackPair, SourceRef, SafetyClass, BackupStrategy, ExpectedSchema, extract_target_table_from_stmt};

/// 对需要原表定义的 DDL 语句统一采用 CREATE TABLE LIKE 模式生成 backup/rollback。
pub fn gen_with_full_backup(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    gen: &mut RollbackGenerator,
) -> BackupRollbackPair {
    let r = gen.renderer;
    let table = extract_target_table_from_stmt(stmt);
    let bks_name = gen.naming.alloc(&table);
    let shadow_name = gen.naming.alloc_shadow(seq, &table);

    // ★ F13 生成 schema 校验语句（只读 SELECT，驱动友好）
    let schema_check = r.render_schema_check(&table, &bks_name);

    // ★ F15 分区表检测语句
    let partition_check = r.render_partition_check(&table);

    // 视图与表分支
    let is_view = matches!(stmt.kind.as_str(), "DROP_VIEW");

    let (backup, rollback, mut safety_extra, mut warnings) = if is_view {
        // 视图无锁、无 LIKE、无 RENAME 切换
        (
            format!(
                "CREATE VIEW {} AS SELECT * FROM {};",
                r.quote_ident(&bks_name),
                r.quote_ident(&table)
            ),
            format!(
                "CREATE OR REPLACE VIEW {} AS SELECT * FROM {};",
                r.quote_ident(&table),
                r.quote_ident(&bks_name)
            ),
            SafetyClass { partial: true, ..Default::default() },
            vec![r.partial_like_warning()],
        )
    } else {
        // ★ backup.sql：幂等 DROP+CREATE + 段内锁（FTWRL / ACCESS SHARE）
        let backup = r.render_idempotent_backup(&table, &bks_name);
        // ★ rollback.sql：按"语句类型 × 方言"走三策略之一
        let (rollback, irrev_if_missing) = match r.atomic_ddl_rollback_strategy(&stmt.kind) {
            AtomicStrategy::AtomicRename => (
                r.render_atomic_rename_rollback(&table, &bks_name, &shadow_name),
                false,
            ),
            AtomicStrategy::Transactional => (
                r.render_transactional_rollback(&table, &bks_name),
                false,
            ),
            // ★ DROP TABLE 特例：原表已不存在，无法 RENAME，直接从 bks_ 重建
            // 非原子，CREATE 失败则原表无法恢复
            AtomicStrategy::RebuildFromBackup => (
                r.render_rebuild_from_backup_rollback(&table, &bks_name),
                true,
            ),
        };
        let safety = SafetyClass {
            partial: true,
            counter_unrestored: has_auto_increment_or_serial(stmt),
            requires_lock: true,
            irreversible_if_backup_missing: irrev_if_missing,
            ..Default::default()
        };
        (backup, rollback, safety, vec![r.partial_like_warning()])
    };

    // ★ N8：lock_type 由 lock_scope 决定，M1 暂以 global=FTWRL 兜底（M5 接入 resolve_lock_type）
    let lock_type = if safety_extra.requires_lock {
        if matches!(stmt.kind.as_str(), "DROP_VIEW") {
            Some("NONE".to_string())
        } else {
            Some("FTWRL".to_string())
        }
    } else {
        None
    };
    safety_extra.lock_type = lock_type;

    // ★ N3：expected_schema 一律 Some，row_count 占位 0（生成期不可知）
    let expected_schema = ExpectedSchema {
        table_exists: true,
        row_count: 0,
        columns: extract_expected_columns(stmt),
    };

    let backup_sql = format!("{}\n{}", schema_check, backup);
    let rollback_sql = format!("{}\n{}\n{}", schema_check, partition_check, rollback);

    // 把 schema_check 和 partition_check 提示作为 warning（便于 manifest 审计）
    if !partition_check.is_empty() {
        warnings.push("F15 partition check emitted; platform should verify and apply on_partitioned_table policy".to_string());
    }

    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: Some(backup_sql),
        rollback: Some(rollback_sql),
        safety: safety_extra,
        strategy: BackupStrategy {
            backup_mode: "full".to_string(),
            incremental_source: None,
            column_constraints_check: None,
        },
        expected_schema: Some(expected_schema),
        warnings,
    }
}

/// 从 StmtInfo 推断 expected_schema.columns。
///
/// M4 补全：
/// - CREATE TABLE：从 create_table.columns 提取完整列定义（name + data_type）
/// - ALTER TABLE DROP COLUMN：被 drop 的列名加入期望（data_type 空，表示"列存在即可"），
///   用于 F13 回滚后校验该列是否被成功还原
/// - ALTER TABLE MODIFY/ALTER COLUMN：被改的列名加入期望（data_type 空，因为旧类型静态不可知）
/// - 其余场景（DROP TABLE/INDEX/VIEW）：原表完整列定义静态不可知（除非同脚本内有 CREATE TABLE
///   上下文，跨语句上下文 M4 不做），返回空 Vec，依赖 row_count + table_exists 校验
fn extract_expected_columns(stmt: &StmtInfo) -> Vec<super::ExpectedColumn> {
    if let Some(ci) = &stmt.create_table {
        return ci.columns.iter().map(|c| super::ExpectedColumn {
            name: c.name.clone(),
            data_type: c.data_type.clone(),
        }).collect();
    }
    // ALTER TABLE：从 operations 提取被 DROP/MODIFY 的列名
    if let Some(alter) = &stmt.alter_table {
        return alter.operations.iter()
            .filter_map(|op| match op.operation_type.as_str() {
                "DROP_COLUMN" | "ALTER_COLUMN" if !op.column_name.is_empty() => {
                    Some(super::ExpectedColumn {
                        name: op.column_name.clone(),
                        data_type: String::new(),  // 旧类型静态不可知
                    })
                }
                _ => None,
            })
            .collect();
    }
    Vec::new()
}

/// 检测语句涉及的表是否含 AUTO_INCREMENT（MySQL）/ SERIAL（PG）列。
///
/// M4 补全：
/// - CREATE TABLE：检查列级 is_auto_increment 或 data_type 含 SERIAL
/// - ALTER TABLE ADD COLUMN：检查 operation.detail 是否含 AUTO_INCREMENT/SERIAL
///   （带 AUTO_INCREMENT 的 ADD COLUMN 会被 alter_op_has_constraints 判为 true，走 ddl_like 路径）
/// - 其余场景：返回 false，counter_unrestored 由发布平台运行期校验
fn has_auto_increment_or_serial(stmt: &StmtInfo) -> bool {
    if let Some(ci) = &stmt.create_table {
        return ci.columns.iter().any(|c| c.is_auto_increment || c.data_type.to_uppercase().contains("SERIAL"));
    }
    if let Some(alter) = &stmt.alter_table {
        return alter.operations.iter().any(|op| {
            if op.operation_type != "ADD_COLUMN" {
                return false;
            }
            let d = op.detail.to_uppercase();
            d.contains("AUTO_INCREMENT") || d.contains("SERIAL")
        });
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RollbackConfig};
    use crate::rule::engine::ast::{StmtInfo, DropInfo, TruncateInfo, AlterTableInfo, AlterOpInfo, CreateInfo, ColumnInfo};
    use crate::rollback::{dialect::{MySqlRenderer, PostgreSqlRenderer}, RollbackGenerator};

    fn make_drop_table_stmt() -> StmtInfo {
        StmtInfo {
            kind: "DROP_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None,
            drop_object: Some(DropInfo { object_type: "TABLE".to_string(), name: "users".to_string(), if_exists: false }),
            select: None, insert: None, update: None, delete: None,
            alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_truncate_stmt() -> StmtInfo {
        StmtInfo {
            kind: "TRUNCATE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None, update: None, delete: None,
            alter_table: None,
            truncate: Some(TruncateInfo { table_name: "logs".to_string(), has_table_keyword: true }),
            create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_cfg() -> Config {
        Config { structure: crate::config::StructureConfig { paths: vec![], strict: false, allow_extra: vec![] }, classification: crate::config::ClassificationConfig { rules: vec![], default_type: "other".to_string() }, rules: vec![], rules_file: None, rules_dir: std::path::PathBuf::new(), output: crate::config::OutputConfig::default(), mapper: crate::config::MapperConfig::default(), scan: crate::config::ScanConfig::default(), file_check: crate::config::FileCheckConfig::default(), rollback: RollbackConfig::default(), dialect: crate::config::CheckDialect::default() }
    }

    fn make_alter_drop_column_stmt() -> StmtInfo {
        StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, truncate: None, create_view: None,
            create_index: None, transaction: None,
            alter_table: Some(AlterTableInfo {
                table_name: "users".to_string(),
                operations: vec![AlterOpInfo {
                    operation_type: "DROP_COLUMN".to_string(),
                    column_name: "age".to_string(),
                    table_name: String::new(),
                    constraint_name: String::new(),
                    detail: "DROP COLUMN age".to_string(),
                }],
                adds_primary_key: false,
                drops_primary_key: false,
                added_primary_key_columns: vec![],
            }),
        }
    }

    fn make_create_table_with_auto_increment_stmt() -> StmtInfo {
        StmtInfo {
            kind: "CREATE_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: Some(CreateInfo {
                table_name: "users".to_string(),
                columns: vec![ColumnInfo {
                    name: "id".to_string(),
                    data_type: "BIGINT".to_string(),
                    is_auto_increment: true,
                    ..Default::default()
                }],
                ..Default::default()
            }),
            drop_object: None, select: None, insert: None, update: None, delete: None,
            alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_alter_add_auto_increment_column_stmt() -> StmtInfo {
        StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, truncate: None, create_view: None,
            create_index: None, transaction: None,
            alter_table: Some(AlterTableInfo {
                table_name: "users".to_string(),
                operations: vec![AlterOpInfo {
                    operation_type: "ADD_COLUMN".to_string(),
                    column_name: "seq".to_string(),
                    table_name: String::new(),
                    constraint_name: String::new(),
                    detail: "ADD COLUMN seq INT AUTO_INCREMENT".to_string(),
                }],
                adds_primary_key: false,
                drops_primary_key: false,
                added_primary_key_columns: vec![],
            }),
        }
    }

    #[test]
    fn drop_table_mysql_uses_rebuild_from_backup() {
        let cfg = make_cfg();
        let rc = RollbackConfig::default();
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_drop_table_stmt();
        let pair = gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "DROP TABLE users", &mut gen);
        assert!(pair.backup.is_some());
        assert!(pair.rollback.is_some());
        assert!(pair.safety.partial);
        assert!(pair.safety.irreversible_if_backup_missing);
        assert_eq!(pair.safety.lock_type.as_deref(), Some("FTWRL"));
        let backup = pair.backup.unwrap();
        assert!(backup.contains("FLUSH TABLES WITH READ LOCK;"));
        assert!(backup.contains("CREATE TABLE `bks_users_"));
        let rollback = pair.rollback.unwrap();
        // DROP TABLE → RebuildFromBackup → CREATE TABLE users LIKE bks_users
        assert!(rollback.contains("CREATE TABLE `users` LIKE `bks_users_"));
    }

    #[test]
    fn drop_table_pg_uses_rebuild_from_backup() {
        let cfg = make_cfg();
        let rc = RollbackConfig::default();
        let mut gen = RollbackGenerator::new(&cfg, &rc, &PostgreSqlRenderer);
        let stmt = make_drop_table_stmt();
        let pair = gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "DROP TABLE users", &mut gen);
        let rollback = pair.rollback.unwrap();
        assert!(rollback.contains("CREATE TABLE \"users\" (LIKE \"bks_users_"));
        assert!(rollback.contains("INSERT INTO \"users\" SELECT * FROM \"bks_users_"));
    }

    // ===== M4 推断逻辑测试 =====

    #[test]
    fn alter_drop_column_includes_dropped_column_in_expected_schema() {
        // M4：ALTER DROP COLUMN 的被 drop 列名应进入 expected_schema.columns（data_type 空）
        let pair = {
            let cfg = make_cfg();
            let rc = RollbackConfig::default();
            let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
            let stmt = make_alter_drop_column_stmt();
            gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users DROP COLUMN age", &mut gen)
        };
        let schema = pair.expected_schema.expect("expected_schema should be Some");
        assert_eq!(schema.columns.len(), 1);
        assert_eq!(schema.columns[0].name, "age");
        assert!(schema.columns[0].data_type.is_empty(), "data_type should be empty (旧类型静态不可知)");
    }

    #[test]
    fn create_table_with_auto_increment_marks_counter_unrestored() {
        // M4：CREATE TABLE 含 AUTO_INCREMENT 列时 counter_unrestored = true
        let pair = {
            let cfg = make_cfg();
            let rc = RollbackConfig::default();
            let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
            let stmt = make_create_table_with_auto_increment_stmt();
            gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "CREATE TABLE users (id BIGINT AUTO_INCREMENT, ...)", &mut gen)
        };
        assert!(pair.safety.counter_unrestored, "AUTO_INCREMENT 列应触发 counter_unrestored");
        // CREATE TABLE 上下文也应填入 expected_schema.columns
        let schema = pair.expected_schema.expect("expected_schema should be Some");
        assert_eq!(schema.columns.len(), 1);
        assert_eq!(schema.columns[0].name, "id");
        assert_eq!(schema.columns[0].data_type, "BIGINT");
    }

    #[test]
    fn alter_add_auto_increment_column_marks_counter_unrestored() {
        // M4：ALTER ADD COLUMN 含 AUTO_INCREMENT 时 counter_unrestored = true
        // （带 AUTO_INCREMENT 的 ADD COLUMN 被 alter_op_has_constraints 判为 true，走 ddl_like 路径）
        let pair = {
            let cfg = make_cfg();
            let rc = RollbackConfig::default();
            let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
            let stmt = make_alter_add_auto_increment_column_stmt();
            gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users ADD COLUMN seq INT AUTO_INCREMENT", &mut gen)
        };
        assert!(pair.safety.counter_unrestored, "ALTER ADD COLUMN AUTO_INCREMENT 应触发 counter_unrestored");
    }

    #[test]
    fn drop_table_expected_schema_has_empty_columns_but_table_exists() {
        // M4：DROP TABLE 原表完整列定义静态不可知，columns 为空 Vec，但 table_exists=true
        let pair = {
            let cfg = make_cfg();
            let rc = RollbackConfig::default();
            let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
            let stmt = make_drop_table_stmt();
            gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "DROP TABLE users", &mut gen)
        };
        let schema = pair.expected_schema.expect("expected_schema should be Some");
        assert!(schema.table_exists);
        assert!(schema.columns.is_empty(), "DROP TABLE 静态不可知列定义");
    }

    #[test]
    fn alter_drop_column_emits_partition_check_warning() {
        // M4：partitioned 静态不可知，依赖运行期 partition_check，warning 应提示
        let pair = {
            let cfg = make_cfg();
            let rc = RollbackConfig::default();
            let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
            let stmt = make_alter_drop_column_stmt();
            gen_with_full_backup(&stmt, 1, SourceRef::placeholder(), "ALTER TABLE users DROP COLUMN age", &mut gen)
        };
        assert!(pair.warnings.iter().any(|w| w.contains("partition check")),
            "应有 partition check warning");
        // safety.partitioned 静态不可知，保持 false（运行期由 partition_check SELECT 判断）
        assert!(!pair.safety.partitioned);
    }
}
