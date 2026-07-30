use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum SqlGuardError {
    ConfigError(String),
    IoError(std::io::Error),
    /// ★ D2：规则执行错误变体。当前规则引擎错误统一走 CheckError，此变体保留用于
    /// 未来细粒度错误分类（按 rule_name 区分），是公开 API 契约的一部分。
    #[allow(dead_code)]
    RuleError {
        rule_name: String,
        message: String,
    },
    ScriptError(String),
    CheckError(String),
    MapperError(String),
}

impl std::fmt::Display for SqlGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SqlGuardError::ConfigError(msg) => write!(f, "Config error: {}", msg),
            SqlGuardError::IoError(err) => write!(f, "IO error: {}", err),
            SqlGuardError::RuleError { rule_name, message } => {
                write!(f, "Rule '{}' error: {}", rule_name, message)
            }
            SqlGuardError::ScriptError(msg) => write!(f, "Script error: {}", msg),
            SqlGuardError::CheckError(msg) => write!(f, "Check error: {}", msg),
            SqlGuardError::MapperError(msg) => write!(f, "Mapper error: {}", msg),
        }
    }
}

impl std::error::Error for SqlGuardError {}

impl From<std::io::Error> for SqlGuardError {
    fn from(err: std::io::Error) -> Self {
        SqlGuardError::IoError(err)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
    pub rule_id: String,
    pub rule_name: String,
    pub rule_group: Option<String>,
    pub severity: String,
    pub message: String,
    pub file_path: PathBuf,
    pub script_type: String,
    #[doc(hidden)]
    pub line: Option<usize>,
    /// 语句结束行（含）。`run_single_rule` 会自动从 AST 回填
    /// （找到包含 `line` 的语句范围），规则脚本无需关心。
    /// 增量校验时用于语句级范围交集判断。
    #[doc(hidden)]
    pub end_line: Option<usize>,
    #[doc(hidden)]
    pub column: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct DirectoryIssue {
    pub path: PathBuf,
    /// ★ D2：问题类型分类。当前 main.rs 仅按 path 列出问题，未读取此字段区分
    /// Missing/Unexpected 的展示文案，保留用于未来按类型分组的展示需求。
    #[allow(dead_code)]
    pub issue_type: DirectoryIssueType,
}

#[derive(Debug, Clone)]
pub enum DirectoryIssueType {
    Missing,
    Unexpected,
}
