//! 回滚脚本生成模块（gen-rollback 子命令）。
//!
//! 对应设计文档 `docs/backup-rollback-design.md` §4。
//! 与 `rule/` 平级，复用 `rule::engine::ast::StmtInfo`，但生成逻辑用 Rust 原生实现
//! （不引入 Rhai 引擎，保证性能与可测试性）。
//!
//! 模块划分：
//! - [`dialect`]：方言适配层（MySQL / PostgreSQL）
//! - [`naming`]：备份表命名（bks_xxx_YYYYMMDD_NNNN）
//! - [`generator`]：主调度，按 `StmtInfo.kind` 分发到 ddl / ddl_like / dml
//! - [`ddl`]：纯反向 DDL（CREATE TABLE/INDEX/VIEW、ALTER ADD/RENAME）
//! - [`ddl_like`]：CREATE TABLE LIKE 统一模式（DROP/ALTER DROP/MODIFY 等）
//! - [`dml`]：DML（INSERT/UPDATE/DELETE/TRUNCATE/REPLACE）
//! - [`pk`]：主键解析（配置 → StmtInfo → bks_ 表 JOIN 推断）
//! - [`render`]：SQL 文本渲染（事务包裹、锁合并、预检查）
//! - [`manifest`]：rollback-manifest.json 序列化

pub mod dialect;
pub mod naming;
pub mod generator;
pub mod ddl;
pub mod ddl_like;
pub mod dml;
pub mod pk;
pub mod render;
pub mod manifest;
pub mod util;

use crate::rule::engine::ast::StmtInfo;
use serde::Serialize;

/// ★ D3：精简重导出——仅保留 main.rs / 测试实际通过 `crate::rollback::X` 路径访问的类型。
/// `MySqlRenderer` / `PostgreSqlRenderer` / `AtomicStrategy` / `DialectRenderer` / `ManifestItem`
/// 均通过 `crate::rollback::dialect::X` / `crate::rollback::manifest::X` 直接路径访问，
/// 无需在此重导出。
pub use dialect::{Dialect, renderer_for};
pub use manifest::{Manifest, serialize_manifest};
pub use generator::RollbackGenerator;

/// ★ C2 架构修正：聚合所有"安全分类"标志，避免 flag 散装。
/// 生成器填充，渲染器/manifest 序列化统一读取，CI 按 class 决策退出码（见 §4.14 决策表）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SafetyClass {
    /// 主键缺失等场景下为 false（仅 DML 生效），CI 退出码 2
    pub reliable: bool,
    /// 部分回滚（外键/CHECK/触发器未保留 / RENAME 后 FK 拓扑可能错乱 / REPLACE 新行无法回滚）
    /// CI 退出码 2
    pub partial: bool,
    /// 不可逆变更（MERGE/UPSERT 等真正不支持的语句）。注：REPLACE 不归此类，用 `partial`
    pub irreversible: bool,
    /// AUTO_INCREMENT / SEQUENCE 当前值无法静态还原，需 DBA 手工修复
    pub counter_unrestored: bool,
    /// 备份段持锁（FTWRL / ACCESS SHARE），发布平台需提示锁时长
    pub requires_lock: bool,
    /// ★ N8 修正：锁类型："FTWRL" / "SNAPSHOT" / "TABLE_UNSAFE" / "NONE" / None
    /// （不是方言名；由 resolve_lock_type 按 lock_scope 决定）
    pub lock_type: Option<String>,
    /// ★ N12/R7：lock_wait_timeout 对 FTWRL 行为不完全一致，仅 best-effort 兜底
    pub lock_timeout_best_effort: bool,
    /// ★ N11/R1：snapshot 模式下 DDL 外提到事务前，建表到快照间无保护窗口期
    pub snapshot_window_unprotected: bool,
    /// 分区表（LIKE 生成非分区表，F15 检测）
    pub partitioned: bool,
    /// DROP TABLE 回滚特例：CREATE 失败则原表无法恢复，强依赖 bks_ 表存在
    pub irreversible_if_backup_missing: bool,
}

/// 备份策略元数据（与 SafetyClass 正交，记录"怎么备份的"而非"安不安全"）
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupStrategy {
    /// incremental / full（记录实际使用的模式，便于审计降级场景）
    pub backup_mode: String,
    /// 升级增量的来源："create_table_context" / None（仅 ALTER DROP/MODIFY 在升级时填）
    pub incremental_source: Option<String>,
    /// ADD COLUMN 约束检查："passed"（无约束，走 metadata-only）/ "fallback_to_full"（有约束，降级全表）/ None（非 ADD COLUMN）
    pub column_constraints_check: Option<String>,
}

/// 执行期 schema 漂移校验的期望值（F13），发布平台在执行期比对
#[derive(Debug, Clone, Serialize)]
pub struct ExpectedSchema {
    pub table_exists: bool,
    pub row_count: i64,
    pub columns: Vec<ExpectedColumn>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpectedColumn {
    pub name: String,
    pub data_type: String,
}

/// 单条变更语句的生成结果。
#[derive(Debug, Clone)]
pub struct BackupRollbackPair {
    /// 全局序号（1-indexed），用于 backup/rollback 配对
    pub seq: u64,
    /// 语句类型（StmtInfo.kind），如 "INSERT" / "UPDATE" / "CREATE_TABLE" / "ALTER_TABLE" / "DROP_TABLE"
    /// 用于 render 阶段判断 DDL/DML 归类，避免文本嗅探
    pub stmt_kind: String,
    /// 来源信息：文件路径 + 行号 + （mapper 时）statement_id
    pub source: SourceRef,
    /// 变更语句原文（去掉尾部分号）
    pub original_sql: String,
    /// 备份语句（变更前执行）。None 表示该语句无需备份。
    pub backup: Option<String>,
    /// 回滚语句（变更失败后执行）。None 表示无法生成（在 warnings 中说明）。
    pub rollback: Option<String>,
    /// ★ C2：聚合安全分类（取代散装 flag）
    pub safety: SafetyClass,
    /// 备份策略元数据
    pub strategy: BackupStrategy,
    /// 执行期 schema 漂移校验期望值（F13），None 表示该校验不适用
    pub expected_schema: Option<ExpectedSchema>,
    /// 生成过程中的提示，非致命
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRef {
    pub file: String,
    pub line: i64,
    pub end_line: i64,
    pub statement_id: Option<String>, // Mapper 模式
    pub variant_label: Option<String>, // Mapper 动态分支变体
}

impl SourceRef {
    /// 占位 SourceRef，由调用方回填实际值。
    /// ★ D2：仅在测试模块使用（generator/dml 测试构造 pair 时调用），
    /// 生产代码直接构造 SourceRef 字面量。保留用于测试便利。
    #[allow(dead_code)]
    pub fn placeholder() -> Self {
        SourceRef {
            file: String::new(),
            line: 0,
            end_line: 0,
            statement_id: None,
            variant_label: None,
        }
    }
}

/// 从 StmtInfo 提取目标表名（去掉反引号/双引号）。
/// 供 render.rs / ddl_like.rs / dml.rs 复用。
pub fn extract_target_table_from_stmt(stmt: &StmtInfo) -> String {
    if let Some(ci) = &stmt.create_table {
        return strip_ident_quotes(&ci.table_name);
    }
    if let Some(di) = &stmt.drop_object {
        return strip_ident_quotes(&di.name);
    }
    if let Some(ai) = &stmt.alter_table {
        return strip_ident_quotes(&ai.table_name);
    }
    if let Some(ii) = &stmt.insert {
        return strip_ident_quotes(&ii.table_name);
    }
    if let Some(ui) = &stmt.update {
        return strip_ident_quotes(&ui.table_name);
    }
    if let Some(di) = &stmt.delete {
        return strip_ident_quotes(&di.table_name);
    }
    if let Some(ti) = &stmt.truncate {
        return strip_ident_quotes(&ti.table_name);
    }
    if let Some(vi) = &stmt.create_view {
        return strip_ident_quotes(&vi.name);
    }
    if let Some(cix) = &stmt.create_index {
        return strip_ident_quotes(&cix.table_name);
    }
    String::new()
}

/// 去掉标识符外围的反引号（MySQL）或双引号（PG），保留内部转义。
pub fn strip_ident_quotes(name: &str) -> String {
    let s = name.trim();
    if s.len() >= 2 && s.starts_with('`') && s.ends_with('`') {
        s[1..s.len() - 1].replace("``", "`")
    } else if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].replace("\"\"", "\"")
    } else {
        s.to_string()
    }
}
