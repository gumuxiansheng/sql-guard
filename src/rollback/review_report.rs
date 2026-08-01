//! `rollback-review-report.html` 生成器。
//!
//! 读 `Manifest`(已含 [`crate::rollback::review_level_for`] 推导的 review_level
//! + 汇总计数),产出纯静态 HTML,供 DBA 快速定位需重点复核的语句。
//!
//! 设计:
//! - **按 review_level 分区**:required(强制)置顶 → optional(提示)→ none(免审,默认折叠)
//! - **风险 badges**:从 SafetyClass 渲染 IRREVERSIBLE / UNRELIABLE / PARTIAL 等
//! - **SQL 三栏折叠**:original / backup / rollback 用 `<details>` 原生折叠
//! - **纯静态 HTML**:无外部依赖,离线可用
//!
//! 与 `reporter::html`(check violations 报告)独立,因数据模型不同
//! (rollback 用 Manifest/SafetyClass,check 用 Violation)。

use super::manifest::{Manifest, ManifestItem};
use super::SafetyClass;

/// 生成 `rollback-review-report.html` 全文。
///
/// 数据源:已构建的 `Manifest`(内存中,无需回读 JSON 文件)。
pub fn generate_review_report(manifest: &Manifest) -> String {
    let status_class = if manifest.review_required {
        "status-fail"
    } else {
        "status-pass"
    };
    let status_text = if manifest.review_required {
        "NEEDS REVIEW"
    } else {
        "ALL CLEAR"
    };

    let required_items: Vec<&ManifestItem> = manifest
        .items
        .iter()
        .filter(|i| i.review_level == "required")
        .collect();
    let optional_items: Vec<&ManifestItem> = manifest
        .items
        .iter()
        .filter(|i| i.review_level == "optional")
        .collect();
    let auto_items: Vec<&ManifestItem> = manifest
        .items
        .iter()
        .filter(|i| i.review_level == "none")
        .collect();

    let required_section = render_section(
        "⚠ 强制复核",
        "required",
        &required_items,
        /* default_open = */ true,
        "这些语句涉及不可逆/不可靠/部分回滚风险,发布平台将阻断执行,直到 review-manifest 覆盖通过。",
    );
    let optional_section = render_section(
        "◐ 提示性复核",
        "optional",
        &optional_items,
        true,
        "这些语句涉及持锁或计数器未还原,建议 DBA 确认但不阻断执行。",
    );
    let auto_section = render_section(
        "✓ 自动放行",
        "none",
        &auto_items,
        false,
        "这些语句可靠且无风险 flag,发布平台自动放行,无需复核。",
    );

    let dialect = escape_html(&manifest.dialect);
    let generated_at = escape_html(&manifest.generated_at);

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Rollback Review Report</title>
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
  .header-meta {{ font-size: 0.75rem; color: var(--text-muted); margin-top: 0.25rem; }}
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
  .section-header {{ display: flex; align-items: baseline; gap: 0.5rem; margin-bottom: 0.75rem; }}
  .section-header h2 {{ font-size: 0.875rem; font-weight: 600; color: var(--text); }}
  .section-header .count {{ font-size: 0.75rem; color: var(--text-muted); }}
  .section-desc {{ font-size: 0.75rem; color: var(--text-secondary); margin-bottom: 0.75rem; padding-left: 0.25rem; }}
  .item-card {{ background: var(--surface); border: 1px solid var(--border); border-radius: 0.5rem; margin-bottom: 0.75rem; overflow: hidden; }}
  .item-card.required {{ border-left: 3px solid var(--accent-error); }}
  .item-card.optional {{ border-left: 3px solid var(--accent-warning); }}
  .item-card.none {{ border-left: 3px solid var(--accent-success); }}
  .item-header {{ display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; padding: 0.625rem 1rem; border-bottom: 1px solid var(--border); }}
  .item-seq {{ font-family: 'SF Mono', Monaco, Consolas, monospace; font-size: 0.75rem; color: var(--text-muted); font-weight: 600; }}
  .item-kind {{ font-family: 'SF Mono', Monaco, Consolas, monospace; font-size: 0.6875rem; font-weight: 600; color: var(--accent-info); background: var(--accent-info-bg); padding: 0.125rem 0.5rem; border-radius: 0.25rem; }}
  .item-source {{ font-family: 'SF Mono', Monaco, Consolas, monospace; font-size: 0.75rem; color: var(--text-secondary); word-break: break-all; }}
  .badges {{ display: flex; flex-wrap: wrap; gap: 0.25rem; padding: 0.5rem 1rem; }}
  .badge {{ display: inline-flex; align-items: center; padding: 0.125rem 0.5rem; border-radius: 0.25rem; font-size: 0.6875rem; font-weight: 600; line-height: 1; }}
  .badge-error {{ background: var(--accent-error-bg); color: var(--accent-error); }}
  .badge-warning {{ background: var(--accent-warning-bg); color: var(--accent-warning); }}
  .badge-info {{ background: var(--accent-info-bg); color: var(--accent-info); }}
  .sql-block {{ padding: 0 1rem 0.5rem; }}
  .sql-block details {{ background: var(--surface-2); border: 1px solid var(--border); border-radius: 0.375rem; padding: 0.5rem 0.75rem; margin-top: 0.375rem; }}
  .sql-block summary {{ cursor: pointer; font-size: 0.6875rem; font-weight: 600; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.05em; user-select: none; }}
  .sql-block summary:hover {{ color: var(--text); }}
  .sql-block pre {{ margin-top: 0.5rem; font-family: 'SF Mono', Monaco, Consolas, monospace; font-size: 0.75rem; color: var(--text); white-space: pre-wrap; word-break: break-all; line-height: 1.5; }}
  .sql-block .sql-empty {{ margin-top: 0.5rem; font-size: 0.75rem; color: var(--text-muted); font-style: italic; }}
  .warnings {{ padding: 0.25rem 1rem 0.625rem; }}
  .warning-item {{ font-size: 0.75rem; color: var(--accent-warning); padding: 0.125rem 0; }}
  .warning-item::before {{ content: "⚠ "; }}
  .empty-section {{ padding: 1rem 1.25rem; font-size: 0.8125rem; color: var(--text-muted); font-style: italic; }}
  .footer {{ text-align: center; padding: 1.5rem 0; color: var(--text-muted); font-size: 0.75rem; }}
  @media (max-width: 768px) {{
    .container {{ padding: 1rem; }}
    .header {{ flex-direction: column; align-items: flex-start; gap: 0.75rem; }}
    .item-header {{ flex-direction: column; align-items: flex-start; }}
  }}
</style>
</head>
<body>
<div class="container">
  <div class="header">
    <div>
      <h1>Rollback Review Report</h1>
      <div class="header-meta">Dialect: {dialect} · Generated: {generated_at}</div>
    </div>
    <div class="status {status_class}">{status_text}</div>
  </div>
  <div class="summary">
    <div class="summary-card">
      <div class="value value-blue">{source_count}</div>
      <div class="label">Total Statements</div>
    </div>
    <div class="summary-card">
      <div class="value value-red">{required_count}</div>
      <div class="label">Required Review</div>
    </div>
    <div class="summary-card">
      <div class="value value-yellow">{optional_count}</div>
      <div class="label">Optional Review</div>
    </div>
    <div class="summary-card">
      <div class="value value-green">{auto_count}</div>
      <div class="label">Auto Approved</div>
    </div>
  </div>
  {required_section}
  {optional_section}
  {auto_section}
  <div class="footer">
    Generated by SqlGuard gen-rollback · Review manifest for details
  </div>
</div>
</body>
</html>"#,
        dialect = dialect,
        generated_at = generated_at,
        status_class = status_class,
        status_text = status_text,
        source_count = manifest.source_count,
        required_count = manifest.required_review_count,
        optional_count = manifest.optional_review_count,
        auto_count = manifest.auto_approved_count,
        required_section = required_section,
        optional_section = optional_section,
        auto_section = auto_section,
    )
}

/// 渲染一个 review_level 分区。
fn render_section(
    title: &str,
    level_class: &str,
    items: &[&ManifestItem],
    default_open: bool,
    desc: &str,
) -> String {
    if items.is_empty() {
        return format!(
            r#"<div class="section">
  <div class="section-header"><h2>{title}</h2><span class="count">(0)</span></div>
  <div class="empty-section">No items in this category.</div>
</div>"#,
            title = escape_html(title),
        );
    }

    let cards: String = items
        .iter()
        .map(|i| render_item_card(i, level_class))
        .collect();

    // none 分区用 <details> 折叠,减少视觉噪音
    if default_open {
        format!(
            r#"<div class="section">
  <div class="section-header"><h2>{title}</h2><span class="count">({count})</span></div>
  <div class="section-desc">{desc}</div>
  {cards}
</div>"#,
            title = escape_html(title),
            count = items.len(),
            desc = escape_html(desc),
            cards = cards,
        )
    } else {
        format!(
            r#"<div class="section">
  <details>
    <summary><h2 style="display:inline;font-size:0.875rem;font-weight:600;color:var(--text);">{title} ({count})</h2></summary>
    <div class="section-desc" style="margin-top:0.5rem;">{desc}</div>
    {cards}
  </details>
</div>"#,
            title = escape_html(title),
            count = items.len(),
            desc = escape_html(desc),
            cards = cards,
        )
    }
}

/// 渲染单个语句卡片。
fn render_item_card(item: &ManifestItem, level_class: &str) -> String {
    let kind_display = if item.stmt_kind.is_empty() {
        "SKIP"
    } else {
        &item.stmt_kind
    };
    let source_loc = render_source_loc(item);
    let badges = render_risk_badges(&item.safety);

    let original_block = render_sql_block("Original", Some(&item.original_sql));
    let backup_block = render_sql_block("Backup", item.backup.as_deref());
    let rollback_block = render_sql_block("Rollback", item.rollback.as_deref());

    let warnings_html = if item.warnings.is_empty() {
        String::new()
    } else {
        let items: String = item
            .warnings
            .iter()
            .map(|w| format!(r#"<div class="warning-item">{}</div>"#, escape_html(w)))
            .collect();
        format!(r#"<div class="warnings">{}</div>"#, items)
    };

    let badges_html = if badges.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="badges">{}</div>"#, badges)
    };

    format!(
        r#"<div class="item-card {level_class}">
  <div class="item-header">
    <span class="item-seq">#{seq}</span>
    <span class="item-kind">{kind}</span>
    <span class="item-source">{source_loc}</span>
  </div>
  {badges_html}
  <div class="sql-block">
    {original_block}
    {backup_block}
    {rollback_block}
  </div>
  {warnings_html}
</div>"#,
        level_class = level_class,
        seq = item.seq,
        kind = escape_html(kind_display),
        source_loc = source_loc,
        badges_html = badges_html,
        original_block = original_block,
        backup_block = backup_block,
        rollback_block = rollback_block,
        warnings_html = warnings_html,
    )
}

/// 渲染来源位置:`file:line` (+ mapper statement_id)。
fn render_source_loc(item: &ManifestItem) -> String {
    let file = escape_html(&item.source.file);
    let line = item.source.line;
    let base = if line > 0 {
        format!("{}:{}", file, line)
    } else {
        file
    };
    if let Some(sid) = &item.source.statement_id {
        format!(
            r#"{} <span style="color:var(--text-muted);">(mapper: {})</span>"#,
            base,
            escape_html(sid)
        )
    } else {
        base
    }
}

/// 渲染风险 badges(从 SafetyClass 的 true flag 生成)。
fn render_risk_badges(safety: &SafetyClass) -> String {
    let mut badges = Vec::new();
    if safety.irreversible {
        badges.push(r#"<span class="badge badge-error">IRREVERSIBLE</span>"#.to_string());
    }
    if safety.irreversible_if_backup_missing {
        badges.push(
            r#"<span class="badge badge-error">IRREVERSIBLE_IF_BACKUP_MISSING</span>"#.to_string(),
        );
    }
    if !safety.reliable {
        badges.push(r#"<span class="badge badge-error">UNRELIABLE</span>"#.to_string());
    }
    if safety.partial {
        badges.push(r#"<span class="badge badge-warning">PARTIAL</span>"#.to_string());
    }
    if safety.requires_lock {
        let lock_type = safety.lock_type.as_deref().unwrap_or("LOCK");
        badges.push(format!(
            r#"<span class="badge badge-warning">REQUIRES_LOCK: {}</span>"#,
            escape_html(lock_type)
        ));
    }
    if safety.counter_unrestored {
        badges.push(r#"<span class="badge badge-warning">COUNTER_UNRESTORED</span>"#.to_string());
    }
    if safety.partitioned {
        badges.push(r#"<span class="badge badge-info">PARTITIONED</span>"#.to_string());
    }
    badges.join("\n    ")
}

/// 渲染 SQL 折叠块。`sql=None` 时显示"(none)"提示。
fn render_sql_block(label: &str, sql: Option<&str>) -> String {
    match sql {
        Some(s) if !s.trim().is_empty() => format!(
            r#"<details><summary>{label}</summary><pre>{content}</pre></details>"#,
            label = escape_html(label),
            content = escape_html(s),
        ),
        _ => format!(
            r#"<details><summary>{label}</summary><div class="sql-empty">(none)</div></details>"#,
            label = escape_html(label),
        ),
    }
}

/// HTML 特殊字符转义。
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RollbackConfig;
    use crate::rollback::{BackupRollbackPair, BackupStrategy, SourceRef};

    fn make_manifest_item(
        seq: u64,
        stmt_kind: &str,
        reliable: bool,
        irreversible: bool,
        requires_lock: bool,
    ) -> ManifestItem {
        ManifestItem {
            seq,
            source: SourceRef {
                file: format!("src/{}.sql", seq),
                line: seq as i64 * 10,
                end_line: seq as i64 * 10,
                statement_id: None,
                variant_label: None,
            },
            original_sql: format!("-- original SQL #{}", seq),
            stmt_kind: stmt_kind.to_string(),
            backup: Some(format!("-- backup #{}", seq)),
            rollback: Some(format!("-- rollback #{}", seq)),
            review_level: if irreversible || !reliable {
                "required"
            } else if requires_lock {
                "optional"
            } else {
                "none"
            },
            safety: SafetyClass {
                reliable,
                irreversible,
                requires_lock,
                lock_type: if requires_lock {
                    Some("FTWRL".to_string())
                } else {
                    None
                },
                ..SafetyClass::default()
            },
            strategy: BackupStrategy::default(),
            expected_schema: None,
            warnings: vec![],
        }
    }

    fn make_manifest_with_items(items: Vec<ManifestItem>) -> Manifest {
        let required = items
            .iter()
            .filter(|i| i.review_level == "required")
            .count();
        let optional = items
            .iter()
            .filter(|i| i.review_level == "optional")
            .count();
        let auto = items.iter().filter(|i| i.review_level == "none").count();
        Manifest {
            version: 1,
            generator: "sqlguard test".to_string(),
            generated_at: "2026-07-31T16:00:00Z".to_string(),
            dialect: "mysql".to_string(),
            source_count: items.len(),
            backup_count: items.iter().filter(|i| i.backup.is_some()).count(),
            rollback_count: items.iter().filter(|i| i.rollback.is_some()).count(),
            unreliable_count: items.iter().filter(|i| !i.safety.reliable).count(),
            irreversible_count: items.iter().filter(|i| i.safety.irreversible).count(),
            partial_count: 0,
            counter_unrestored_count: 0,
            requires_lock_count: optional,
            partitioned_count: 0,
            irreversible_if_backup_missing_count: 0,
            assert_on_schema_mismatch: "abort".to_string(),
            on_partitioned_table: "warn".to_string(),
            review_required: required > 0,
            required_review_count: required,
            optional_review_count: optional,
            auto_approved_count: auto,
            items,
            warnings: vec![],
        }
    }

    #[test]
    fn escape_html_escapes_all_special_chars() {
        assert_eq!(escape_html("&"), "&amp;");
        assert_eq!(escape_html("<"), "&lt;");
        assert_eq!(escape_html(">"), "&gt;");
        assert_eq!(escape_html("\""), "&quot;");
        assert_eq!(escape_html("'"), "&#39;");
        assert_eq!(escape_html("&<>\"'"), "&amp;&lt;&gt;&quot;&#39;");
    }

    #[test]
    fn report_shows_needs_review_when_required_exists() {
        let m = make_manifest_with_items(vec![
            make_manifest_item(1, "INSERT", true, false, false),
            make_manifest_item(2, "DROP_TABLE", true, true, false),
        ]);
        let html = generate_review_report(&m);
        assert!(html.contains("NEEDS REVIEW"));
        assert!(html.contains("status-fail"));
        assert!(html.contains("Required Review"));
        // required 项的 seq 应出现在强制复核区
        assert!(html.contains("#2"));
        assert!(html.contains("DROP_TABLE"));
    }

    #[test]
    fn report_shows_all_clear_when_no_required() {
        let m = make_manifest_with_items(vec![
            make_manifest_item(1, "INSERT", true, false, false),
            make_manifest_item(2, "UPDATE", true, false, true),
        ]);
        let html = generate_review_report(&m);
        assert!(html.contains("ALL CLEAR"));
        assert!(html.contains("status-pass"));
    }

    #[test]
    fn report_includes_summary_counts() {
        let m = make_manifest_with_items(vec![
            make_manifest_item(1, "INSERT", true, false, false), // none
            make_manifest_item(2, "UPDATE", true, false, true),  // optional
            make_manifest_item(3, "DROP_TABLE", true, true, false), // required
        ]);
        let html = generate_review_report(&m);
        assert!(html.contains("value-red\">1</div>"));
        assert!(html.contains("value-yellow\">1</div>"));
        assert!(html.contains("value-green\">1</div>"));
        assert!(html.contains("value-blue\">3</div>"));
    }

    #[test]
    fn report_includes_risk_badges() {
        let m =
            make_manifest_with_items(vec![make_manifest_item(1, "DROP_TABLE", true, true, false)]);
        let html = generate_review_report(&m);
        assert!(html.contains("IRREVERSIBLE"));
    }

    #[test]
    fn report_includes_lock_type_in_badge() {
        let m = make_manifest_with_items(vec![make_manifest_item(1, "UPDATE", true, false, true)]);
        let html = generate_review_report(&m);
        assert!(html.contains("REQUIRES_LOCK"));
        assert!(html.contains("FTWRL"));
    }

    #[test]
    fn report_includes_sql_blocks() {
        let m = make_manifest_with_items(vec![make_manifest_item(1, "INSERT", true, false, false)]);
        let html = generate_review_report(&m);
        assert!(html.contains("Original"));
        assert!(html.contains("Backup"));
        assert!(html.contains("Rollback"));
        assert!(html.contains("-- original SQL #1"));
        assert!(html.contains("-- backup #1"));
        assert!(html.contains("-- rollback #1"));
    }

    #[test]
    fn report_shows_none_for_missing_backup_rollback() {
        let mut item = make_manifest_item(1, "MERGE", true, true, false);
        item.backup = None;
        item.rollback = None;
        let m = make_manifest_with_items(vec![item]);
        let html = generate_review_report(&m);
        assert!(html.contains("(none)"));
    }

    #[test]
    fn report_includes_warnings() {
        let mut item = make_manifest_item(1, "DROP_TABLE", true, true, false);
        item.warnings = vec!["bks_ 表必须存在".to_string()];
        let m = make_manifest_with_items(vec![item]);
        let html = generate_review_report(&m);
        assert!(html.contains("warning-item"));
        assert!(html.contains("bks_ 表必须存在"));
    }

    #[test]
    fn report_includes_source_location() {
        let m = make_manifest_with_items(vec![make_manifest_item(1, "INSERT", true, false, false)]);
        let html = generate_review_report(&m);
        assert!(html.contains("src/1.sql:10"));
    }

    #[test]
    fn report_includes_mapper_statement_id() {
        let mut item = make_manifest_item(1, "INSERT", true, false, false);
        item.source.statement_id = Some("insertUser".to_string());
        let m = make_manifest_with_items(vec![item]);
        let html = generate_review_report(&m);
        assert!(html.contains("mapper: insertUser"));
    }

    #[test]
    fn report_auto_section_collapsed_by_default() {
        // none 分区应使用 <details> 折叠(无 open 属性)
        let m = make_manifest_with_items(vec![
            make_manifest_item(1, "INSERT", true, false, false), // none
            make_manifest_item(2, "DROP_TABLE", true, true, false), // required
        ]);
        let html = generate_review_report(&m);
        // 自动放行区应折叠
        assert!(html.contains("自动放行"));
    }

    #[test]
    fn report_empty_section_shows_placeholder() {
        // 没有 optional 项时应显示 "No items in this category"
        let m = make_manifest_with_items(vec![
            make_manifest_item(1, "INSERT", true, false, false), // none
        ]);
        let html = generate_review_report(&m);
        assert!(html.contains("No items in this category"));
    }

    #[test]
    fn report_dialect_and_timestamp_in_header() {
        let m = make_manifest_with_items(vec![make_manifest_item(1, "INSERT", true, false, false)]);
        let html = generate_review_report(&m);
        assert!(html.contains("Dialect: mysql"));
        assert!(html.contains("2026-07-31T16:00:00Z"));
    }

    #[test]
    fn report_escapes_html_in_sql_content() {
        let mut item = make_manifest_item(1, "INSERT", true, false, false);
        item.original_sql = "INSERT INTO t VALUES ('<script>alert(1)</script>')".to_string();
        let m = make_manifest_with_items(vec![item]);
        let html = generate_review_report(&m);
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn render_risk_badges_empty_for_clean_safety() {
        let safety = SafetyClass {
            reliable: true,
            ..SafetyClass::default()
        };
        assert_eq!(render_risk_badges(&safety), "");
    }

    #[test]
    fn render_risk_badges_all_flags() {
        let safety = SafetyClass {
            reliable: false,
            irreversible: true,
            irreversible_if_backup_missing: true,
            partial: true,
            requires_lock: true,
            lock_type: Some("FTWRL".to_string()),
            counter_unrestored: true,
            partitioned: true,
            ..SafetyClass::default()
        };
        let badges = render_risk_badges(&safety);
        assert!(badges.contains("IRREVERSIBLE"));
        assert!(badges.contains("IRREVERSIBLE_IF_BACKUP_MISSING"));
        assert!(badges.contains("UNRELIABLE"));
        assert!(badges.contains("PARTIAL"));
        assert!(badges.contains("REQUIRES_LOCK: FTWRL"));
        assert!(badges.contains("COUNTER_UNRESTORED"));
        assert!(badges.contains("PARTITIONED"));
    }

    #[test]
    fn render_sql_block_none_for_empty_string() {
        let block = render_sql_block("Backup", Some(""));
        assert!(block.contains("(none)"));
    }

    #[test]
    fn render_sql_block_none_for_whitespace_only() {
        let block = render_sql_block("Backup", Some("   \n  "));
        assert!(block.contains("(none)"));
    }

    // ===== 集成测试:从 BackupRollbackPair 端到端生成报告 =====

    #[test]
    fn end_to_end_from_pairs_to_html() {
        use crate::rollback::Manifest;
        let pairs = vec![
            BackupRollbackPair {
                seq: 1,
                stmt_kind: "INSERT".to_string(),
                source: SourceRef::placeholder(),
                original_sql: "INSERT INTO users VALUES (1, 'alice')".to_string(),
                backup: Some("CREATE TABLE bks_users_...".to_string()),
                rollback: Some("DELETE FROM users WHERE id = 1".to_string()),
                safety: SafetyClass {
                    reliable: true,
                    ..SafetyClass::default()
                },
                strategy: BackupStrategy::default(),
                expected_schema: None,
                warnings: vec![],
            },
            BackupRollbackPair {
                seq: 2,
                stmt_kind: "DROP_TABLE".to_string(),
                source: SourceRef::placeholder(),
                original_sql: "DROP TABLE orders".to_string(),
                backup: Some("CREATE TABLE bks_orders_...".to_string()),
                rollback: Some("RENAME TABLE bks_orders_ TO orders".to_string()),
                safety: SafetyClass {
                    reliable: true,
                    irreversible: true,
                    ..SafetyClass::default()
                },
                strategy: BackupStrategy::default(),
                expected_schema: None,
                warnings: vec!["DROP TABLE 不可逆,bks_ 表必须存在".to_string()],
            },
        ];
        let manifest = Manifest::from_pairs(&pairs, "mysql", &RollbackConfig::default(), vec![]);
        let html = generate_review_report(&manifest);

        // 状态
        assert!(html.contains("NEEDS REVIEW"));
        // 汇总
        assert!(html.contains("value-red\">1</div>")); // required
        assert!(html.contains("value-green\">1</div>")); // auto
                                                         // required 项
        assert!(html.contains("#2"));
        assert!(html.contains("DROP_TABLE"));
        assert!(html.contains("IRREVERSIBLE"));
        assert!(html.contains("DROP TABLE 不可逆"));
        // SQL 内容
        assert!(html.contains("DROP TABLE orders"));
        assert!(html.contains("RENAME TABLE bks_orders_ TO orders"));
    }
}
