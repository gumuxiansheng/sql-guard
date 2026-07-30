pub mod html;
pub mod json;
pub mod plain;
pub mod sarif;

use std::path::Path;

use crate::error::{DirectoryIssue, Violation};

/// 报告格式 trait。
///
/// 每种格式（plain / json / html / sarif）实现此 trait，
/// 由 [`output_reports`] 统一调度。新增格式只需实现 trait 并在
/// [`get_reporter`] 中注册，无需修改 `main.rs` 的调度逻辑。
pub trait Reporter {
    /// 格式名（与 `sqlguard.toml` 的 `formats` 配置项对应，如 `"json"`）。
    fn name(&self) -> &str;

    /// 是否需要写入文件（plain 返回 false，json/html 返回 true）。
    fn needs_file_output(&self) -> bool;

    /// 生成报告内容。
    fn generate(
        &self,
        violations: &[Violation],
        missing: &[DirectoryIssue],
        unexpected: &[DirectoryIssue],
        files_checked: usize,
    ) -> String;
}

/// 按格式名返回对应的 Reporter 实例。
///
/// 新增格式时在此处注册即可，调用方（main.rs）无需改动。
pub fn get_reporter(fmt: &str) -> Option<Box<dyn Reporter>> {
    match fmt {
        "plain" => Some(Box::new(plain::PlainReporter)),
        "json" => Some(Box::new(json::JsonReporter)),
        "html" => Some(Box::new(html::HtmlReporter)),
        "sarif" => Some(Box::new(sarif::SarifReporter)),
        _ => None,
    }
}

/// 遍历 formats 列表，逐个调用对应 Reporter 生成报告。
///
/// - `needs_file_output() == false` 的格式（如 plain）直接打印到 stdout；
/// - `needs_file_output() == true` 的格式写入 `output_dir/sqlguard-report.<ext>`。
pub fn output_reports(
    violations: &[Violation],
    missing: &[DirectoryIssue],
    unexpected: &[DirectoryIssue],
    files_checked: usize,
    formats: &[&str],
    output_dir: &Path,
) -> Result<(), String> {
    // 收集需要写文件的格式，据此决定是否创建输出目录
    let needs_dir = formats
        .iter()
        .filter_map(|f| get_reporter(f))
        .any(|r| r.needs_file_output());
    if needs_dir && !output_dir.exists() {
        std::fs::create_dir_all(output_dir)
            .map_err(|e| format!("Failed to create output dir: {}", e))?;
    }

    for fmt in formats {
        let reporter = match get_reporter(fmt) {
            Some(r) => r,
            None => {
                eprintln!("Unknown format: {}", fmt);
                continue;
            }
        };

        let content = reporter.generate(violations, missing, unexpected, files_checked);

        if reporter.needs_file_output() {
            let ext = reporter.name();
            let report_path = output_dir.join(format!("sqlguard-report.{}", ext));
            std::fs::write(&report_path, &content)
                .map_err(|e| format!("Failed to write {} report: {}", ext, e))?;
            eprintln!(
                "{} report saved: {}",
                ext.to_uppercase(),
                report_path.display()
            );
        } else {
            println!("{}", content);
        }
    }
    Ok(())
}
