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
    pub name: String,
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
        toml::from_str(&content)
            .map_err(|e| SqlGuardError::ConfigError(format!("Failed to parse config file '{}': {}", path.display(), e)))
    }

    pub fn resolve_script_path(&self, script_path: &Path, config_dir: &Path) -> PathBuf {
        if script_path.is_absolute() {
            script_path.to_path_buf()
        } else {
            config_dir.join(script_path)
        }
    }
}
