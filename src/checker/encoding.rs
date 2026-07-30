//! 文件级字节检查：编码格式与换行符。
//!
//! 与基于 SQL AST 的 Rhai 规则不同，本模块直接读取文件字节，
//! 检查两类与 SQL 语法无关的文件属性：
//! - **编码必须为 UTF-8 且不带 BOM**（rule_id `FILE001`，默认 `error` —— 必须）
//! - **换行符应为 LF**（rule_id `FILE002`，默认 `warning` —— 提示）
//!
//! 产出标准 [`Violation`]，复用现有 reporter；两条检查归入 `file-format` 分组，
//! 支持通过 `--exclude-rules FILE001` / `--exclude-groups file-format` 过滤。

use std::path::Path;

use crate::config::FileCheckConfig;
use crate::error::Violation;
use crate::rule::engine::RuleFilter;

/// 编码检查规则编号。
pub const RULE_ID_ENCODING: &str = "FILE001";
/// 编码检查规则名。
pub const RULE_NAME_ENCODING: &str = "file_encoding_utf8_no_bom";
/// 换行符检查规则编号。
pub const RULE_ID_LINE_ENDING: &str = "FILE002";
/// 换行符检查规则名。
pub const RULE_NAME_LINE_ENDING: &str = "file_line_ending_lf";
/// 文件格式检查统一分组名。
pub const RULE_GROUP: &str = "file-format";

/// 对单个文件做字节级检查，返回违规列表（可能为空）。
///
/// - `script_type` 仅用于报告展示（承袭该文件的分类结果）。
/// - `filter` 用于按 CLI 的 id / 分组过滤；文件级检查也遵守同一套过滤规则。
/// - 文件读取失败时返回空（后续读取文本流程会给出更明确的错误），不在此处报错。
pub fn check_file(
    path: &Path,
    script_type: &str,
    config: &FileCheckConfig,
    filter: &RuleFilter,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !config.enabled {
        return violations;
    }

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return violations,
    };

    // ===== 1. 编码：UTF-8 无 BOM（必须）=====
    if config.check_encoding && filter.matches_id_group(RULE_ID_ENCODING, Some(RULE_GROUP)) {
        if let Some(message) = detect_encoding_issue(&bytes) {
            violations.push(Violation {
                rule_id: RULE_ID_ENCODING.to_string(),
                rule_name: RULE_NAME_ENCODING.to_string(),
                rule_group: Some(RULE_GROUP.to_string()),
                severity: config.encoding_severity.clone(),
                message,
                file_path: path.to_path_buf(),
                script_type: script_type.to_string(),
                line: Some(1),
                end_line: None,
                column: None,
            });
        }
    }

    // ===== 2. 换行符：LF（提示）=====
    if config.check_line_ending && filter.matches_id_group(RULE_ID_LINE_ENDING, Some(RULE_GROUP)) {
        if let Some((line, message)) = detect_line_ending_issue(&bytes) {
            violations.push(Violation {
                rule_id: RULE_ID_LINE_ENDING.to_string(),
                rule_name: RULE_NAME_LINE_ENDING.to_string(),
                rule_group: Some(RULE_GROUP.to_string()),
                severity: config.line_ending_severity.clone(),
                message,
                file_path: path.to_path_buf(),
                script_type: script_type.to_string(),
                line: Some(line),
                end_line: None,
                column: None,
            });
        }
    }

    violations
}

/// 检查编码问题。返回 `Some(message)` 表示存在违规。
///
/// 规则：不允许任何 BOM；无 BOM 时内容必须是合法 UTF-8。
fn detect_encoding_issue(bytes: &[u8]) -> Option<String> {
    // UTF-8 BOM: EF BB BF
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(
            "File must be UTF-8 without BOM, but a UTF-8 BOM (EF BB BF) was found at the start of the file. Remove the BOM."
                .to_string(),
        );
    }
    // UTF-32 LE (FF FE 00 00) 必须先于 UTF-16 LE (FF FE) 判断，因前缀相同
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Some(
            "File must be UTF-8 without BOM, but a UTF-32 LE BOM was found. Re-encode the file as UTF-8."
                .to_string(),
        );
    }
    if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Some(
            "File must be UTF-8 without BOM, but a UTF-32 BE BOM was found. Re-encode the file as UTF-8."
                .to_string(),
        );
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some(
            "File must be UTF-8 without BOM, but a UTF-16 LE BOM was found. Re-encode the file as UTF-8."
                .to_string(),
        );
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some(
            "File must be UTF-8 without BOM, but a UTF-16 BE BOM was found. Re-encode the file as UTF-8."
                .to_string(),
        );
    }
    // 无 BOM：校验内容是否为合法 UTF-8
    if let Err(e) = std::str::from_utf8(bytes) {
        return Some(format!(
            "File is not valid UTF-8: invalid byte sequence at offset {}. Re-encode the file as UTF-8 (no BOM).",
            e.valid_up_to()
        ));
    }
    None
}

/// 检查换行符问题。返回 `Some((line, message))` 表示存在非 LF 换行。
///
/// 规则：应全部为 LF（`\n`）。检测 CRLF（`\r\n`）与单独的 CR（`\r`），
/// 报告首处非 LF 换行的行号，并统计总数以便定位。
fn detect_line_ending_issue(bytes: &[u8]) -> Option<(usize, String)> {
    let mut line: usize = 1;
    let mut first_bad_line: Option<usize> = None;
    let mut crlf_count: usize = 0;
    let mut cr_count: usize = 0;

    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                let is_crlf = bytes.get(i + 1) == Some(&b'\n');
                if is_crlf {
                    crlf_count += 1;
                    i += 2;
                } else {
                    cr_count += 1;
                    i += 1;
                }
                if first_bad_line.is_none() {
                    first_bad_line = Some(line);
                }
                line += 1;
            }
            b'\n' => {
                line += 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    let first = first_bad_line?;
    let kind = if crlf_count > 0 && cr_count > 0 {
        format!(
            "mixed CRLF ({}) and lone CR ({}) line endings",
            crlf_count, cr_count
        )
    } else if crlf_count > 0 {
        format!("CRLF line endings ({} line(s))", crlf_count)
    } else {
        format!("lone CR line endings ({} line(s))", cr_count)
    };
    Some((
        first,
        format!(
            "Line endings should be LF (\\n), but {} were found; first at line {}. Convert line endings to LF.",
            kind, first
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_utf8_lf_has_no_issue() {
        let bytes = b"SELECT 1;\nSELECT 2;\n";
        assert!(detect_encoding_issue(bytes).is_none());
        assert!(detect_line_ending_issue(bytes).is_none());
    }

    #[test]
    fn empty_file_has_no_issue() {
        let bytes: &[u8] = b"";
        assert!(detect_encoding_issue(bytes).is_none());
        assert!(detect_line_ending_issue(bytes).is_none());
    }

    #[test]
    fn utf8_bom_is_flagged() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"SELECT 1;\n");
        assert!(detect_encoding_issue(&bytes).is_some());
    }

    #[test]
    fn utf16_le_bom_is_flagged() {
        let bytes = [0xFF, 0xFE, 0x41, 0x00];
        assert!(detect_encoding_issue(&bytes).is_some());
    }

    #[test]
    fn utf32_le_bom_is_flagged_before_utf16() {
        let bytes = [0xFF, 0xFE, 0x00, 0x00];
        let msg = detect_encoding_issue(&bytes).unwrap();
        assert!(msg.contains("UTF-32 LE"), "got: {}", msg);
    }

    #[test]
    fn invalid_utf8_is_flagged() {
        // 0xFF 单独出现不是合法 UTF-8，且不构成任何 BOM 前缀
        let bytes = [0x53, 0x45, 0xFF, 0x4C];
        assert!(detect_encoding_issue(&bytes).is_some());
    }

    #[test]
    fn crlf_is_flagged_with_line() {
        let bytes = b"SELECT 1;\nSELECT 2;\r\nSELECT 3;\n";
        let (line, msg) = detect_line_ending_issue(bytes).unwrap();
        assert_eq!(line, 2);
        assert!(msg.contains("CRLF"), "got: {}", msg);
    }

    #[test]
    fn lone_cr_is_flagged() {
        let bytes = b"SELECT 1;\rSELECT 2;\n";
        let (line, msg) = detect_line_ending_issue(bytes).unwrap();
        assert_eq!(line, 1);
        assert!(msg.contains("CR"), "got: {}", msg);
    }
}
