//! `rollback-manifest.json` 序列化。
//!
//! 对应设计文档 §4.3 `Manifest` / `ManifestItem`。
//! `ManifestItem` 通过 `#[serde(flatten)]` 把 `SafetyClass` + `BackupStrategy` 字段平铺到 item 下，
//! 兼容旧发布平台按字段名读取。

use serde::Serialize;
use super::{BackupRollbackPair, SafetyClass, BackupStrategy, SourceRef, ExpectedSchema};
use super::util::current_iso8601_utc;
use crate::config::RollbackConfig;

#[derive(Serialize)]
pub struct Manifest {
    pub version: u32,
    pub generator: String,
    pub generated_at: String,
    pub dialect: String,
    pub source_count: usize,
    pub backup_count: usize,
    pub rollback_count: usize,
    /// ★ 汇总计数（供 CI 快速判断，无需遍历 items）
    pub unreliable_count: usize,
    pub irreversible_count: usize,
    pub partial_count: usize,
    pub counter_unrestored_count: usize,
    pub requires_lock_count: usize,
    pub partitioned_count: usize,
    pub irreversible_if_backup_missing_count: usize,
    /// ★ P1-2：执行期策略（发布平台消费，原仅存在于 RollbackConfig 未透出）
    /// F13 schema 漂移校验策略：abort / warn / ignore
    pub assert_on_schema_mismatch: String,
    /// F15 分区表处理策略：abort / warn / fallback
    pub on_partitioned_table: String,
    pub items: Vec<ManifestItem>,
    pub warnings: Vec<String>,
}

/// ★ C1 修正：补齐文档通篇引用的 8 个字段，对齐发布平台契约。
#[derive(Serialize)]
pub struct ManifestItem {
    pub seq: u64,
    pub source: SourceRef,
    pub original_sql: String,
    pub backup: Option<String>,
    pub rollback: Option<String>,
    /// ★ 嵌入聚合结构（取代散装 flag），序列化后字段平铺到 item 下
    #[serde(flatten)]
    pub safety: SafetyClass,
    #[serde(flatten)]
    pub strategy: BackupStrategy,
    /// F13 执行期 schema 漂移校验期望值
    pub expected_schema: Option<ExpectedSchema>,
    pub warnings: Vec<String>,
}

impl Manifest {
    /// 从已生成的 pairs 与方言名称构造 manifest。
    /// ★ P1-2：新增 `rc` 参数以透出 `assert_on_schema_mismatch` / `on_partitioned_table` 执行期策略。
    pub fn from_pairs(pairs: &[BackupRollbackPair], dialect: &str, rc: &RollbackConfig, warnings: Vec<String>) -> Self {
        let mut unreliable = 0usize;
        let mut irreversible = 0usize;
        let mut partial = 0usize;
        let mut counter = 0usize;
        let mut requires_lock = 0usize;
        let mut partitioned = 0usize;
        let mut irreversible_if_missing = 0usize;
        let mut backup_count = 0usize;
        let mut rollback_count = 0usize;

        for p in pairs {
            if p.backup.is_some() {
                backup_count += 1;
            }
            if p.rollback.is_some() {
                rollback_count += 1;
            }
            if !p.safety.reliable {
                unreliable += 1;
            }
            if p.safety.irreversible {
                irreversible += 1;
            }
            if p.safety.partial {
                partial += 1;
            }
            if p.safety.counter_unrestored {
                counter += 1;
            }
            if p.safety.requires_lock {
                requires_lock += 1;
            }
            if p.safety.partitioned {
                partitioned += 1;
            }
            if p.safety.irreversible_if_backup_missing {
                irreversible_if_missing += 1;
            }
        }

        let items = pairs.iter().map(|p| ManifestItem {
            seq: p.seq,
            source: p.source.clone(),
            original_sql: p.original_sql.clone(),
            backup: p.backup.clone(),
            rollback: p.rollback.clone(),
            safety: p.safety.clone(),
            strategy: p.strategy.clone(),
            expected_schema: p.expected_schema.clone(),
            warnings: p.warnings.clone(),
        }).collect();

        Manifest {
            version: 1,
            generator: format!("sqlguard {}", env!("CARGO_PKG_VERSION")),
            generated_at: current_iso8601_utc(),
            dialect: dialect.to_string(),
            source_count: pairs.len(),
            backup_count,
            rollback_count,
            unreliable_count: unreliable,
            irreversible_count: irreversible,
            partial_count: partial,
            counter_unrestored_count: counter,
            requires_lock_count: requires_lock,
            partitioned_count: partitioned,
            irreversible_if_backup_missing_count: irreversible_if_missing,
            // ★ P1-2：透出执行期策略供发布平台读取
            assert_on_schema_mismatch: rc.assert_on_schema_mismatch.clone(),
            on_partitioned_table: rc.on_partitioned_table.clone(),
            items,
            warnings,
        }
    }

    /// 按 §4.14 决策表计算 CI 退出码。
    pub fn exit_code(&self, fail_on_warning: bool, allow_partial: bool) -> i32 {
        let has_error = self.irreversible_count > 0
            || self.unreliable_count > 0
            || (!allow_partial && self.partial_count > 0)
            || self.irreversible_if_backup_missing_count > 0;
        if has_error {
            return 2;
        }
        let has_warning = self.counter_unrestored_count > 0 || self.requires_lock_count > 0;
        if fail_on_warning && has_warning {
            return 2;
        }
        if has_warning {
            return 1;
        }
        0
    }
}

/// 序列化为 pretty JSON。
pub fn serialize_manifest(m: &Manifest) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollback::SourceRef;

    fn make_pair(seq: u64, reliable: bool, partial: bool, irreversible: bool, requires_lock: bool) -> BackupRollbackPair {
        BackupRollbackPair {
            seq,
            stmt_kind: String::new(),
            source: SourceRef::placeholder(),
            original_sql: format!("-- stmt {}", seq),
            backup: Some(format!("-- backup {}", seq)),
            rollback: Some(format!("-- rollback {}", seq)),
            safety: SafetyClass {
                reliable,
                partial,
                irreversible,
                counter_unrestored: false,
                requires_lock,
                lock_type: if requires_lock { Some("FTWRL".to_string()) } else { None },
                lock_timeout_best_effort: false,
                snapshot_window_unprotected: false,
                partitioned: false,
                irreversible_if_backup_missing: false,
            },
            strategy: BackupStrategy::default(),
            expected_schema: None,
            warnings: vec![],
        }
    }

    fn default_rc() -> RollbackConfig {
        RollbackConfig::default()
    }

    #[test]
    fn from_pairs_counts_correctly() {
        let pairs = vec![
            make_pair(1, true, false, false, true),
            make_pair(2, false, true, false, false),
            make_pair(3, true, false, true, false),
        ];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.source_count, 3);
        assert_eq!(m.backup_count, 3);
        assert_eq!(m.rollback_count, 3);
        assert_eq!(m.unreliable_count, 1);
        assert_eq!(m.partial_count, 1);
        assert_eq!(m.irreversible_count, 1);
        assert_eq!(m.requires_lock_count, 1);
    }

    #[test]
    fn exit_code_zero_when_all_clean() {
        let pairs = vec![make_pair(1, true, false, false, false)];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.exit_code(false, false), 0);
    }

    #[test]
    fn exit_code_two_when_irreversible() {
        let pairs = vec![make_pair(1, true, false, true, false)];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.exit_code(false, false), 2);
    }

    #[test]
    fn exit_code_two_when_partial_unless_allowed() {
        let pairs = vec![make_pair(1, true, true, false, false)];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.exit_code(false, false), 2);
        assert_eq!(m.exit_code(false, true), 0);
    }

    #[test]
    fn exit_code_one_when_only_warning() {
        let pairs = vec![make_pair(1, true, false, false, true)];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.exit_code(false, false), 1);
        assert_eq!(m.exit_code(true, false), 2);
    }

    #[test]
    fn serialize_produces_valid_json() {
        let pairs = vec![make_pair(1, true, false, false, true)];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec!["test warning".to_string()]);
        let json = serialize_manifest(&m).unwrap();
        assert!(json.contains("\"dialect\": \"mysql\""));
        assert!(json.contains("\"requires_lock_count\": 1"));
        assert!(json.contains("\"lock_type\": \"FTWRL\""));
        assert!(json.contains("\"test warning\""));
    }

    // ===== P1-2: 执行期策略透出测试 =====

    #[test]
    fn manifest_includes_assert_and_partitioned_strategy() {
        // P1-2：assert_on_schema_mismatch / on_partitioned_table 应从 rc 透出到 manifest 顶层
        let pairs = vec![make_pair(1, true, false, false, false)];
        let rc = RollbackConfig {
            assert_on_schema_mismatch: "warn".to_string(),
            on_partitioned_table: "fallback".to_string(),
            ..RollbackConfig::default()
        };
        let m = Manifest::from_pairs(&pairs, "mysql", &rc, vec![]);
        assert_eq!(m.assert_on_schema_mismatch, "warn");
        assert_eq!(m.on_partitioned_table, "fallback");
        let json = serialize_manifest(&m).unwrap();
        assert!(json.contains("\"assert_on_schema_mismatch\": \"warn\""));
        assert!(json.contains("\"on_partitioned_table\": \"fallback\""));
    }
}
