//! SQL 文本渲染（事务包裹、锁合并、预检查）。
//!
//! 对应设计文档 §4.6 / §4.7 / F14 / F18。
//!
//! ★ M5 阶段完整实现，本文件当前为占位骨架。
//!
//! 计划实现的函数：
//! - `render_backup(pairs, rc, renderer) -> String`：渲染 backup.sql
//!   - 头部：长事务预检查（innodb_trx + processlist，F18）
//!   - 头部：MySQL 方言按 binlog_strategy 决定是否加 `SET SESSION sql_log_bin=0`（F16）
//!   - 主体：按 SEQ 配对，coalesce_locks 合并连续同表段（D6/R6）
//! - `render_rollback(pairs, rc, renderer) -> String`：渲染 rollback.sql
//!   - LIFO 排序（与 backup 相反）
//!   - PG 方言整体包裹 BEGIN/COMMIT；MySQL 仅 DML 段加 START TRANSACTION/COMMIT
//! - `render_cleanup(pairs, rc, renderer) -> String`：渲染 cleanup.sql
//!   - 按 backup_table_retention_days 生成 DROP TABLE bks_xxx 语句
//! - `resolve_lock_type(lock_scope, stmt_kind, has_ddl) -> &'static str`：
//!   auto → global/snapshot 决策（D1）
//! - `group_for_coalesce(pairs, mode) -> Vec<Vec<&BackupRollbackPair>>`：D6 锁合并分组
//!
//! 关键约束（M5 实现时必须落地）：
//! - **N11**：snapshot 模式 DDL 外提到事务前，manifest 标 `snapshot_window_unprotected: true`
//! - **N9**：coalesce_locks 启用时，组内统一发射锁/解锁，剥离段内锁
//! - **N10**：长事务预检查必须含 processlist（覆盖非事务长查询）
//! - **N8**：lock_type 由 resolve_lock_type 决定，不是方言名
//! - **R2**：lock_scope=table 模式要求 accept_table_lock_risk=true，否则报错退出
//! - **R4**：v1 静态 + binlog_strategy=auto + 含 DDL → 报错退出

use super::BackupRollbackPair;
use super::dialect::DialectRenderer;
use crate::config::RollbackConfig;

/// 渲染 backup.sql（M5 完整实现）。
pub fn render_backup(
    _pairs: &[BackupRollbackPair],
    _rc: &RollbackConfig,
    _renderer: &dyn DialectRenderer,
) -> String {
    String::new()
}

/// 渲染 rollback.sql（M5 完整实现）。
pub fn render_rollback(
    _pairs: &[BackupRollbackPair],
    _rc: &RollbackConfig,
    _renderer: &dyn DialectRenderer,
) -> String {
    String::new()
}

/// 渲染 cleanup.sql（M5 完整实现）。
pub fn render_cleanup(
    _pairs: &[BackupRollbackPair],
    _rc: &RollbackConfig,
    _renderer: &dyn DialectRenderer,
) -> String {
    String::new()
}
