//! `rollback-manifest.json` 序列化。
//!
//! 对应设计文档 §4.3 `Manifest` / `ManifestItem`。
//! `ManifestItem` 通过 `#[serde(flatten)]` 把 `SafetyClass` + `BackupStrategy` 字段平铺到 item 下，
//! 兼容旧发布平台按字段名读取。

use super::util::current_iso8601_utc;
use super::{
    review_level_for, BackupRollbackPair, BackupStrategy, ExpectedSchema, SafetyClass, SourceRef,
};
use crate::config::RollbackConfig;
use serde::Serialize;

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
    /// ★ Review 契约：是否含强制复核项（`review_level=required` 的 item 数 > 0）。
    /// 发布平台据此做最小 gate 判断，无需遍历 items。
    pub review_required: bool,
    /// 强制复核项数（`review_level=required`）
    pub required_review_count: usize,
    /// 提示性复核项数（`review_level=optional`）
    pub optional_review_count: usize,
    /// 自动放行项数（`review_level=none`）
    pub auto_approved_count: usize,
    pub items: Vec<ManifestItem>,
    pub warnings: Vec<String>,
}

/// ★ C1 修正：补齐文档通篇引用的 8 个字段，对齐发布平台契约。
#[derive(Serialize)]
pub struct ManifestItem {
    pub seq: u64,
    pub source: SourceRef,
    pub original_sql: String,
    /// 语句类型（StmtInfo.kind），如 "INSERT" / "DROP_TABLE" / "ALTER_TABLE"。
    /// 供发布平台按类型过滤、HTML 报告按类型展示。
    pub stmt_kind: String,
    pub backup: Option<String>,
    pub rollback: Option<String>,
    /// ★ Review 契约：由 [`super::review_level_for`] 从 safety 推导，无需用户配置。
    /// `"none"`（免审）/ `"optional"`（提示）/ `"required"`（强制阻断）
    pub review_level: &'static str,
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
    pub fn from_pairs(
        pairs: &[BackupRollbackPair],
        dialect: &str,
        rc: &RollbackConfig,
        warnings: Vec<String>,
    ) -> Self {
        let mut unreliable = 0usize;
        let mut irreversible = 0usize;
        let mut partial = 0usize;
        let mut counter = 0usize;
        let mut requires_lock = 0usize;
        let mut partitioned = 0usize;
        let mut irreversible_if_missing = 0usize;
        let mut backup_count = 0usize;
        let mut rollback_count = 0usize;
        // ★ Review 契约汇总
        let mut required_review = 0usize;
        let mut optional_review = 0usize;
        let mut auto_approved = 0usize;

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
            match review_level_for(&p.safety) {
                "required" => required_review += 1,
                "optional" => optional_review += 1,
                _ => auto_approved += 1,
            }
        }

        let items = pairs
            .iter()
            .map(|p| ManifestItem {
                seq: p.seq,
                source: p.source.clone(),
                original_sql: p.original_sql.clone(),
                stmt_kind: p.stmt_kind.clone(),
                backup: p.backup.clone(),
                rollback: p.rollback.clone(),
                review_level: review_level_for(&p.safety),
                safety: p.safety.clone(),
                strategy: p.strategy.clone(),
                expected_schema: p.expected_schema.clone(),
                warnings: p.warnings.clone(),
            })
            .collect();

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
            // ★ Review 契约：汇总字段供发布平台 O(1) gate
            review_required: required_review > 0,
            required_review_count: required_review,
            optional_review_count: optional_review,
            auto_approved_count: auto_approved,
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

    fn make_pair(
        seq: u64,
        reliable: bool,
        partial: bool,
        irreversible: bool,
        requires_lock: bool,
    ) -> BackupRollbackPair {
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
                lock_type: if requires_lock {
                    Some("FTWRL".to_string())
                } else {
                    None
                },
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
        let m = Manifest::from_pairs(
            &pairs,
            "mysql",
            &default_rc(),
            vec!["test warning".to_string()],
        );
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

    // ===== Review 契约:review_level 推导与汇总计数测试 =====

    /// 构造指定 SafetyClass 的 pair（弥补 make_pair 不支持 counter_unrestored /
    /// irreversible_if_backup_missing 的不足）。
    fn make_pair_with_safety(seq: u64, safety: SafetyClass) -> BackupRollbackPair {
        BackupRollbackPair {
            seq,
            stmt_kind: String::new(),
            source: SourceRef::placeholder(),
            original_sql: format!("-- stmt {}", seq),
            backup: Some(format!("-- backup {}", seq)),
            rollback: Some(format!("-- rollback {}", seq)),
            safety,
            strategy: BackupStrategy::default(),
            expected_schema: None,
            warnings: vec![],
        }
    }

    fn safety(reliable: bool) -> SafetyClass {
        SafetyClass {
            reliable,
            ..SafetyClass::default()
        }
    }

    #[test]
    fn review_level_none_for_clean_reliable() {
        // reliable=true 且无任何 flag → "none"
        let s = safety(true);
        assert_eq!(review_level_for(&s), "none");
    }

    #[test]
    fn review_level_optional_for_requires_lock_only() {
        // 仅 requires_lock → "optional"
        let s = SafetyClass {
            reliable: true,
            requires_lock: true,
            ..SafetyClass::default()
        };
        assert_eq!(review_level_for(&s), "optional");
    }

    #[test]
    fn review_level_optional_for_counter_unrestored_only() {
        // 仅 counter_unrestored → "optional"
        let s = SafetyClass {
            reliable: true,
            counter_unrestored: true,
            ..SafetyClass::default()
        };
        assert_eq!(review_level_for(&s), "optional");
    }

    #[test]
    fn review_level_required_for_irreversible() {
        let s = SafetyClass {
            reliable: true,
            irreversible: true,
            ..SafetyClass::default()
        };
        assert_eq!(review_level_for(&s), "required");
    }

    #[test]
    fn review_level_required_for_unreliable() {
        // !reliable（主键缺失）→ "required"
        let s = safety(false);
        assert_eq!(review_level_for(&s), "required");
    }

    #[test]
    fn review_level_required_for_partial() {
        let s = SafetyClass {
            reliable: true,
            partial: true,
            ..SafetyClass::default()
        };
        assert_eq!(review_level_for(&s), "required");
    }

    #[test]
    fn review_level_required_for_irreversible_if_backup_missing() {
        let s = SafetyClass {
            reliable: true,
            irreversible_if_backup_missing: true,
            ..SafetyClass::default()
        };
        assert_eq!(review_level_for(&s), "required");
    }

    #[test]
    fn review_level_required_dominates_optional() {
        // required flag 与 optional flag 同时存在 → "required"（强审优先级更高）
        let s = SafetyClass {
            reliable: true,
            irreversible: true,
            requires_lock: true,
            counter_unrestored: true,
            ..SafetyClass::default()
        };
        assert_eq!(review_level_for(&s), "required");
    }

    #[test]
    fn manifest_review_summary_counts_correctly() {
        // 混合批次：1 none + 1 optional + 2 required
        let pairs = vec![
            make_pair_with_safety(1, safety(true)), // none
            make_pair_with_safety(
                2,
                SafetyClass {
                    reliable: true,
                    requires_lock: true,
                    ..SafetyClass::default()
                },
            ), // optional
            make_pair_with_safety(
                3,
                SafetyClass {
                    reliable: true,
                    irreversible: true,
                    ..SafetyClass::default()
                },
            ), // required
            make_pair_with_safety(4, safety(false)), // required (unreliable)
        ];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert!(m.review_required);
        assert_eq!(m.required_review_count, 2);
        assert_eq!(m.optional_review_count, 1);
        assert_eq!(m.auto_approved_count, 1);
    }

    #[test]
    fn manifest_review_required_false_when_all_clean() {
        let pairs = vec![make_pair_with_safety(1, safety(true))];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert!(!m.review_required);
        assert_eq!(m.required_review_count, 0);
        assert_eq!(m.auto_approved_count, 1);
    }

    #[test]
    fn manifest_item_review_level_serialized() {
        // per-item review_level 应出现在序列化 JSON 中
        let pairs = vec![
            make_pair_with_safety(1, safety(true)),  // none
            make_pair_with_safety(2, safety(false)), // required
        ];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        let json = serialize_manifest(&m).unwrap();
        assert!(json.contains("\"review_level\": \"none\""));
        assert!(json.contains("\"review_level\": \"required\""));
        assert!(json.contains("\"review_required\": true"));
        assert!(json.contains("\"required_review_count\": 1"));
    }

    #[test]
    fn review_required_aligned_with_exit_code_2() {
        // 不变式：exit_code=2 ⇒ review_required=true
        // 场景 1: irreversible → exit_code=2, review_required=true
        let pairs = vec![make_pair_with_safety(
            1,
            SafetyClass {
                reliable: true,
                irreversible: true,
                ..SafetyClass::default()
            },
        )];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.exit_code(false, false), 2);
        assert!(m.review_required);

        // 场景 2: 全 clean → exit_code=0, review_required=false
        let pairs = vec![make_pair_with_safety(1, safety(true))];
        let m = Manifest::from_pairs(&pairs, "mysql", &default_rc(), vec![]);
        assert_eq!(m.exit_code(false, false), 0);
        assert!(!m.review_required);
    }
}
