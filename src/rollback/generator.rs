//! 主调度：按 `StmtInfo.kind` 分发到 ddl / ddl_like / dml。
//!
//! 对应设计文档 §4.4。

use super::dialect::DialectRenderer;
use super::naming::NamingAllocator;
use super::pk::PrimaryKeyResolver;
use super::{strip_ident_quotes, BackupRollbackPair, BackupStrategy, SafetyClass, SourceRef};
use crate::config::{Config, RollbackConfig};
use crate::rule::engine::ast::StmtInfo;

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
}

impl<'a> RollbackGenerator<'a> {
    pub fn new(
        _config: &'a Config,
        rc: &'a RollbackConfig,
        renderer: &'a dyn DialectRenderer,
    ) -> Self {
        RollbackGenerator {
            renderer,
            naming: NamingAllocator::new(rc),
            pk_resolver: PrimaryKeyResolver::new(rc),
            seq: 0,
            backup_mode: rc.backup_mode.clone(),
        }
    }

    /// 对单条语句生成 backup/rollback。stmt_kind 决定走 ddl / ddl_like / dml 分支。
    pub fn generate(
        &mut self,
        stmt: &StmtInfo,
        source: SourceRef,
        original: &str,
    ) -> BackupRollbackPair {
        self.seq += 1;
        let seq = self.seq;

        // ★ 表名大小写规范化：全大写转小写（MySQL 约定全大写=不区分大小写），
        // 大小写混合保持原样（靠 quote_ident 引号包裹保留大小写）。
        // 在分发前规范化，所有子函数和 extract_target_table_from_stmt 自动用上规范化后的表名。
        let normalized = normalize_table_name_case(stmt);
        let stmt: &StmtInfo = &normalized;

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
}

/// 规范化 StmtInfo 中所有表名字段的大小写。
///
/// 规则：先剥掉外层引号，若裸名全大写（且含至少一个字母）则转小写，否则保留原样。
/// - 全大写（如 `USERS`、`ORDER_ITEMS`）→ 转小写 `users`、`order_items`
///   （MySQL 约定全大写=不区分大小写，目标库存储为小写）
/// - 大小写混合（如 `Users`、`OrderItems`）→ 保持原样，靠 `quote_ident` 引号包裹保留大小写
/// - 全小写 / 无字母 → 保持原样
///
/// 覆盖所有含表名的字段（create_table/drop_object/alter_table/insert/update/delete/
/// truncate/create_view/create_index）。仅规范化表名，不规范化列名。
fn normalize_table_name_case(stmt: &StmtInfo) -> StmtInfo {
    let mut s = stmt.clone();
    if let Some(ci) = &mut s.create_table {
        ci.table_name = to_lowercase_if_all_upper(&ci.table_name);
    }
    if let Some(di) = &mut s.drop_object {
        di.name = to_lowercase_if_all_upper(&di.name);
    }
    if let Some(ai) = &mut s.alter_table {
        ai.table_name = to_lowercase_if_all_upper(&ai.table_name);
    }
    if let Some(ii) = &mut s.insert {
        ii.table_name = to_lowercase_if_all_upper(&ii.table_name);
    }
    if let Some(ui) = &mut s.update {
        ui.table_name = to_lowercase_if_all_upper(&ui.table_name);
    }
    if let Some(di) = &mut s.delete {
        di.table_name = to_lowercase_if_all_upper(&di.table_name);
    }
    if let Some(ti) = &mut s.truncate {
        ti.table_name = to_lowercase_if_all_upper(&ti.table_name);
    }
    if let Some(vi) = &mut s.create_view {
        vi.name = to_lowercase_if_all_upper(&vi.name);
    }
    if let Some(cix) = &mut s.create_index {
        cix.table_name = to_lowercase_if_all_upper(&cix.table_name);
    }
    s
}

/// 若 `name` 去引号后为全大写（含至少一个字母）则返回小写形式，否则返回去引号后的原样。
///
/// 输入可能带外层引号（如 `` `USERS` `` 或 `"USERS"`），先 `strip_ident_quotes` 剥掉再判断。
fn to_lowercase_if_all_upper(name: &str) -> String {
    let bare = strip_ident_quotes(name);
    let has_letter = bare.chars().any(|c| c.is_ascii_alphabetic());
    let all_upper = bare
        .chars()
        .all(|c| !c.is_ascii_alphabetic() || c.is_ascii_uppercase());
    if has_letter && all_upper {
        bare.to_lowercase()
    } else {
        bare
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
                "ADD_CONSTRAINT" | "ADD_INDEX" | "RENAME_COLUMN" | "RENAME_TABLE"
                | "ADD_PRIMARY_KEY" => true,
                _ => false, // DROP/MODIFY COLUMN、DROP INDEX/CONSTRAINT/PK 等
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
pub fn unsupported(
    seq: u64,
    source: SourceRef,
    original: &str,
    reason: &str,
) -> BackupRollbackPair {
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
    use crate::rule::engine::ast::{AlterOpInfo, AlterTableInfo, StmtInfo};

    fn make_alter_stmt(ops: Vec<AlterOpInfo>) -> StmtInfo {
        StmtInfo {
            kind: "ALTER_TABLE".to_string(),
            line: 1,
            end_line: 1,
            column: 0,
            create_table: None,
            drop_object: None,
            select: None,
            insert: None,
            update: None,
            delete: None,
            truncate: None,
            create_view: None,
            create_index: None,
            transaction: None,
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
            line: 1,
            end_line: 1,
            column: 0,
            create_table: None,
            drop_object: None,
            select: None,
            insert: None,
            update: None,
            delete: None,
            truncate: None,
            create_view: None,
            create_index: None,
            transaction: None,
            alter_table: None,
        };
        assert!(!is_metadata_only_alter(&stmt));
    }

    #[test]
    fn unsupported_marks_irreversible() {
        let pair = unsupported(
            1,
            SourceRef::placeholder(),
            "MERGE INTO ...",
            "MERGE not supported",
        );
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
            structure: crate::config::StructureConfig {
                paths: vec![],
                strict: false,
                allow_extra: vec![],
            },
            classification: crate::config::ClassificationConfig {
                rules: vec![],
                default_type: "other".to_string(),
            },
            rules: vec![],
            rules_file: None,
            rules_dir: std::path::PathBuf::new(),
            output: crate::config::OutputConfig::default(),
            mapper: crate::config::MapperConfig::default(),
            scan: crate::config::ScanConfig::default(),
            file_check: crate::config::FileCheckConfig::default(),
            rollback: RollbackConfig {
                backup_mode: backup_mode.to_string(),
                ..RollbackConfig::default()
            },
            cache: crate::config::CacheConfig::default(),
            dialect: crate::config::CheckDialect::default(),
            dialect_fallback: None,
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
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "ALTER TABLE t DROP COLUMN x",
        );
        assert!(
            pair.safety.irreversible,
            "incremental 模式下 ALTER DROP COLUMN 应标 irreversible"
        );
        assert!(pair.rollback.is_none());
        assert!(pair
            .warnings
            .iter()
            .any(|w| w.contains("backup_mode=incremental")));
    }

    #[test]
    fn backup_mode_auto_allows_alter_drop_column() {
        // P1-1：auto 模式（默认）下，ALTER DROP COLUMN 走全表 LIKE 路径，不报错
        let (cfg, rc) = make_cfg_with_backup_mode("auto");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_alter_stmt(vec![op("DROP_COLUMN", "")]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "ALTER TABLE t DROP COLUMN x",
        );
        assert!(
            !pair.safety.irreversible,
            "auto 模式下 ALTER DROP COLUMN 不应标 irreversible"
        );
        assert!(pair.rollback.is_some());
    }

    #[test]
    fn backup_mode_full_allows_alter_drop_column() {
        // P1-1：full 模式下，ALTER DROP COLUMN 走全表 LIKE 路径（与 auto 一致）
        let (cfg, rc) = make_cfg_with_backup_mode("full");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_alter_stmt(vec![op("DROP_COLUMN", "")]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "ALTER TABLE t DROP COLUMN x",
        );
        assert!(!pair.safety.irreversible);
        assert!(pair.rollback.is_some());
    }

    // ===== 表名大小写规范化测试（全大写→小写，混合→保留） =====

    use crate::rule::engine::ast::InsertInfo;

    fn make_insert_stmt(table: &str, columns: &[&str]) -> StmtInfo {
        StmtInfo {
            kind: "INSERT".to_string(),
            line: 1,
            end_line: 1,
            column: 0,
            create_table: None,
            drop_object: None,
            select: None,
            insert: Some(InsertInfo {
                table_name: table.to_string(),
                columns: columns.iter().map(|s| s.to_string()).collect(),
            }),
            update: None,
            delete: None,
            alter_table: None,
            truncate: None,
            create_view: None,
            create_index: None,
            transaction: None,
        }
    }

    #[test]
    fn to_lowercase_if_all_upper_converts_all_uppercase() {
        // 全大写（含下划线数字）→ 转小写
        assert_eq!(to_lowercase_if_all_upper("USERS"), "users");
        assert_eq!(to_lowercase_if_all_upper("ORDER_ITEMS"), "order_items");
        assert_eq!(to_lowercase_if_all_upper("T1"), "t1");
    }

    #[test]
    fn to_lowercase_if_all_upper_preserves_mixed_case() {
        // 大小写混合 → 保留原样
        assert_eq!(to_lowercase_if_all_upper("Users"), "Users");
        assert_eq!(to_lowercase_if_all_upper("OrderItems"), "OrderItems");
        assert_eq!(to_lowercase_if_all_upper("orderId"), "orderId");
    }

    #[test]
    fn to_lowercase_if_all_upper_preserves_all_lowercase() {
        // 全小写 → 保留原样
        assert_eq!(to_lowercase_if_all_upper("users"), "users");
        assert_eq!(to_lowercase_if_all_upper("order_items"), "order_items");
    }

    #[test]
    fn to_lowercase_if_all_upper_strips_quotes_before_check() {
        // 带引号的全大写 → 剥引号后转小写
        assert_eq!(to_lowercase_if_all_upper("`USERS`"), "users");
        assert_eq!(to_lowercase_if_all_upper("\"ORDER_ITEMS\""), "order_items");
        // 带引号的大小写混合 → 剥引号后保留原样
        assert_eq!(to_lowercase_if_all_upper("`Users`"), "Users");
    }

    #[test]
    fn to_lowercase_if_all_upper_preserves_no_letter_names() {
        // 无字母（纯数字/符号）→ 保留原样
        assert_eq!(to_lowercase_if_all_upper("123"), "123");
        assert_eq!(to_lowercase_if_all_upper("_t1_"), "_t1_");
    }

    #[test]
    fn normalize_all_uppercase_insert_table_to_lowercase() {
        // 端到端：INSERT 全大写表名 → rollback DELETE 用小写表名
        let (cfg, rc) = make_cfg_with_backup_mode("auto");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_insert_stmt("USERS", &["id", "name"]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "INSERT INTO USERS (id, name) VALUES (1, 'alice')",
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(
            rollback.contains("`users`"),
            "全大写表名应转小写 `users`，got: {}",
            rollback
        );
        assert!(
            !rollback.contains("`USERS`"),
            "不应保留全大写 `USERS`，got: {}",
            rollback
        );
    }

    #[test]
    fn normalize_preserves_mixed_case_insert_table() {
        // 端到端：INSERT 大小写混合表名 → rollback DELETE 保留原大小写（引号包裹）
        let (cfg, rc) = make_cfg_with_backup_mode("auto");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_insert_stmt("Users", &["id", "name"]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "INSERT INTO Users (id, name) VALUES (1, 'alice')",
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(
            rollback.contains("`Users`"),
            "大小写混合表名应保留 `Users`，got: {}",
            rollback
        );
    }

    #[test]
    fn normalize_preserves_all_lowercase_insert_table() {
        // 端到端：INSERT 全小写表名 → 保持原样
        let (cfg, rc) = make_cfg_with_backup_mode("auto");
        let mut gen = RollbackGenerator::new(&cfg, &rc, &crate::rollback::dialect::MySqlRenderer);
        let stmt = make_insert_stmt("users", &["id", "name"]);
        let pair = gen.generate(
            &stmt,
            SourceRef::placeholder(),
            "INSERT INTO users (id, name) VALUES (1, 'alice')",
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(
            rollback.contains("`users`"),
            "全小写表名保持 `users`，got: {}",
            rollback
        );
    }
}
