use std::path::Path;

use crate::error::{DirectoryIssue, Violation};

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn generate_html_report(
    violations: &[Violation],
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
    files_checked: usize,
) -> String {
    let error_count = violations.iter().filter(|v| v.severity == "error").count();
    let warning_count = violations.iter().filter(|v| v.severity == "warning").count();
    let has_dir_issues = !missing.is_empty() || !unexpected.is_empty();
    let passed = violations.is_empty() && missing.is_empty();

    let _status_color = if passed { "#22c55e" } else { "#ef4444" };
    let status_text = if passed { "PASSED" } else { "FAILED" };

    let mut dir_issues_rows = String::new();
    for issue in missing {
        dir_issues_rows.push_str(&format!(
            "<tr><td>{}</td><td><span class=\"badge badge-error\">missing</span></td></tr>\n",
            escape_html(&issue.path.to_string_lossy())
        ));
    }
    for issue in unexpected {
        dir_issues_rows.push_str(&format!(
            "<tr><td>{}</td><td><span class=\"badge badge-warning\">unexpected</span></td></tr>\n",
            escape_html(&issue.path.to_string_lossy())
        ));
    }

    let mut violation_rows = String::new();
    let mut sorted = violations.to_vec();
    sorted.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.file_path.cmp(&b.file_path))
    });
    for v in &sorted {
        let sev_class = match v.severity.as_str() {
            "error" => "severity-error",
            "warning" => "severity-warning",
            _ => "severity-info",
        };
        violation_rows.push_str(&format!(
            r#"<tr>
                <td><span class="severity-badge {}">{}</span></td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
            </tr>"#,
            sev_class,
            escape_html(&v.severity.to_uppercase()),
            escape_html(&v.rule_name),
            escape_html(&v.file_path.to_string_lossy()),
            escape_html(&v.message)
        ));
    }

    let dir_section = if has_dir_issues {
        format!(
            r#"<div class="section">
                <h2>Directory Structure Issues</h2>
                <table>
                    <thead><tr><th>Path</th><th>Issue</th></tr></thead>
                    <tbody>{}</tbody>
                </table>
            </div>"#,
            dir_issues_rows
        )
    } else {
        String::new()
    };

    let violation_section = if !violations.is_empty() {
        format!(
            r#"<div class="section">
                <h2>Violations</h2>
                <table>
                    <thead><tr><th>Severity</th><th>Rule</th><th>File</th><th>Message</th></tr></thead>
                    <tbody>{}</tbody>
                </table>
            </div>"#,
            violation_rows
        )
    } else {
        r#"<div class="section"><h2>Violations</h2><p class="pass-text">No violations found.</p></div>"#
            .to_string()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>SqlGuard Report</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #0f172a; color: #e2e8f0; line-height: 1.6; }}
  .container {{ max-width: 1200px; margin: 0 auto; padding: 2rem; }}
  .header {{ text-align: center; padding: 2rem 0; border-bottom: 1px solid #334155; margin-bottom: 2rem; }}
  .header h1 {{ font-size: 2rem; color: #f8fafc; }}
  .status {{ display: inline-block; padding: 0.5rem 1.5rem; border-radius: 9999px; font-weight: 700; font-size: 1.125rem; margin-top: 1rem; }}
  .status-pass {{ background: #22c55e; color: #052e16; }}
  .status-fail {{ background: #ef4444; color: #450a0a; }}
  .summary {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 1rem; margin-bottom: 2rem; }}
  .summary-card {{ background: #1e293b; border-radius: 0.75rem; padding: 1.25rem; text-align: center; }}
  .summary-card .value {{ font-size: 2rem; font-weight: 700; }}
  .summary-card .label {{ font-size: 0.875rem; color: #94a3b8; }}
  .value-green {{ color: #22c55e; }}
  .value-red {{ color: #ef4444; }}
  .value-yellow {{ color: #eab308; }}
  .value-blue {{ color: #3b82f6; }}
  .section {{ background: #1e293b; border-radius: 0.75rem; padding: 1.5rem; margin-bottom: 1.5rem; }}
  .section h2 {{ font-size: 1.25rem; margin-bottom: 1rem; color: #f1f5f9; }}
  table {{ width: 100%; border-collapse: collapse; }}
  th, td {{ padding: 0.75rem 1rem; text-align: left; border-bottom: 1px solid #334155; }}
  th {{ color: #94a3b8; font-weight: 600; font-size: 0.875rem; text-transform: uppercase; letter-spacing: 0.05em; }}
  .badge {{ display: inline-block; padding: 0.25rem 0.75rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 600; }}
  .badge-error {{ background: #7f1d1d; color: #fca5a5; }}
  .badge-warning {{ background: #713f12; color: #fcd34d; }}
  .severity-badge {{ display: inline-block; padding: 0.25rem 0.75rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 600; }}
  .severity-error {{ background: #7f1d1d; color: #fca5a5; }}
  .severity-warning {{ background: #713f12; color: #fcd34d; }}
  .severity-info {{ background: #1e3a5f; color: #93c5fd; }}
  .pass-text {{ color: #4ade80; font-size: 1.125rem; }}
  .footer {{ text-align: center; padding: 2rem 0; color: #475569; font-size: 0.875rem; }}
</style>
</head>
<body>
<div class="container">
  <div class="header">
    <h1>SqlGuard Report</h1>
    <div class="status status-{status_class}">{status_text}</div>
  </div>
  <div class="summary">
    <div class="summary-card">
      <div class="value value-blue">{files_checked}</div>
      <div class="label">Files Checked</div>
    </div>
    <div class="summary-card">
      <div class="value value-red">{error_count}</div>
      <div class="label">Errors</div>
    </div>
    <div class="summary-card">
      <div class="value value-yellow">{warning_count}</div>
      <div class="label">Warnings</div>
    </div>
    <div class="summary-card">
      <div class="value value-blue">{total_violations}</div>
      <div class="label">Total Violations</div>
    </div>
  </div>
  {dir_section}
  {violation_section}
  <div class="footer">
    Generated by SqlGuard | {timestamp}
  </div>
</div>
</body>
</html>"#,
        files_checked = files_checked,
        error_count = error_count,
        warning_count = warning_count,
        total_violations = violations.len(),
        status_class = if passed { "pass" } else { "fail" },
        status_text = status_text,
        dir_section = dir_section,
        violation_section = violation_section,
        timestamp = chrono_now(),
    )
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let hours = (secs / 3600) % 24;
    let minutes = (secs / 60) % 60;
    let seconds = secs % 60;
    let days = secs / 86400;
    format!("Day {} {:02}:{:02}:{:02} UTC", days, hours, minutes, seconds)
}

pub fn save_html_report(
    output_dir: &Path,
    violations: &[Violation],
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
    files_checked: usize,
) -> Result<String, String> {
    let html = generate_html_report(violations, missing, unexpected, files_checked);
    let report_path = output_dir.join("sqlguard-report.html");
    std::fs::write(&report_path, &html)
        .map_err(|e| format!("Failed to write HTML report: {}", e))?;
    Ok(report_path.to_string_lossy().to_string())
}
