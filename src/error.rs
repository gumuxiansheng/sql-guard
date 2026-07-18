use std::path::PathBuf;

#[derive(Debug)]
pub enum SqlGuardError {
    ConfigError(String),
    IoError(std::io::Error),
    RuleError { rule_name: String, message: String },
    ScriptError(String),
    CheckError(String),
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
        }
    }
}

impl std::error::Error for SqlGuardError {}

impl From<std::io::Error> for SqlGuardError {
    fn from(err: std::io::Error) -> Self {
        SqlGuardError::IoError(err)
    }
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub rule_name: String,
    pub severity: String,
    pub message: String,
    pub file_path: PathBuf,
    pub script_type: String,
}

#[derive(Debug, Clone)]
pub struct DirectoryIssue {
    pub path: PathBuf,
    pub issue_type: DirectoryIssueType,
}

#[derive(Debug, Clone)]
pub enum DirectoryIssueType {
    Missing,
    Unexpected,
}
