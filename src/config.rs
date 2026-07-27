use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::fs;

use crate::error::SqlGuardError;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub structure: StructureConfig,
    pub classification: ClassificationConfig,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
    /// 可选：外部规则文件路径。指定时从该文件加载规则（覆盖内联 `[[rules]]`）。
    /// 相对路径基于主配置文件所在目录解析。
    #[serde(default)]
    pub rules_file: Option<PathBuf>,
    /// 规则脚本相对路径的解析基准目录（规则文件所在目录）。
    /// 不序列化，加载时根据规则来源设置；为空时回退到主配置目录（config_dir）。
    #[serde(skip)]
    pub rules_dir: PathBuf,
    /// ★ P2-4：输出配置，当前未被 check 子命令消费（输出由 CLI flag 控制），保留用于未来扩展。
    #[serde(default)]
    #[allow(dead_code)]
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
    /// 回滚脚本生成配置。缺省时使用 RollbackConfig::default()（不启用）。
    #[serde(default)]
    pub rollback: RollbackConfig,
    /// 文件级缓存配置（P2-8）。缺省时禁用——大仓库重复 check 时可启用提速。
    #[serde(default)]
    pub cache: CacheConfig,
    /// check 流程使用的 SQL 方言。缺省为 `generic`（兼容旧行为）。
    /// 影响 sqlparser 解析：mysql 方言支持 `INSERT IGNORE` / `ON DUPLICATE KEY UPDATE` 等
    /// MySQL 专有语法；postgresql 方言支持 PG 扩展语法。
    #[serde(default)]
    pub dialect: CheckDialect,
}

/// check 流程的 SQL 方言选择。
///
/// - `Generic`：默认，兼容大多数标准 SQL（向后兼容）
/// - `MySql`：支持 MySQL 专有语法（INSERT IGNORE / ON DUPLICATE KEY UPDATE / 反引号标识符等）
/// - `PostgreSql`：支持 PostgreSQL 扩展语法
/// - `Ansi`：严格 ANSI SQL
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckDialect {
    Generic,
    MySql,
    PostgreSql,
    Ansi,
}

impl Default for CheckDialect {
    fn default() -> Self {
        CheckDialect::Generic
    }
}

impl CheckDialect {
    /// 从字符串解析方言（用于 CLI `--dialect` 覆盖配置）。
    /// 不区分大小写，未知值回退到 Generic。
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "mysql" => CheckDialect::MySql,
            "postgres" | "postgresql" | "pg" => CheckDialect::PostgreSql,
            "ansi" => CheckDialect::Ansi,
            _ => CheckDialect::Generic,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CheckDialect::Generic => "generic",
            CheckDialect::MySql => "mysql",
            CheckDialect::PostgreSql => "postgresql",
            CheckDialect::Ansi => "ansi",
        }
    }
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
    /// ★ P2-4：配置元数据字段，仅用于配置文件可读性，代码未消费。
    #[allow(dead_code)]
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
    /// ★ P2-4：配置元数据字段，仅用于配置文件可读性，代码未消费。
    #[allow(dead_code)]
    pub name: String,
    /// 规则分组，可选。CLI 通过 --groups 引用分组。
    #[serde(default)]
    pub group: Option<String>,
    /// ★ P2-4：配置元数据字段，仅用于配置文件可读性，代码未消费。
    #[allow(dead_code)]
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

/// ★ P2-4：整个 OutputConfig 当前未被 check 子命令消费（输出由 CLI flag 控制），
/// 保留用于未来"配置文件指定输出格式/目录"的扩展。配置层兼容字段。
#[derive(Debug, Default, Deserialize, Clone)]
#[allow(dead_code)]
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
    /// MyBatis 语句标签 → SqlGuard script_type 的映射。
    /// 缺省时使用 `default_statement_type_mapping()`（select/insert/update/delete → "dml"），
    /// 保持向后兼容。配置示例：
    /// ```toml
    /// [mapper.statement_type_mapping]
    /// select = "query"
    /// insert = "dml"
    /// update = "dml"
    /// delete = "dml"
    /// ```
    #[serde(default = "default_statement_type_mapping")]
    pub statement_type_mapping: std::collections::HashMap<String, String>,
}

impl Default for MapperConfig {
    fn default() -> Self {
        MapperConfig {
            enabled: false,
            paths: default_mapper_paths(),
            patterns: default_mapper_patterns(),
            statement_type_mapping: default_statement_type_mapping(),
        }
    }
}

/// 默认 statement_type 映射：所有 MyBatis 语句标签（select/insert/update/delete）→ "dml"。
/// 与 P0 硬编码行为一致，保持向后兼容。
fn default_statement_type_mapping() -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    m.insert("select".to_string(), "dml".to_string());
    m.insert("insert".to_string(), "dml".to_string());
    m.insert("update".to_string(), "dml".to_string());
    m.insert("delete".to_string(), "dml".to_string());
    m
}

fn default_mapper_paths() -> Vec<String> {
    vec!["src/main/resources/mapper".to_string()]
}

fn default_mapper_patterns() -> Vec<String> {
    // 仅匹配以 Mapper.xml 结尾的文件，避免误匹配 pom.xml / web.xml 等非 MyBatis 配置文件。
    // 如需扫描无 Mapper 后缀的 XML，可在配置中显式追加 "**/*.xml"。
    vec!["**/*Mapper.xml".to_string()]
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

/// 文件级缓存配置（P2-8）。
///
/// 启用后，`check` 与 `check-diff` 会对未修改的文件（mtime + size 不变）
/// 复用上次检查的 violations，跳过解析与规则执行。缓存文件默认写到
/// `target_dir` 下的 `.sqlguard-cache.json`，运行签名（配置 + 规则脚本 +
/// 方言 + filter + 版本）变化时整体失效。
///
/// 默认关闭——大仓库重复 check 时可显式 `enabled = true` 启用。
#[derive(Debug, Deserialize, Clone)]
pub struct CacheConfig {
    /// 总开关。`false` 时完全跳过缓存读写。
    #[serde(default)]
    pub enabled: bool,
    /// 缓存文件名（相对 target_dir 解析）。默认 `.sqlguard-cache.json`。
    #[serde(default = "default_cache_file")]
    pub cache_file: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            enabled: false,
            cache_file: default_cache_file(),
        }
    }
}

fn default_cache_file() -> String {
    ".sqlguard-cache.json".to_string()
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
        let mut config: Config = toml::from_str(&content)
            .map_err(|e| SqlGuardError::ConfigError(format!("Failed to parse config file '{}': {}", path.display(), e)))?;

        let config_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        // 规则来源解析（优先级由高到低）：
        //   1) 显式 `rules_file` -> 从该文件加载（覆盖内联 `[[rules]]`）
        //   2) 同级 `sqlguard.rules.toml` 存在 -> 从该文件加载
        //   3) 否则沿用主配置内联的 `[[rules]]`（保持向后兼容）
        if let Some(rf) = &config.rules_file {
            let rules_path = if rf.is_absolute() {
                rf.clone()
            } else {
                config_dir.join(rf)
            };
            let loaded = Self::load_rules_file(&rules_path)?;
            config.rules = loaded;
            config.rules_dir = rules_path
                .parent()
                .unwrap_or(&config_dir)
                .to_path_buf();
        } else {
            let sibling = config_dir.join("sqlguard.rules.toml");
            if sibling.exists() {
                let loaded = Self::load_rules_file(&sibling)?;
                config.rules = loaded;
                config.rules_dir = sibling
                    .parent()
                    .unwrap_or(&config_dir)
                    .to_path_buf();
            }
            // 否则沿用内联 rules，rules_dir 保持空（回退到主配置目录）
        }

        config.validate_rule_ids()?;
        Ok(config)
    }

    /// 从独立的规则文件（仅含 `[[rules]]` 数组）加载规则列表。
    fn load_rules_file(path: &Path) -> Result<Vec<RuleConfig>, SqlGuardError> {
        let content = fs::read_to_string(path).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Failed to read rules file '{}': {}",
                path.display(),
                e
            ))
        })?;
        #[derive(Deserialize)]
        struct RulesFile {
            rules: Vec<RuleConfig>,
        }
        let rf: RulesFile = toml::from_str(&content).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Failed to parse rules file '{}': {}",
                path.display(),
                e
            ))
        })?;
        Ok(rf.rules)
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
            // 若规则来自独立规则文件，则脚本路径相对规则文件所在目录解析；
            // 否则（内联规则）相对主配置目录解析。
            let base: &Path = if self.rules_dir.as_os_str().is_empty() {
                config_dir
            } else {
                &self.rules_dir
            };
            base.join(script_path)
        }
    }
}

// ===== Rollback 配置（gen-rollback 子命令）=====

/// 回滚脚本生成配置。
///
/// 对应设计文档 §4.6。CLI `--dialect` / `--lock-scope` / `--lock-timeout`
/// / `--accept-table-lock-risk` 覆盖此处的字段。
#[derive(Debug, Deserialize, Clone)]
pub struct RollbackConfig {
    /// ★ P2-4：gen-rollback 子命令由 CLI 显式调用，此字段当前不控制任何行为，
    /// 保留用于未来"check 子命令自动触发 rollback 生成"的场景。配置层兼容字段。
    #[serde(default)]
    #[allow(dead_code)]
    pub enabled: bool,
    /// 方言：mysql / postgresql。CLI --dialect 覆盖此值。
    #[serde(default = "default_dialect")]
    pub dialect: String,
    /// 备份模式：auto（默认）/ full / incremental
    #[serde(default = "default_backup_mode")]
    pub backup_mode: String,
    #[serde(default = "default_backup_file")]
    pub backup_file: String,
    #[serde(default = "default_rollback_file")]
    pub rollback_file: String,
    #[serde(default = "default_manifest_file")]
    pub manifest_file: String,
    #[serde(default = "default_cleanup_file")]
    pub cleanup_file: String,
    /// 仅 PG 生效，MySQL 方言忽略（DDL 隐式提交无效）
    #[serde(default = "default_true")]
    pub wrap_transaction: bool,
    #[serde(default = "default_bks_prefix")]
    pub backup_table_prefix: String,
    /// 备份表名是否包含 8 位日期段（bks_xxx_YYYYMMDD_NNNN）
    #[serde(default = "default_true")]
    pub backup_table_with_date: bool,
    /// ★ P2-4：日期段格式配置。当前实现固定为 YYYYMMDD（见 util::current_date_yyyymmdd），
    /// 不支持自定义格式，保留此字段用于未来扩展。配置层兼容字段。
    #[serde(default = "default_date_fmt")]
    #[allow(dead_code)]
    pub backup_table_date_format: String,
    /// ★ P1-11 默认改为 false（保留备份表便于审计）
    #[serde(default = "default_false")]
    pub cleanup_backup_tables_after_rollback: bool,
    /// 备份段是否加锁（旧配置，保留兼容；新配置用 lock_scope）。
    /// ★ P2-3：在 main.rs 中消费——false 时将 lock_scope 从 auto 映射为 none。
    #[serde(default = "default_true")]
    pub lock_tables_during_backup: bool,
    /// ★ D1 新增：锁策略，见 F14
    /// auto（默认）：脚本含 DDL 或 backup 含 DDL → global，纯 DML → snapshot
    /// global / table / snapshot / none
    #[serde(default = "default_lock_scope")]
    pub lock_scope: String,
    /// ★ D1 新增：FTWRL 等待超时（秒），0 表示无限等待，见 F18
    #[serde(default = "default_lock_timeout")]
    pub lock_timeout: u64,
    /// ★ D1 新增：长事务预检查策略：abort / warn / ignore，见 F18
    #[serde(default = "default_long_tx_strategy")]
    pub on_long_transaction: String,
    /// ★ D1 新增：长事务阈值（秒），见 F18
    #[serde(default = "default_long_tx_threshold")]
    pub long_transaction_threshold: u64,
    /// 是否在 backup.sql 头部加 SET SESSION sql_log_bin=0（MySQL 专用，旧配置，保留兼容）。
    /// ★ D2 修正：此字段已被 `binlog_strategy` 取代（auto/always/never）。
    /// ★ P2-4：当前代码仅消费 `binlog_strategy`，此字段保留用于旧配置文件向后兼容读取，
    /// 不再影响生成行为。迁移建议：用 `binlog_strategy = "always"` 替代 `disable_binlog_for_bks = true`。
    #[serde(default = "default_true")]
    #[allow(dead_code)]
    pub disable_binlog_for_bks: bool,
    /// ★ D2 新增：sql_log_bin 策略：auto / always / never，见 F16
    #[serde(default = "default_binlog_strategy")]
    pub binlog_strategy: String,
    /// schema 漂移校验策略：abort / warn / ignore，见 F13
    #[serde(default = "default_assert_strategy")]
    pub assert_on_schema_mismatch: String,
    /// 分区表处理策略：abort / warn / fallback，见 F15
    #[serde(default = "default_partitioned_strategy")]
    pub on_partitioned_table: String,
    #[serde(default)]
    pub include_select: bool,
    #[serde(default)]
    pub primary_keys: Vec<PrimaryKeyDecl>,
    /// ★ D5 新增：备份表保留天数（默认 7 天），见 F19
    #[serde(default = "default_retention_days")]
    pub backup_table_retention_days: u64,
    /// ★ D6 新增：是否合并连续同表 backup 段的锁区间，见 render.rs coalesce_locks
    #[serde(default = "default_true")]
    pub coalesce_locks: bool,
    /// ★ R6 新增：coalesce 策略：conservative / aggressive
    /// conservative（默认）：仅 DDL 全表 LIKE（INSERT SELECT * FROM t 无 WHERE）合并；DML 增量不合并
    /// aggressive：所有同表段都尝试合并（DBA 显式启用，需自负 WHERE 子句含 JOIN/子查询的跨表依赖风险）
    #[serde(default = "default_coalesce_mode")]
    pub coalesce_locks_mode: String,
    /// ★ R2 新增：lock_scope=table 模式必须显式确认（接受隐式提交释放风险）
    /// 不带此 flag 且 lock_scope=table 时报错退出。CLI --accept-table-lock-risk 写入此字段。
    #[serde(default)]
    pub accept_table_lock_risk: bool,
    /// ★ BUG#2 修复：是否将表名统一转为小写（对应 MySQL lower_case_table_names=1）。
    ///
    /// 默认 false：保留 SQL 中原始表名大小写（Linux + lower_case_table_names=0 行为）。
    /// 设为 true 时，所有表名（含 bks_ 备份表名、_rb_ 影子表名）在生成 backup/rollback
    /// 脚本时统一转小写，适配 Windows/macOS 或 Linux + lower_case_table_names=1 的目标库。
    ///
    /// 启用场景：源 SQL 中表名大小写混用（如 `Users`/`users`），但目标库
    /// lower_case_table_names=1（表名不区分大小写，存储为小写）。若脚本保留原始大小写，
    /// 执行时会因表名大小写不匹配而找不到表。
    #[serde(default = "default_false")]
    pub lower_case_table_names: bool,
}

impl Default for RollbackConfig {
    fn default() -> Self {
        RollbackConfig {
            enabled: false,
            dialect: default_dialect(),
            backup_mode: default_backup_mode(),
            backup_file: default_backup_file(),
            rollback_file: default_rollback_file(),
            manifest_file: default_manifest_file(),
            cleanup_file: default_cleanup_file(),
            wrap_transaction: true,
            backup_table_prefix: default_bks_prefix(),
            backup_table_with_date: true,
            backup_table_date_format: default_date_fmt(),
            cleanup_backup_tables_after_rollback: false,
            lock_tables_during_backup: true,
            lock_scope: default_lock_scope(),
            lock_timeout: default_lock_timeout(),
            on_long_transaction: default_long_tx_strategy(),
            long_transaction_threshold: default_long_tx_threshold(),
            disable_binlog_for_bks: true,
            binlog_strategy: default_binlog_strategy(),
            assert_on_schema_mismatch: default_assert_strategy(),
            on_partitioned_table: default_partitioned_strategy(),
            include_select: false,
            primary_keys: Vec::new(),
            backup_table_retention_days: default_retention_days(),
            coalesce_locks: true,
            coalesce_locks_mode: default_coalesce_mode(),
            accept_table_lock_risk: false,
            lower_case_table_names: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PrimaryKeyDecl {
    pub table: String,
    pub columns: Vec<String>,
}

fn default_dialect() -> String { "mysql".to_string() }
fn default_backup_mode() -> String { "auto".to_string() }
fn default_date_fmt() -> String { "%Y%m%d".to_string() }
fn default_cleanup_file() -> String { "cleanup.sql".to_string() }
fn default_assert_strategy() -> String { "abort".to_string() }
fn default_partitioned_strategy() -> String { "abort".to_string() }
fn default_lock_scope() -> String { "auto".to_string() }
fn default_lock_timeout() -> u64 { 30 }
fn default_long_tx_strategy() -> String { "abort".to_string() }
fn default_long_tx_threshold() -> u64 { 5 }
fn default_binlog_strategy() -> String { "auto".to_string() }
fn default_retention_days() -> u64 { 7 }
fn default_coalesce_mode() -> String { "conservative".to_string() }
fn default_false() -> bool { false }

fn default_backup_file() -> String { "backup.sql".to_string() }
fn default_rollback_file() -> String { "rollback.sql".to_string() }
fn default_manifest_file() -> String { "rollback-manifest.json".to_string() }
fn default_bks_prefix() -> String { "bks_".to_string() }
