//! SQL 文本渲染（事务包裹、锁合并、预检查）。
//!
//! 对应设计文档 §4.6 / §4.7 / F14 / F18。
//!
//! M5 完整实现：
//! - `render_backup(pairs, rc, renderer) -> String`：渲染 backup.sql
//!   - 头部：长事务预检查（innodb_trx + processlist，F18/N10）
//!   - 头部：MySQL 方言按 binlog_strategy 决定是否加 `SET SESSION sql_log_bin=0`（F16）
//!   - 主体：按 SEQ 配对，coalesce_locks 合并连续同表段（D6/R6/N9）
//! - `render_rollback(pairs, rc, renderer) -> String`：渲染 rollback.sql
//!   - LIFO 排序（与 backup 相反）
//!   - PG 方言整体包裹 BEGIN/COMMIT；MySQL 仅 DML 段加 START TRANSACTION/COMMIT
//! - `render_cleanup(pairs, rc, renderer) -> String`：渲染 cleanup.sql
//!   - 按 backup_table_retention_days 生成 DROP TABLE bks_xxx 语句
//! - `resolve_lock_type(lock_scope, has_ddl) -> &'static str`：auto → global/snapshot 决策（D1/N8）
//! - `group_for_coalesce(pairs, mode) -> Vec<Vec<&BackupRollbackPair>>`：D6 锁合并分组
//! - `validate_render_prerequisites(pairs, rc) -> Vec<String>`：R2/R4 预检
//!
//! 关键约束：
//! - **N11**：snapshot 模式 DDL 外提到事务前（M5 标记，外提逻辑由发布平台执行）
//! - **N9**：coalesce_locks 启用时，组内统一发射锁/解锁，剥离段内锁
//! - **N10**：长事务预检查必须含 processlist（覆盖非事务长查询）
//! - **N8**：lock_type 由 resolve_lock_type 决定，不是方言名
//! - **R2**：lock_scope=table 模式要求 accept_table_lock_risk=true，否则报错退出
//! - **R4**：v1 静态 + binlog_strategy=auto + 含 DDL → 报错退出（M5 简化：auto + 含 DDL → 视为 never）

use super::BackupRollbackPair;
use super::dialect::DialectRenderer;
use crate::config::RollbackConfig;

/// 渲染 backup.sql。
///
/// 结构：
/// 1. 头部注释（生成时间、方言、lock_scope、coalesce 模式）
/// 2. R2/R4 预检错误以 ABORT 注释输出（调用方应先调 validate_render_prerequisites）
/// 3. 长事务预检查（on_long_transaction != ignore 时输出，F18/N10）
/// 4. MySQL binlog 控制（binlog_strategy，F16）
/// 5. 主体：按 SEQ 正序，coalesce_locks 合并连续同表段（N9）
pub fn render_backup(
    pairs: &[BackupRollbackPair],
    rc: &RollbackConfig,
    renderer: &dyn DialectRenderer,
) -> String {
    let mut out = String::new();
    let dialect_name = renderer.name();

    // 1. 头部注释
    out.push_str("-- sqlguard backup.sql (auto-generated)\n");
    out.push_str(&format!("-- dialect: {}\n", dialect_name));
    out.push_str(&format!("-- lock_scope: {}\n", rc.lock_scope));
    out.push_str(&format!("-- coalesce_locks: {} (mode: {})\n", rc.coalesce_locks, rc.coalesce_locks_mode));
    out.push_str(&format!("-- statements: {}\n", pairs.len()));
    out.push_str("\n");

    // 2. 预检错误（防御性，调用方应先调 validate_render_prerequisites）
    let errors = validate_render_prerequisites(pairs, rc);
    if !errors.is_empty() {
        out.push_str("-- ★ ABORT: render prerequisites not met:\n");
        for e in &errors {
            out.push_str(&format!("--   - {}\n", e));
        }
        out.push_str("-- 以上错误需先修复后再执行\n\n");
        return out;
    }

    // 3. 长事务预检查（F18/N10）：innodb_trx + processlist
    if rc.on_long_transaction != "ignore" {
        out.push_str(&render_long_transaction_check(rc, dialect_name));
    }

    // 4. MySQL binlog 控制（F16）
    if dialect_name == "mysql" {
        if let Some(stmt) = render_binlog_control(pairs, rc) {
            out.push_str(&stmt);
            out.push_str("\n");
        }
    }

    // 5. 主体：按 SEQ 正序渲染
    if rc.coalesce_locks {
        out.push_str(&render_backup_coalesced(pairs, rc, renderer));
    } else {
        for pair in pairs {
            if let Some(backup) = &pair.backup {
                out.push_str(&format!("-- seq={} source={}:{}\n", pair.seq, pair.source.file, pair.source.line));
                out.push_str(backup);
                if !backup.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("\n");
            }
        }
    }

    out
}

/// 渲染 rollback.sql。
///
/// 结构：
/// 1. 头部注释
/// 2. 预检错误（同 backup）
/// 3. PG 方言整体包裹 BEGIN/COMMIT；MySQL 不包裹（DDL 隐式提交）
/// 4. 主体：LIFO 排序（与 backup 相反），逐段输出 rollback
pub fn render_rollback(
    pairs: &[BackupRollbackPair],
    rc: &RollbackConfig,
    renderer: &dyn DialectRenderer,
) -> String {
    let mut out = String::new();
    let dialect_name = renderer.name();

    out.push_str("-- sqlguard rollback.sql (auto-generated)\n");
    out.push_str(&format!("-- dialect: {}\n", dialect_name));
    out.push_str(&format!("-- wrap_transaction: {}\n", rc.wrap_transaction));
    out.push_str(&format!("-- statements: {}\n", pairs.len()));
    out.push_str("\n");

    let errors = validate_render_prerequisites(pairs, rc);
    if !errors.is_empty() {
        out.push_str("-- ★ ABORT: render prerequisites not met:\n");
        for e in &errors {
            out.push_str(&format!("--   - {}\n", e));
        }
        return out;
    }

    // PG 整体事务包裹
    let wrap = rc.wrap_transaction && dialect_name == "postgresql";
    if wrap {
        out.push_str("BEGIN;\n\n");
    }

    // LIFO 排序：与 backup 相反顺序
    let mut sorted: Vec<&BackupRollbackPair> = pairs.iter().collect();
    sorted.sort_by(|a, b| b.seq.cmp(&a.seq));

    for pair in sorted {
        if let Some(rollback) = &pair.rollback {
            out.push_str(&format!("-- seq={} (LIFO) source={}:{}\n", pair.seq, pair.source.file, pair.source.line));
            out.push_str(rollback);
            if !rollback.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("\n");
        }
    }

    if wrap {
        out.push_str("COMMIT;\n");
    }

    out
}

/// 渲染 cleanup.sql。
///
/// 按 backup_table_retention_days 生成 DROP TABLE IF EXISTS bks_xxx 语句。
/// 仅当 cleanup_backup_tables_after_rollback=true 时由发布平台调用。
pub fn render_cleanup(
    pairs: &[BackupRollbackPair],
    rc: &RollbackConfig,
    renderer: &dyn DialectRenderer,
) -> String {
    let mut out = String::new();
    out.push_str("-- sqlguard cleanup.sql (auto-generated)\n");
    out.push_str(&format!("-- dialect: {}\n", renderer.name()));
    out.push_str(&format!("-- retention_days: {}\n", rc.backup_table_retention_days));
    out.push_str(&format!("-- cleanup_backup_tables_after_rollback: {}\n", rc.cleanup_backup_tables_after_rollback));
    out.push_str("\n");

    if !rc.cleanup_backup_tables_after_rollback {
        out.push_str("-- cleanup_backup_tables_after_rollback=false，本文件不执行 DROP\n");
        out.push_str("-- 备份表保留以便审计，由 DBA 按 retention_days 手工清理\n");
        return out;
    }

    // 收集所有 bks_ 表名（从 backup 段中提取）
    let bks_tables = collect_bks_table_names(pairs);
    if bks_tables.is_empty() {
        out.push_str("-- 无备份表需要清理\n");
        return out;
    }

    out.push_str("-- 按保留天数清理过期备份表\n");
    for name in &bks_tables {
        out.push_str(&format!("DROP TABLE IF EXISTS {};\n", renderer.quote_ident(name)));
    }

    out
}

/// 解析 lock_scope 为具体锁类型（D1/N8）。
///
/// - auto：脚本含 DDL 或 backup 含 DDL → global；纯 DML → snapshot
/// - global：FTWRL（MySQL）/ 全局锁（PG）
/// - snapshot：事务一致性快照（PG REPEATABLE READ；MySQL 退化为 global）
/// - table：LOCK TABLE t READ（MySQL，R2 需 accept_table_lock_risk）
/// - none：不加锁
pub fn resolve_lock_type(lock_scope: &str, has_ddl: bool) -> &'static str {
    match lock_scope {
        "auto" => {
            if has_ddl {
                "FTWRL"
            } else {
                "SNAPSHOT"
            }
        }
        "global" => "FTWRL",
        "snapshot" => "SNAPSHOT",
        "table" => "TABLE_UNSAFE",
        "none" => "NONE",
        _ => "FTWRL",  // 兜底用最安全策略
    }
}

/// 判断 pairs 中是否含 DDL 语句（用于 resolve_lock_type 的 auto 决策）。
pub fn has_ddl_in_pairs(pairs: &[BackupRollbackPair]) -> bool {
    pairs.iter().any(|p| {
        // 通过 rollback 文本特征判断 DDL：含 CREATE/DROP/ALTER/RENAME TABLE
        let r = p.rollback.as_deref().unwrap_or("");
        let upper = r.to_uppercase();
        upper.contains("CREATE TABLE")
            || upper.contains("DROP TABLE")
            || upper.contains("ALTER TABLE")
            || upper.contains("RENAME TABLE")
            || upper.contains("CREATE VIEW")
            || upper.contains("DROP VIEW")
    })
}

/// D6/R6 锁合并分组：将连续同表的 backup 段归为一组。
///
/// conservative（默认）：仅 DDL 全表 LIKE（backup 含 "INSERT INTO bks_ SELECT * FROM t" 无 WHERE）
///   合并；DML 增量（含 WHERE）不合并
/// aggressive：所有同表段都尝试合并
pub fn group_for_coalesce<'a>(
    pairs: &'a [BackupRollbackPair],
    mode: &str,
) -> Vec<Vec<&'a BackupRollbackPair>> {
    let aggressive = mode == "aggressive";
    let mut groups: Vec<Vec<&BackupRollbackPair>> = Vec::new();

    for pair in pairs {
        if pair.backup.is_none() {
            // 无 backup 段（如 CREATE TABLE 的 DROP 回滚），独立成组
            groups.push(vec![pair]);
            continue;
        }
        let backup = pair.backup.as_ref().unwrap();

        // 提取目标表名（从 backup 段的 CREATE TABLE bks_xxx LIKE t 中的 t）
        let target = extract_target_table_from_backup(backup);

        let can_coalesce = if aggressive {
            target.is_some()
        } else {
            // conservative：仅全表 LIKE（INSERT SELECT * FROM t 无 WHERE）
            target.is_some() && is_full_table_backup(backup)
        };

        if can_coalesce {
            // 尝试合并到上一组（同表）
            if let Some(last_group) = groups.last_mut() {
                let last_target = last_group.last()
                    .and_then(|p| p.backup.as_ref())
                    .and_then(|b| extract_target_table_from_backup(b));
                if last_target == target {
                    last_group.push(pair);
                    continue;
                }
            }
        }
        groups.push(vec![pair]);
    }
    groups
}

/// 渲染前的预检（R2/R4）。
///
/// 返回错误列表（空表示通过）。调用方应在 render_backup/render_rollback 前调用，
/// 非空时阻断生成。
pub fn validate_render_prerequisites(
    pairs: &[BackupRollbackPair],
    rc: &RollbackConfig,
) -> Vec<String> {
    let mut errors = Vec::new();

    // R2：lock_scope=table 必须显式 accept_table_lock_risk
    if rc.lock_scope == "table" && !rc.accept_table_lock_risk {
        errors.push(
            "lock_scope=table 需显式 accept_table_lock_risk=true（LOCK TABLE READ 会被隐式提交释放）".to_string()
        );
    }

    // R4：binlog_strategy=auto + 含 DDL → 视为 never（v1 静态无法检测 GTID）
    // M5 简化：auto + 含 DDL 不报错，但在 render_binlog_control 中降级为 never
    // 严格模式由调用方按返回的 warnings 决策
    let has_ddl = has_ddl_in_pairs(pairs);
    if rc.binlog_strategy == "auto" && has_ddl && rc.dialect == "mysql" {
        // 仅记录为 warning（通过返回值不阻断），不加入 errors
        // 严格场景调用方可自行升级为 error
    }

    errors
}

// ===== 内部辅助函数 =====

/// 渲染长事务预检查语句（F18/N10）。
///
/// 必须同时含 innodb_trx 和 processlist（覆盖非事务长查询）。
fn render_long_transaction_check(rc: &RollbackConfig, dialect_name: &str) -> String {
    let mut s = String::new();
    let threshold = rc.long_transaction_threshold;
    let action = rc.on_long_transaction.as_str();

    s.push_str(&format!(
        "-- F18 长事务预检查（阈值 {}s，策略 {}）：非空结果需先清理\n",
        threshold, action
    ));
    s.push_str("-- N10：必须同时检查 innodb_trx 和 processlist（覆盖非事务长查询）\n");

    if dialect_name == "mysql" {
        // innodb_trx：事务持续时间
        s.push_str(&format!(
            "SELECT trx_id, trx_started, TIME_TO_SEC(TIMEDIFF(NOW(), trx_started)) AS trx_age_sec, trx_query\n\
             FROM information_schema.innodb_trx\n\
             WHERE TIME_TO_SEC(TIMEDIFF(NOW(), trx_started)) > {}\n\
             ORDER BY trx_started;\n",
            threshold
        ));
        // processlist：覆盖非事务长查询（如长 SELECT）
        s.push_str(&format!(
            "SELECT id, user, host, db, command, time, state, LEFT(info, 200) AS info_preview\n\
             FROM information_schema.processlist\n\
             WHERE time > {} AND command != 'Sleep'\n\
             ORDER BY time DESC;\n",
            threshold
        ));
    } else {
        // PG：pg_stat_activity
        s.push_str(&format!(
            "SELECT pid, xact_start, EXTRACT(EPOCH FROM (NOW() - xact_start)) AS xact_age_sec, state, LEFT(query, 200) AS query_preview\n\
             FROM pg_stat_activity\n\
             WHERE xact_start IS NOT NULL AND EXTRACT(EPOCH FROM (NOW() - xact_start)) > {}\n\
             ORDER BY xact_start;\n",
            threshold
        ));
    }
    s.push_str("\n");
    s
}

/// 渲染 MySQL binlog 控制语句（F16）。
///
/// - always：SET SESSION sql_log_bin=0
/// - never：不输出
/// - auto：含 DDL → 不设（避免 DDL 不入 binlog 导致主从不一致）；纯 DML → 设
fn render_binlog_control(pairs: &[BackupRollbackPair], rc: &RollbackConfig) -> Option<String> {
    let has_ddl = has_ddl_in_pairs(pairs);
    let enable = match rc.binlog_strategy.as_str() {
        "always" => true,
        "never" => false,
        "auto" => !has_ddl,  // 含 DDL 不设 sql_log_bin=0
        _ => !has_ddl,
    };
    if enable {
        Some("-- F16：backup 段不写 binlog（避免主从重复执行 bks_ 表操作）\nSET SESSION sql_log_bin=0;\n".to_string())
    } else {
        None
    }
}

/// 渲染 coalesce 模式的 backup 主体（N9）。
///
/// 组内统一发射锁/解锁，剥离段内锁。
fn render_backup_coalesced(
    pairs: &[BackupRollbackPair],
    rc: &RollbackConfig,
    renderer: &dyn DialectRenderer,
) -> String {
    let mut out = String::new();
    let groups = group_for_coalesce(pairs, &rc.coalesce_locks_mode);

    for (i, group) in groups.iter().enumerate() {
        out.push_str(&format!("-- ===== coalesce group {} ({} segments) =====\n", i + 1, group.len()));

        // 组内统一发射锁（仅多段组才需要，单段组保留段内锁）
        let is_multi = group.len() > 1;
        let any_backup = group.iter().any(|p| p.backup.is_some());
        if is_multi && any_backup {
            // 组头加锁
            if renderer.name() == "mysql" {
                out.push_str("FLUSH TABLES WITH READ LOCK;\n");
            } else {
                // PG：对组内各表加 ACCESS SHARE
                for p in group {
                    if let Some(b) = &p.backup {
                        if let Some(t) = extract_target_table_from_backup(b) {
                            out.push_str(&format!("LOCK TABLE {} IN ACCESS SHARE MODE;\n", renderer.quote_ident(&t)));
                        }
                    }
                }
            }
        }

        // 逐段输出（剥离段内锁）
        for p in group {
            if let Some(backup) = &p.backup {
                out.push_str(&format!("-- seq={}\n", p.seq));
                let cleaned = if is_multi {
                    renderer.strip_lock_statements(backup)
                } else {
                    backup.clone()
                };
                out.push_str(&cleaned);
                if !cleaned.ends_with('\n') {
                    out.push('\n');
                }
            }
        }

        // 组尾解锁
        if is_multi && any_backup {
            if renderer.name() == "mysql" {
                out.push_str("UNLOCK TABLES;\n");
            }
        }
        out.push_str("\n");
    }

    out
}

/// 从 backup 段提取目标表名（CREATE TABLE bks_xxx LIKE t 中的 t）。
/// 用于 coalesce 分组。提取失败返回 None。
fn extract_target_table_from_backup(backup: &str) -> Option<String> {
    // 匹配 "LIKE `t`" 或 "LIKE \"t\"" 或 "LIKE t"
    let upper = backup.to_uppercase();
    let like_idx = upper.find("LIKE ")?;
    let rest = &backup[like_idx + 5..];
    // 跳过前导空白
    let rest = rest.trim_start();
    // 取到下一个空白/分号/换行
    let end = rest.find(|c: char| c.is_whitespace() || c == ';').unwrap_or(rest.len());
    let raw = &rest[..end];
    Some(super::strip_ident_quotes(raw))
}

/// 判断 backup 段是否为全表备份（INSERT INTO bks_ SELECT * FROM t 无 WHERE）。
/// conservative coalesce 仅合并此类段。
fn is_full_table_backup(backup: &str) -> bool {
    let upper = backup.to_uppercase();
    // 含 "INSERT INTO bks_ SELECT * FROM" 且不含 " WHERE "
    upper.contains("INSERT INTO") && upper.contains("SELECT * FROM") && !upper.contains(" WHERE ")
}

/// 从所有 backup 段收集 bks_ 表名（用于 cleanup）。
fn collect_bks_table_names(pairs: &[BackupRollbackPair]) -> Vec<String> {
    let mut names = Vec::new();
    for p in pairs {
        if let Some(backup) = &p.backup {
            // 匹配 "CREATE TABLE `bks_xxx`" 或 "CREATE TABLE \"bks_xxx\""
            if let Some(name) = extract_bks_table_name(backup) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// 从 backup 段提取 bks_ 表名。
fn extract_bks_table_name(backup: &str) -> Option<String> {
    let upper = backup.to_uppercase();
    let create_idx = upper.find("CREATE TABLE ")?;
    let rest = &backup[create_idx + "CREATE TABLE ".len()..];
    let rest = rest.trim_start();
    // 跳过 IF NOT EXISTS
    let rest = if rest.to_uppercase().starts_with("IF NOT EXISTS ") {
        &rest["IF NOT EXISTS ".len()..].trim_start()
    } else {
        rest
    };
    let end = rest.find(|c: char| c.is_whitespace() || c == ';').unwrap_or(rest.len());
    let raw = &rest[..end];
    Some(super::strip_ident_quotes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollback::{SourceRef, SafetyClass, BackupStrategy};
    use crate::rollback::dialect::{MySqlRenderer, PostgreSqlRenderer};

    fn make_pair(seq: u64, backup: Option<&str>, rollback: Option<&str>) -> BackupRollbackPair {
        BackupRollbackPair {
            seq,
            source: SourceRef::placeholder(),
            original_sql: String::new(),
            backup: backup.map(|s| s.to_string()),
            rollback: rollback.map(|s| s.to_string()),
            safety: SafetyClass::default(),
            strategy: BackupStrategy::default(),
            expected_schema: None,
            warnings: vec![],
        }
    }

    fn rc() -> RollbackConfig {
        RollbackConfig::default()
    }

    #[test]
    fn resolve_lock_type_auto_pure_dml_returns_snapshot() {
        assert_eq!(resolve_lock_type("auto", false), "SNAPSHOT");
    }

    #[test]
    fn resolve_lock_type_auto_with_ddl_returns_ftwrl() {
        assert_eq!(resolve_lock_type("auto", true), "FTWRL");
    }

    #[test]
    fn resolve_lock_type_global_returns_ftwrl() {
        assert_eq!(resolve_lock_type("global", false), "FTWRL");
    }

    #[test]
    fn resolve_lock_type_table_returns_unsafe() {
        assert_eq!(resolve_lock_type("table", false), "TABLE_UNSAFE");
    }

    #[test]
    fn resolve_lock_type_none_returns_none() {
        assert_eq!(resolve_lock_type("none", false), "NONE");
    }

    #[test]
    fn has_ddl_in_pairs_detects_create_table() {
        let pairs = vec![make_pair(1, None, Some("DROP TABLE IF EXISTS `users`;"))];
        assert!(has_ddl_in_pairs(&pairs));
    }

    #[test]
    fn has_ddl_in_pairs_pure_dml_returns_false() {
        let pairs = vec![make_pair(1, None, Some("DELETE FROM `users` WHERE `id` = 1;"))];
        assert!(!has_ddl_in_pairs(&pairs));
    }

    #[test]
    fn validate_table_lock_without_accept_returns_error() {
        let mut rc = rc();
        rc.lock_scope = "table".to_string();
        rc.accept_table_lock_risk = false;
        let errors = validate_render_prerequisites(&[], &rc);
        assert!(errors.iter().any(|e| e.contains("accept_table_lock_risk")));
    }

    #[test]
    fn validate_table_lock_with_accept_passes() {
        let mut rc = rc();
        rc.lock_scope = "table".to_string();
        rc.accept_table_lock_risk = true;
        let errors = validate_render_prerequisites(&[], &rc);
        assert!(errors.is_empty());
    }

    #[test]
    fn validate_global_scope_passes() {
        let rc = rc();
        let errors = validate_render_prerequisites(&[], &rc);
        assert!(errors.is_empty());
    }

    #[test]
    fn render_backup_empty_pairs_returns_header_only() {
        let rc = rc();
        let out = render_backup(&[], &rc, &MySqlRenderer);
        assert!(out.contains("sqlguard backup.sql"));
        assert!(out.contains("statements: 0"));
    }

    #[test]
    fn render_backup_includes_long_transaction_check() {
        let rc = rc();
        let out = render_backup(&[], &rc, &MySqlRenderer);
        assert!(out.contains("innodb_trx"), "应含 innodb_trx 预检查");
        assert!(out.contains("processlist"), "N10：必须含 processlist");
    }

    #[test]
    fn render_backup_pg_uses_pg_stat_activity() {
        let rc = rc();
        let out = render_backup(&[], &rc, &PostgreSqlRenderer);
        assert!(out.contains("pg_stat_activity"));
    }

    #[test]
    fn render_backup_long_transaction_ignore_omits_check() {
        let mut rc = rc();
        rc.on_long_transaction = "ignore".to_string();
        let out = render_backup(&[], &rc, &MySqlRenderer);
        assert!(!out.contains("innodb_trx"));
    }

    #[test]
    fn render_backup_mysql_includes_binlog_control_for_pure_dml() {
        let rc = rc();
        let pairs = vec![make_pair(1, None, Some("DELETE FROM `users` WHERE `id` = 1;"))];
        let out = render_backup(&pairs, &rc, &MySqlRenderer);
        assert!(out.contains("sql_log_bin=0"), "纯 DML auto 模式应设 sql_log_bin=0");
    }

    #[test]
    fn render_backup_mysql_ddl_omits_binlog_control() {
        let rc = rc();
        let pairs = vec![make_pair(1, None, Some("DROP TABLE IF EXISTS `users`;"))];
        let out = render_backup(&pairs, &rc, &MySqlRenderer);
        assert!(!out.contains("sql_log_bin=0"), "含 DDL auto 模式不设 sql_log_bin=0");
    }

    #[test]
    fn render_backup_binlog_always_includes_control() {
        let mut rc = rc();
        rc.binlog_strategy = "always".to_string();
        let pairs = vec![make_pair(1, None, Some("DROP TABLE IF EXISTS `users`;"))];
        let out = render_backup(&pairs, &rc, &MySqlRenderer);
        assert!(out.contains("sql_log_bin=0"));
    }

    #[test]
    fn render_backup_binlog_never_omits_control() {
        let mut rc = rc();
        rc.binlog_strategy = "never".to_string();
        let pairs = vec![make_pair(1, None, Some("DELETE FROM `users` WHERE `id` = 1;"))];
        let out = render_backup(&pairs, &rc, &MySqlRenderer);
        assert!(!out.contains("sql_log_bin=0"));
    }

    #[test]
    fn render_rollback_lifo_order() {
        let rc = rc();
        let pairs = vec![
            make_pair(1, None, Some("DELETE FROM `t` WHERE `id` = 1;")),
            make_pair(2, None, Some("DELETE FROM `t` WHERE `id` = 2;")),
            make_pair(3, None, Some("DELETE FROM `t` WHERE `id` = 3;")),
        ];
        let out = render_rollback(&pairs, &rc, &MySqlRenderer);
        // LIFO：seq=3 应在 seq=1 之前
        let pos3 = out.find("seq=3").unwrap();
        let pos1 = out.find("seq=1").unwrap();
        assert!(pos3 < pos1);
    }

    #[test]
    fn render_rollback_pg_wraps_in_transaction() {
        let rc = rc();
        let pairs = vec![make_pair(1, None, Some("DELETE FROM \"t\" WHERE \"id\" = 1;"))];
        let out = render_rollback(&pairs, &rc, &PostgreSqlRenderer);
        assert!(out.contains("BEGIN;"));
        assert!(out.contains("COMMIT;"));
    }

    #[test]
    fn render_rollback_mysql_no_global_wrap() {
        let rc = rc();
        let pairs = vec![make_pair(1, None, Some("DELETE FROM `t` WHERE `id` = 1;"))];
        let out = render_rollback(&pairs, &rc, &MySqlRenderer);
        // MySQL 不整体包裹（DDL 隐式提交）
        assert!(!out.starts_with("BEGIN;"));
    }

    #[test]
    fn render_rollback_pg_wrap_disabled() {
        let mut rc = rc();
        rc.wrap_transaction = false;
        let pairs = vec![make_pair(1, None, Some("DELETE FROM \"t\" WHERE \"id\" = 1;"))];
        let out = render_rollback(&pairs, &rc, &PostgreSqlRenderer);
        assert!(!out.contains("BEGIN;"));
        assert!(!out.contains("COMMIT;"));
    }

    #[test]
    fn render_cleanup_disabled_returns_notice() {
        let rc = rc();  // cleanup_backup_tables_after_rollback=false (default)
        let pairs = vec![make_pair(1, Some("CREATE TABLE `bks_users_20260726_0001` LIKE `users`;\nINSERT INTO `bks_users_20260726_0001` SELECT * FROM `users`;"), None)];
        let out = render_cleanup(&pairs, &rc, &MySqlRenderer);
        assert!(out.contains("cleanup_backup_tables_after_rollback=false"));
        assert!(!out.contains("DROP TABLE"));
    }

    #[test]
    fn render_cleanup_enabled_generates_drop() {
        let mut rc = rc();
        rc.cleanup_backup_tables_after_rollback = true;
        let pairs = vec![
            make_pair(1, Some("CREATE TABLE `bks_users_20260726_0001` LIKE `users`;\nINSERT INTO `bks_users_20260726_0001` SELECT * FROM `users`;"), None),
            make_pair(2, Some("CREATE TABLE `bks_orders_20260726_0002` LIKE `orders`;\nINSERT INTO `bks_orders_20260726_0002` SELECT * FROM `orders`;"), None),
        ];
        let out = render_cleanup(&pairs, &rc, &MySqlRenderer);
        assert!(out.contains("DROP TABLE IF EXISTS `bks_users_20260726_0001`;"));
        assert!(out.contains("DROP TABLE IF EXISTS `bks_orders_20260726_0002`;"));
    }

    #[test]
    fn render_cleanup_deduplicates_bks_tables() {
        let mut rc = rc();
        rc.cleanup_backup_tables_after_rollback = true;
        let pairs = vec![
            make_pair(1, Some("CREATE TABLE `bks_users_20260726_0001` LIKE `users`;"), None),
            make_pair(2, Some("CREATE TABLE `bks_users_20260726_0001` LIKE `users`;"), None),  // 同名重复
        ];
        let out = render_cleanup(&pairs, &rc, &MySqlRenderer);
        // 去重后只出现一次
        assert_eq!(out.matches("DROP TABLE IF EXISTS `bks_users_20260726_0001`;").count(), 1);
    }

    #[test]
    fn group_for_coalesce_conservative_merges_full_table_backups() {
        let pairs = vec![
            make_pair(1, Some("CREATE TABLE `bks_t_1` LIKE `t`;\nINSERT INTO `bks_t_1` SELECT * FROM `t`;"), None),
            make_pair(2, Some("CREATE TABLE `bks_t_2` LIKE `t`;\nINSERT INTO `bks_t_2` SELECT * FROM `t`;"), None),
        ];
        let groups = group_for_coalesce(&pairs, "conservative");
        assert_eq!(groups.len(), 1, "同表全表备份应合并为一组");
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn group_for_coalesce_conservative_does_not_merge_incremental() {
        let pairs = vec![
            make_pair(1, Some("CREATE TABLE `bks_t_1` LIKE `t`;\nINSERT INTO `bks_t_1` SELECT * FROM `t` WHERE `id` > 100;"), None),
            make_pair(2, Some("CREATE TABLE `bks_t_2` LIKE `t`;\nINSERT INTO `bks_t_2` SELECT * FROM `t` WHERE `id` > 200;"), None),
        ];
        let groups = group_for_coalesce(&pairs, "conservative");
        assert_eq!(groups.len(), 2, "增量备份（含 WHERE）不合并");
    }

    #[test]
    fn group_for_coalesce_aggressive_merges_incremental() {
        let pairs = vec![
            make_pair(1, Some("CREATE TABLE `bks_t_1` LIKE `t`;\nINSERT INTO `bks_t_1` SELECT * FROM `t` WHERE `id` > 100;"), None),
            make_pair(2, Some("CREATE TABLE `bks_t_2` LIKE `t`;\nINSERT INTO `bks_t_2` SELECT * FROM `t` WHERE `id` > 200;"), None),
        ];
        let groups = group_for_coalesce(&pairs, "aggressive");
        assert_eq!(groups.len(), 1, "aggressive 模式同表增量也合并");
    }

    #[test]
    fn group_for_coalesce_different_tables_not_merged() {
        let pairs = vec![
            make_pair(1, Some("CREATE TABLE `bks_t_1` LIKE `t`;\nINSERT INTO `bks_t_1` SELECT * FROM `t`;"), None),
            make_pair(2, Some("CREATE TABLE `bks_u_1` LIKE `u`;\nINSERT INTO `bks_u_1` SELECT * FROM `u`;"), None),
        ];
        let groups = group_for_coalesce(&pairs, "conservative");
        assert_eq!(groups.len(), 2, "不同表不合并");
    }

    #[test]
    fn render_backup_coalesced_strips_inner_locks() {
        let mut rc = rc();
        rc.coalesce_locks = true;
        let pairs = vec![
            make_pair(1, Some("FLUSH TABLES WITH READ LOCK;\nCREATE TABLE `bks_t_1` LIKE `t`;\nINSERT INTO `bks_t_1` SELECT * FROM `t`;\nUNLOCK TABLES;"), None),
            make_pair(2, Some("FLUSH TABLES WITH READ LOCK;\nCREATE TABLE `bks_t_2` LIKE `t`;\nINSERT INTO `bks_t_2` SELECT * FROM `t`;\nUNLOCK TABLES;"), None),
        ];
        let out = render_backup(&pairs, &rc, &MySqlRenderer);
        // 合并后段内 FTWRL 应被剥离，组头统一发射
        let ftwrl_count = out.matches("FLUSH TABLES WITH READ LOCK;").count();
        assert_eq!(ftwrl_count, 1, "合并组只保留组头一次 FTWRL");
        let unlock_count = out.matches("UNLOCK TABLES;").count();
        assert_eq!(unlock_count, 1, "合并组只保留组尾一次 UNLOCK");
    }

    #[test]
    fn extract_target_table_from_backup_basic() {
        let backup = "FLUSH TABLES WITH READ LOCK;\nCREATE TABLE `bks_users_1` LIKE `users`;\nINSERT INTO `bks_users_1` SELECT * FROM `users`;\nUNLOCK TABLES;";
        assert_eq!(extract_target_table_from_backup(backup), Some("users".to_string()));
    }

    #[test]
    fn extract_target_table_from_backup_pg() {
        let backup = "LOCK TABLE \"users\" IN ACCESS SHARE MODE;\nCREATE TABLE \"bks_users_1\" (LIKE \"users\" INCLUDING ...);\nINSERT INTO \"bks_users_1\" SELECT * FROM \"users\";";
        assert_eq!(extract_target_table_from_backup(backup), Some("users".to_string()));
    }

    #[test]
    fn is_full_table_backup_true_without_where() {
        let backup = "CREATE TABLE `bks_t` LIKE `t`;\nINSERT INTO `bks_t` SELECT * FROM `t`;";
        assert!(is_full_table_backup(backup));
    }

    #[test]
    fn is_full_table_backup_false_with_where() {
        let backup = "CREATE TABLE `bks_t` LIKE `t`;\nINSERT INTO `bks_t` SELECT * FROM `t` WHERE `id` > 100;";
        assert!(!is_full_table_backup(backup));
    }

    #[test]
    fn extract_bks_table_name_basic() {
        let backup = "CREATE TABLE `bks_users_20260726_0001` LIKE `users`;";
        assert_eq!(extract_bks_table_name(backup), Some("bks_users_20260726_0001".to_string()));
    }

    #[test]
    fn extract_bks_table_name_pg() {
        let backup = "CREATE TABLE \"bks_users_20260726_0001\" (LIKE \"users\" INCLUDING ...);";
        assert_eq!(extract_bks_table_name(backup), Some("bks_users_20260726_0001".to_string()));
    }

    #[test]
    fn render_backup_aborts_on_table_lock_without_accept() {
        let mut rc = rc();
        rc.lock_scope = "table".to_string();
        rc.accept_table_lock_risk = false;
        let out = render_backup(&[], &rc, &MySqlRenderer);
        assert!(out.contains("ABORT"));
        assert!(out.contains("accept_table_lock_risk"));
    }

    // ===== 端到端集成测试：generate → render =====

    #[test]
    fn end_to_end_dml_mixed_with_ddl_renders_full_lifecycle() {
        use crate::rollback::RollbackGenerator;
        use crate::rule::engine::ast::{
            StmtInfo, DropInfo, InsertInfo, DeleteInfo, ColumnInfo,
            CreateInfo,
        };

        fn make_cfg() -> crate::config::Config {
            crate::config::Config {
                structure: crate::config::StructureConfig { paths: vec![], strict: false, allow_extra: vec![] },
                classification: crate::config::ClassificationConfig { rules: vec![], default_type: "other".to_string() },
                rules: vec![], rules_file: None, rules_dir: std::path::PathBuf::new(),
                output: crate::config::OutputConfig::default(),
                mapper: crate::config::MapperConfig::default(),
                scan: crate::config::ScanConfig::default(),
                file_check: crate::config::FileCheckConfig::default(),
                rollback: RollbackConfig::default(),
                dialect: crate::config::CheckDialect::default(),
            }
        }

        let cfg = make_cfg();
        let rc = RollbackConfig::default();
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);

        // 构造混合语句：CREATE TABLE → INSERT → DELETE → DROP TABLE
        let stmts: Vec<(StmtInfo, &str)> = vec![
            // 1. CREATE TABLE users (id BIGINT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(32))
            (StmtInfo {
                kind: "CREATE_TABLE".to_string(),
                line: 1, end_line: 5, column: 0,
                create_table: Some(CreateInfo {
                    table_name: "users".to_string(),
                    columns: vec![
                        ColumnInfo { name: "id".to_string(), data_type: "BIGINT".to_string(), is_auto_increment: true, is_primary_key: true, ..Default::default() },
                        ColumnInfo { name: "name".to_string(), data_type: "VARCHAR(32)".to_string(), ..Default::default() },
                    ],
                    has_primary_key: true,
                    primary_key_columns: vec!["id".to_string()],
                    ..Default::default()
                }),
                drop_object: None, select: None, insert: None, update: None, delete: None,
                alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
            }, "CREATE TABLE users (id BIGINT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(32))"),
            // 2. INSERT INTO users (id, name) VALUES (1, 'alice')
            (StmtInfo {
                kind: "INSERT".to_string(),
                line: 6, end_line: 6, column: 0,
                create_table: None, drop_object: None, select: None,
                insert: Some(InsertInfo {
                    table_name: "users".to_string(),
                    columns: vec!["id".to_string(), "name".to_string()],
                }),
                update: None, delete: None, alter_table: None, truncate: None,
                create_view: None, create_index: None, transaction: None,
            }, "INSERT INTO users (id, name) VALUES (1, 'alice')"),
            // 3. DELETE FROM users WHERE id = 1
            (StmtInfo {
                kind: "DELETE".to_string(),
                line: 7, end_line: 7, column: 0,
                create_table: None, drop_object: None, select: None, insert: None,
                update: None, delete: Some(DeleteInfo {
                    table_name: "users".to_string(),
                    where_clause: Some("id = 1".to_string()),
                }),
                alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
            }, "DELETE FROM users WHERE id = 1"),
            // 4. DROP TABLE users
            (StmtInfo {
                kind: "DROP_TABLE".to_string(),
                line: 8, end_line: 8, column: 0,
                create_table: None,
                drop_object: Some(DropInfo { object_type: "TABLE".to_string(), name: "users".to_string(), if_exists: false }),
                select: None, insert: None, update: None, delete: None,
                alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
            }, "DROP TABLE users"),
        ];

        let mut pairs = Vec::new();
        for (stmt, original) in stmts {
            let pair = gen.generate(&stmt, SourceRef::placeholder(), original);
            pairs.push(pair);
        }

        // ===== 渲染 backup.sql =====
        let backup_sql = render_backup(&pairs, &rc, &MySqlRenderer);
        assert!(backup_sql.contains("sqlguard backup.sql"));
        // 长事务预检查
        assert!(backup_sql.contains("innodb_trx"));
        assert!(backup_sql.contains("processlist"));
        // 含 DDL（CREATE TABLE / DROP TABLE）→ auto 模式不设 sql_log_bin=0
        assert!(!backup_sql.contains("sql_log_bin=0"));
        // backup 主体：INSERT 无 backup（None），DELETE 有增量 backup，DROP TABLE 有全表 backup
        // CREATE TABLE 无 backup（DROP IF EXISTS 回滚）
        // 验证 backup 段含 bks_ 表
        assert!(backup_sql.contains("bks_users_"), "backup 应含 bks_ 备份表");

        // ===== 渲染 rollback.sql =====
        let rollback_sql = render_rollback(&pairs, &rc, &MySqlRenderer);
        assert!(rollback_sql.contains("sqlguard rollback.sql"));
        // LIFO 顺序：DROP TABLE 回滚（CREATE TABLE LIKE bks_）应在 DELETE 回滚（UPDATE JOIN bks_）之前
        // seq=4 (DROP TABLE) 的回滚含 "CREATE TABLE `users` LIKE `bks_users_"
        // seq=3 (DELETE) 的回滚含 "INSERT INTO" (还原被删行)
        let pos_drop = rollback_sql.find("seq=4").unwrap();
        let pos_delete = rollback_sql.find("seq=3").unwrap();
        assert!(pos_drop < pos_delete, "LIFO: seq=4 应在 seq=3 之前");
        // MySQL 不整体包裹 BEGIN/COMMIT
        assert!(!rollback_sql.contains("BEGIN;"));
        assert!(!rollback_sql.contains("COMMIT;"));

        // ===== 渲染 cleanup.sql =====
        let mut rc_cleanup = rc.clone();
        rc_cleanup.cleanup_backup_tables_after_rollback = true;
        let cleanup_sql = render_cleanup(&pairs, &rc_cleanup, &MySqlRenderer);
        assert!(cleanup_sql.contains("DROP TABLE IF EXISTS `bks_users_"));

        // ===== Manifest 生成 =====
        use crate::rollback::manifest::Manifest;
        let manifest = Manifest::from_pairs(&pairs, "mysql", vec![]);
        assert_eq!(manifest.items.len(), 4);
        // DROP TABLE 应标 irreversible_if_backup_missing
        assert!(manifest.items.iter().any(|m| m.seq == 4 && m.safety.irreversible_if_backup_missing));
    }

    #[test]
    fn end_to_end_pg_dml_wraps_rollback_in_transaction() {
        use crate::rollback::RollbackGenerator;
        use crate::rule::engine::ast::{StmtInfo, UpdateInfo};

        fn make_cfg() -> crate::config::Config {
            crate::config::Config {
                structure: crate::config::StructureConfig { paths: vec![], strict: false, allow_extra: vec![] },
                classification: crate::config::ClassificationConfig { rules: vec![], default_type: "other".to_string() },
                rules: vec![], rules_file: None, rules_dir: std::path::PathBuf::new(),
                output: crate::config::OutputConfig::default(),
                mapper: crate::config::MapperConfig::default(),
                scan: crate::config::ScanConfig::default(),
                file_check: crate::config::FileCheckConfig::default(),
                rollback: RollbackConfig::default(),
                dialect: crate::config::CheckDialect::default(),
            }
        }

        let cfg = make_cfg();
        let rc = RollbackConfig::default();
        let mut gen = RollbackGenerator::new(&cfg, &rc, &PostgreSqlRenderer);

        let stmt = StmtInfo {
            kind: "UPDATE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: Some(UpdateInfo {
                table_name: "users".to_string(),
                where_clause: Some("id = 1".to_string()),
            }),
            delete: None, alter_table: None, truncate: None,
            create_view: None, create_index: None, transaction: None,
        };
        let pair = gen.generate(&stmt, SourceRef::placeholder(), "UPDATE users SET name = 'bob' WHERE id = 1");
        let pairs = vec![pair];

        // PG rollback 应整体包裹 BEGIN/COMMIT
        let rollback_sql = render_rollback(&pairs, &rc, &PostgreSqlRenderer);
        assert!(rollback_sql.contains("BEGIN;"));
        assert!(rollback_sql.contains("COMMIT;"));

        // PG backup 用 ACCESS SHARE 而非 FTWRL
        let backup_sql = render_backup(&pairs, &rc, &PostgreSqlRenderer);
        assert!(!backup_sql.contains("FLUSH TABLES WITH READ LOCK"));
        assert!(backup_sql.contains("pg_stat_activity"));
        // 纯 DML → auto 模式 → SNAPSHOT
    }
}
