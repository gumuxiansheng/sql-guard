//! 规则包（Rule Pack）元数据、版本契约与多包加载合并。
//!
//! 规则与引擎分开发版时，最大的风险是**规则静默失败**：旧引擎没有规则用到的
//! API 时，规则可能整条不生效却毫无报错，用户误以为"检查通过"。本模块定义
//! 版本声明与校验，把这种情况变成**可读的硬错误**。
//!
//! - **M1**：`RULE_API_VERSION` + `[pack]` 段（`api_version` / `engine` / `namespace`）
//!   的解析与校验；`[pack]` 可写在现有规则文件或主配置中。
//! - **M2**：`rules-pack.toml` 清单格式 + 多包加载/合并/优先级 + `namespace` 生效
//!   + `[rule_packs.overrides]` + 包内 helpers（见 [`resolve_and_merge`]）。
//!
//! 设计文档：`docs/rule-pack-design.md`。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{Config, PackRef, RuleConfig, RuleOverride};
use crate::error::SqlGuardError;

/// 当前引擎支持的**规则 API 版本**（单调递增的整体单号，见设计稿 §4.1）。
///
/// 语义：引擎暴露给 Rhai 规则的全部 API 面——注册的类型、注册的函数、
/// 内置 helpers、`context` 字段。以下变更都必须递增本值：
/// - 增删改注册类型 / 其字段（`runner.rs::build_engine` 的 `register_type_with_name`）；
/// - 增删改注册函数（`register_fn`）——**新增也算**，旧引擎没有该函数；
/// - 修改内置 helpers（`runner.rs::HELPERS_SCRIPT`）；
/// - 增删改 `context` 字段。
///
/// 规则在 `[pack].api_version` 声明其所需版本；声明值大于本常量时引擎拒绝加载，
/// 避免"新规则跑在旧引擎上静默失效"。纯内部实现变更（解析器实现、性能优化）
/// 不影响规则可见面，**不**递增本值。
pub const RULE_API_VERSION: u32 = 1;

/// 规则包元数据（`[pack]` 段）。
///
/// 全部字段可选：缺省即"无声明"，保持对现有规则文件的完全向后兼容。
/// 本结构同时是 M2 `rules-pack.toml` 清单 `[pack]` 段的前身，M2 直接复用。
#[derive(Debug, Deserialize, Clone, Default)]
pub struct RulePackMeta {
    /// 包名。M1 用于报错提示；M2 作为锁文件与 CLI 的引用键、搜索路径下的目录名。
    #[serde(default)]
    pub name: Option<String>,
    /// 包版本（SemVer）。M2 起在 `rules-pack.toml` 清单中必填。
    #[serde(default)]
    pub version: Option<String>,
    /// 规则 id 命名空间。**M1 仅解析并校验字符集，尚未生效**（M2 生效）；
    /// 设置了它 M1 只给出一条提示，id 仍为裸 id。
    #[serde(default)]
    pub namespace: Option<String>,
    /// 本包使用的规则 API 版本。缺省视为 `1`（兼容未声明版本的旧规则文件）。
    #[serde(default)]
    pub api_version: Option<u32>,
    /// 兼容的引擎版本范围，如 `">=0.2.7 <0.3"`。缺省表示不声明。
    #[serde(default)]
    pub engine: Option<String>,
    /// 包描述（可选，纯元数据）。
    #[serde(default)]
    pub description: Option<String>,
    /// 许可证标识（可选，纯元数据）。
    #[serde(default)]
    pub license: Option<String>,
    /// 包内私有辅助函数文件（相对包根，可选）。内容会在**引擎 helpers 之后**
    /// prepend，供包内规则共享工具函数；禁止逃逸包根。
    #[serde(default)]
    pub helpers: Option<String>,
}

impl RulePackMeta {
    /// 声明所需的 API 版本（缺省 = 1，兼容旧规则文件）。
    pub fn declared_api_version(&self) -> u32 {
        self.api_version.unwrap_or(1)
    }

    /// 包名（未声明时的占位），用于报错文案。
    fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("<unnamed pack>")
    }

    /// 硬校验：任一失败都必须拒绝加载（返回可读错误消息）。
    ///
    /// 覆盖三项：`api_version` 过高、`namespace` 字符集非法、`engine` 范围不可解析。
    pub fn validate(&self) -> Result<(), String> {
        if let Some(err) = self.api_version_error() {
            return Err(err);
        }
        if let Some(err) = self.namespace_error() {
            return Err(err);
        }
        if let Some(range) = self.engine.as_deref() {
            if let Err(err) = parse_engine_range(range) {
                return Err(format!(
                    "rule pack '{}' has an invalid engine range '{}': {}",
                    self.display_name(),
                    range,
                    err
                ));
            }
        }
        Ok(())
    }

    /// API 版本不兼容时的错误消息（兼容则返回 `None`）。
    pub fn api_version_error(&self) -> Option<String> {
        let declared = self.declared_api_version();
        if declared <= RULE_API_VERSION {
            return None;
        }
        Some(format!(
            "rule pack '{}' requires rule API version {}, but this engine supports up to {}.\n\
             hint: upgrade sqlguard, or use a rule set written for rule API version {} or lower.",
            self.display_name(),
            declared,
            RULE_API_VERSION,
            RULE_API_VERSION
        ))
    }

    /// `namespace` 字符集校验：非空且仅含 `[a-z0-9_-]`（见设计稿 §5.3）。
    pub fn namespace_error(&self) -> Option<String> {
        let ns = self.namespace.as_deref()?;
        if ns.is_empty() {
            return Some(format!(
                "rule pack '{}' has an empty namespace",
                self.display_name()
            ));
        }
        if let Some(bad) = ns
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'))
        {
            return Some(format!(
                "rule pack '{}' has an invalid namespace '{}': character '{}' is not allowed \
                 (allowed: a-z, 0-9, '_' and '-').",
                self.display_name(),
                ns,
                bad
            ));
        }
        None
    }

    /// 引擎版本范围不满足时的提示消息（满足或未声明则返回 `None`）。
    ///
    /// 这是**非致命**诊断：调用方决定 warning 还是 error（`--strict-engine`）。
    /// 注意：`engine` 字段本身不可解析属硬错误，已在 [`Self::validate`] 拦截。
    pub fn engine_mismatch(&self, engine_version: &str) -> Option<String> {
        let range = self.engine.as_deref()?;
        match engine_range_satisfied(range, engine_version) {
            Ok(true) => None,
            Ok(false) => Some(format!(
                "rule pack '{}' declares engine compatibility '{}', but this engine is {}.\n\
                 note: the rule set was not validated on this engine version; \
                 pass --strict-engine to fail instead of warn.",
                self.display_name(),
                range,
                engine_version
            )),
            // 实际不可达（validate 已拦截），保留给出可读兜底而非 panic。
            Err(err) => Some(format!(
                "rule pack '{}' has an invalid engine range '{}': {}",
                self.display_name(),
                range,
                err
            )),
        }
    }
}

/// 引擎版本范围求值：`engine_version` 是否满足 `range`。
///
/// 支持 `>=` `>` `<=` `<` `=`（裸版本号等价 `=`）；多个比较器以空白或逗号分隔，
/// 语义为 **AND**（如 `">=0.2.7 <0.3"`）。不支持 `||`（OR），遇到时返回错误提示
/// 改用 AND 形式——避免引入一个半吊子的范围语法。
fn engine_range_satisfied(range: &str, engine_version: &str) -> Result<bool, String> {
    let comparators = parse_engine_range(range)?;
    let actual = parse_version(engine_version)?;
    Ok(comparators.iter().all(|c| {
        let ord = compare(&actual, &c.version);
        match c.op {
            Op::Lt => ord == Ordering::Less,
            Op::Le => ord != Ordering::Greater,
            Op::Gt => ord == Ordering::Greater,
            Op::Ge => ord != Ordering::Less,
            Op::Eq => ord == Ordering::Equal,
        }
    }))
}

/// 语义化版本号的数字部分（`major.minor.patch`，缺省补 0）。故意忽略
/// `-pre` / `+build` 后缀的排序语义——本工具只需要判断范围，不需要精确的
/// 预发布比较（预发布版本不会用于引擎 tag）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, Copy)]
struct Comparator {
    op: Op,
    version: Version,
}

fn compare(a: &Version, b: &Version) -> Ordering {
    (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch))
}

/// 解析版本号：接受 `1` / `1.2` / `1.2.3`（缺省位补 0），忽略 `-` / `+` 后缀。
fn parse_version(raw: &str) -> Result<Version, String> {
    let core = raw.trim().split(['-', '+']).next().unwrap_or("");
    let invalid = || {
        format!(
            "'{}' is not a valid version (expected MAJOR[.MINOR[.PATCH]])",
            raw
        )
    };
    let mut parts = core.split('.');
    let major = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)?
        .parse::<u64>()
        .map_err(|_| invalid())?;
    let minor = match parts.next() {
        Some(s) if !s.is_empty() => s.parse::<u64>().map_err(|_| invalid())?,
        _ => 0,
    };
    let patch = match parts.next() {
        Some(s) if !s.is_empty() => s.parse::<u64>().map_err(|_| invalid())?,
        _ => 0,
    };
    if parts.next().is_some() {
        return Err(invalid());
    }
    Ok(Version {
        major,
        minor,
        patch,
    })
}

/// 解析范围表达式为比较器列表（全部为 AND 关系）。
fn parse_engine_range(range: &str) -> Result<Vec<Comparator>, String> {
    if range.contains("||") {
        return Err(
            "OR ('||') is not supported; use space- or comma-separated AND comparators, \
             e.g. \">=0.2.7 <0.3\""
                .to_string(),
        );
    }
    let mut out = Vec::new();
    for token in range.split(|c: char| c.is_whitespace() || c == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (op, rest) = if let Some(r) = token.strip_prefix(">=") {
            (Op::Ge, r)
        } else if let Some(r) = token.strip_prefix("<=") {
            (Op::Le, r)
        } else if let Some(r) = token.strip_prefix('>') {
            (Op::Gt, r)
        } else if let Some(r) = token.strip_prefix('<') {
            (Op::Lt, r)
        } else if let Some(r) = token.strip_prefix('=') {
            (Op::Eq, r)
        } else {
            (Op::Eq, token)
        };
        out.push(Comparator {
            op,
            version: parse_version(rest.trim())?,
        });
    }
    if out.is_empty() {
        return Err("empty version range".to_string());
    }
    Ok(out)
}

// ============================================================================
// M2：规则包加载、合并与覆盖
// ============================================================================

/// 包清单文件名（包根下）。
pub const PACK_MANIFEST_FILE: &str = "rules-pack.toml";

/// 解析后的规则包。供缓存签名、`rules list`（M3）与包内 helpers 查找使用。
#[derive(Debug, Clone)]
pub struct ResolvedPack {
    pub name: String,
    pub version: String,
    pub namespace: Option<String>,
    pub api_version: u32,
    /// 包根（脚本/helpers 的解析基准，已规范化）。
    pub root: PathBuf,
    /// 包内规则条数（帮助信息用）。
    pub rule_count: usize,
    /// 包内 helpers 文件内容（清单声明了 `helpers` 时），在引擎 helpers 之后 prepend。
    pub helpers: Option<String>,
}

/// 多包合并结果。
#[derive(Debug, Default)]
pub struct MergedRules {
    /// 生效规则。`id` 已按 namespace 规范化为 canonical id（`<ns>:<id>` 或裸 id）。
    pub rules: Vec<RuleConfig>,
    /// 生效规则包，按 `[rule_packs].packs` 声明顺序。
    pub packs: Vec<ResolvedPack>,
    /// 非致命提示（id 被高优先级来源覆盖等），调用方决定是否展示。
    pub notes: Vec<String>,
    /// 跨来源**同 id 冲突**记录（`--strict-ids` 时按错误处理）。
    pub id_conflicts: Vec<String>,
}

/// `rules-pack.toml` 清单结构。
#[derive(Debug, Deserialize)]
struct PackManifest {
    pack: RulePackMeta,
    #[serde(default)]
    rules: Vec<RuleConfig>,
}

fn pack_err(msg: impl Into<String>) -> SqlGuardError {
    SqlGuardError::ConfigError(msg.into())
}

fn canonical_or_self(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// 规则 id 的「短 id」：canonical id 去掉 `<namespace>:` 前缀后的部分。
///
/// 未加 namespace 时即 id 本身。用于 `overrides` 的裸 id 匹配与提示文案。
pub fn short_id(id: &str) -> &str {
    id.split_once(':').map(|(_, s)| s).unwrap_or(id)
}

/// 把规则 id 规范化为 canonical id（`<namespace>:<id>`）。
///
/// `namespace` 为 `None` 时保持裸 id。规则 id 本身不允许包含 `:`
/// （否则无法与 namespace 前缀区分）。
fn qualify_rule_id(
    rule: &mut RuleConfig,
    namespace: Option<&str>,
    source: &str,
) -> Result<(), SqlGuardError> {
    if rule.id.contains(':') {
        return Err(pack_err(format!(
            "rule id '{}' from {source} must not contain ':' (it is reserved for the \
             namespace prefix, e.g. \"gaussdb:GDB001\")",
            rule.id
        )));
    }
    if let Some(ns) = namespace {
        rule.id = format!("{ns}:{}", rule.id);
    }
    Ok(())
}

/// 按优先级分层合并：同 canonical id 时**后者覆盖前者**（原地替换，保持声明顺序）。
#[derive(Default)]
struct Layered {
    rules: Vec<RuleConfig>,
    /// 同 id 被覆盖的记录（供 `--strict-ids` 判定）。
    conflicts: Vec<String>,
}

impl Layered {
    fn insert(&mut self, rule: RuleConfig, source: &str, notes: &mut Vec<String>) {
        match self.rules.iter().position(|r| r.id == rule.id) {
            Some(pos) => {
                self.conflicts
                    .push(format!("{} (redefined by {source})", rule.id));
                notes.push(format!(
                    "rule id '{}' redefined by {source}; the earlier definition is replaced",
                    rule.id
                ));
                self.rules[pos] = rule;
            }
            None => self.rules.push(rule),
        }
    }
}

/// 读取包清单声明的包名（供 `rules add` 解析目录形式的 SPEC；不做完整性校验）。
pub fn read_pack_name(root: &Path) -> Result<String, SqlGuardError> {
    let manifest_path = root.join(PACK_MANIFEST_FILE);
    let content = fs::read_to_string(&manifest_path).map_err(|e| {
        pack_err(format!(
            "failed to read pack manifest '{}': {e}",
            manifest_path.display()
        ))
    })?;
    let manifest: PackManifest = toml::from_str(&content).map_err(|e| {
        pack_err(format!(
            "failed to parse pack manifest '{}': {e}",
            manifest_path.display()
        ))
    })?;
    manifest.pack.name.ok_or_else(|| {
        pack_err(format!(
            "pack manifest '{}' must declare [pack].name",
            manifest_path.display()
        ))
    })
}

/// 解析规则包、合并各层规则并应用覆盖。
///
/// 合并优先级（低 → 高）：引擎内置兜底（无 Rhai 内置规则，仅 helpers +
/// 非 AST 轻量检查，天然不受影响）< `[rule_packs].packs`（按声明顺序）
/// < 项目本地规则（`rules_file` / 同级 `sqlguard.rules.toml` / 内联 `[[rules]]`）
/// < `[rule_packs.overrides]`。
///
/// 无 `[rule_packs]` 配置时，仅当本地 `[pack]` 声明了 `namespace` 才会改写 id，
/// 否则结果与合并前逐字节一致。
pub fn resolve_and_merge(config: &Config, config_dir: &Path) -> Result<MergedRules, SqlGuardError> {
    if config.rule_packs.allow_remote {
        return Err(pack_err(
            "[rule_packs].allow_remote = true is not supported yet; remote rule packs are \
             planned for a later milestone. Use local `path` entries instead.",
        ));
    }

    let mut merged = MergedRules::default();
    let mut layer = Layered::default();
    let search_paths = collect_search_paths(config, config_dir);

    // 1) 包层（低优先级，按声明顺序：靠后覆盖靠前）
    for pref in &config.rule_packs.packs {
        // 统一取规范化路径：包内脚本路径也是规范化后的绝对路径，
        // 二者必须同源，包根前缀匹配（helpers 查找）才能在 Windows 的
        // `\\?\C:\...` UNC 前缀下正确工作。
        let root = canonical_or_self(&locate_pack(pref, &search_paths, config_dir)?);
        let manifest_path = root.join(PACK_MANIFEST_FILE);
        let content = fs::read_to_string(&manifest_path).map_err(|e| {
            pack_err(format!(
                "failed to read pack manifest '{}': {e}",
                manifest_path.display()
            ))
        })?;
        let manifest: PackManifest = toml::from_str(&content).map_err(|e| {
            pack_err(format!(
                "failed to parse pack manifest '{}': {e}",
                manifest_path.display()
            ))
        })?;

        let meta = manifest.pack;
        meta.validate().map_err(pack_err)?;
        let name = meta.name.clone().ok_or_else(|| {
            pack_err(format!(
                "pack manifest '{}' must declare [pack].name",
                manifest_path.display()
            ))
        })?;
        let version = meta
            .version
            .clone()
            .ok_or_else(|| pack_err(format!("rule pack '{name}' must declare [pack].version")))?;
        // 引用名必须与清单声明一致，否则锁文件 / `rules list` 里的名字会与实际包不符。
        if name != pref.name {
            return Err(pack_err(format!(
                "rule pack reference '{}' points at a pack whose manifest declares name '{}' \
                 ({}). Use name = \"{}\", or fix [pack].name.",
                pref.name,
                name,
                manifest_path.display(),
                name
            )));
        }
        let api_version = meta.api_version.ok_or_else(|| {
            pack_err(format!(
                "rule pack '{name}' must declare [pack].api_version"
            ))
        })?;
        if let Some(want) = &pref.version {
            if want != &version {
                return Err(pack_err(format!(
                    "rule pack '{name}' resolved to version {version}, but version {want} was \
                     required. Update [rule_packs].packs to {version}, or point `path` at the \
                     intended pack."
                )));
            }
        }

        let helpers = match meta.helpers.as_deref() {
            Some(rel) => Some(read_pack_file(&root, Path::new(rel), "helpers")?),
            None => None,
        };

        let namespace = meta.namespace.clone();
        let mut seen: HashSet<String> = HashSet::new();
        let mut rule_count = 0usize;
        for mut rule in manifest.rules {
            if !seen.insert(rule.id.clone()) {
                return Err(pack_err(format!(
                    "rule pack '{name}' declares duplicate rule id '{}'",
                    rule.id
                )));
            }
            qualify_rule_id(&mut rule, namespace.as_deref(), &format!("pack '{name}'"))?;
            rule.script_path = resolve_pack_script(&root, &rule.script_path, &name, &rule.id)?;
            layer.insert(rule, &format!("pack '{name}'"), &mut merged.notes);
            rule_count += 1;
        }

        merged.packs.push(ResolvedPack {
            name,
            version,
            namespace,
            api_version,
            root,
            rule_count,
            helpers,
        });
    }

    // 2) 项目本地层（中优先级）：rules_file / 同级 rules.toml / 内联 [[rules]]
    let local_namespace = config.pack.as_ref().and_then(|p| p.namespace.clone());
    for rule in &config.rules {
        let mut local = rule.clone();
        qualify_rule_id(&mut local, local_namespace.as_deref(), "project rules")?;
        // 本地规则保持相对 script_path，交由 Config::resolve_script_path 按 rules_dir 解析。
        layer.insert(local, "project rules", &mut merged.notes);
    }

    // 3) overrides（最高优先级，只改 enabled / severity / params）
    apply_overrides(&mut layer.rules, &config.rule_packs.overrides)?;

    merged.rules = layer.rules;
    merged.id_conflicts = layer.conflicts;
    Ok(merged)
}

/// 搜索路径 = `[rule_packs].search_paths` + 环境变量 `SQLGUARD_RULE_PATH`（`;` 分隔）。
/// 相对路径基于配置文件所在目录解析。
fn collect_search_paths(config: &Config, config_dir: &Path) -> Vec<PathBuf> {
    let mut raw: Vec<String> = config.rule_packs.search_paths.clone();
    if let Ok(env) = std::env::var("SQLGUARD_RULE_PATH") {
        raw.extend(env.split(';').map(str::to_string));
    }
    raw.into_iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let path = Path::new(&p);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                config_dir.join(path)
            }
        })
        .collect()
}

/// 定位包根。`path` 优先；否则在搜索路径下查找 `<search_path>/<name>/rules-pack.toml`。
fn locate_pack(
    pref: &PackRef,
    search_paths: &[PathBuf],
    config_dir: &Path,
) -> Result<PathBuf, SqlGuardError> {
    if let Some(p) = &pref.path {
        let root = if p.is_absolute() {
            p.clone()
        } else {
            config_dir.join(p)
        };
        if !root.join(PACK_MANIFEST_FILE).is_file() {
            return Err(pack_err(format!(
                "rule pack '{}' at '{}' has no {PACK_MANIFEST_FILE}",
                pref.name,
                root.display()
            )));
        }
        return Ok(root);
    }

    for dir in search_paths {
        let nested = dir.join(&pref.name);
        if nested.join(PACK_MANIFEST_FILE).is_file() {
            return Ok(nested);
        }
    }

    Err(pack_err(format!(
        "rule pack '{}' not found. Searched: [{}].\n\
         hint: add its parent directory to [rule_packs].search_paths (packs live at \
         <search_path>/<name>/{PACK_MANIFEST_FILE}), or reference it directly with \
         {{ name = \"{}\", path = \"...\" }}.",
        pref.name,
        search_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        pref.name
    )))
}

/// 读取包内文件（相对包根），校验存在且不逃逸包根。
fn read_pack_file(root: &Path, rel: &Path, what: &str) -> Result<String, SqlGuardError> {
    let canon = resolve_pack_path(root, rel, what)?;
    fs::read_to_string(&canon).map_err(|e| {
        pack_err(format!(
            "failed to read pack {what} file '{}': {e}",
            canon.display()
        ))
    })
}

/// 把包内相对路径解析为绝对路径，拦截 `..` 逃逸与缺失文件。
fn resolve_pack_path(root: &Path, rel: &Path, what: &str) -> Result<PathBuf, SqlGuardError> {
    if rel.is_absolute() {
        return Err(pack_err(format!(
            "pack {what} path must be relative to the pack root, got '{}'",
            rel.display()
        )));
    }
    let canon_root = canonical_or_self(root);
    let joined = root.join(rel);
    let canon = joined.canonicalize().map_err(|e| {
        pack_err(format!(
            "pack {what} file not found: '{}' ({e})",
            joined.display()
        ))
    })?;
    if !canon.starts_with(&canon_root) {
        return Err(pack_err(format!(
            "pack {what} path '{}' escapes the pack root '{}'",
            rel.display(),
            root.display()
        )));
    }
    Ok(canon)
}

/// 解析包内规则脚本路径（相对包根），返回可直接使用的绝对路径。
fn resolve_pack_script(
    root: &Path,
    rel: &Path,
    pack: &str,
    rule_id: &str,
) -> Result<PathBuf, SqlGuardError> {
    resolve_pack_path(root, rel, "script").map_err(|e| {
        pack_err(format!(
            "rule pack '{pack}', rule '{rule_id}': {}",
            strip_config_prefix(&e.to_string())
        ))
    })
}

/// 去掉 `SqlGuardError::ConfigError` 的 Display 前缀，避免嵌套报错时重复。
fn strip_config_prefix(msg: &str) -> &str {
    msg.strip_prefix("Config error: ").unwrap_or(msg)
}

/// `overrides` 的 id 是否命中某条规则。规范 id 与裸 id 均可。
fn override_matches(pattern: &str, canonical_id: &str) -> bool {
    pattern == canonical_id || pattern == short_id(canonical_id)
}

/// 应用 `[rule_packs.overrides]`：只允许改 `enabled` / `severity` / `params`。
///
/// 裸 id 命中多条时按合并顺序取**最后一条**（即优先级最高者）。
/// 未命中任何规则 → 报错并给出可用 id 提示（防 typo）。
fn apply_overrides(
    rules: &mut [RuleConfig],
    overrides: &[RuleOverride],
) -> Result<(), SqlGuardError> {
    for ov in overrides {
        let Some(idx) = rules.iter().rposition(|r| override_matches(&ov.id, &r.id)) else {
            let want = short_id(&ov.id).to_lowercase();
            let mut similar: Vec<&str> = rules
                .iter()
                .map(|r| r.id.as_str())
                .filter(|id| id.to_lowercase().contains(&want))
                .collect();
            if similar.is_empty() {
                similar = rules.iter().map(|r| r.id.as_str()).take(10).collect();
            }
            return Err(pack_err(format!(
                "[rule_packs.overrides] references unknown rule id '{}'. Known ids: {}",
                ov.id,
                similar.join(", ")
            )));
        };

        if let Some(enabled) = ov.enabled {
            rules[idx].enabled = enabled;
        }
        if let Some(severity) = &ov.severity {
            rules[idx].severity = severity.clone();
        }
        if let Some(params) = &ov.params {
            rules[idx].params = Some(params.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(api_version: Option<u32>, engine: Option<&str>) -> RulePackMeta {
        RulePackMeta {
            name: Some("rules-test".to_string()),
            version: None,
            namespace: None,
            api_version,
            engine: engine.map(str::to_string),
            description: None,
            license: None,
            helpers: None,
        }
    }

    // ===== api_version =====

    #[test]
    fn api_version_defaults_to_one() {
        assert_eq!(meta(None, None).declared_api_version(), 1);
        assert!(meta(None, None).validate().is_ok());
    }

    #[test]
    fn api_version_at_or_below_engine_is_ok() {
        assert!(meta(Some(1), None).validate().is_ok());
        assert!(meta(Some(RULE_API_VERSION), None)
            .api_version_error()
            .is_none());
    }

    #[test]
    fn api_version_above_engine_is_rejected_with_hint() {
        let m = meta(Some(RULE_API_VERSION + 1), None);
        let err = m.validate().expect_err("must reject future api_version");
        assert!(err.contains("requires rule API version"), "got: {err}");
        assert!(err.contains("hint:"), "error must carry an actionable hint");
    }

    // ===== namespace =====

    #[test]
    fn namespace_valid_charset_accepted() {
        for ns in ["gaussdb", "core2", "my-pack", "a_b-c9"] {
            let mut m = meta(None, None);
            m.namespace = Some(ns.to_string());
            assert!(m.namespace_error().is_none(), "should accept '{ns}'");
        }
    }

    #[test]
    fn namespace_invalid_charset_rejected() {
        for ns in ["GaussDB", "gauss db", "gauss.db", "ns:sub", ""] {
            let mut m = meta(None, None);
            m.namespace = Some(ns.to_string());
            assert!(m.namespace_error().is_some(), "should reject '{ns}'");
        }
    }

    // ===== engine range =====

    #[test]
    fn engine_range_and_comparators() {
        assert!(engine_range_satisfied(">=0.2.7 <0.3", "0.2.7").unwrap());
        assert!(engine_range_satisfied(">=0.2.7 <0.3", "0.2.99").unwrap());
        assert!(!engine_range_satisfied(">=0.2.7 <0.3", "0.3.0").unwrap());
        assert!(!engine_range_satisfied(">=0.2.7 <0.3", "0.2.6").unwrap());
        // 裸版本号 = 精确匹配；缺省位补 0
        assert!(engine_range_satisfied("0.2.7", "0.2.7").unwrap());
        assert!(!engine_range_satisfied("0.2.7", "0.2.8").unwrap());
        assert!(engine_range_satisfied("<0.3", "0.2.7").unwrap());
        // 逗号分隔同样是 AND
        assert!(engine_range_satisfied(">=0.2.0, <0.3", "0.2.7").unwrap());
    }

    #[test]
    fn engine_range_rejects_or_and_garbage() {
        assert!(parse_engine_range(">=0.2.7 || <0.1").is_err());
        assert!(parse_engine_range("").is_err());
        assert!(parse_engine_range(">=abc").is_err());
        assert!(parse_engine_range(">=1.2.3.4").is_err());
    }

    #[test]
    fn engine_mismatch_message_only_when_out_of_range() {
        let m = meta(None, Some(">=0.2.7 <0.3"));
        assert!(m.engine_mismatch("0.2.7").is_none());
        let msg = m.engine_mismatch("0.3.1").expect("must warn");
        assert!(msg.contains("--strict-engine"), "got: {msg}");
        // 未声明 engine → 永不告警
        assert!(meta(None, None).engine_mismatch("9.9.9").is_none());
    }

    #[test]
    fn validate_rejects_unparsable_engine_range() {
        let err = meta(None, Some("~> 0.2"))
            .validate()
            .expect_err("must reject");
        assert!(err.contains("invalid engine range"), "got: {err}");
    }

    #[test]
    fn version_parsing_ignores_prerelease_suffix() {
        assert_eq!(
            parse_version("0.2.7-rc.1").unwrap(),
            Version {
                major: 0,
                minor: 2,
                patch: 7
            }
        );
        assert_eq!(
            parse_version("1.2+build9").unwrap(),
            Version {
                major: 1,
                minor: 2,
                patch: 0
            }
        );
    }

    // ===== M2：命名空间 / 合并 / 覆盖（纯函数） =====

    fn rule(id: &str, severity: &str) -> RuleConfig {
        RuleConfig {
            id: id.to_string(),
            name: "r".to_string(),
            group: None,
            description: None,
            enabled: true,
            script_path: PathBuf::from("x.rhai"),
            applies_to: vec!["dml".to_string()],
            severity: severity.to_string(),
            params: None,
        }
    }

    #[test]
    fn short_id_strips_namespace_only() {
        assert_eq!(short_id("gaussdb:GDB001"), "GDB001");
        assert_eq!(short_id("DDL001"), "DDL001");
    }

    #[test]
    fn qualify_rule_id_applies_namespace_and_rejects_colon() {
        let mut r = rule("GDB001", "warning");
        qualify_rule_id(&mut r, Some("gaussdb"), "pack 'x'").unwrap();
        assert_eq!(r.id, "gaussdb:GDB001");

        let mut r = rule("DDL001", "warning");
        qualify_rule_id(&mut r, None, "project rules").unwrap();
        assert_eq!(r.id, "DDL001", "no namespace → bare id unchanged");

        let mut r = rule("a:b", "warning");
        assert!(
            qualify_rule_id(&mut r, Some("ns"), "pack 'x'").is_err(),
            "':' in a rule id must be rejected"
        );
    }

    #[test]
    fn override_matches_canonical_and_bare() {
        assert!(override_matches("gaussdb:GDB001", "gaussdb:GDB001"));
        assert!(override_matches("GDB001", "gaussdb:GDB001"));
        assert!(override_matches("DDL001", "DDL001"));
        assert!(!override_matches("OTHER", "gaussdb:GDB001"));
    }

    #[test]
    fn apply_overrides_last_match_wins_and_unknown_id_fails() {
        let mut rules = vec![rule("a:R001", "warning"), rule("b:R001", "warning")];
        // 裸 id 命中两条 → 取最后一条（合并顺序中优先级最高者）
        let ov = RuleOverride {
            id: "R001".to_string(),
            enabled: Some(false),
            severity: Some("error".to_string()),
            params: Some(toml::Value::Integer(7)),
        };
        apply_overrides(&mut rules, &[ov]).unwrap();
        assert!(rules[0].enabled, "the earlier rule must stay untouched");
        assert!(!rules[1].enabled);
        assert_eq!(rules[1].severity, "error");
        assert_eq!(
            rules[1].params.as_ref().and_then(|v| v.as_integer()),
            Some(7)
        );

        // 未命中任何规则 → 报错（防 typo）
        let bad = RuleOverride {
            id: "NOPE".to_string(),
            enabled: None,
            severity: None,
            params: None,
        };
        let err = apply_overrides(&mut rules, &[bad]).expect_err("unknown id must fail");
        assert!(err.to_string().contains("unknown rule id"), "got: {err}");
    }

    #[test]
    fn layered_insert_replaces_in_place_and_notes() {
        let mut layer = Layered::default();
        let mut notes = Vec::new();
        layer.insert(rule("DDL001", "warning"), "pack 'a'", &mut notes);
        layer.insert(rule("core:DDL001", "warning"), "pack 'b'", &mut notes);
        assert_eq!(layer.rules.len(), 2, "different ids coexist");

        layer.insert(rule("DDL001", "error"), "project rules", &mut notes);
        assert_eq!(
            layer.rules.len(),
            2,
            "same id replaces instead of appending"
        );
        assert_eq!(layer.rules[0].severity, "error");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("DDL001"));
        assert_eq!(
            layer.conflicts.len(),
            1,
            "conflict must be recorded for --strict-ids"
        );
        assert!(layer.conflicts[0].contains("project rules"));
    }
}
