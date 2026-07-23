use colored::Colorize;

use crate::error::{DirectoryIssue, Violation};
use crate::reporter::Reporter;

/// Plain text reporter — 打印到 stdout。
pub struct PlainReporter;

impl Reporter for PlainReporter {
    fn name(&self) -> &str {
        "plain"
    }

    fn needs_file_output(&self) -> bool {
        false
    }

    fn generate(
        &self,
        violations: &[Violation],
        missing: &[DirectoryIssue],
        unexpected: &[DirectoryIssue],
        files_checked: usize,
    ) -> String {
        generate_plain_report(violations, missing, unexpected, files_checked)
    }
}

pub fn generate_plain_report(
    violations: &[Violation],
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
    files_checked: usize,
) -> String {
    let mut output = String::new();

    output.push_str(&format!("{}\n", "═".repeat(60).bright_black()));
    output.push_str(&format!("{}\n", " SqlGuard Report".bold()));
    output.push_str(&format!("{}\n\n", "═".repeat(60).bright_black()));

    output.push_str(&format!("{} checked: {}\n\n", "Files".bold().underline(), files_checked));

    let has_dir_issues = !missing.is_empty() || !unexpected.is_empty();
    if has_dir_issues {
        output.push_str(&format!("{}\n", "── Directory Structure Issues ──".yellow().bold()));
        output.push_str(&format!("{}\n", "─".repeat(40).bright_black()));

        for issue in missing {
            output.push_str(&format!(
                "  {} {} (missing)\n",
                "✗".red().bold(),
                issue.path.display()
            ));
        }
        for issue in unexpected {
            output.push_str(&format!(
                "  {} {} (unexpected)\n",
                "!".yellow().bold(),
                issue.path.display()
            ));
        }
        output.push('\n');
    }

    if !violations.is_empty() {
        output.push_str(&format!("{}\n", "── Violations ──".red().bold()));
        output.push_str(&format!("{}\n", "─".repeat(40).bright_black()));

        let mut sorted = violations.to_vec();
        sorted.sort_by(|a, b| {
            b.severity.cmp(&a.severity)
                .then_with(|| a.file_path.cmp(&b.file_path))
        });

        for v in &sorted {
            let sev = match v.severity.as_str() {
                "error" => format!("{}", "ERROR".red().bold()),
                "warning" => format!("{}", "WARN".yellow().bold()),
                other => format!("{}", other.to_uppercase().cyan().bold()),
            };
            let location = match (v.line, v.end_line, v.column) {
                (Some(line), Some(end), Some(col)) if end > line => format!(":{}-{}:{}", line, end, col),
                (Some(line), Some(end), None) if end > line => format!(":{}-{}", line, end),
                (Some(line), _, Some(col)) => format!(":{}:{}", line, col),
                (Some(line), _, None) => format!(":{}", line),
                _ => String::new(),
            };
            let group_tag = match &v.rule_group {
                Some(g) => format!(" [{}]", g),
                None => String::new(),
            };
            output.push_str(&format!(
                "  [{}] {}{} (rule: {} {}{})\n",
                sev,
                v.file_path.display(),
                location,
                v.rule_id,
                v.rule_name,
                group_tag
            ));
            output.push_str(&format!("        {}\n", v.message));
        }
        output.push('\n');
    } else {
        output.push_str(&format!("{}\n\n", "✓ No violations found".green().bold()));
    }

    let error_count = violations.iter().filter(|v| v.severity == "error").count();
    let warning_count = violations.iter().filter(|v| v.severity == "warning").count();

    output.push_str(&format!("{}\n", "─".repeat(40).bright_black()));
    output.push_str(&format!("{}: ", "Summary".bold()));
    if error_count > 0 {
        output.push_str(&format!("{} ", format!("{} error(s)", error_count).red().bold()));
    }
    if warning_count > 0 {
        output.push_str(&format!("{} ", format!("{} warning(s)", warning_count).yellow().bold()));
    }
    if error_count == 0 && warning_count == 0 && !has_dir_issues {
        output.push_str(&format!("{}", "All checks passed".green().bold()));
    }
    output.push('\n');

    output
}
