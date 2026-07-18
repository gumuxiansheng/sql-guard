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
        let location = match (v.line, v.column) {
            (Some(line), Some(col)) => format!(":{}:{}", line, col),
            (Some(line), None) => format!(":{}", line),
            _ => String::new(),
        };
        let rule_cell = format!(
            "<div class=\"rule-cell\"><span class=\"rule-id\">{}</span> <span class=\"rule-name\">{}</span>{}",
            escape_html(&v.rule_id),
            escape_html(&v.rule_name),
            match &v.rule_group {
                Some(g) => format!(" <span class=\"badge badge-info\">{}</span>", escape_html(g)),
                None => String::new(),
            }
        );
        violation_rows.push_str(&format!(
            r#"<tr>
                <td><span class="severity-badge {}">{}</span></td>
                <td>{}</div></td>
                <td class="file-path">{}{}</td>
                <td class="message">{}</td>
            </tr>"#,
            sev_class,
            escape_html(&v.severity.to_uppercase()),
            rule_cell,
            escape_html(&v.file_path.to_string_lossy()),
            escape_html(&location),
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
  :root {{
    --bg: #09090b;
    --surface: #18181b;
    --surface-2: #121214;
    --border: #27272a;
    --text: #e4e4e7;
    --text-secondary: #a1a1aa;
    --text-muted: #71717a;
    --accent-error: #f87171;
    --accent-error-bg: rgba(239, 68, 68, 0.12);
    --accent-warning: #facc15;
    --accent-warning-bg: rgba(234, 179, 8, 0.12);
    --accent-info: #60a5fa;
    --accent-info-bg: rgba(59, 130, 246, 0.12);
    --accent-success: #4ade80;
    --accent-success-bg: rgba(34, 197, 94, 0.12);
  }}
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif; background: var(--bg); color: var(--text); line-height: 1.5; -webkit-font-smoothing: antialiased; }}
  .container {{ max-width: 1200px; margin: 0 auto; padding: 2rem; }}
  .header {{ display: flex; align-items: center; justify-content: space-between; padding: 1.25rem 0; border-bottom: 1px solid var(--border); margin-bottom: 1.5rem; }}
  .header h1 {{ font-size: 1.25rem; font-weight: 600; color: #fafafa; letter-spacing: -0.01em; }}
  .status {{ display: inline-flex; align-items: center; gap: 0.5rem; padding: 0.375rem 0.875rem; border-radius: 0.375rem; font-weight: 600; font-size: 0.8125rem; }}
  .status::before {{ content: ""; width: 0.5rem; height: 0.5rem; border-radius: 50%; background: currentColor; }}
  .status-pass {{ background: var(--accent-success-bg); color: var(--accent-success); }}
  .status-fail {{ background: var(--accent-error-bg); color: var(--accent-error); }}
  .summary {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 0.75rem; margin-bottom: 1.5rem; }}
  .summary-card {{ background: var(--surface); border: 1px solid var(--border); border-radius: 0.5rem; padding: 1rem 1.25rem; }}
  .summary-card .value {{ font-size: 1.5rem; font-weight: 700; line-height: 1.2; margin-bottom: 0.25rem; }}
  .summary-card .label {{ font-size: 0.75rem; color: var(--text-secondary); font-weight: 500; text-transform: uppercase; letter-spacing: 0.05em; }}
  .value-green {{ color: var(--accent-success); }}
  .value-red {{ color: var(--accent-error); }}
  .value-yellow {{ color: var(--accent-warning); }}
  .value-blue {{ color: var(--accent-info); }}
  .section {{ margin-bottom: 1.5rem; }}
  .section h2 {{ font-size: 0.75rem; font-weight: 600; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.08em; margin-bottom: 0.75rem; }}
  table {{ width: 100%; border-collapse: separate; border-spacing: 0; background: var(--surface); border: 1px solid var(--border); border-radius: 0.5rem; overflow: hidden; font-size: 0.8125rem; }}
  th, td {{ padding: 0.625rem 1rem; text-align: left; border-bottom: 1px solid var(--border); vertical-align: middle; }}
  th {{ color: var(--text-secondary); font-weight: 500; font-size: 0.6875rem; text-transform: uppercase; letter-spacing: 0.08em; background: var(--surface-2); }}
  tbody tr:last-child td {{ border-bottom: none; }}
  tbody tr:hover td {{ background: rgba(255, 255, 255, 0.02); }}
  .badge {{ display: inline-flex; align-items: center; padding: 0.125rem 0.5rem; border-radius: 0.25rem; font-size: 0.6875rem; font-weight: 600; line-height: 1; }}
  .badge-error {{ background: var(--accent-error-bg); color: var(--accent-error); }}
  .badge-warning {{ background: var(--accent-warning-bg); color: var(--accent-warning); }}
  .badge-info {{ background: var(--accent-info-bg); color: var(--accent-info); }}
  .rule-cell {{ display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.35rem; }}
  .rule-id {{ font-family: 'SF Mono', Monaco, Consolas, 'Liberation Mono', monospace; color: var(--accent-info); font-size: 0.75rem; font-weight: 500; }}
  .rule-name {{ color: var(--text); font-weight: 500; }}
  .severity-badge {{ display: inline-flex; align-items: center; padding: 0.25rem 0.625rem; border-radius: 0.25rem; font-size: 0.6875rem; font-weight: 700; letter-spacing: 0.04em; text-transform: uppercase; line-height: 1; }}
  .severity-error {{ background: var(--accent-error-bg); color: var(--accent-error); }}
  .severity-warning {{ background: var(--accent-warning-bg); color: var(--accent-warning); }}
  .severity-info {{ background: var(--accent-info-bg); color: var(--accent-info); }}
  .file-path {{ font-family: 'SF Mono', Monaco, Consolas, 'Liberation Mono', monospace; color: var(--text-secondary); font-size: 0.75rem; word-break: break-all; line-height: 1.4; }}
  .message {{ color: var(--text); line-height: 1.4; word-break: normal; overflow-wrap: break-word; }}
  .pass-text {{ color: var(--accent-success); font-size: 0.875rem; padding: 1rem 1.25rem; background: var(--surface); border: 1px solid var(--border); border-radius: 0.5rem; }}
  .footer {{ text-align: center; padding: 1.5rem 0; color: var(--text-muted); font-size: 0.75rem; }}
  @media (max-width: 768px) {{
    .container {{ padding: 1rem; }}
    .header {{ flex-direction: column; align-items: flex-start; gap: 0.75rem; }}
    th, td {{ padding: 0.5rem 0.75rem; }}
  }}
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
    Generated by SqlGuard | <span id="timestamp">-</span>
  </div>
</div>
<script>
  (function() {{
    var d = new Date();
    var pad = function(n) {{ return n < 10 ? '0' + n : n; }};
    var s = d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) +
            ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes()) + ':' + pad(d.getSeconds()) +
            ' ' + Intl.DateTimeFormat().resolvedOptions().timeZone;
    var el = document.getElementById('timestamp');
    if (el) el.textContent = s;
  }})();
</script>
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
    )
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
