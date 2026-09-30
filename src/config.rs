use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::SqlGuardError;
use crate::rule::pack::{ResolvedPack, RulePackMeta};

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
    /// 规则包元数据（`[pack]` 段）。承载「引擎 ↔ 规则」版本契约：
    /// `api_version`（硬校验）、`engine`（版本范围，默认 warning）、
    /// `namespace`（M2 起生效：本地规则的 id 会加上该前缀）。来源优先级：外置规则文件
    /// （`rules_file` 或同级 `sqlguard.rules.toml`）的 `[pack]` 优先，
    /// 其次主配置内联的 `[pack]`。缺省为 `None`，行为与现状完全一致。
    #[serde(default)]
    pub pack: Option<RulePackMeta>,
    /// 规则包配置（M2）：多包加载 / 搜索路径 / 覆盖。
    #[serde(default)]
    pub rule_packs: RulePacksConfig,
    /// 解析后的生效规则包（M2，`Config::load` 时计算）。
    /// 供缓存签名与包内 helpers 查找使用。
    #[serde(skip)]
    pub resolved_packs: Vec<ResolvedPack>,
    /// 规则包非致命提示（如 id 被高优先级来源覆盖）。由调用方决定是否展示。
    #[serde(skip)]
    pub rule_pack_notices: Vec<String>,
    /// 跨来源**同 id 冲突**记录（`--strict-ids` 时按错误处理）。
    #[serde(skip)]
    pub rule_pack_id_conflicts: Vec<String>,
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
    /// 方言回退链的第二候选方言（可选）。
    ///
    /// 当某条语句用主 `dialect` 解析失败时，按「主方言 → 本字段（若存在）
    /// → Generic」的顺序用后续方言重试解析，首个成功即采用。适用于
    /// GaussDB 等「PG 内核 + Oracle 外壳」的混合方言：推荐直接用
    /// `dialect = "gaussdb"`（自带词法级 Oracle/MySQL 兼容重写 + Oracle 兜底），
    /// 也可以显式设 `dialect = "postgresql"` + 本字段 `"oracle"` 作为兼容旧配置。
    ///
    /// 解析优先级与行为：
    /// - 主方言成功 → 直接采用，不再尝试回退（不会用回退方言覆盖已成功的解析）。
    /// - 主方言失败 → 依次用回退链方言重试；全部失败才记 `PARSE_ERROR`。
    /// - 不配置时：`gaussdb` 主方言默认回退到 `oracle`（覆盖重写层未处理的复杂构造），
    ///   其余主方言（含 `postgresql`）默认仅回退到 `Generic`（见 `CheckDialect::default_fallback`）。
    ///   CLI `--dialect-fallback` 可覆盖本值，传 `generic` 可显式关闭回退
    ///   （链退化为「主方言 → Generic」）。
    #[serde(default)]
    pub dialect_fallback: Option<CheckDialect>,
    /// 信任跳过 MyBatis `${}` 动态替换（默认开启）。
    ///
    /// `${}` 的内容在运行时才确定，静态期无法解析。开启时，含 `${}` 的语句若解析失败，
    /// 不报误导性的 `PARSE` 错误，改报诚实的 `DYN`（动态 substitution 未静态校验）警告，
    /// 避免报告里出现一批"假"解析错误。关闭时回退到旧行为（报 `PARSE` 警告）。
    #[serde(default = "default_true")]
    pub trust_dynamic_substitution: bool,
}

/// check 流程的 SQL 方言选择。
///
/// - `Generic`：默认，兼容大多数标准 SQL（向后兼容）
/// - `MySql`：支持 MySQL 专有语法（INSERT IGNORE / ON DUPLICATE KEY UPDATE / 反引号标识符等）
/// - `PostgreSql`：支持 PostgreSQL 扩展语法（保持干净，不再默认回退 Oracle）
/// - `Ansi`：严格 ANSI SQL
/// - `Oracle`：支持 Oracle 兼容语法（CONNECT BY / MINUS / `(+)` 外连接 / DUAL / ROWNUM 等）
/// - `GaussDB`：基于 PG 内核，兼容部分 Oracle/MySQL 语法。解析前先对 SQL 文本做
///   词法级归一化重写（MINUS→EXCEPT、SYSDATE→CURRENT_TIMESTAMP、NVL→COALESCE、
///   FROM dual→FROM (SELECT 1) AS dual、反引号→双引号），再用 PostgreSqlDialect 解析。
///   适用于 GaussDB 等「PG 内核 + Oracle/MySQL 外壳」的混合方言场景——
///   AST 语义 100% 来自 PG 方言，避免原「PG→Oracle 回退」对标识符大小写等语义的污染。
///   重写后仍解析失败的语句才回退 Oracle，最终由 Generic 兜底。
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum CheckDialect {
    #[default]
    Generic,
    MySql,
    PostgreSql,
    Ansi,
    Oracle,
    GaussDB,
}

impl CheckDialect {
    /// 从字符串解析方言（用于 CLI `--dialect` 覆盖配置）。
    /// 不区分大小写，未知值回退到 Generic。
    pub fn parse_dialect(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "mysql" => CheckDialect::MySql,
            "postgres" | "postgresql" | "pg" => CheckDialect::PostgreSql,
            "ansi" => CheckDialect::Ansi,
            "oracle" => CheckDialect::Oracle,
            "gaussdb" | "gauss" => CheckDialect::GaussDB,
            _ => CheckDialect::Generic,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CheckDialect::Generic => "generic",
            CheckDialect::MySql => "mysql",
            CheckDialect::PostgreSql => "postgresql",
            CheckDialect::Ansi => "ansi",
            CheckDialect::Oracle => "oracle",
            CheckDialect::GaussDB => "gaussdb",
        }
    }

    /// 主方言未显式配置 `dialect_fallback` 时采用的默认第二候选。
    ///
    /// - `GaussDB` → `Oracle`：GaussDB 重写层只覆盖词法级 Oracle/MySQL 兼容构造，
    ///   重写后仍失败的语句（如 `CONNECT BY`、`(+)` 外连接等复杂构造）由 Oracle 方言兜底。
    /// - `PostgreSql` → `None`：**保持 PG 方言语义干净**，不再隐式回退 Oracle。
    ///   原 v0.1.0 行为（PG 默认回退 Oracle）会导致带 Oracle 语法的语句整条走 Oracle
    ///   解析路径，污染标识符大小写等 PG 语义。需 Oracle 兼容请改用 `dialect = "gaussdb"`。
    /// - 其余 → `None`：仅回退到链尾的 `Generic`。
    ///
    /// CLI `--dialect-fallback` 与配置 `dialect_fallback` 均优先于本默认值。
    pub fn default_fallback(&self) -> Option<CheckDialect> {
        match self {
            CheckDialect::GaussDB => Some(CheckDialect::Oracle),
            _ => None,
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
    /// 规则参数（可选）：原样注入到脚本的 `context["params"]`。
    ///
    /// 用途：把规范里的**阈值**外部化，避免硬编码在 `.rhai` 中。规范改阈值时
    /// 只改配置、不动脚本，也便于同一规则在不同项目用不同口径。
    ///
    /// TOML 写法（`[[rules]]` 数组元素下的子表）：
    /// ```toml
    /// [[rules]]
    /// id = "GDML001"
    /// name = "max_join_tables"
    /// # ...
    /// [rules.params]
    /// max_join_tables = 3
    /// max_join_tables_batch = 5
    /// ```
    /// 或内联：`params = { max_join_tables = 3 }`。
    ///
    /// 未配置时脚本拿到的 `context["params"]` 是空 Map，脚本应自行给默认值。
    #[serde(default)]
    pub params: Option<toml::Value>,
}

fn default_enabled() -> bool {
    true
}

fn default_severity() -> String {
    "error".to_string()
}

// ===== 规则包（M2）：多包加载 / 合并 / 覆盖 =====

/// `[rule_packs]` 配置段（M2）。
///
/// 缺省（不写该段）时 `packs`/`search_paths`/`overrides` 均为空，行为与合并前
/// 完全一致——规则仍只来自 `rules_file` / 同级 `sqlguard.rules.toml` / 内联 `[[rules]]`。
#[derive(Debug, Deserialize, Clone, Default)]
pub struct RulePacksConfig {
    /// 包搜索路径（相对配置文件目录或绝对路径）。
    /// 包位于 `<search_path>/<name>/rules-pack.toml`。环境变量
    /// `SQLGUARD_RULE_PATH`（`;` 分隔）会追加在其后。
    #[serde(default)]
    pub search_paths: Vec<String>,
    /// 生效的规则包，**按声明顺序决定优先级**（靠后覆盖靠前）。
    #[serde(default)]
    pub packs: Vec<PackRef>,
    /// 规则覆盖（最高优先级），只允许改 `enabled` / `severity` / `params`。
    #[serde(default)]
    pub overrides: Vec<RuleOverride>,
    /// 是否允许远程包来源。当前不支持远程拉取，设为 `true` 会直接报错
    /// （避免"看起来放开了、其实没生效"的静默行为）。
    #[serde(default)]
    pub allow_remote: bool,
}

/// `[rule_packs].packs` 的一项：引用一个规则包。
///
/// `path` 优先（直接指向包根）；否则在 `search_paths` 下按 `name` 查找。
#[derive(Debug, Deserialize, Clone)]
pub struct PackRef {
    /// 包名（对应包清单 `[pack].name`，也是 `<search_path>/<name>` 的目录名）。
    pub name: String,
    /// 期望版本（可选）。与实际解析到的 `[pack].version` 不符时报错。
    #[serde(default)]
    pub version: Option<String>,
    /// 包根路径（相对配置文件目录或绝对路径），可选。
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// `[[rule_packs.overrides]]` 的一项：按 id 覆盖包内/本地规则的属性。
///
/// `id` 接受 canonical id（`gaussdb:GDB001`）或裸 id（`GDB001`）；裸 id 命中
/// 多条时按合并顺序取最后一条（优先级最高者）。
#[derive(Debug, Deserialize, Clone)]
pub struct RuleOverride {
    pub id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub params: Option<toml::Value>,
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
    /// Mapper 扫描根目录。支持通配符：条目含 `*`/`?`/`[`/`{` 时视为 glob，
    /// 相对扫描根（或绝对路径）匹配目录与 XML 文件（正斜杠分隔）。
    /// 例如 `paths = ["src/**/mapper"]` 可命中任意层级的 mapper 目录。
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
/// - `encoding`：扫描文件的编码格式（SQL 脚本 + Mapper XML 统一使用）。
///   默认 `utf-8`（与历史行为一致）；GBK / GB18030 / UTF-16 / Big5 等
///   历史项目编码可用 WHATWG 编码标签指定，如 `encoding = "gbk"`。
///   文件带 BOM 时 BOM 优先于本配置。配置非 UTF-8 时 FILE001
///   「必须 UTF-8 无 BOM」策略检查自动跳过（见 `checker::encoding`）。
#[derive(Debug, Deserialize, Clone)]
pub struct ScanConfig {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_exclude_dirs")]
    pub exclude_dirs: Vec<String>,
    /// 扫描文件的编码格式，默认 `utf-8`。详见 [`crate::encoding`]。
    #[serde(default = "default_scan_encoding")]
    pub encoding: String,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            paths: Vec::new(),
            exclude_dirs: default_exclude_dirs(),
            encoding: default_scan_encoding(),
        }
    }
}

/// 默认扫描编码：UTF-8（向后兼容 `std::fs::read_to_string`）。
fn default_scan_encoding() -> String {
    crate::encoding::DEFAULT_ENCODING.to_string()
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
        let content = fs::read_to_string(path).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Failed to read config file '{}': {}",
                path.display(),
                e
            ))
        })?;
        let mut config: Config = toml::from_str(&content).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Failed to parse config file '{}': {}",
                path.display(),
                e
            ))
        })?;

        let config_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        // 规则来源解析（优先级由高到低）：
        //   1) 显式 `rules_file` -> 从该文件加载（覆盖内联 `[[rules]]`）
        //   2) 同级 `sqlguard.rules.toml` 存在 -> 从该文件加载
        //   3) 否则沿用主配置内联的 `[[rules]]`（保持向后兼容）
        if let Some(rf) = &config.rules_file {
            // P2-2：绝对路径保持允许（兼容现有用法）；
            // 相对路径必须解析到 config 目录内，拦截 `../../xxx` 之类的目录逃逸。
            let rules_path = if rf.is_absolute() {
                rf.clone()
            } else {
                Self::sanitize_relative_rules_path(&config_dir.join(rf), &config_dir)?
            };
            let (loaded, pack) = Self::load_rules_file(&rules_path)?;
            config.rules = loaded;
            // 外置规则文件声明的 [pack] 覆盖主配置内联的 [pack]。
            if pack.is_some() {
                config.pack = pack;
            }
            config.rules_dir = rules_path.parent().unwrap_or(&config_dir).to_path_buf();
        } else {
            let sibling = config_dir.join("sqlguard.rules.toml");
            if sibling.exists() {
                let (loaded, pack) = Self::load_rules_file(&sibling)?;
                config.rules = loaded;
                if pack.is_some() {
                    config.pack = pack;
                }
                config.rules_dir = sibling.parent().unwrap_or(&config_dir).to_path_buf();
            }
            // 否则沿用内联 rules，rules_dir 保持空（回退到主配置目录）
        }

        config.validate_rule_ids()?;
        config.validate_scan_encoding()?;
        config.validate_rule_pack()?;

        // M2：加载规则包并与本地规则合并（namespace 生效、overrides 应用）。
        // 无 [rule_packs] 且本地 [pack] 无 namespace 时，结果与合并前完全一致。
        let merged = crate::rule::pack::resolve_and_merge(&config, &config_dir)?;
        config.rules = merged.rules;
        config.resolved_packs = merged.packs;
        config.rule_pack_notices = merged.notes;
        config.rule_pack_id_conflicts = merged.id_conflicts;

        Ok(config)
    }

    /// 校验 `[scan] encoding` 是否受支持；不合法时给出可用标签清单。
    fn validate_scan_encoding(&self) -> Result<(), SqlGuardError> {
        crate::encoding::validate(&self.scan.encoding).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Invalid [scan] encoding '{}': {}",
                self.scan.encoding, e
            ))
        })
    }

    /// P2-2：校验相对 `rules_file` 路径不逃逸 config 目录（拦截 `..` 组件与符号链接）。
    ///
    /// canonicalize 成功（文件存在）时要求解析结果仍在 config 目录内；
    /// canonicalize 失败（文件不存在）时退化为组件检查，拒绝显式 `..` 逃逸，
    /// 其余情形交由后续 `load_rules_file` 给出更友好的「文件读取失败」错误。
    fn sanitize_relative_rules_path(
        rules_path: &Path,
        config_dir: &Path,
    ) -> Result<PathBuf, SqlGuardError> {
        use std::path::Component;
        if let Ok(canonical) = rules_path.canonicalize() {
            let config_canonical = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            if canonical.starts_with(&config_canonical) {
                return Ok(canonical);
            }
            return Err(SqlGuardError::ConfigError(format!(
                "rules_file '{}' resolves outside the config directory '{}'",
                rules_path.display(),
                config_dir.display()
            )));
        }
        if rules_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(SqlGuardError::ConfigError(format!(
                "rules_file '{}' escapes the config directory '{}'",
                rules_path.display(),
                config_dir.display()
            )));
        }
        Ok(rules_path.to_path_buf())
    }

    /// 从独立的规则文件（`[pack]` 元数据 + `[[rules]]` 数组）加载。
    ///
    /// 返回 `(规则列表, [pack] 元数据)`；`[pack]` 缺省为 `None`，保持对不含
    /// 该段的旧规则文件的完全兼容。
    fn load_rules_file(
        path: &Path,
    ) -> Result<(Vec<RuleConfig>, Option<RulePackMeta>), SqlGuardError> {
        let content = fs::read_to_string(path).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Failed to read rules file '{}': {}",
                path.display(),
                e
            ))
        })?;
        #[derive(Deserialize)]
        struct RulesFile {
            #[serde(default)]
            pack: Option<RulePackMeta>,
            /// 允许「只有 `[pack]` 声明、没有 `[[rules]]`」的规则文件
            /// （纯规则包项目可能需要声明 namespace / 版本契约）。
            #[serde(default)]
            rules: Vec<RuleConfig>,
        }
        let rf: RulesFile = toml::from_str(&content).map_err(|e| {
            SqlGuardError::ConfigError(format!(
                "Failed to parse rules file '{}': {}",
                path.display(),
                e
            ))
        })?;
        Ok((rf.rules, rf.pack))
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

    /// M1：规则包声明硬校验。
    ///
    /// 拦截三类错误：`api_version` 高于引擎支持、`namespace` 字符集非法、
    /// `engine` 版本范围不可解析。`engine` 范围"不满足当前引擎"属非致命诊断，
    /// 见 [`Self::rule_pack_engine_mismatch`]。
    fn validate_rule_pack(&self) -> Result<(), SqlGuardError> {
        if let Some(pack) = &self.pack {
            pack.validate().map_err(|e| {
                SqlGuardError::ConfigError(format!("Invalid [pack] declaration: {e}"))
            })?;
        }
        Ok(())
    }

    /// M1：`[pack].engine` 与当前引擎版本不匹配时的提示消息。
    ///
    /// 返回 `Some` 表示需要提示：调用方决定 warning 还是 error
    /// （`--strict-engine` 时升级为 error）。
    pub fn rule_pack_engine_mismatch(&self) -> Option<String> {
        self.pack
            .as_ref()?
            .engine_mismatch(env!("CARGO_PKG_VERSION"))
    }

    /// M2：规则包的非致命提示（如 id 被高优先级来源覆盖）。
    ///
    /// 由调用方（`check` / `check-diff`）决定是否展示——通常仅在 stderr 为
    /// 交互终端时输出，避免污染 CI 日志。
    pub fn rule_pack_notes(&self) -> &[String] {
        &self.rule_pack_notices
    }

    /// M3：跨来源同 id 冲突（高优先级来源覆盖低优先级时记录）。
    /// `check --strict-ids` / `check-diff --strict-ids` 会把它按错误处理。
    pub fn rule_pack_id_conflicts(&self) -> &[String] {
        &self.rule_pack_id_conflicts
    }

    /// M2：查找覆盖 `script_path` 的规则包内 helpers（按包根前缀匹配）。
    ///
    /// 包内规则的 `script_path` 是包内脚本的规范化绝对路径，据此定位其所属包
    /// 即可取得该包的 helpers；本地规则返回 `None`。
    pub fn pack_helpers_for(&self, script_path: &Path) -> Option<&str> {
        self.resolved_packs
            .iter()
            .find(|p| script_path.starts_with(&p.root))
            .and_then(|p| p.helpers.as_deref())
    }

    /// M3：`script_path` 所属规则包名（本地规则返回 `None`）。供 `rules list` 展示来源。
    pub fn pack_name_for(&self, script_path: &Path) -> Option<&str> {
        self.resolved_packs
            .iter()
            .find(|p| script_path.starts_with(&p.root))
            .map(|p| p.name.as_str())
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
    /// ★ per-file 输出：每个源 SQL 文件生成同名 backup 脚本，写在 output_dir/backup/ 下
    /// 镜像输入目录结构（如 sql/dml/users.sql → backup/sql/dml/users.sql）。
    #[serde(default = "default_backup_file")]
    pub backup_file: String,
    /// ★ per-file 输出：每个源 SQL 文件生成同名 rollback 脚本，写在 output_dir/rollback/ 下
    /// 镜像输入目录结构（如 sql/dml/users.sql → rollback/sql/dml/users.sql）。
    #[serde(default = "default_rollback_file")]
    pub rollback_file: String,
    /// 全局 manifest 文件名（跨文件汇总，写在 output_dir 根目录）。
    #[serde(default = "default_manifest_file")]
    pub manifest_file: String,
    /// 全局 cleanup 文件名（跨文件汇总，写在 output_dir 根目录）。
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
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PrimaryKeyDecl {
    pub table: String,
    pub columns: Vec<String>,
}

fn default_dialect() -> String {
    "mysql".to_string()
}
fn default_backup_mode() -> String {
    "auto".to_string()
}
fn default_date_fmt() -> String {
    "%Y%m%d".to_string()
}
fn default_cleanup_file() -> String {
    "cleanup.sql".to_string()
}
fn default_assert_strategy() -> String {
    "abort".to_string()
}
fn default_partitioned_strategy() -> String {
    "abort".to_string()
}
fn default_lock_scope() -> String {
    "auto".to_string()
}
fn default_lock_timeout() -> u64 {
    30
}
fn default_long_tx_strategy() -> String {
    "abort".to_string()
}
fn default_long_tx_threshold() -> u64 {
    5
}
fn default_binlog_strategy() -> String {
    "auto".to_string()
}
fn default_retention_days() -> u64 {
    7
}
fn default_coalesce_mode() -> String {
    "conservative".to_string()
}
fn default_false() -> bool {
    false
}

fn default_backup_file() -> String {
    "backup.sql".to_string()
}
fn default_rollback_file() -> String {
    "rollback.sql".to_string()
}
fn default_manifest_file() -> String {
    "rollback-manifest.json".to_string()
}
fn default_bks_prefix() -> String {
    "bks_".to_string()
}

#[cfg(test)]
mod dialect_config_tests {
    use super::*;

    #[test]
    fn config_dialect_from_toml_postgresql() {
        let toml = r#"
dialect = "postgresql"

[structure]
paths = ["x"]

[classification]
default_type = "other"

[[classification.rules]]
name = "sql-by-ext"
pattern = "*.sql"
type = "sql"
"#;
        let cfg: Config = toml::from_str(toml).expect("parse");
        eprintln!(
            "parsed dialect = {:?} (as_str={})",
            cfg.dialect,
            cfg.dialect.as_str()
        );
        assert_eq!(
            cfg.dialect,
            CheckDialect::PostgreSql,
            "config dialect should be PostgreSql"
        );
    }

    #[test]
    fn config_dialect_default_is_generic() {
        let toml = r#"
[structure]
paths = ["x"]

[classification]
default_type = "other"

[[classification.rules]]
name = "sql-by-ext"
pattern = "*.sql"
type = "sql"
"#;
        let cfg: Config = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.dialect, CheckDialect::Generic);
    }

    #[test]
    fn config_dialect_gaussdb_from_toml_and_default_fallback() {
        // GaussDB 方言应能从 toml 解析，且默认回退 Oracle（覆盖重写层未处理的复杂构造）
        let toml = r#"
dialect = "gaussdb"

[structure]
paths = ["x"]

[classification]
default_type = "other"

[[classification.rules]]
name = "sql-by-ext"
pattern = "*.sql"
type = "sql"
"#;
        let cfg: Config = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.dialect, CheckDialect::GaussDB);
        assert_eq!(cfg.dialect.as_str(), "gaussdb");
        // ★ 关键：GaussDB 默认回退 Oracle，PostgreSql 不再默认回退（污染消除）
        assert_eq!(cfg.dialect.default_fallback(), Some(CheckDialect::Oracle));
        assert_eq!(
            CheckDialect::PostgreSql.default_fallback(),
            None,
            "PG must NOT default to Oracle anymore"
        );
    }

    #[test]
    fn config_dialect_from_str_accepts_gaussdb_aliases() {
        assert_eq!(
            CheckDialect::parse_dialect("gaussdb"),
            CheckDialect::GaussDB
        );
        assert_eq!(
            CheckDialect::parse_dialect("GaussDB"),
            CheckDialect::GaussDB
        );
        assert_eq!(CheckDialect::parse_dialect("gauss"), CheckDialect::GaussDB);
        assert_eq!(CheckDialect::parse_dialect("GAUSS"), CheckDialect::GaussDB);
    }

    // ===== rules_file 路径校验（P2-2） =====

    /// 生成一个最小可解析的主配置文本（classification.rules 为必填项）。
    fn minimal_config(rules_file: &str) -> String {
        format!(
            "rules_file = \"{}\"\n\n\
             [structure]\n\
             paths = [\"x\"]\n\n\
             [classification]\n\
             default_type = \"other\"\n\n\
             [[classification.rules]]\n\
             name = \"sql-by-ext\"\n\
             pattern = \"*.sql\"\n\
             type = \"sql\"\n",
            rules_file
        )
    }

    #[test]
    fn config_rules_file_escaping_rejected() {
        let dir = std::env::temp_dir().join("sqlguard_cfg_escape_test");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        // 逃逸目标文件放在 cfg 目录之外、dir 之内：确保 canonicalize 能解析，
        // 走「规范化后越界」分支而非「文件不存在」的组件检查分支。
        let outside = dir.join("outside_rules.toml");
        std::fs::write(&outside, "[[rules]]\nid = \"X1\"\nname = \"x\"\nscript_path = \"x.rhai\"\napplies_to = [\"ddl\"]\n").unwrap();

        let cfg_path = cfg_dir.join("sqlguard.toml");
        std::fs::write(&cfg_path, minimal_config("../outside_rules.toml")).unwrap();

        let result = Config::load(&cfg_path);
        assert!(
            result.is_err(),
            "rules_file escaping the config dir must be rejected"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_rules_file_missing_with_parent_dir_rejected() {
        // 文件不存在 + 显式 `..`：走组件检查分支，同样拒绝
        let dir = std::env::temp_dir().join("sqlguard_cfg_escape_missing_test");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        let cfg_path = cfg_dir.join("sqlguard.toml");
        std::fs::write(&cfg_path, minimal_config("../no_such_rules.toml")).unwrap();

        let result = Config::load(&cfg_path);
        assert!(result.is_err(), "rules_file with '..' must be rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_rules_file_relative_inside_loaded() {
        let dir = std::env::temp_dir().join("sqlguard_cfg_relative_test");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("my.rules.toml"),
            "[[rules]]\nid = \"DDL001\"\nname = \"r\"\nscript_path = \"x.rhai\"\napplies_to = [\"ddl\"]\n",
        )
        .unwrap();

        let cfg_path = cfg_dir.join("sqlguard.toml");
        std::fs::write(&cfg_path, minimal_config("my.rules.toml")).unwrap();

        let cfg = Config::load(&cfg_path).expect("rules_file inside config dir should load");
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].id, "DDL001");
        // rules_dir 应指向规则文件所在目录（即 config 目录），
        // 与 canonicalize 后的实际路径比较（canonicalize 可能带 \\?\ 前缀）
        let expected_dir = cfg_dir.canonicalize().unwrap();
        assert_eq!(cfg.rules_dir, expected_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== [pack] 规则包兼容声明（M1） =====

    /// 最小规则文件体：一条规则（`rules` 是 `RulesFile` 的必填字段）。
    const ONE_RULE: &str =
        "[[rules]]\nid = \"DDL001\"\nname = \"r\"\nscript_path = \"x.rhai\"\napplies_to = [\"ddl\"]\n";

    /// 最小主配置文本（不引用 `rules_file`，走同级 `sqlguard.rules.toml` 自动发现）。
    /// `pack_header` 放在最前，保证 `[pack]` 是顶层表而非落在别的 section 下。
    fn minimal_main_config(pack_header: &str) -> String {
        format!(
            "{pack_header}\n\
             [structure]\n\
             paths = [\"x\"]\n\n\
             [classification]\n\
             default_type = \"other\"\n\n\
             [[classification.rules]]\n\
             name = \"sql-by-ext\"\n\
             pattern = \"*.sql\"\n\
             type = \"sql\"\n"
        )
    }

    /// 建一个「主配置 + 同级 sqlguard.rules.toml」的最小工程，返回 (dir, cfg_path)。
    fn setup_pack_project(name: &str, main_pack: &str, rules_body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("sqlguard.rules.toml"), rules_body).unwrap();
        let cfg_path = cfg_dir.join("sqlguard.toml");
        std::fs::write(&cfg_path, minimal_main_config(main_pack)).unwrap();
        (dir, cfg_path)
    }

    #[test]
    fn config_pack_absent_leaves_pack_none() {
        let (dir, cfg_path) = setup_pack_project("sqlguard_cfg_pack_absent", "", ONE_RULE);
        let cfg = Config::load(&cfg_path).expect("load");
        assert!(cfg.pack.is_none());
        assert!(cfg.rule_pack_engine_mismatch().is_none());
        assert!(cfg.rule_pack_notes().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_pack_future_api_version_rejected() {
        let body = format!("[pack]\nname = \"rules-x\"\napi_version = 999\n\n{ONE_RULE}");
        let (dir, cfg_path) = setup_pack_project("sqlguard_cfg_pack_apiver", "", &body);
        let err = Config::load(&cfg_path).expect_err("future api_version must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("Invalid [pack] declaration"), "got: {msg}");
        assert!(
            msg.contains("hint:"),
            "error must carry an actionable hint: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_pack_engine_mismatch_is_diagnostic_not_error() {
        let body = format!("[pack]\nname = \"rules-x\"\nengine = \">=0.0.1 <0.0.2\"\n\n{ONE_RULE}");
        let (dir, cfg_path) = setup_pack_project("sqlguard_cfg_pack_engine", "", &body);
        let cfg = Config::load(&cfg_path).expect("engine mismatch must NOT fail load");
        let msg = cfg.rule_pack_engine_mismatch().expect("expected mismatch");
        assert!(msg.contains("--strict-engine"), "got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_pack_namespace_is_applied_and_invalid_charset_rejected() {
        // 合法 namespace → M2 起生效：本地规则 id 加上前缀
        let body = format!("[pack]\nnamespace = \"gaussdb\"\n\n{ONE_RULE}");
        let (dir, cfg_path) = setup_pack_project("sqlguard_cfg_pack_ns_ok", "", &body);
        let cfg = Config::load(&cfg_path).expect("load");
        assert_eq!(
            cfg.pack.as_ref().unwrap().namespace.as_deref(),
            Some("gaussdb")
        );
        assert_eq!(cfg.rules[0].id, "gaussdb:DDL001");
        let _ = std::fs::remove_dir_all(&dir);

        // 非法 namespace → 硬错误
        let body = format!("[pack]\nnamespace = \"Bad Ns\"\n\n{ONE_RULE}");
        let (dir, cfg_path) = setup_pack_project("sqlguard_cfg_pack_ns_bad", "", &body);
        assert!(Config::load(&cfg_path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_external_pack_overrides_inline_pack() {
        let inline_pack = "[pack]\nname = \"inline\"\n";
        let body = format!("[pack]\nname = \"external\"\n\n{ONE_RULE}");
        let (dir, cfg_path) = setup_pack_project("sqlguard_cfg_pack_override", inline_pack, &body);
        let cfg = Config::load(&cfg_path).expect("load");
        assert_eq!(cfg.pack.as_ref().unwrap().name.as_deref(), Some("external"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_inline_pack_used_without_external_rules_file() {
        // 无同级 sqlguard.rules.toml、无 rules_file → 内联 [[rules]] + 内联 [pack] 生效。
        // 注意 TOML 顺序：`[pack]` 与 `[[rules]]` 都是顶层，必须写在 `[structure]` 之前。
        let dir = std::env::temp_dir().join("sqlguard_cfg_pack_inline");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("sqlguard.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[pack]\nname = \"inline\"\n\n{ONE_RULE}\n{}",
                minimal_main_config("")
            ),
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).expect("load");
        assert_eq!(cfg.pack.as_ref().unwrap().name.as_deref(), Some("inline"));
        assert_eq!(cfg.rules.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== [rule_packs] 多包加载 / 合并 / 覆盖（M2） =====

    /// 建一个带 `[rule_packs]` 的工程：主配置 + 同级 sqlguard.rules.toml。
    /// `main_extra` 会被插到主配置最前（保证 `[rule_packs]` 是顶层表）。
    fn setup_pack_proj(name: &str, main_extra: &str, rules_body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("sqlguard.rules.toml"), rules_body).unwrap();
        let cfg_path = cfg_dir.join("sqlguard.toml");
        std::fs::write(&cfg_path, minimal_main_config(main_extra)).unwrap();
        (dir, cfg_path)
    }

    /// 在 `<cfg>/packs/<name>/` 下写一个规则包（含 rules/ 目录）。
    fn write_pack(cfg_dir: &Path, name: &str, manifest: &str, scripts: &[(&str, &str)]) {
        let root = cfg_dir.join("packs").join(name);
        std::fs::create_dir_all(root.join("rules")).unwrap();
        std::fs::write(root.join("rules-pack.toml"), manifest).unwrap();
        for (file, body) in scripts {
            std::fs::write(root.join("rules").join(file), body).unwrap();
        }
    }

    /// 包清单文本：单条规则 `R001`，`script` 为 `rules/` 下的文件名。
    fn pack_manifest(name: &str, namespace: Option<&str>, script: &str) -> String {
        let ns = namespace
            .map(|n| format!("namespace = \"{n}\"\n"))
            .unwrap_or_default();
        format!(
            "[pack]\nname = \"{name}\"\nversion = \"1.0.0\"\napi_version = 1\n{ns}\n\
             [[rules]]\nid = \"R001\"\nname = \"rule_one\"\nscript_path = \"rules/{script}\"\n\
             applies_to = [\"dml\"]\nseverity = \"warning\"\n"
        )
    }

    #[test]
    fn pack_two_namespaced_packs_keep_both_ids() {
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\n\
                     packs = [{ name = \"pack-a\" }, { name = \"pack-b\" }]\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_two", extra, ONE_RULE);
        let cfg_dir = cfg_path.parent().unwrap();
        write_pack(
            cfg_dir,
            "pack-a",
            &pack_manifest("pack-a", Some("a"), "a.rhai"),
            &[("a.rhai", "// a")],
        );
        write_pack(
            cfg_dir,
            "pack-b",
            &pack_manifest("pack-b", Some("b"), "b.rhai"),
            &[("b.rhai", "// b")],
        );

        let cfg = Config::load(&cfg_path).expect("load");
        let ids: Vec<&str> = cfg.rules.iter().map(|r| r.id.as_str()).collect();
        // 两个包各写 R001，靠 namespace 共存；本地 DDL001 保持裸 id
        assert!(ids.contains(&"a:R001"), "ids={ids:?}");
        assert!(ids.contains(&"b:R001"), "ids={ids:?}");
        assert!(ids.contains(&"DDL001"), "ids={ids:?}");
        assert_eq!(cfg.resolved_packs.len(), 2);
        assert_eq!(cfg.resolved_packs[0].version, "1.0.0");
        assert!(cfg.rule_pack_notes().is_empty(), "no conflict expected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn later_pack_overrides_earlier_for_same_bare_id() {
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\n\
                     packs = [{ name = \"pack-a\" }, { name = \"pack-b\" }]\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_pp", extra, ONE_RULE);
        let cfg_dir = cfg_path.parent().unwrap();
        // 两个包都不带 namespace → 同裸 id R001，靠后的 pack-b 覆盖 pack-a
        write_pack(
            cfg_dir,
            "pack-a",
            &pack_manifest("pack-a", None, "a.rhai"),
            &[("a.rhai", "// a")],
        );
        let b = pack_manifest("pack-b", None, "b.rhai").replace("warning", "error");
        write_pack(cfg_dir, "pack-b", &b, &[("b.rhai", "// b")]);

        let cfg = Config::load(&cfg_path).expect("load");
        let matched: Vec<&_> = cfg.rules.iter().filter(|r| r.id == "R001").collect();
        assert_eq!(matched.len(), 1, "same id must dedupe");
        assert_eq!(matched[0].severity, "error", "later pack wins");
        assert_eq!(cfg.resolved_packs.len(), 2, "both packs still resolved");
        assert!(
            cfg.rule_pack_notes().iter().any(|n| n.contains("R001")),
            "override must be reported: {:?}",
            cfg.rule_pack_notes()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_rule_overrides_pack_with_same_bare_id() {
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\npacks = [{ name = \"pack-a\" }]\n";
        let local =
            "[[rules]]\nid = \"R001\"\nname = \"local_rule\"\nscript_path = \"local.rhai\"\n\
                     applies_to = [\"dml\"]\nseverity = \"error\"\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_prec", extra, local);
        write_pack(
            cfg_path.parent().unwrap(),
            "pack-a",
            &pack_manifest("pack-a", None, "a.rhai"),
            &[("a.rhai", "// a")],
        );

        let cfg = Config::load(&cfg_path).expect("load");
        let rule = cfg
            .rules
            .iter()
            .find(|r| r.id == "R001")
            .expect("R001 present");
        assert_eq!(rule.name, "local_rule", "project rules win over packs");
        assert_eq!(rule.severity, "error");
        // 冲突被记录：`check --strict-ids` 据此报错
        assert_eq!(
            cfg.rule_pack_id_conflicts().len(),
            1,
            "id conflict must be recorded: {:?}",
            cfg.rule_pack_id_conflicts()
        );
        assert!(cfg.rule_pack_id_conflicts()[0].contains("R001"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_overrides_change_severity_enabled_params() {
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\npacks = [{ name = \"pack-a\" }]\n\n\
                     [[rule_packs.overrides]]\nid = \"a:R001\"\nenabled = false\n\
                     severity = \"error\"\n\n\
                     [rule_packs.overrides.params]\nmax = 7\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_ovr", extra, ONE_RULE);
        write_pack(
            cfg_path.parent().unwrap(),
            "pack-a",
            &pack_manifest("pack-a", Some("a"), "a.rhai"),
            &[("a.rhai", "// a")],
        );

        let cfg = Config::load(&cfg_path).expect("load");
        let rule = cfg
            .rules
            .iter()
            .find(|r| r.id == "a:R001")
            .expect("a:R001 present");
        assert!(!rule.enabled, "override disabled the rule");
        assert_eq!(rule.severity, "error");
        assert_eq!(
            rule.params
                .as_ref()
                .and_then(|p| p.get("max"))
                .and_then(|v| v.as_integer()),
            Some(7)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_overrides_unknown_id_is_rejected() {
        let extra = "[rule_packs]\npacks = []\n\n[[rule_packs.overrides]]\nid = \"NOPE\"\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_ovr_bad", extra, ONE_RULE);
        let err = Config::load(&cfg_path).expect_err("unknown override id must fail");
        assert!(err.to_string().contains("unknown rule id"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_not_found_reports_search_paths() {
        let extra =
            "[rule_packs]\nsearch_paths = [\"packs\"]\npacks = [{ name = \"missing-pack\" }]\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_missing", extra, ONE_RULE);
        let err = Config::load(&cfg_path).expect_err("missing pack must fail");
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(msg.contains("search_paths"), "hint expected: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_version_mismatch_rejected() {
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\n\
                     packs = [{ name = \"pack-a\", version = \"9.9.9\" }]\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_ver", extra, ONE_RULE);
        write_pack(
            cfg_path.parent().unwrap(),
            "pack-a",
            &pack_manifest("pack-a", None, "a.rhai"),
            &[("a.rhai", "// a")],
        );
        let err = Config::load(&cfg_path).expect_err("version mismatch must fail");
        assert!(
            err.to_string().contains("resolved to version"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_helpers_loaded_and_matched_by_script_path() {
        let manifest = "[pack]\nname = \"pack-h\"\nversion = \"1.0.0\"\napi_version = 1\n\
                        namespace = \"h\"\nhelpers = \"lib/h.rhai\"\n\n\
                        [[rules]]\nid = \"R001\"\nname = \"rule_one\"\n\
                        script_path = \"rules/a.rhai\"\napplies_to = [\"dml\"]\nseverity = \"warning\"\n";
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\npacks = [{ name = \"pack-h\" }]\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_helpers", extra, ONE_RULE);
        let cfg_dir = cfg_path.parent().unwrap();
        write_pack(cfg_dir, "pack-h", manifest, &[("a.rhai", "// a")]);
        std::fs::create_dir_all(cfg_dir.join("packs/pack-h/lib")).unwrap();
        std::fs::write(
            cfg_dir.join("packs/pack-h/lib/h.rhai"),
            "fn pack_only() { 1 }",
        )
        .unwrap();

        let cfg = Config::load(&cfg_path).expect("load");
        let rule = cfg
            .rules
            .iter()
            .find(|r| r.id == "h:R001")
            .expect("h:R001 present");
        assert_eq!(
            cfg.pack_helpers_for(&rule.script_path),
            Some("fn pack_only() { 1 }"),
            "pack helpers must be found by the rule's script path"
        );
        assert!(
            cfg.pack_helpers_for(Path::new("not/in/any/pack.rhai"))
                .is_none(),
            "local rules must not pick up pack helpers"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_script_escaping_root_rejected() {
        let manifest = "[pack]\nname = \"pack-e\"\nversion = \"1.0.0\"\napi_version = 1\n\n\
                        [[rules]]\nid = \"R001\"\nname = \"rule_one\"\n\
                        script_path = \"../../outside.rhai\"\napplies_to = [\"dml\"]\nseverity = \"warning\"\n";
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\npacks = [{ name = \"pack-e\" }]\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_escape", extra, ONE_RULE);
        let cfg_dir = cfg_path.parent().unwrap();
        write_pack(cfg_dir, "pack-e", manifest, &[]);
        // 逃逸目标真实存在，确保命中「规范化后越界」分支
        std::fs::write(cfg_dir.join("outside.rhai"), "// outside").unwrap();

        let err = Config::load(&cfg_path).expect_err("escaping script_path must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("escapes the pack root") || msg.contains("not found"),
            "got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn allow_remote_is_rejected_until_supported() {
        let extra = "[rule_packs]\nallow_remote = true\n";
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_remote", extra, ONE_RULE);
        let err = Config::load(&cfg_path).expect_err("allow_remote must fail for now");
        assert!(err.to_string().contains("allow_remote"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_rule_packs_config_keeps_ids_and_no_resolved_packs() {
        let (dir, cfg_path) = setup_pack_proj("sqlguard_pack_noop", "", ONE_RULE);
        let cfg = Config::load(&cfg_path).expect("load");
        assert!(cfg.resolved_packs.is_empty());
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].id, "DDL001");
        assert!(cfg.rule_pack_notes().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== sqlguard.lock 校验（M3） =====

    /// 建一个「单包」工程并返回 (dir, config, config_dir, cfg_path)。
    fn pack_project_with_lock(name: &str) -> (PathBuf, Config, PathBuf, PathBuf) {
        let extra = "[rule_packs]\nsearch_paths = [\"packs\"]\npacks = [{ name = \"pack-a\" }]\n";
        let (dir, cfg_path) = setup_pack_proj(name, extra, ONE_RULE);
        let cfg_dir = cfg_path.parent().unwrap().to_path_buf();
        write_pack(
            &cfg_dir,
            "pack-a",
            &pack_manifest("pack-a", Some("a"), "a.rhai"),
            &[("a.rhai", "// a")],
        );
        let cfg = Config::load(&cfg_path).expect("load");
        (dir, cfg, cfg_dir, cfg_path)
    }

    #[test]
    fn lock_roundtrip_verifies_in_both_modes() {
        use crate::rule::lock::{self, LockMode};
        let (dir, cfg, cfg_dir, _p) = pack_project_with_lock("sqlguard_lock_ok");

        let lock_file = lock::build_lock(&cfg, &cfg_dir).expect("build lock");
        assert_eq!(lock_file.packs.len(), 1);
        assert_eq!(lock_file.packs[0].name, "pack-a");
        assert_eq!(lock_file.packs[0].version, "1.0.0");
        assert!(lock_file.packs[0].checksum.starts_with("sha256:"));
        let path = lock::write_lock(&lock_file, &cfg_dir).expect("write lock");
        assert!(path.ends_with("sqlguard.lock"));

        assert!(lock::verify_lock(&cfg, &cfg_dir, LockMode::Auto).is_ok());
        assert!(lock::verify_lock(&cfg, &cfg_dir, LockMode::Strict).is_ok());
        // Off 模式永远不检查
        assert!(lock::verify_lock(&cfg, &cfg_dir, LockMode::Off).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_absent_is_note_by_default_and_error_when_strict() {
        use crate::rule::lock::{self, LockMode};
        let (dir, cfg, cfg_dir, _p) = pack_project_with_lock("sqlguard_lock_absent");

        let notes =
            lock::verify_lock(&cfg, &cfg_dir, LockMode::Auto).expect("auto tolerates absence");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("sqlguard.lock"), "got: {notes:?}");

        let err = lock::verify_lock(&cfg, &cfg_dir, LockMode::Strict)
            .expect_err("strict requires the lock file");
        assert!(err.to_string().contains("not found"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_version_mismatch_is_rejected() {
        use crate::rule::lock::{self, LockMode};
        let (dir, cfg, cfg_dir, _p) = pack_project_with_lock("sqlguard_lock_ver");

        let mut lock_file = lock::build_lock(&cfg, &cfg_dir).unwrap();
        lock_file.packs[0].version = "9.9.9".to_string();
        lock::write_lock(&lock_file, &cfg_dir).unwrap();

        let err = lock::verify_lock(&cfg, &cfg_dir, LockMode::Auto)
            .expect_err("version mismatch must fail");
        let msg = err.to_string();
        assert!(msg.contains("pins"), "got: {msg}");
        assert!(msg.contains("rules lock"), "remedy expected: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_strict_detects_tampered_pack_content() {
        use crate::rule::lock::{self, LockMode};
        let (dir, cfg, cfg_dir, _p) = pack_project_with_lock("sqlguard_lock_tamper");

        let lock_file = lock::build_lock(&cfg, &cfg_dir).unwrap();
        lock::write_lock(&lock_file, &cfg_dir).unwrap();

        // 版本不变，但包内脚本被改 → 仅 Strict 能发现（默认只比对版本）
        std::fs::write(cfg_dir.join("packs/pack-a/rules/a.rhai"), "// tampered").unwrap();
        assert!(
            lock::verify_lock(&cfg, &cfg_dir, LockMode::Auto).is_ok(),
            "Auto only compares versions"
        );

        let err = lock::verify_lock(&cfg, &cfg_dir, LockMode::Strict)
            .expect_err("checksum mismatch must fail in strict mode");
        assert!(err.to_string().contains("checksum mismatch"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_strict_rejects_extra_and_missing_entries() {
        use crate::rule::lock::{self, LockFile, LockMode, LockedPack};
        let (dir, cfg, cfg_dir, _p) = pack_project_with_lock("sqlguard_lock_extra");

        // 锁里只有另一个包 → 解析出的 pack-a 未被记录
        let lock_file = LockFile {
            version: 1,
            packs: vec![LockedPack {
                name: "ghost-pack".to_string(),
                version: "1.0.0".to_string(),
                source: "path:ghost".to_string(),
                checksum: "sha256:0".to_string(),
                api_version: Some(1),
            }],
        };
        lock::write_lock(&lock_file, &cfg_dir).unwrap();

        let err = lock::verify_lock(&cfg, &cfg_dir, LockMode::Strict)
            .expect_err("unknown/missing entries must fail");
        let msg = err.to_string();
        assert!(msg.contains("not recorded"), "got: {msg}");
        assert!(msg.contains("no longer resolves"), "got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
