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
    /// MyBatis Mapper 模式配置。缺失或 `enabled = false` 时完全保持现有行为。
    #[serde(default)]
    pub mapper: MapperConfig,
    /// 文件扫描行为配置。控制白名单扫描根与黑名单跳过目录。
    /// 缺省时 exclude_dirs 生效（默认跳过 .git/target/node_modules 等），
    /// paths 为空时回退到 structure.paths，仍为空则扫描整个 target_dir。
    #[serde(default)]
    pub scan: ScanConfig,
    /// 文件格式检查配置（编码 / 换行符）。缺省时默认启用：
    /// UTF-8 无 BOM（error，必须）+ 换行符 LF（warning，提示）。
    #[serde(default)]
    pub file_check: FileCheckConfig,
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

/// MyBatis Mapper 模式配置。`enabled = false` 时其余字段被忽略。
#[derive(Debug, Deserialize, Clone)]
pub struct MapperConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_mapper_paths")]
    pub paths: Vec<String>,
    #[serde(default = "default_mapper_patterns")]
    pub patterns: Vec<String>,
}

impl Default for MapperConfig {
    fn default() -> Self {
        MapperConfig {
            enabled: false,
            paths: default_mapper_paths(),
            patterns: default_mapper_patterns(),
        }
    }
}

fn default_mapper_paths() -> Vec<String> {
    vec!["src/main/resources/mapper".to_string()]
}

fn default_mapper_patterns() -> Vec<String> {
    vec!["**/*Mapper.xml".to_string(), "**/*.xml".to_string()]
}

/// 文件扫描行为配置。
///
/// - `paths`：SQL 脚本扫描白名单。为空时回退到 `structure.paths`，
///   仍为空则扫描整个 `target_dir`（兜底，保持向后兼容）。
/// - `exclude_dirs`：递归扫描时跳过的目录名黑名单，适用于 SQL 扫描、
///   Mapper XML 扫描、目录结构校验三个场景。默认包含版本控制与构建产物目录。
#[derive(Debug, Deserialize, Clone)]
pub struct ScanConfig {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_exclude_dirs")]
    pub exclude_dirs: Vec<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            paths: Vec::new(),
            exclude_dirs: default_exclude_dirs(),
        }
    }
}

/// 默认跳过的目录名黑名单：版本控制元数据 + 常见构建产物 / IDE 配置。
/// 注意：按目录名匹配（非路径），任意层级中遇到同名目录都会跳过。
fn default_exclude_dirs() -> Vec<String> {
    vec![
        // 版本控制
        ".git".to_string(),
        ".svn".to_string(),
        ".hg".to_string(),
        ".bzr".to_string(),
        // Rust / Node / Java 构建产物
        "target".to_string(),
        "node_modules".to_string(),
        "build".to_string(),
        "dist".to_string(),
        "out".to_string(),
        // IDE
        ".idea".to_string(),
        ".vscode".to_string(),
    ]
}

/// 文件格式检查配置（编码 / 换行符），对扫描到的每个文件做字节级检查。
///
/// 与基于 SQL AST 的规则不同，这些检查关注与 SQL 语法无关的文件属性：
/// - `check_encoding`：编码必须为 UTF-8 且不带 BOM（对应 rule_id `FILE001`）。
/// - `check_line_ending`：换行符应为 LF（对应 rule_id `FILE002`）。
///
/// 严重级别可配置，默认编码违规为 `error`（必须），换行符违规为 `warning`（提示）。
/// 两条检查均归入 `file-format` 分组，可用 `--exclude-groups file-format` 临时关闭。
#[derive(Debug, Deserialize, Clone)]
pub struct FileCheckConfig {
    /// 总开关。`false` 时完全跳过文件格式检查。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 是否检查编码为 UTF-8 无 BOM（FILE001）。
    #[serde(default = "default_true")]
    pub check_encoding: bool,
    /// 是否检查换行符为 LF（FILE002）。
    #[serde(default = "default_true")]
    pub check_line_ending: bool,
    /// 编码违规的严重级别，默认 `error`（必须）。
    #[serde(default = "default_encoding_severity")]
    pub encoding_severity: String,
    /// 换行符违规的严重级别，默认 `warning`（提示）。
    #[serde(default = "default_line_ending_severity")]
    pub line_ending_severity: String,
}

impl Default for FileCheckConfig {
    fn default() -> Self {
        FileCheckConfig {
            enabled: true,
            check_encoding: true,
            check_line_ending: true,
            encoding_severity: default_encoding_severity(),
            line_ending_severity: default_line_ending_severity(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_encoding_severity() -> String {
    "error".to_string()
}

fn default_line_ending_severity() -> String {
    "warning".to_string()
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
