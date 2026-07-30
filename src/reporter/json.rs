use crate::error::{DirectoryIssue, Violation};
use crate::reporter::Reporter;

/// JSON reporter — 写入 `sqlguard-report.json`。
pub struct JsonReporter;

impl Reporter for JsonReporter {
    fn name(&self) -> &str {
        "json"
    }

    fn needs_file_output(&self) -> bool {
        true
    }

    fn generate(
        &self,
        violations: &[Violation],
        missing: &[DirectoryIssue],
        unexpected: &[DirectoryIssue],
        files_checked: usize,
    ) -> String {
        generate_json_report(violations, missing, unexpected, files_checked)
    }
}

#[derive(serde::Serialize)]
pub struct JsonReport {
    pub summary: JsonSummary,
    pub directory_issues: Vec<JsonDirectoryIssue>,
    pub violations: Vec<JsonViolation>,
    pub files_checked: usize,
}

#[derive(serde::Serialize)]
pub struct JsonSummary {
    pub passed: bool,
    pub total_files: usize,
    pub total_violations: usize,
    pub errors: usize,
    pub warnings: usize,
    pub directory_issues_count: usize,
}

#[derive(serde::Serialize)]
pub struct JsonDirectoryIssue {
    pub path: String,
    pub issue_type: String,
}

#[derive(serde::Serialize)]
pub struct JsonViolation {
    pub rule_id: String,
    pub rule: String,
    pub group: Option<String>,
    pub severity: String,
    pub message: String,
    pub file: String,
    pub script_type: String,
    pub line: Option<usize>,
    /// 语句结束行（含），用于增量校验时的语句级范围判断。
    pub end_line: Option<usize>,
    pub column: Option<usize>,
}

pub fn generate_json_report(
    violations: &[Violation],
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
    files_checked: usize,
) -> String {
    let error_count = violations.iter().filter(|v| v.severity == "error").count();
    let warning_count = violations
        .iter()
        .filter(|v| v.severity == "warning")
        .count();

    let dir_issues: Vec<JsonDirectoryIssue> = missing
        .iter()
        .map(|d| JsonDirectoryIssue {
            path: d.path.to_string_lossy().to_string(),
            issue_type: "missing".to_string(),
        })
        .chain(unexpected.iter().map(|d| JsonDirectoryIssue {
            path: d.path.to_string_lossy().to_string(),
            issue_type: "unexpected".to_string(),
        }))
        .collect();

    let json_violations: Vec<JsonViolation> = violations
        .iter()
        .map(|v| JsonViolation {
            rule_id: v.rule_id.clone(),
            rule: v.rule_name.clone(),
            group: v.rule_group.clone(),
            severity: v.severity.clone(),
            message: v.message.clone(),
            file: v.file_path.to_string_lossy().to_string(),
            script_type: v.script_type.clone(),
            line: v.line,
            end_line: v.end_line,
            column: v.column,
        })
        .collect();

    let report = JsonReport {
        summary: JsonSummary {
            passed: violations.is_empty() && missing.is_empty(),
            total_files: files_checked,
            total_violations: violations.len(),
            errors: error_count,
            warnings: warning_count,
            directory_issues_count: dir_issues.len(),
        },
        directory_issues: dir_issues,
        violations: json_violations,
        files_checked,
    };

    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::error::{DirectoryIssue, DirectoryIssueType, Violation};

    use super::generate_json_report;

    fn make_violation(
        rule_id: &str,
        severity: &str,
        line: Option<usize>,
        message: &str,
    ) -> Violation {
        Violation {
            rule_id: rule_id.to_string(),
            rule_name: "test_rule".to_string(),
            rule_group: Some("test".to_string()),
            severity: severity.to_string(),
            message: message.to_string(),
            file_path: PathBuf::from("test.sql"),
            script_type: "dml".to_string(),
            line,
            end_line: line,
            column: None,
        }
    }

    #[test]
    fn generate_json_report_empty_violations() {
        let output = generate_json_report(&[], &[], &[], 5);
        let json: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(json["summary"]["passed"], true);
        assert_eq!(json["summary"]["total_violations"], 0);
    }

    #[test]
    fn generate_json_report_with_violations() {
        let v = make_violation("S001", "error", Some(42), "bad sql");
        let output = generate_json_report(&[v], &[], &[], 5);
        let json: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        let violations = json["violations"]
            .as_array()
            .expect("violations is an array");
        assert_eq!(violations.len(), 1);
        let first = &violations[0];
        assert_eq!(first["rule_id"], "S001");
        assert_eq!(first["severity"], "error");
        assert_eq!(first["file"], "test.sql");
        assert_eq!(first["line"], 42);
    }

    #[test]
    fn generate_json_report_with_directory_issues() {
        let missing = DirectoryIssue {
            path: PathBuf::from("missing_dir"),
            issue_type: DirectoryIssueType::Missing,
        };
        let unexpected = DirectoryIssue {
            path: PathBuf::from("unexpected_dir"),
            issue_type: DirectoryIssueType::Unexpected,
        };
        let output = generate_json_report(&[], &[missing], &[unexpected], 5);
        let json: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        let dir_issues = json["directory_issues"]
            .as_array()
            .expect("directory_issues is an array");
        assert_eq!(dir_issues.len(), 2);
        assert_eq!(dir_issues[0]["issue_type"], "missing");
        assert_eq!(dir_issues[1]["issue_type"], "unexpected");
    }

    #[test]
    fn generate_json_report_counts() {
        let violations = vec![
            make_violation("E001", "error", Some(1), "err1"),
            make_violation("E002", "error", Some(2), "err2"),
            make_violation("W001", "warning", Some(3), "warn1"),
        ];
        let output = generate_json_report(&violations, &[], &[], 5);
        let json: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(json["summary"]["errors"], 2);
        assert_eq!(json["summary"]["warnings"], 1);
    }
}
