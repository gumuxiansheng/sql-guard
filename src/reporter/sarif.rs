//! SARIF v2.1.0 报告器。
//!
//! 输出符合 SARIF（Static Analysis Results Interchange Format）规范的 JSON 报告，
//! 用于接入 GitHub Code Scanning / Azure DevOps / GitLab code scanning 等现代 CI 面板。
//!
//! 规范参考：<https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html>
//!
//! 输出结构要点：
//! - `runs[].tool.driver.rules[]`：去重后的规则元数据（id / name / helpUri）
//! - `runs[].results[]`：每条 violation 一个 result
//! - `level` 映射：error→error，warning→warning，其它→note
//! - `locations[].physicalLocation.region`：行号 1-based，column 可选

use crate::error::{DirectoryIssue, Violation};
use crate::reporter::Reporter;

/// SARIF reporter — 写入 `sqlguard-report.sarif`。
pub struct SarifReporter;

impl Reporter for SarifReporter {
    fn name(&self) -> &str {
        "sarif"
    }

    fn needs_file_output(&self) -> bool {
        true
    }

    fn generate(
        &self,
        violations: &[Violation],
        missing: &[DirectoryIssue],
        unexpected: &[DirectoryIssue],
        _files_checked: usize,
    ) -> String {
        generate_sarif_report(violations, missing, unexpected)
    }
}

// ===== SARIF 数据结构（仅序列化需要的字段）=====
//
// SARIF v2.1.0 JSON 字段一律使用 camelCase（ruleId / ruleIndex / startLine 等），
// 因此所有结构体统一加 `rename_all = "camelCase"`，Rust 端保留 snake_case 风格。

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLog {
    #[serde(rename = "$schema")]
    pub schema: &'static str,
    pub version: &'static str,
    pub runs: Vec<SarifRun>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRun {
    pub tool: SarifTool,
    pub results: Vec<SarifResult>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifTool {
    pub driver: SarifDriver,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifDriver {
    pub name: &'static str,
    pub version: &'static str,
    pub information_uri: &'static str,
    pub rules: Vec<SarifRule>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRule {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_description: Option<SarifMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_uri: Option<&'static str>,
}

#[derive(serde::Serialize)]
pub struct SarifMessage {
    pub text: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifResult {
    pub rule_id: String,
    /// ruleIndex 必须指向本 run 的 tool.driver.rules[] 数组下标。
    /// SARIF 规范要求两者之一必须存在；同时给出可最大兼容面板实现。
    pub rule_index: usize,
    pub level: &'static str,
    pub message: SarifMessage,
    pub locations: Vec<SarifLocation>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLocation {
    pub physical_location: SarifPhysicalLocation,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifPhysicalLocation {
    pub artifact_location: SarifArtifactLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<SarifRegion>,
}

#[derive(serde::Serialize)]
pub struct SarifArtifactLocation {
    pub uri: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRegion {
    pub start_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_column: Option<usize>,
}

/// 把 SqlGuard severity 映射到 SARIF level。
///
/// SARIF level 仅允许 `error | warning | note | none`。
fn severity_to_level(severity: &str) -> &'static str {
    match severity.to_lowercase().as_str() {
        "error" | "fatal" | "critical" => "error",
        "warning" | "warn" => "warning",
        "info" | "note" => "note",
        _ => "warning", // 未知 severity 默认 warning，避免被面板忽略
    }
}

const SARIF_SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/cs01/schemas/sarif-schema-2.1.0.json";
const SARIF_VERSION: &str = "2.1.0";
const TOOL_NAME: &str = "SqlGuard";
const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
const TOOL_INFO_URI: &str = "https://github.com/sqlguard/sqlguard";

/// 生成 SARIF v2.1.0 报告字符串。
///
/// - `violations`：所有 SQL 违规
/// - `missing` / `unexpected`：目录结构问题，作为 `note` 级别 result 上报
///   （SARIF 中没有「目录问题」概念，统一映射为结果）
pub fn generate_sarif_report(
    violations: &[Violation],
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
) -> String {
    // 1) 收集去重的规则元数据，建立 rule_id → index 映射
    let mut rules: Vec<SarifRule> = Vec::new();
    let mut rule_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for v in violations {
        if rule_index.contains_key(&v.rule_id) {
            continue;
        }
        rule_index.insert(v.rule_id.clone(), rules.len());
        rules.push(SarifRule {
            id: v.rule_id.clone(),
            name: v.rule_name.clone(),
            short_description: v.rule_group.as_ref().map(|g| SarifMessage {
                text: format!("group: {}", g),
            }),
            help_uri: Some(TOOL_INFO_URI),
        });
    }

    // 2) 构造 results
    let mut results: Vec<SarifResult> =
        Vec::with_capacity(violations.len() + missing.len() + unexpected.len());

    for v in violations {
        let idx = *rule_index.get(&v.rule_id).unwrap_or(&0);
        results.push(SarifResult {
            rule_id: v.rule_id.clone(),
            rule_index: idx,
            level: severity_to_level(&v.severity),
            message: SarifMessage {
                text: v.message.clone(),
            },
            locations: vec![SarifLocation {
                physical_location: SarifPhysicalLocation {
                    artifact_location: SarifArtifactLocation {
                        uri: v.file_path.to_string_lossy().to_string(),
                    },
                    region: v.line.map(|start_line| SarifRegion {
                        start_line,
                        end_line: v.end_line,
                        start_column: v.column,
                    }),
                },
            }],
        });
    }

    // 3) 目录结构问题：归到一条虚拟规则 STRUCT001（group: structure）
    const STRUCT_RULE_ID: &str = "STRUCT001";
    let struct_idx = if !rule_index.contains_key(STRUCT_RULE_ID)
        && (!missing.is_empty() || !unexpected.is_empty())
    {
        let idx = rules.len();
        rule_index.insert(STRUCT_RULE_ID.to_string(), idx);
        rules.push(SarifRule {
            id: STRUCT_RULE_ID.to_string(),
            name: "Directory structure issue".to_string(),
            short_description: Some(SarifMessage {
                text: "group: structure".to_string(),
            }),
            help_uri: Some(TOOL_INFO_URI),
        });
        idx
    } else {
        *rule_index.get(STRUCT_RULE_ID).unwrap_or(&0)
    };

    for d in missing.iter().map(|d| (d, "missing")) {
        results.push(SarifResult {
            rule_id: STRUCT_RULE_ID.to_string(),
            rule_index: struct_idx,
            level: "error",
            message: SarifMessage {
                text: format!("Missing required directory: {}", d.0.path.to_string_lossy()),
            },
            locations: vec![SarifLocation {
                physical_location: SarifPhysicalLocation {
                    artifact_location: SarifArtifactLocation {
                        uri: d.0.path.to_string_lossy().to_string(),
                    },
                    region: None,
                },
            }],
        });
    }

    for d in unexpected.iter().map(|d| (d, "unexpected")) {
        results.push(SarifResult {
            rule_id: STRUCT_RULE_ID.to_string(),
            rule_index: struct_idx,
            level: "warning",
            message: SarifMessage {
                text: format!("Unexpected directory: {}", d.0.path.to_string_lossy()),
            },
            locations: vec![SarifLocation {
                physical_location: SarifPhysicalLocation {
                    artifact_location: SarifArtifactLocation {
                        uri: d.0.path.to_string_lossy().to_string(),
                    },
                    region: None,
                },
            }],
        });
    }

    let log = SarifLog {
        schema: SARIF_SCHEMA,
        version: SARIF_VERSION,
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: TOOL_NAME,
                    version: TOOL_VERSION,
                    information_uri: TOOL_INFO_URI,
                    rules,
                },
            },
            results,
        }],
    };

    serde_json::to_string_pretty(&log).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{DirectoryIssue, DirectoryIssueType};
    use std::path::PathBuf;

    fn make_violation(rule_id: &str, severity: &str, line: Option<usize>) -> Violation {
        Violation {
            rule_id: rule_id.to_string(),
            rule_name: format!("rule-{}", rule_id),
            rule_group: Some("dml".to_string()),
            severity: severity.to_string(),
            message: "test violation".to_string(),
            file_path: PathBuf::from("/tmp/test.sql"),
            script_type: "dml".to_string(),
            line,
            end_line: None,
            column: Some(1),
        }
    }

    #[test]
    fn test_severity_mapping() {
        assert_eq!(severity_to_level("error"), "error");
        assert_eq!(severity_to_level("warning"), "warning");
        assert_eq!(severity_to_level("info"), "note");
        assert_eq!(severity_to_level("unknown"), "warning");
    }

    #[test]
    fn test_sarif_basic_structure() {
        let v = vec![make_violation("DML001", "error", Some(10))];
        let out = generate_sarif_report(&v, &[], &[]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(parsed["version"], "2.1.0");
        assert_eq!(parsed["$schema"], SARIF_SCHEMA);
        assert_eq!(parsed["runs"][0]["tool"]["driver"]["name"], "SqlGuard");
        assert_eq!(
            parsed["runs"][0]["tool"]["driver"]["rules"][0]["id"],
            "DML001"
        );
        assert_eq!(parsed["runs"][0]["results"][0]["ruleId"], "DML001");
        assert_eq!(parsed["runs"][0]["results"][0]["level"], "error");
        assert_eq!(
            parsed["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]
                ["startLine"],
            10
        );
    }

    #[test]
    fn test_sarif_dedup_rules() {
        // 同一 rule_id 多次违规，rules 数组只出现一次
        let v = vec![
            make_violation("DML001", "error", Some(1)),
            make_violation("DML001", "error", Some(5)),
            make_violation("DML002", "warning", Some(2)),
        ];
        let out = generate_sarif_report(&v, &[], &[]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        let rules = parsed["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap();
        assert_eq!(rules.len(), 2, "rules should be deduplicated");

        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 3, "all violations should be reported");
        assert_eq!(results[0]["ruleIndex"], 0);
        assert_eq!(results[2]["ruleIndex"], 1);
    }

    #[test]
    fn test_sarif_directory_issues() {
        let missing = vec![DirectoryIssue {
            path: PathBuf::from("/tmp/sql/ddl"),
            issue_type: DirectoryIssueType::Missing,
        }];
        let unexpected = vec![DirectoryIssue {
            path: PathBuf::from("/tmp/sql/others/extra"),
            issue_type: DirectoryIssueType::Unexpected,
        }];
        let out = generate_sarif_report(&[], &missing, &unexpected);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        let rules = parsed["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap();
        assert_eq!(rules[0]["id"], "STRUCT001");

        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["level"], "error"); // missing
        assert_eq!(results[1]["level"], "warning"); // unexpected
    }

    #[test]
    fn test_sarif_no_line_skips_region() {
        // 无行号的违规不应包含 region（SARIF 规范允许缺省）
        let v = vec![make_violation("DML001", "error", None)];
        let out = generate_sarif_report(&v, &[], &[]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        let loc = &parsed["runs"][0]["results"][0]["locations"][0]["physicalLocation"];
        assert!(
            loc.get("region").is_none() || loc["region"].is_null(),
            "region should be absent when line is None"
        );
        assert_eq!(loc["artifactLocation"]["uri"], "/tmp/test.sql");
    }
}
