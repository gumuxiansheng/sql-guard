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
    let warning_count = violations.iter().filter(|v| v.severity == "warning").count();

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
