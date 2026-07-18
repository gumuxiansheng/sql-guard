use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::fs;

use crate::error::SqlGuardError;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub structure: StructureConfig,
    pub classification: ClassificationConfig,
    pub rules: Vec<RuleConfig>,
    #[serde(default)]
    pub output: OutputConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StructureConfig {
    pub paths: Vec<String>,
    #[serde(default = "default_strict")]
    pub strict: bool,
    #[serde(default)]
    pub allow_extra: Vec<String>,
}

fn default_strict() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
pub struct ClassificationConfig {
    pub rules: Vec<ClassificationRule>,
    #[serde(default = "default_script_type")]
    pub default_type: String,
}

fn default_script_type() -> String {
    "other".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct ClassificationRule {
    pub name: String,
    pub pattern: String,
    #[serde(rename = "type")]
    pub script_type: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_priority() -> i32 {
    0
}

#[derive(Debug, Deserialize, Clone)]
pub struct RuleConfig {
    /// 规则编号，必填且全局唯一。CLI 通过 id 引用规则。
    pub id: String,
    pub name: String,
    /// 规则分组，可选。CLI 通过 --groups 引用分组。
    #[serde(default)]
    pub group: Option<String>,
    pub description: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub script_path: PathBuf,
    pub applies_to: Vec<String>,
    #[serde(default = "default_severity")]
    pub severity: String,
}

fn default_enabled() -> bool {
    true
}

fn default_severity() -> String {
    "error".to_string()
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct OutputConfig {
    #[serde(default = "default_formats")]
    pub formats: Vec<String>,
    pub output_dir: Option<PathBuf>,
}

fn default_formats() -> Vec<String> {
    vec!["plain".to_string(), "json".to_string(), "html".to_string()]
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, SqlGuardError> {
        let content = fs::read_to_string(path)
            .map_err(|e| SqlGuardError::ConfigError(format!("Failed to read config file '{}': {}", path.display(), e)))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| SqlGuardError::ConfigError(format!("Failed to parse config file '{}': {}", path.display(), e)))?;
        config.validate_rule_ids()?;
        Ok(config)
    }

    /// 校验规则 id 必填且全局唯一。
    fn validate_rule_ids(&self) -> Result<(), SqlGuardError> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for rule in &self.rules {
            if rule.id.trim().is_empty() {
                return Err(SqlGuardError::ConfigError(format!(
                    "Rule '{}' is missing required field 'id'",
                    rule.name
                )));
            }
            if !seen.insert(rule.id.as_str()) {
                return Err(SqlGuardError::ConfigError(format!(
                    "Duplicate rule id '{}' (rule name '{}')",
                    rule.id, rule.name
                )));
            }
        }
        Ok(())
    }

    pub fn resolve_script_path(&self, script_path: &Path, config_dir: &Path) -> PathBuf {
        if script_path.is_absolute() {
            script_path.to_path_buf()
        } else {
            config_dir.join(script_path)
        }
    }
}
