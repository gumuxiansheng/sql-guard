use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;

use sqlguard::cache;
use sqlguard::checker::classification;
use sqlguard::checker::directory;
use sqlguard::checker::encoding;
use sqlguard::cli::{Cli, Commands, RulesAction};
use sqlguard::config::{CheckDialect, Config};
use sqlguard::error::{SqlGuardError, Violation};
use sqlguard::git_diff;
use sqlguard::mapper;
use sqlguard::replay_export;
use sqlguard::reporter;
use sqlguard::rule::engine;
use sqlguard::rule::lock;
use sqlguard::rule::pack;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 在大栈线程中运行：真实业务 mapper 可含数百甚至上千个 <if>，AllTrue 渲染后
    // SQL 可达数万行，sqlparser 递归下降解析器对超长输入需要较大栈空间。
    let handle = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024) // 16 MB
        .spawn(|| main_inner().map_err(|e| e.to_string()))
        .expect("failed to spawn main thread");
    match handle.join().expect("main thread panicked") {
        Ok(()) => Ok(()),
        Err(msg) => Err(msg.into()),
    }
}

fn main_inner() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Check {
            path,
            config,
            format,
            output_dir,
            rules,
            groups,
            exclude_rules,
            exclude_groups,
            dialect,
            dialect_fallback,
            cache,
            no_cache,
            encoding,
            strict_engine,
            strict_ids,
            locked,
            no_lock,
        } => {
            let config_path = config
                .clone()
                .unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
            let explicit_config = config.is_some();
            run_check(
                &path,
                &config_path,
                explicit_config,
                &format,
                output_dir.as_deref(),
                &rules,
                &groups,
                &exclude_rules,
                &exclude_groups,
                dialect.as_deref(),
                dialect_fallback.as_deref(),
                cache,
                no_cache,
                encoding.as_deref(),
                strict_engine,
                strict_ids,
                locked,
                no_lock,
            )?;
        }
        Commands::Init {
            path,
            force,
            with_default_pack,
        } => {
            if with_default_pack {
                run_init_with_default_pack(&path, force)?;
            } else {
                run_init(&path, force)?;
            }
        }
        Commands::Rules { action } => {
            run_rules_subcommand(action)?;
        }
        Commands::CheckDiff {
            base,
            path,
            config,
            format,
            output_dir,
            rules,
            groups,
            exclude_rules,
            exclude_groups,
            dialect,
            dialect_fallback,
            encoding,
            strict_engine,
            strict_ids,
            locked,
            no_lock,
        } => {
            let config_path = config
                .clone()
                .unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
            let explicit_config = config.is_some();
            run_check_diff(
                &base,
                &path,
                &config_path,
                explicit_config,
                &format,
                output_dir.as_deref(),
                &rules,
                &groups,
                &exclude_rules,
                &exclude_groups,
                dialect.as_deref(),
                dialect_fallback.as_deref(),
                encoding.as_deref(),
                strict_engine,
                strict_ids,
                locked,
                no_lock,
            )?;
        }
        Commands::ReplayExport {
            path,
            config,
            output_dir,
            types,
            base,
            encoding,
        } => {
            let config_path = config
                .clone()
                .unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
            let explicit_config = config.is_some();
            run_replay_export(
                &path,
                &config_path,
                explicit_config,
                &output_dir,
                &types,
                base.as_deref(),
                encoding.as_deref(),
            )?;
        }
        Commands::GenRollback {
            path,
            config,
            output_dir,
            dialect,
            lock_scope,
            lock_timeout,
            accept_table_lock_risk,
            fail_on_warning,
            allow_partial,
            review_report,
            encoding,
        } => {
            let config_path = config
                .clone()
                .unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
            let explicit_config = config.is_some();
            let code = run_gen_rollback(
                &path,
                &config_path,
                explicit_config,
                &output_dir,
                dialect.as_deref(),
                lock_scope.as_deref(),
                lock_timeout,
                accept_table_lock_risk,
                fail_on_warning,
                allow_partial,
                review_report,
                encoding.as_deref(),
            )?;
            std::process::exit(code);
        }
        Commands::Explain {
            path,
            config,
            dialect,
            dialect_fallback,
            mapper: is_mapper,
            json,
            encoding,
        } => {
            let config_path = config
                .clone()
                .unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
            let explicit_config = config.is_some();
            run_explain(
                &path,
                &config_path,
                explicit_config,
                dialect.as_deref(),
                dialect_fallback.as_deref(),
                is_mapper,
                json,
                encoding.as_deref(),
            )?;
        }
    }

    Ok(())
}

/// `sqlguard explain` 入口：解析 SQL/Mapper 并输出 AST 结构。
#[allow(clippy::too_many_arguments)]
fn run_explain(
    target_path: &Path,
    config_path: &Path,
    explicit_config: bool,
    dialect_override: Option<&str>,
    dialect_fallback_override: Option<&str>,
    is_mapper: bool,
    json: bool,
    encoding_override: Option<&str>,
) -> Result<(), SqlGuardError> {
    // 加载配置（复用 load_config）
    let (config, _config_dir) = load_config(config_path, explicit_config)?;

    // 解析方言
    let dialect = if let Some(d) = dialect_override {
        CheckDialect::parse_dialect(d)
    } else {
        config.dialect
    };

    let fallback = if let Some(fb) = dialect_fallback_override {
        Some(CheckDialect::parse_dialect(fb))
    } else if let Some(cfg_fb) = config.dialect_fallback {
        Some(cfg_fb)
    } else {
        dialect.default_fallback()
    };

    // 读取文件
    let encoding = encoding_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| config.scan.encoding.clone());

    if target_path.is_dir() {
        return Err(SqlGuardError::CheckError(format!(
            "explain requires a single file, got directory: '{}'",
            target_path.display()
        )));
    }

    let _content = sqlguard::encoding::read_to_string(target_path, &encoding)
        .map_err(SqlGuardError::CheckError)?;

    sqlguard::explain::run_explain(target_path, dialect, fallback, is_mapper, json)?;

    Ok(())
}

// ===== 公共辅助函数（任务 3：消除 run_check / run_check_diff 重复逻辑）=====

/// 加载配置文件。
///
/// `explicit` 表示用户是否通过 `--config` 显式指定了配置文件路径。
/// 当配置文件找不到时：
/// - 显式指定（`--config <path>`）但文件不存在 → 直接报错，避免静默退回默认配置，
///   否则用户写在配置里的 `[dialect]` 等设置会被「内置默认配置（dialect=generic）」
///   悄悄覆盖而毫无提示，表现为「配置不生效，只能靠命令行才生效」。
/// - 未显式指定（使用默认的 `sqlguard.toml`）→ 先尝试同目录的 `sqlguard.toml`，
///   仍找不到则退回内置默认配置并打印警告，保留「零配置」开箱即用的行为。
fn load_config(config_path: &Path, explicit: bool) -> Result<(Config, PathBuf), SqlGuardError> {
    // `parent()` 对裸文件名（如默认的 "sqlguard.toml"）返回**空路径**而非 "."。
    // 空路径无法 canonicalize，会让 `sqlguard.lock` 的 source 退化成机器相关的
    // 绝对路径（`path://?/C:/...`），锁文件因此不可移植。这里统一兜底为 "."。
    let config_parent = match config_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let config_dir = config_parent
        .canonicalize()
        .unwrap_or_else(|_| config_parent.clone());
    let config = if config_path.exists() {
        Config::load(config_path)?
    } else if explicit {
        return Err(SqlGuardError::ConfigError(format!(
            "Config file not found: '{}'.\n\
             The check would otherwise fall back to a built-in default config with dialect=generic,\n\
             which ignores your [dialect] setting. Check the --config path (note: Windows paths like\n\
             /tmp/... resolve to C:\\tmp\\..., not the Git-Bash /tmp).",
            config_path.display()
        )));
    } else if config_dir.join("sqlguard.toml").exists() {
        Config::load(&config_dir.join("sqlguard.toml"))?
    } else {
        eprintln!(
            "Warning: config file '{}' not found; using built-in default config (dialect = generic).\n\
             Set [dialect] in your config file (or pass --dialect) to change the SQL dialect.",
            config_path.display()
        );
        generate_default_config()
    };
    Ok((config, config_dir))
}

/// M1/M2：规则包（`[pack]` / `[rule_packs]`）兼容诊断。
///
/// `api_version` 高于引擎支持属硬错误，已在 `Config::load` 拦截，这里不重复。
/// 本函数只处理非致命信息：
/// - 规则包提示（如 id 被高优先级来源覆盖）→ 仅在交互终端输出，避免污染 CI 日志；
/// - `--strict-ids` 且存在跨来源同 id 冲突 → error；
/// - `engine` 版本范围不满足当前引擎 → 默认 warning，`--strict-engine` 时 error。
///
/// 诊断写 stderr，不污染报告输出。
fn check_rule_pack_compat(
    config: &Config,
    strict_engine: bool,
    strict_ids: bool,
) -> Result<(), SqlGuardError> {
    emit_notes(config.rule_pack_notes());
    if strict_ids && !config.rule_pack_id_conflicts().is_empty() {
        return Err(SqlGuardError::ConfigError(format!(
            "--strict-ids: duplicate rule ids across rule sources:\n  {}",
            config.rule_pack_id_conflicts().join("\n  ")
        )));
    }
    // engine 范围：本地 [pack] + 每个生效规则包都参与诊断（可解析性已在加载时硬校验）。
    let mismatches = config.rule_pack_engine_mismatches();
    if strict_engine && !mismatches.is_empty() {
        return Err(SqlGuardError::ConfigError(format!(
            "--strict-engine:\n  {}",
            mismatches.join("\n  ")
        )));
    }
    for msg in &mismatches {
        eprintln!("Warning: {msg}");
    }
    Ok(())
}

/// 终端展示路径：去掉 Windows `canonicalize()` 引入的 `\\?\` 前缀（仅影响显示）。
fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix("\\\\?\\").unwrap_or(&s).to_string()
}

/// 提示仅在交互终端输出，避免污染 CI 日志。
fn emit_notes(notes: &[String]) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    for note in notes {
        eprintln!("Note: {note}");
    }
}

/// M3：`--locked` / `--no-lock` → 锁校验强度。
fn lock_mode(locked: bool, no_lock: bool) -> Result<lock::LockMode, SqlGuardError> {
    if locked && no_lock {
        return Err(SqlGuardError::CheckError(
            "--locked and --no-lock are mutually exclusive".to_string(),
        ));
    }
    Ok(if no_lock {
        lock::LockMode::Off
    } else if locked {
        lock::LockMode::Strict
    } else {
        lock::LockMode::Auto
    })
}

// ===== sqlguard rules（M3） =====

fn run_rules_subcommand(action: RulesAction) -> Result<(), SqlGuardError> {
    match action {
        RulesAction::List { config, json } => rules_list(config.as_deref(), json),
        RulesAction::Lock { config } => rules_lock(config.as_deref()),
        RulesAction::Verify { config, locked } => rules_verify(config.as_deref(), locked),
        RulesAction::Vendor { config, output_dir } => {
            rules_vendor(config.as_deref(), output_dir.as_deref())
        }
        RulesAction::Add { spec, config } => rules_add(&spec, config.as_deref()),
    }
}

/// 解析 `-c/--config`（默认 `sqlguard.toml`）并加载配置，返回
/// `(config, config_dir, config_path, 是否显式指定)`。
fn load_for_rules(
    config: Option<&Path>,
) -> Result<(Config, PathBuf, PathBuf, bool), SqlGuardError> {
    let config_path = config
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
    let explicit = config.is_some();
    let (cfg, dir) = load_config(&config_path, explicit)?;
    Ok((cfg, dir, config_path, explicit))
}

/// 规则来源标签：`<pack>@<version>`（包内规则）或 `project`（本地规则）。
fn rule_source_label(cfg: &Config, script_path: &Path) -> String {
    match cfg.pack_name_for(script_path) {
        Some(name) => cfg
            .resolved_packs
            .iter()
            .find(|p| p.name == name)
            .map(|p| format!("{}@{}", p.name, p.version))
            .unwrap_or_else(|| name.to_string()),
        None => "project".to_string(),
    }
}

fn rules_list(config: Option<&Path>, json: bool) -> Result<(), SqlGuardError> {
    let (cfg, _dir, _path, _explicit) = load_for_rules(config)?;

    if json {
        let packs: Vec<serde_json::Value> = cfg
            .resolved_packs
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "version": p.version,
                    "namespace": p.namespace,
                    "api_version": p.api_version,
                    "rule_count": p.rule_count,
                    "root": display_path(&p.root),
                    "has_helpers": p.helpers.is_some(),
                })
            })
            .collect();
        let rules: Vec<serde_json::Value> = cfg
            .rules
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "name": r.name,
                    "group": r.group,
                    "severity": r.severity,
                    "enabled": r.enabled,
                    "applies_to": r.applies_to,
                    "source": rule_source_label(&cfg, &r.script_path),
                })
            })
            .collect();
        let doc = serde_json::json!({
            "packs": packs,
            "rule_count": cfg.rules.len(),
            "rules": rules,
            "notices": cfg.rule_pack_notes(),
        });
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|e| SqlGuardError::CheckError(format!("failed to serialize rules: {e}")))?;
        println!("{text}");
        return Ok(());
    }

    if cfg.resolved_packs.is_empty() {
        println!("No rule packs configured; rules come from sqlguard.rules.toml only.");
    } else {
        println!("Rule packs (declaration order decides precedence, later wins):");
        for p in &cfg.resolved_packs {
            println!(
                "  {:<24} {:<10} namespace={:<10} api_version={} rules={} helpers={}",
                p.name,
                p.version,
                p.namespace.as_deref().unwrap_or("-"),
                p.api_version,
                p.rule_count,
                if p.helpers.is_some() { "yes" } else { "no" },
            );
        }
    }

    println!();
    let id_w = cfg
        .rules
        .iter()
        .map(|r| r.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let grp_w = cfg
        .rules
        .iter()
        .map(|r| r.group.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(5)
        .max(5);
    println!(
        "{:<id_w$}  {:<grp_w$}  {:<7}  {:<7}  SOURCE",
        "ID", "GROUP", "SEV", "ENABLED"
    );
    for r in &cfg.rules {
        println!(
            "{:<id_w$}  {:<grp_w$}  {:<7}  {:<7}  {}",
            r.id,
            r.group.as_deref().unwrap_or("-"),
            r.severity,
            if r.enabled { "yes" } else { "no" },
            rule_source_label(&cfg, &r.script_path),
        );
    }
    println!();
    println!("{} rule(s) effective", cfg.rules.len());
    for note in cfg.rule_pack_notes() {
        println!("note: {note}");
    }
    Ok(())
}

fn rules_lock(config: Option<&Path>) -> Result<(), SqlGuardError> {
    let (cfg, dir, _path, _explicit) = load_for_rules(config)?;
    if cfg.resolved_packs.is_empty() {
        println!("No rule packs configured; nothing to lock.");
        return Ok(());
    }
    let lock_file = lock::build_lock(&cfg, &dir)?;
    let path = lock::write_lock(&lock_file, &dir)?;
    println!("Wrote {}", display_path(&path));
    for p in &lock_file.packs {
        println!("  {:<24} {:<10} {}", p.name, p.version, p.source);
    }
    Ok(())
}

fn rules_verify(config: Option<&Path>, locked: bool) -> Result<(), SqlGuardError> {
    let (cfg, dir, _path, _explicit) = load_for_rules(config)?;
    for msg in cfg.rule_pack_engine_mismatches() {
        eprintln!("Warning: {msg}");
    }
    let mode = if locked {
        lock::LockMode::Strict
    } else {
        lock::LockMode::Auto
    };
    for note in lock::verify_lock(&cfg, &dir, mode)? {
        println!("note: {note}");
    }
    for note in cfg.rule_pack_notes() {
        println!("note: {note}");
    }
    println!(
        "OK: {} rule pack(s), {} rule(s) effective",
        cfg.resolved_packs.len(),
        cfg.rules.len()
    );
    Ok(())
}

fn rules_vendor(config: Option<&Path>, output_dir: Option<&Path>) -> Result<(), SqlGuardError> {
    let (cfg, dir, _path, _explicit) = load_for_rules(config)?;
    if cfg.resolved_packs.is_empty() {
        return Err(SqlGuardError::ConfigError(
            "no rule packs to vendor (declare them under [rule_packs].packs first)".to_string(),
        ));
    }
    let dest_root = match output_dir {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => dir.join(p),
        None => dir.join("vendor").join("rules"),
    };
    for p in &cfg.resolved_packs {
        let dest = dest_root.join(&p.name);
        copy_dir(&p.root, &dest)?;
        println!(
            "vendored {}@{} → {}",
            p.name,
            p.version,
            display_path(&dest)
        );
    }
    let rel = dest_root
        .strip_prefix(&dir)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| dest_root.to_string_lossy().replace('\\', "/"));
    let covered = cfg
        .rule_packs
        .search_paths
        .iter()
        .any(|s| s.trim_end_matches('/') == rel);
    if !covered {
        println!();
        println!("Add the vendored directory to [rule_packs].search_paths so it is found:");
        println!("    search_paths = [\"{rel}\"]");
    }
    Ok(())
}

/// 待追加的 `[[rule_packs.packs]]` 片段。
struct PackEntry {
    name: String,
    toml: String,
}

fn rules_add(spec: &str, config: Option<&Path>) -> Result<(), SqlGuardError> {
    let (cfg, dir, config_path, explicit) = load_for_rules(config)?;
    if !explicit && !config_path.exists() {
        return Err(SqlGuardError::ConfigError(format!(
            "config file '{}' not found; pass --config or run `sqlguard init` first",
            config_path.display()
        )));
    }

    let entry = pack_entry_toml(spec, &dir)?;
    if cfg.resolved_packs.iter().any(|p| p.name == entry.name) {
        return Err(SqlGuardError::ConfigError(format!(
            "[rule_packs] already declares a pack named '{}' in '{}'",
            entry.name,
            config_path.display()
        )));
    }

    let original = fs::read_to_string(&config_path).map_err(SqlGuardError::IoError)?;
    let mut updated = original.clone();
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&entry.toml);
    fs::write(&config_path, &updated).map_err(SqlGuardError::IoError)?;

    // 追加后必须仍能解析（含包定位与兼容校验）；否则回滚，保证配置文件永不被改坏。
    match load_config(&config_path, true) {
        Ok((cfg2, dir2)) => {
            let lock_file = lock::build_lock(&cfg2, &dir2)?;
            let lock_path = lock::write_lock(&lock_file, &dir2)?;
            println!(
                "Added rule pack '{}' to {}",
                entry.name,
                config_path.display()
            );
            println!("Wrote {}", display_path(&lock_path));
            Ok(())
        }
        Err(err) => {
            fs::write(&config_path, &original).map_err(SqlGuardError::IoError)?;
            Err(SqlGuardError::ConfigError(format!(
                "appending the pack entry would make '{}' invalid; the file has been restored.\n\
                 cause: {err}\n\
                 hint: if packs are declared inline (`packs = [...]`), add the entry there by hand.",
                config_path.display()
            )))
        }
    }
}

/// 依据 SPEC 构造待追加的 TOML：`name` / `name@version` / 目录路径。
fn pack_entry_toml(spec: &str, config_dir: &Path) -> Result<PackEntry, SqlGuardError> {
    let looks_like_path = spec.contains('/') || spec.contains('\\');
    if looks_like_path {
        let given = PathBuf::from(spec);
        let abs = if given.is_absolute() {
            given
        } else {
            config_dir.join(&given)
        };
        let name = pack::read_pack_name(&abs)?;
        let rel = spec.replace('\\', "/");
        let toml = format!("\n[[rule_packs.packs]]\nname = \"{name}\"\npath = \"{rel}\"\n");
        Ok(PackEntry { name, toml })
    } else {
        let (name, version) = match spec.split_once('@') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (spec.to_string(), None),
        };
        let mut toml = format!("\n[[rule_packs.packs]]\nname = \"{name}\"\n");
        if let Some(v) = &version {
            toml.push_str(&format!("version = \"{v}\"\n"));
        }
        Ok(PackEntry { name, toml })
    }
}

/// 递归复制目录（跳过 `.git` 等版本控制目录），用于 `rules vendor`。
fn copy_dir(src: &Path, dest: &Path) -> Result<(), SqlGuardError> {
    fs::create_dir_all(dest).map_err(SqlGuardError::IoError)?;
    let entries = fs::read_dir(src).map_err(SqlGuardError::IoError)?;
    for entry in entries {
        let entry = entry.map_err(SqlGuardError::IoError)?;
        let name = entry.file_name();
        let file_type = entry.file_type().map_err(SqlGuardError::IoError)?;
        let target = dest.join(&name);
        if file_type.is_dir() {
            if name.to_string_lossy() == ".git" {
                continue;
            }
            copy_dir(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target).map_err(SqlGuardError::IoError)?;
        }
    }
    Ok(())
}

fn resolve_absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn resolve_output_dir(output_dir: Option<&Path>, target_dir: &Path) -> PathBuf {
    output_dir
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
            }
        })
        .unwrap_or_else(|| target_dir.to_path_buf())
}

fn parse_formats(format: &str) -> Vec<&str> {
    match format {
        "all" => vec!["plain", "json", "html", "sarif"],
        f => f.split(',').map(|s| s.trim()).collect(),
    }
}

fn check_files(
    config: &Config,
    config_dir: &Path,
    filter: &engine::RuleFilter,
    engine_instance: &rhai::Engine,
    sql_files: &[PathBuf],
    mapper_files: &[PathBuf],
    cache: &mut cache::FileCache,
) -> Result<(Vec<Violation>, usize), SqlGuardError> {
    let mut all_violations = Vec::new();
    let files_checked = sql_files.len() + mapper_files.len();

    // 脚本模式
    for file_path in sql_files {
        let classification_result =
            classification::classify_file(file_path, &config.classification)?;
        // 编码 / 换行符检查属于轻量的文件属性检查，不进缓存（每次都跑）
        all_violations.extend(encoding::check_file(
            file_path,
            &classification_result.script_type,
            &config.file_check,
            filter,
            &config.scan.encoding,
        ));

        // 查缓存：命中则跳过读文件 + 解析 + 规则执行
        if let Some(cached) = cache.get(file_path) {
            all_violations.extend(cached);
            continue;
        }

        let sql_content = sqlguard::encoding::read_to_string(file_path, &config.scan.encoding)
            .map_err(SqlGuardError::CheckError)?;
        let violations = engine::run_rules_for_file(
            engine_instance,
            file_path,
            &sql_content,
            &classification_result.script_type,
            config,
            config_dir,
            filter,
            0,
            false,
        )?;
        // 写入缓存（clone 一份，原始 violations 用于本次输出）
        cache.insert(file_path, violations.clone());
        all_violations.extend(violations);
    }

    // Mapper 模式
    if config.mapper.enabled {
        for file_path in mapper_files {
            let mapper_script_type =
                classification::classify_file(file_path, &config.classification)
                    .map(|r| r.script_type)
                    .unwrap_or_else(|_| "mapper".to_string());
            all_violations.extend(encoding::check_file(
                file_path,
                &mapper_script_type,
                &config.file_check,
                filter,
                &config.scan.encoding,
            ));

            // 查缓存：命中则跳过 XML 解析 + 逐条规则执行
            if let Some(cached) = cache.get(file_path) {
                all_violations.extend(cached);
                continue;
            }

            let extracted = match mapper::extract_sql_from_xml(file_path, &config.scan.encoding) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to parse mapper XML '{}': {}",
                        file_path.display(),
                        e
                    );
                    continue;
                }
            };
            let mut file_violations = Vec::new();
            for sql in extracted {
                let script_type = mapper::map_statement_type(
                    &sql.statement_type,
                    &config.mapper.statement_type_mapping,
                );
                // 行号映射基准：规则产出的行号是「渲染后 SQL 内行号」，加上
                // `<select>` 标签起始行偏移即得 XML 行号。注意这是**近似值**——
                // 渲染压缩了 `<if>`/`<where>` 等动态标签行，含多行动态标签的语句
                // 违规行号会有 ± 几行偏差（详见 `ExtractedSql::raw_xml_line`）。
                let line_offset = sql.raw_xml_line.saturating_sub(1);
                // 主渲染（所有 <if> 取真，覆盖面最大）若解析不过，按
                // ExclusiveNested → FirstBranch 顺序尝试备用渲染（互斥 if 折叠），
                // 并对候选先做标点补丁（逗号前置/尾逗号等渲染噪音）。三份都不过时
                // 回退到做了补丁的主渲染，保证 PARSE 错误指向覆盖面最大的那份。
                let chosen = pick_parseable_candidate(&sql, config);
                let violations = engine::run_rules_for_file(
                    engine_instance,
                    file_path,
                    &chosen,
                    script_type,
                    config,
                    config_dir,
                    filter,
                    line_offset,
                    sql.has_dynamic,
                )?;
                file_violations.extend(violations);
            }
            cache.insert(file_path, file_violations.clone());
            all_violations.extend(file_violations);
        }
    }

    Ok((all_violations, files_checked))
}

/// SQL 能否被当前方言（含回退链）**完整**解析：无 tokenize 错误、无 PARSE_ERROR 语句。
///
/// 供 mapper 模式在「主渲染 / 备用渲染」间做静默选择，避免为了试探而先打一遍
/// 解析失败的 warning。
fn sql_parses_cleanly(sql: &str, config: &Config) -> bool {
    let ast = engine::parser::parse_sql_to_ast_fb(sql, config.dialect, config.dialect_fallback);
    ast.parse_error.is_none() && !ast.statements.iter().any(|s| s.kind == "PARSE_ERROR")
}

/// 从一条 mapper 语句的多份渲染候选里挑出**第一份能完整解析**的，并对候选先做
/// 标点补丁（逗号前置 / 尾逗号等渲染噪音）。
///
/// 候选顺序：主渲染 `processed_sql`（AllTrue，覆盖面最大）→ `processed_sql_alt`
/// （ExclusiveNested，折叠嵌套互斥 if）→ `processed_sql_alt2`（FirstBranch，折叠同层
/// 互斥 if）。三份都解析不过时回退到「做了补丁的主渲染」，保证规则引擎至少看到
/// 清理后的文本，且 PARSE 错误指向覆盖面最大的那份。
fn pick_parseable_candidate(sql: &mapper::parser::ExtractedSql, config: &Config) -> String {
    let candidates = [
        Some(sql.processed_sql.as_str()),
        sql.processed_sql_alt.as_deref(),
        sql.processed_sql_alt2.as_deref(),
    ];
    // 超长 SQL（常见于数百个 <if> 的真实 mapper）：sqlparser 递归下降解析器
    // 对超长输入会爆栈。跳过候选选择，直接返回 repair 后的主渲染，让规则引擎
    // 至少能看到清理后的文本；如果主渲染本身解析不过，run_rules_for_file 会
    // 正常报 PARSE 违规。
    const PARSE_PROBE_MAX_BYTES: usize = 50_000;
    for c in candidates.into_iter().flatten() {
        let repaired = mapper::dynamic::repair_dynamic_artifacts(c);
        if repaired.len() > PARSE_PROBE_MAX_BYTES {
            continue;
        }
        if !repaired.trim().is_empty() && sql_parses_cleanly(&repaired, config) {
            return repaired;
        }
    }
    // 所有候选都过长或都解析不过：用主渲染的 repair 结果，保证规则引擎有文本可查
    let main_repaired = mapper::dynamic::repair_dynamic_artifacts(&sql.processed_sql);
    if main_repaired.trim().is_empty() {
        // 主渲染为空（极端情况），退回原始主渲染
        sql.processed_sql.clone()
    } else {
        main_repaired
    }
}

// ===== run_check =====

#[allow(clippy::too_many_arguments)]
fn run_check(
    target_dir: &Path,
    config_path: &Path,
    explicit_config: bool,
    format: &str,
    output_dir: Option<&Path>,
    rules: &Option<String>,
    groups: &Option<String>,
    exclude_rules: &Option<String>,
    exclude_groups: &Option<String>,
    dialect_override: Option<&str>,
    dialect_fallback_override: Option<&str>,
    cache_flag: bool,
    no_cache_flag: bool,
    encoding_override: Option<&str>,
    strict_engine: bool,
    strict_ids: bool,
    locked: bool,
    no_lock: bool,
) -> Result<(), SqlGuardError> {
    if cache_flag && no_cache_flag {
        return Err(SqlGuardError::CheckError(
            "--cache and --no-cache are mutually exclusive".to_string(),
        ));
    }
    let (mut config, config_dir) = load_config(config_path, explicit_config)?;
    // M1：规则包兼容诊断（api_version 已在 Config::load 硬校验）。
    check_rule_pack_compat(&config, strict_engine, strict_ids)?;
    // M3：锁文件校验（默认比对 name+version；--locked 额外校验 checksum）。
    let lock_notes = lock::verify_lock(&config, &config_dir, lock_mode(locked, no_lock)?)?;
    emit_notes(&lock_notes);
    if let Some(enc) = encoding_override {
        sqlguard::encoding::validate(enc).map_err(SqlGuardError::ConfigError)?;
        config.scan.encoding = enc.to_string();
        eprintln!("Scan encoding override: {}", enc);
    }
    if let Some(d) = dialect_override {
        config.dialect = sqlguard::config::CheckDialect::parse_dialect(d);
        eprintln!("Dialect override: {} → {}", config.dialect.as_str(), d);
    }
    // 解析方言回退链：CLI --dialect-fallback > 配置 dialect_fallback > 主方言默认回退。
    let fallback = if let Some(fb) = dialect_fallback_override {
        Some(sqlguard::config::CheckDialect::parse_dialect(fb))
    } else if let Some(cfg_fb) = config.dialect_fallback {
        Some(cfg_fb)
    } else {
        config.dialect.default_fallback()
    };
    config.dialect_fallback = fallback;
    if let Some(fb) = fallback {
        eprintln!(
            "Dialect fallback chain: {} → {} → generic",
            config.dialect.as_str(),
            fb.as_str()
        );
    } else {
        eprintln!(
            "Dialect fallback chain: {} → generic (no second candidate)",
            config.dialect.as_str()
        );
    }

    let filter = engine::RuleFilter::from_cli(rules, groups, exclude_rules, exclude_groups);
    if !filter.is_empty() {
        eprintln!(
            "Rule filter active: include_rules={:?} include_groups={:?} exclude_rules={:?} exclude_groups={:?}",
            filter.include_rules, filter.include_groups, filter.exclude_rules, filter.exclude_groups
        );
    }

    let absolute_target = resolve_absolute_path(target_dir);

    let effective_scan_paths: Vec<String> = if !config.scan.paths.is_empty() {
        config.scan.paths.clone()
    } else {
        config.structure.paths.clone()
    };
    let exclude_dirs: &[String] = &config.scan.exclude_dirs;

    let (missing, unexpected) =
        directory::check_directory_structure(&absolute_target, &config.structure, exclude_dirs);

    let sql_files =
        classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
    let mapper_files = mapper::collect_mapper_files(&absolute_target, &config.mapper, exclude_dirs);

    // 缓存开关优先级：CLI --no-cache > CLI --cache > [cache].enabled
    let cache_enabled = if no_cache_flag {
        false
    } else if cache_flag {
        true
    } else {
        config.cache.enabled
    };

    let mut file_cache = if cache_enabled {
        let run_signature = cache::compute_run_signature(
            config_path,
            &config,
            &config_dir,
            config.dialect,
            config.dialect_fallback,
            &filter,
        );
        let cache_path = absolute_target.join(&config.cache.cache_file);
        let cache = cache::FileCache::load(cache_path, absolute_target.clone(), run_signature);
        eprintln!(
            "File cache enabled: {} (signature {})",
            config.cache.cache_file,
            cache.run_signature()
        );
        cache
    } else {
        cache::FileCache::disabled()
    };

    let engine_instance = engine::build_engine();
    let (all_violations, files_checked) = check_files(
        &config,
        &config_dir,
        &filter,
        &engine_instance,
        &sql_files,
        &mapper_files,
        &mut file_cache,
    )?;

    // 写回缓存（仅在启用且有变更时实际落盘）
    file_cache.flush();

    let formats = parse_formats(format);
    let output_dir_path = resolve_output_dir(output_dir, &absolute_target);
    reporter::output_reports(
        &all_violations,
        &missing,
        &unexpected,
        files_checked,
        &formats,
        &output_dir_path,
    )
    .map_err(SqlGuardError::CheckError)?;

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    let has_missing = !missing.is_empty();

    if has_errors || has_missing {
        Err(SqlGuardError::CheckError("Checks failed".to_string()))
    } else {
        Ok(())
    }
}

/// 进度条单行文本：`Exporting SQL manifest: 50% [==========----------] 3/6 files  a.sql`。
/// 文件名右侧截断到 24 字符，避免长路径撑满整行。`total == 0` 时返回 None（无进度可显示）。
fn progress_line(done: usize, total: usize, name: &str) -> Option<String> {
    if total == 0 {
        return None;
    }
    let pct = done * 100 / total;
    let width = 20usize;
    let filled = done * width / total;
    let bar: String = std::iter::repeat_n('=', filled)
        .chain(std::iter::repeat_n('-', width - filled))
        .collect();
    let name: String = if name.chars().count() > 24 {
        let tail: String = name.chars().rev().take(24).collect();
        format!("…{}", tail.chars().rev().collect::<String>())
    } else {
        name.to_string()
    };
    Some(format!(
        "Exporting SQL manifest: {}% [{}] {}/{} files  {}",
        pct, bar, done, total, name
    ))
}

// ===== run_replay_export =====

fn run_replay_export(
    target_dir: &Path,
    config_path: &Path,
    explicit_config: bool,
    output_dir: &Path,
    types: &Option<String>,
    base: Option<&str>,
    encoding_override: Option<&str>,
) -> Result<(), SqlGuardError> {
    let (mut config, _config_dir) = load_config(config_path, explicit_config)?;
    if let Some(enc) = encoding_override {
        sqlguard::encoding::validate(enc).map_err(SqlGuardError::ConfigError)?;
        config.scan.encoding = enc.to_string();
        eprintln!("Scan encoding override: {}", enc);
    }

    let absolute_target = resolve_absolute_path(target_dir);

    let effective_scan_paths: Vec<String> = if !config.scan.paths.is_empty() {
        config.scan.paths.clone()
    } else {
        config.structure.paths.clone()
    };
    let exclude_dirs: &[String] = &config.scan.exclude_dirs;

    let sql_files =
        classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
    let mapper_files = mapper::collect_mapper_files(&absolute_target, &config.mapper, exclude_dirs);

    let type_filter = replay_export::parse_type_filter(types);

    let output_dir_abs = resolve_absolute_path(output_dir);
    if !output_dir_abs.exists() {
        fs::create_dir_all(&output_dir_abs).map_err(SqlGuardError::IoError)?;
    }

    // 导出进度条：仅 stderr 为 TTY 时渲染（CI / 管道日志不输出 \r 控制字符）。
    // 每处理完一个文件回调一次 (done, total, 当前文件)。
    let stderr_tty = std::io::stderr().is_terminal();
    let mut render_progress = |done: usize, total: usize, file: &Path| {
        if !stderr_tty {
            return;
        }
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(line) = progress_line(done, total, &name) else {
            return;
        };
        if done >= total {
            // 完成：换行，后续摘要从新行开始
            eprint!("\x1b[2K\r{}\n", line);
        } else {
            eprint!("\x1b[2K\r{}", line);
        }
    };

    // 增量导出：--base 指定 git 基线，只导出改动语句
    let (manifest, removed_manifest, sql_count, mapper_count) = if let Some(base_ref) = base {
        eprintln!(
            "Computing diff: {}...HEAD + working tree (uncommitted changes included)",
            base_ref
        );
        let mut patterns: Vec<&str> = vec!["*.sql", "*.ddl", "*.dml"];
        let mapper_patterns_owned: Vec<String> = if config.mapper.enabled {
            config.mapper.patterns.clone()
        } else {
            Vec::new()
        };
        for p in &mapper_patterns_owned {
            patterns.push(p.as_str());
        }
        let diffs = git_diff::get_diff(base_ref, &patterns)?;
        if diffs.is_empty() {
            eprintln!("No SQL changes detected since {}", base_ref);
        } else {
            eprintln!(
                "Incremental export on {} file(s) changed since {}",
                diffs.len(),
                base_ref
            );
        }
        let inc = replay_export::build_incremental_manifest(
            &absolute_target,
            &sql_files,
            &mapper_files,
            &diffs,
            base_ref,
            &type_filter,
            config.trust_dynamic_substitution,
            &config.scan.encoding,
            &mut render_progress,
        )?;
        let removed_manifest = replay_export::RemovedManifest {
            version: 1,
            generator: "sqlguard replay-export".to_string(),
            base: base_ref.to_string(),
            generated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default(),
            removed_count: inc.removed.len(),
            removed: inc.removed,
        };
        (
            inc.manifest,
            Some(removed_manifest),
            sql_files.len(),
            mapper_files.len(),
        )
    } else {
        let manifest = replay_export::build_manifest(
            &absolute_target,
            &sql_files,
            &mapper_files,
            &type_filter,
            config.trust_dynamic_substitution,
            &config.scan.encoding,
            &mut render_progress,
        )?;
        (manifest, None, sql_files.len(), mapper_files.len())
    };

    let json = replay_export::manifest_to_json(&manifest)?;

    let manifest_path = output_dir_abs.join("sql-manifest.json");
    fs::write(&manifest_path, &json).map_err(SqlGuardError::IoError)?;

    // 增量模式额外输出被删语句清单
    if let Some(removed) = removed_manifest {
        let removed_json = replay_export::removed_manifest_to_json(&removed)?;
        let removed_path = output_dir_abs.join("sql-manifest-removed.json");
        fs::write(&removed_path, &removed_json).map_err(SqlGuardError::IoError)?;
        eprintln!(
            "Replay manifest exported (incremental, base={}): {} ({} statements, {} removed, {} sql files, {} mapper files)",
            manifest.base.as_deref().unwrap_or(""),
            manifest_path.display(),
            manifest.statement_count,
            removed.removed_count,
            sql_count,
            mapper_count,
        );
    } else {
        eprintln!(
            "Replay manifest exported: {} ({} statements, {} sql files, {} mapper files)",
            manifest_path.display(),
            manifest.statement_count,
            sql_count,
            mapper_count,
        );
    }

    Ok(())
}

// ===== run_check_diff =====

#[allow(clippy::too_many_arguments)]
fn run_check_diff(
    base: &str,
    target_dir: &Path,
    config_path: &Path,
    explicit_config: bool,
    format: &str,
    output_dir: Option<&Path>,
    rules: &Option<String>,
    groups: &Option<String>,
    exclude_rules: &Option<String>,
    exclude_groups: &Option<String>,
    dialect_override: Option<&str>,
    dialect_fallback_override: Option<&str>,
    encoding_override: Option<&str>,
    strict_engine: bool,
    strict_ids: bool,
    locked: bool,
    no_lock: bool,
) -> Result<(), SqlGuardError> {
    let (mut config, config_dir) = load_config(config_path, explicit_config)?;
    // M1：规则包兼容诊断（api_version 已在 Config::load 硬校验）。
    check_rule_pack_compat(&config, strict_engine, strict_ids)?;
    // M3：锁文件校验（默认比对 name+version；--locked 额外校验 checksum）。
    let lock_notes = lock::verify_lock(&config, &config_dir, lock_mode(locked, no_lock)?)?;
    emit_notes(&lock_notes);
    if let Some(enc) = encoding_override {
        sqlguard::encoding::validate(enc).map_err(SqlGuardError::ConfigError)?;
        config.scan.encoding = enc.to_string();
        eprintln!("Scan encoding override: {}", enc);
    }
    if let Some(d) = dialect_override {
        config.dialect = sqlguard::config::CheckDialect::parse_dialect(d);
        eprintln!("Dialect override: {} → {}", config.dialect.as_str(), d);
    }
    // 解析方言回退链：CLI --dialect-fallback > 配置 dialect_fallback > 主方言默认回退。
    let fallback = if let Some(fb) = dialect_fallback_override {
        Some(sqlguard::config::CheckDialect::parse_dialect(fb))
    } else if let Some(cfg_fb) = config.dialect_fallback {
        Some(cfg_fb)
    } else {
        config.dialect.default_fallback()
    };
    config.dialect_fallback = fallback;
    if let Some(fb) = fallback {
        eprintln!(
            "Dialect fallback chain: {} → {} → generic",
            config.dialect.as_str(),
            fb.as_str()
        );
    } else {
        eprintln!(
            "Dialect fallback chain: {} → generic (no second candidate)",
            config.dialect.as_str()
        );
    }

    let filter = engine::RuleFilter::from_cli(rules, groups, exclude_rules, exclude_groups);
    if !filter.is_empty() {
        eprintln!(
            "Rule filter active: include_rules={:?} include_groups={:?} exclude_rules={:?} exclude_groups={:?}",
            filter.include_rules, filter.include_groups, filter.exclude_rules, filter.exclude_groups
        );
    }

    // 1. 调用 git diff 获取改动文件 + hunk 行范围
    let mut patterns: Vec<&str> = vec!["*.sql", "*.ddl", "*.dml"];
    let mapper_patterns_owned: Vec<String> = if config.mapper.enabled {
        config.mapper.patterns.clone()
    } else {
        Vec::new()
    };
    for p in &mapper_patterns_owned {
        patterns.push(p.as_str());
    }

    eprintln!(
        "Computing diff: {}...HEAD + working tree (uncommitted changes included)",
        base
    );
    let diffs = git_diff::get_diff(base, &patterns)?;

    if diffs.is_empty() {
        eprintln!("No SQL changes detected since {}", base);
        let formats = parse_formats(format);
        let output_dir_path =
            resolve_output_dir(output_dir, &std::env::current_dir().unwrap_or_default());
        reporter::output_reports(&[], &[], &[], 0, &formats, &output_dir_path)
            .map_err(SqlGuardError::CheckError)?;
        return Ok(());
    }

    eprintln!(
        "Incremental check on {} file(s) changed since {}",
        diffs.len(),
        base
    );

    let absolute_target = resolve_absolute_path(target_dir);

    // 2. 对每个改动文件跑规则，按 hunk 范围过滤
    let engine_instance = engine::build_engine();
    let mut all_violations: Vec<Violation> = Vec::new();
    let mut files_checked = 0;

    for file_diff in &diffs {
        let file_path = &file_diff.path;
        if !file_path.exists() {
            continue;
        }
        files_checked += 1;

        // 文件级检查（编码 / 换行符）：属于整文件属性，不做 hunk 过滤，
        // 只要该文件出现在本次 diff 中就检查。
        {
            let script_type = classification::classify_file(file_path, &config.classification)
                .map(|r| r.script_type)
                .unwrap_or_else(|_| "unknown".to_string());
            all_violations.extend(encoding::check_file(
                file_path,
                &script_type,
                &config.file_check,
                &filter,
                &config.scan.encoding,
            ));
        }

        let is_xml = file_path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xml"));

        if is_xml {
            // mapper 模式：提取所有片段，逐条跑 + 过滤
            let extracted = match mapper::extract_sql_from_xml(file_path, &config.scan.encoding) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to parse mapper XML '{}': {}",
                        file_path.display(),
                        e
                    );
                    continue;
                }
            };
            for sql in extracted {
                let script_type = mapper::map_statement_type(
                    &sql.statement_type,
                    &config.mapper.statement_type_mapping,
                );
                // 行号映射基准：规则产出的行号是「渲染后 SQL 内行号」，加上
                // `<select>` 标签起始行偏移即得 XML 行号。注意这是**近似值**——
                // 渲染压缩了 `<if>`/`<where>` 等动态标签行，含多行动态标签的语句
                // 违规行号会有 ± 几行偏差（详见 `ExtractedSql::raw_xml_line`）。
                let line_offset = sql.raw_xml_line.saturating_sub(1);
                let chosen = pick_parseable_candidate(&sql, &config);
                let violations = engine::run_rules_for_file(
                    &engine_instance,
                    file_path,
                    &chosen,
                    script_type,
                    &config,
                    &config_dir,
                    &filter,
                    line_offset,
                    sql.has_dynamic,
                )?;
                let filtered = filter_violations_by_diff(violations, file_diff);
                all_violations.extend(filtered);
            }
        } else {
            // 脚本模式
            let classification_result =
                classification::classify_file(file_path, &config.classification)?;
            let sql_content = sqlguard::encoding::read_to_string(file_path, &config.scan.encoding)
                .map_err(SqlGuardError::CheckError)?;
            let violations = engine::run_rules_for_file(
                &engine_instance,
                file_path,
                &sql_content,
                &classification_result.script_type,
                &config,
                &config_dir,
                &filter,
                0,
                false,
            )?;
            let filtered = filter_violations_by_diff(violations, file_diff);
            all_violations.extend(filtered);
        }
    }

    // 3. 输出报告
    let formats = parse_formats(format);
    let output_dir_path = resolve_output_dir(output_dir, &absolute_target);
    reporter::output_reports(
        &all_violations,
        &[],
        &[],
        files_checked,
        &formats,
        &output_dir_path,
    )
    .map_err(SqlGuardError::CheckError)?;

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    if has_errors {
        Err(SqlGuardError::CheckError(
            "Incremental checks failed".to_string(),
        ))
    } else {
        Ok(())
    }
}

/// 语句级交集过滤：保留 violation 的 [line, end_line] 与任一 hunk [s, e] 有交集的违规。
/// - 新增文件：全保留
/// - violation 无 line：保守保留（可能是规则脚本错误，宁可误报）
/// - violation 有 line 但无 end_line：保守保留（无法确定语句行范围，
///   「语句跨多行但只带了起始行」时按单点判断会误删，宁可误报）
fn filter_violations_by_diff(
    violations: Vec<Violation>,
    file_diff: &git_diff::FileDiff,
) -> Vec<Violation> {
    if file_diff.is_new {
        return violations;
    }
    violations
        .into_iter()
        .filter(|v| {
            let line = match v.line {
                Some(l) => l,
                None => return true, // 无行号，保守保留
            };
            let end_line = match v.end_line {
                Some(e) => e,
                None => return true, // 无行号范围，保守保留
            };
            // 与任一 hunk 有交集：[line, end_line] ∩ [s, e] != ∅
            file_diff
                .hunks
                .iter()
                .any(|(s, e)| line <= *e && end_line >= *s)
        })
        .collect()
}

// ===== run_gen_rollback =====

/// 生成 backup/rollback 脚本。
///
/// 流程：
/// 1. 加载配置，应用 CLI 覆盖（dialect/lock_scope/lock_timeout/accept_table_lock_risk）
/// 2. 收集 SQL 文件（脚本模式 + Mapper 模式）
/// 3. 对每个文件用 check 方言解析为 SqlAst，遍历 StmtInfo 调 RollbackGenerator::generate
/// 4. 渲染 backup.sql / rollback.sql / cleanup.sql
/// 5. 构建 Manifest 并写 rollback-manifest.json
/// 6. 按 Manifest::exit_code 返回退出码
#[allow(clippy::too_many_arguments)]
fn run_gen_rollback(
    target_dir: &Path,
    config_path: &Path,
    explicit_config: bool,
    output_dir: &Path,
    dialect_override: Option<&str>,
    lock_scope_override: Option<&str>,
    lock_timeout_override: Option<u64>,
    accept_table_lock_risk: bool,
    fail_on_warning: bool,
    allow_partial: bool,
    review_report: bool,
    encoding_override: Option<&str>,
) -> Result<i32, SqlGuardError> {
    let (mut config, _config_dir) = load_config(config_path, explicit_config)?;
    if let Some(enc) = encoding_override {
        sqlguard::encoding::validate(enc).map_err(SqlGuardError::ConfigError)?;
        config.scan.encoding = enc.to_string();
        eprintln!("Scan encoding override: {}", enc);
    }

    // 应用 CLI 覆盖到 rollback 配置
    let rc: &mut sqlguard::config::RollbackConfig = &mut config.rollback;

    // ★ P2-3：旧配置兼容映射——lock_tables_during_backup=false 且未显式覆盖 lock_scope 时，
    // 视为用户意图"不加锁"，将 lock_scope 从默认 auto 改为 none。
    // 必须在 CLI lock_scope 覆盖之前执行，确保 CLI 优先级高于旧配置兼容映射。
    if !rc.lock_tables_during_backup && rc.lock_scope == "auto" && lock_scope_override.is_none() {
        rc.lock_scope = "none".to_string();
        eprintln!("Rollback lock_scope=none (mapped from legacy lock_tables_during_backup=false)");
    }

    if let Some(d) = dialect_override {
        rc.dialect = d.to_string();
        eprintln!("Rollback dialect override: {}", d);
    }
    if let Some(ls) = lock_scope_override {
        rc.lock_scope = ls.to_string();
        eprintln!("Rollback lock_scope override: {}", ls);
    }
    if let Some(lt) = lock_timeout_override {
        rc.lock_timeout = lt;
        eprintln!("Rollback lock_timeout override: {}s", lt);
    }
    if accept_table_lock_risk {
        rc.accept_table_lock_risk = true;
    }

    // 解析方言
    let dialect: sqlguard::rollback::Dialect = rc.dialect.parse().map_err(|e: String| {
        SqlGuardError::ConfigError(format!("Invalid rollback.dialect '{}': {}", rc.dialect, e))
    })?;
    let renderer = sqlguard::rollback::renderer_for(dialect);

    // R2 预检：lock_scope=table 必须显式确认
    let prereq_errors =
        sqlguard::rollback::render::validate_render_prerequisites(&[], &config.rollback);
    if !prereq_errors.is_empty() {
        for e in &prereq_errors {
            eprintln!("Prerequisite error: {}", e);
        }
        return Ok(2);
    }

    let absolute_target = resolve_absolute_path(target_dir);
    let absolute_output = resolve_absolute_path(output_dir);
    fs::create_dir_all(&absolute_output).map_err(SqlGuardError::IoError)?;

    let effective_scan_paths: Vec<String> = if !config.scan.paths.is_empty() {
        config.scan.paths.clone()
    } else {
        config.structure.paths.clone()
    };
    let exclude_dirs: &[String] = &config.scan.exclude_dirs;

    let sql_files =
        classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
    let mapper_files = if config.mapper.enabled {
        mapper::collect_mapper_files(&absolute_target, &config.mapper, exclude_dirs)
    } else {
        Vec::new()
    };

    eprintln!(
        "Scanning {} SQL file(s) and {} mapper file(s) with {} dialect",
        sql_files.len(),
        mapper_files.len(),
        dialect.as_str()
    );

    let mut generator =
        sqlguard::rollback::RollbackGenerator::new(&config, &config.rollback, &*renderer);
    let mut pairs: Vec<sqlguard::rollback::BackupRollbackPair> = Vec::new();

    // 脚本模式：解析每个 SQL 文件为 SqlAst，遍历 StmtInfo
    for file_path in &sql_files {
        let content = sqlguard::encoding::read_to_string(file_path, &config.scan.encoding)
            .map_err(SqlGuardError::CheckError)?;
        let ast =
            engine::parser::parse_sql_to_ast_fb(&content, config.dialect, config.dialect_fallback);

        if let Some(err) = &ast.parse_error {
            eprintln!(
                "Warning: {} failed to tokenize ({}); skipped",
                file_path.display(),
                err
            );
        }

        for stmt in &ast.statements {
            if stmt.kind == "PARSE_ERROR" {
                eprintln!(
                    "Warning: {}:{} parse error; statement skipped",
                    file_path.display(),
                    stmt.line
                );
                continue;
            }
            // 提取原始 SQL 文本片段（按行号）
            let original = extract_sql_lines(&content, stmt.line as usize, stmt.end_line as usize);
            let source = sqlguard::rollback::SourceRef {
                file: file_path.to_string_lossy().to_string(),
                line: stmt.line,
                end_line: stmt.end_line,
                statement_id: None,
                variant_label: None,
            };
            let pair = generator.generate(stmt, source, &original);
            pairs.push(pair);
        }
    }

    // Mapper 模式：解析 XML，对每个 SQL 片段生成
    if config.mapper.enabled {
        for file_path in &mapper_files {
            let extracted = match mapper::extract_sql_from_xml(file_path, &config.scan.encoding) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to parse mapper XML '{}': {}",
                        file_path.display(),
                        e
                    );
                    continue;
                }
            };
            for sql in extracted {
                if !config.rollback.include_select
                    && sql.statement_type.eq_ignore_ascii_case("select")
                {
                    continue;
                }
                let ast = engine::parser::parse_sql_to_ast_fb(
                    &sql.processed_sql,
                    config.dialect,
                    config.dialect_fallback,
                );
                for stmt in &ast.statements {
                    if stmt.kind == "PARSE_ERROR" {
                        continue;
                    }
                    let original = extract_sql_lines(
                        &sql.processed_sql,
                        stmt.line as usize,
                        stmt.end_line as usize,
                    );
                    let source = sqlguard::rollback::SourceRef {
                        file: file_path.to_string_lossy().to_string(),
                        line: sql.raw_xml_line as i64 + stmt.line - 1,
                        end_line: sql.raw_xml_line as i64 + stmt.end_line - 1,
                        statement_id: Some(sql.statement_id.clone()),
                        variant_label: None,
                    };
                    let pair = generator.generate(stmt, source, &original);
                    pairs.push(pair);
                }
            }
        }
    }

    eprintln!(
        "Generated {} rollback pair(s): {} irreversible, {} partial, {} unreliable",
        pairs.len(),
        pairs.iter().filter(|p| p.safety.irreversible).count(),
        pairs.iter().filter(|p| p.safety.partial).count(),
        pairs.iter().filter(|p| !p.safety.reliable).count()
    );

    // ★ 按源文件分组（保持首次出现顺序），用于 per-file 输出
    let mut file_groups: Vec<(String, Vec<sqlguard::rollback::BackupRollbackPair>)> = Vec::new();
    let mut file_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for pair in pairs.drain(..) {
        let file = pair.source.file.clone();
        if let Some(&idx) = file_index.get(&file) {
            file_groups[idx].1.push(pair);
        } else {
            file_index.insert(file.clone(), file_groups.len());
            file_groups.push((file, vec![pair]));
        }
    }

    // P1-2：per-file finalize_safety（每个文件独立判定 lock_type，DML-only 文件不被
    // 其他文件的 DDL 拉高锁级别）
    for (_file, group) in &mut file_groups {
        sqlguard::rollback::render::finalize_safety(group, &config.rollback);
    }

    // R4 预检：per-file 校验，聚合错误
    let mut prereq_errors: Vec<String> = Vec::new();
    for (_file, group) in &file_groups {
        let errs =
            sqlguard::rollback::render::validate_render_prerequisites(group, &config.rollback);
        prereq_errors.extend(errs);
    }
    if !prereq_errors.is_empty() {
        for e in &prereq_errors {
            eprintln!("Prerequisite error: {}", e);
        }
        return Ok(2);
    }

    // ★ per-file 渲染：backup/ 和 rollback/ 分开子目录，各自镜像输入目录结构
    // 输出结构: output_dir/backup/<rel_dir>/<stem>.sql + output_dir/rollback/<rel_dir>/<stem>.sql
    for (file_path_str, group) in &file_groups {
        if group.is_empty() {
            continue;
        }
        let file_path = std::path::Path::new(file_path_str);
        // 计算相对于 target_dir 的相对路径，用于镜像目录结构
        let rel_path = file_path
            .strip_prefix(&absolute_target)
            .unwrap_or(std::path::Path::new(
                file_path.file_name().unwrap_or_default(),
            ));
        let parent_dir = rel_path.parent().unwrap_or(std::path::Path::new(""));
        let file_name = rel_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.sql".to_string());

        // ★ backup/rollback 分子目录：目录名取 backup_file/rollback_file 去扩展名（如 "backup.sql" → "backup"）
        let backup_subdir = config.rollback.backup_file.trim_end_matches(".sql");
        let rollback_subdir = config.rollback.rollback_file.trim_end_matches(".sql");
        let backup_dir = absolute_output.join(backup_subdir).join(parent_dir);
        let rollback_dir = absolute_output.join(rollback_subdir).join(parent_dir);
        fs::create_dir_all(&backup_dir).map_err(SqlGuardError::IoError)?;
        fs::create_dir_all(&rollback_dir).map_err(SqlGuardError::IoError)?;

        let backup_sql =
            sqlguard::rollback::render::render_backup(group, &config.rollback, &*renderer);
        let rollback_sql =
            sqlguard::rollback::render::render_rollback(group, &config.rollback, &*renderer);

        let backup_out = backup_dir.join(&file_name);
        let rollback_out = rollback_dir.join(&file_name);

        fs::write(&backup_out, &backup_sql).map_err(SqlGuardError::IoError)?;
        fs::write(&rollback_out, &rollback_sql).map_err(SqlGuardError::IoError)?;

        eprintln!("Wrote {}", backup_out.display());
        eprintln!("Wrote {}", rollback_out.display());
    }

    // 全局 cleanup + manifest（跨文件汇总，写在 output_dir 根目录）
    let all_pairs: Vec<sqlguard::rollback::BackupRollbackPair> =
        file_groups.into_iter().flat_map(|(_, g)| g).collect();

    let cleanup_sql =
        sqlguard::rollback::render::render_cleanup(&all_pairs, &config.rollback, &*renderer);
    let cleanup_path = absolute_output.join(&config.rollback.cleanup_file);
    fs::write(&cleanup_path, &cleanup_sql).map_err(SqlGuardError::IoError)?;

    // 构建并写 manifest（★ P1-2：传入 rc 以透出 assert_on_schema_mismatch / on_partitioned_table）
    let manifest = sqlguard::rollback::Manifest::from_pairs(
        &all_pairs,
        dialect.as_str(),
        &config.rollback,
        Vec::new(),
    );
    let manifest_json = sqlguard::rollback::serialize_manifest(&manifest)
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to serialize manifest: {}", e)))?;
    let manifest_path = absolute_output.join(&config.rollback.manifest_file);
    fs::write(&manifest_path, &manifest_json).map_err(SqlGuardError::IoError)?;

    eprintln!("Wrote {}", cleanup_path.display());
    eprintln!("Wrote {}", manifest_path.display());

    // ★ Review 契约:可选生成 rollback-review-report.html
    if review_report {
        let html = sqlguard::rollback::generate_review_report(&manifest);
        let report_path = absolute_output.join("rollback-review-report.html");
        fs::write(&report_path, &html).map_err(SqlGuardError::IoError)?;
        eprintln!("Wrote {}", report_path.display());
    }

    let exit_code = manifest.exit_code(fail_on_warning, allow_partial);
    eprintln!(
        "Exit code {} (fail_on_warning={}, allow_partial={})",
        exit_code, fail_on_warning, allow_partial
    );
    Ok(exit_code)
}

/// 从 SQL 文本中按行号提取语句片段（1-indexed, inclusive）。
fn extract_sql_lines(content: &str, start_line: usize, end_line: usize) -> String {
    content
        .lines()
        .skip(start_line.saturating_sub(1))
        .take(end_line.saturating_sub(start_line.saturating_sub(1)))
        .collect::<Vec<_>>()
        .join("\n")
}

// ===== run_init =====

/// `init` 写入的规则脚本清单：(文件名, 子目录, 内容)——content 通过 include_str! 编译时嵌入。
/// 约束：`sqlguard.rules.toml.example` 声明的每个 script_path 必须出现在本清单中
/// （超集允许，有测试守护），否则 init 出的规则文件会引用到不存在的脚本。
const INIT_RULE_SCRIPTS: &[(&str, &str, &str)] = &[
    (
        "no_drop_table",
        "ddl",
        include_str!("../config/rules/ddl/no_drop_table.rhai"),
    ),
    (
        "primary_key_required",
        "ddl",
        include_str!("../config/rules/ddl/primary_key_required.rhai"),
    ),
    (
        "no_reserved_keyword_naming",
        "ddl",
        include_str!("../config/rules/ddl/no_reserved_keyword_naming.rhai"),
    ),
    (
        "backup_table_naming",
        "ddl",
        include_str!("../config/rules/ddl/backup_table_naming.rhai"),
    ),
    (
        "index_naming_convention",
        "ddl",
        include_str!("../config/rules/ddl/index_naming_convention.rhai"),
    ),
    (
        "no_redundant_index",
        "ddl",
        include_str!("../config/rules/ddl/no_redundant_index.rhai"),
    ),
    (
        "table_name_naming",
        "ddl",
        include_str!("../config/rules/ddl/table_name_naming.rhai"),
    ),
    (
        "no_select_all",
        "dml",
        include_str!("../config/rules/dml/no_select_all.rhai"),
    ),
    (
        "no_delete_update_without_where",
        "dml",
        include_str!("../config/rules/dml/no_delete_update_without_where.rhai"),
    ),
    (
        "insert_columns_required",
        "dml",
        include_str!("../config/rules/dml/insert_columns_required.rhai"),
    ),
    (
        "subquery_alias_required",
        "dml",
        include_str!("../config/rules/dml/subquery_alias_required.rhai"),
    ),
    (
        "column_references_qualified",
        "dml",
        include_str!("../config/rules/dml/column_references_qualified.rhai"),
    ),
    (
        "no_join_without_condition",
        "dml",
        include_str!("../config/rules/dml/no_join_without_condition.rhai"),
    ),
    (
        "no_unused_join",
        "dml",
        include_str!("../config/rules/dml/no_unused_join.rhai"),
    ),
    (
        "no_unused_cte",
        "dml",
        include_str!("../config/rules/dml/no_unused_cte.rhai"),
    ),
    (
        "use_is_null",
        "dml",
        include_str!("../config/rules/dml/use_is_null.rhai"),
    ),
    (
        "use_coalesce",
        "dml",
        include_str!("../config/rules/dml/use_coalesce.rhai"),
    ),
    (
        "no_order_by_in_subquery",
        "dml",
        include_str!("../config/rules/dml/no_order_by_in_subquery.rhai"),
    ),
    (
        "union_all_preferred",
        "dml",
        include_str!("../config/rules/dml/union_all_preferred.rhai"),
    ),
    (
        "no_nested_case",
        "dml",
        include_str!("../config/rules/dml/no_nested_case.rhai"),
    ),
    (
        "no_constant_where",
        "dml",
        include_str!("../config/rules/dml/no_constant_where.rhai"),
    ),
    (
        "order_by_required_for_pagination",
        "dml",
        include_str!("../config/rules/dml/order_by_required_for_pagination.rhai"),
    ),
    (
        "join_type_required",
        "dml",
        include_str!("../config/rules/dml/join_type_required.rhai"),
    ),
    (
        "max_join_tables",
        "dml",
        include_str!("../config/rules/dml/max_join_tables.rhai"),
    ),
    (
        "no_or_in_where",
        "dml",
        include_str!("../config/rules/dml/no_or_in_where.rhai"),
    ),

    // ===== GaussDB 规范规则（GNAM/GTYP/GOBJ/GIDX/GDCL/GDDL/GDML/GPERF）=====
    // 对应 docs/gaussdb-rule-gap-analysis.md 的 A 类条目，默认在
    // sqlguard.rules.toml.example 中 enabled = false，按项目启用。
    (
        "gaussdb_object_name_charset",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_object_name_charset.rhai"),
    ),
    (
        "gaussdb_no_quoted_object_name",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_no_quoted_object_name.rhai"),
    ),
    (
        "gaussdb_no_reserved_prefix",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_no_reserved_prefix.rhai"),
    ),
    (
        "gaussdb_object_name_max_bytes",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_object_name_max_bytes.rhai"),
    ),
    (
        "gaussdb_max_large_fields",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_max_large_fields.rhai"),
    ),
    (
        "gaussdb_no_system_column",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_no_system_column.rhai"),
    ),
    (
        "gaussdb_recommended_data_types",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_recommended_data_types.rhai"),
    ),
    (
        "gaussdb_no_materialized_view",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_no_materialized_view.rhai"),
    ),
    (
        "gaussdb_no_order_by_in_view",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_no_order_by_in_view.rhai"),
    ),
    (
        "gaussdb_view_usage_warning",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_view_usage_warning.rhai"),
    ),
    (
        "gaussdb_create_index_concurrently",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_create_index_concurrently.rhai"),
    ),
    (
        "gaussdb_no_quoted_column_in_ddl",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_no_quoted_column_in_ddl.rhai"),
    ),
    (
        "gaussdb_require_commit_in_transaction",
        "ddl",
        include_str!("../config/rules/ddl/gaussdb_require_commit_in_transaction.rhai"),
    ),
    (
        "gaussdb_no_order_by_group_by_in_update",
        "dml",
        include_str!("../config/rules/dml/gaussdb_no_order_by_group_by_in_update.rhai"),
    ),
    (
        "gaussdb_no_order_by_group_by_in_delete",
        "dml",
        include_str!("../config/rules/dml/gaussdb_no_order_by_group_by_in_delete.rhai"),
    ),
    (
        "gaussdb_update_subquery_to_join",
        "dml",
        include_str!("../config/rules/dml/gaussdb_update_subquery_to_join.rhai"),
    ),
    (
        "gaussdb_order_by_explicit_sort",
        "dml",
        include_str!("../config/rules/dml/gaussdb_order_by_explicit_sort.rhai"),
    ),
    (
        "gaussdb_statement_max_bytes",
        "dml",
        include_str!("../config/rules/dml/gaussdb_statement_max_bytes.rhai"),
    ),
];

///
/// 幂等 init：默认只补缺失文件，已存在的原样保留——防止内嵌默认配置覆盖
/// gates-toolkit 等工具链按模板渲染过的定制配置（历史行为是无条件覆盖）。
/// `force = true` 时恢复旧的覆盖语义，整体重写全部文件。
fn run_init(target_dir: &Path, force: bool) -> Result<(), SqlGuardError> {
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    let rules_dir = target_dir.join("config").join("rules");
    for dir in &[rules_dir.join("ddl"), rules_dir.join("dml")] {
        fs::create_dir_all(dir).map_err(SqlGuardError::IoError)?;
    }

    let mut files: Vec<(String, &'static str)> = vec![
        (
            "sqlguard.toml".to_string(),
            get_default_config_content(),
        ),
        (
            "sqlguard.rules.toml".to_string(),
            get_default_rules_content(),
        ),
    ];
    for (name, rule_type, content) in INIT_RULE_SCRIPTS {
        files.push((
            format!("config/rules/{}/{}.rhai", rule_type, name),
            content,
        ));
    }

    for (rel, content) in &files {
        let path = target_dir.join(rel);
        if path.exists() && !force {
            skipped.push(rel.clone());
        } else {
            fs::write(&path, content).map_err(SqlGuardError::IoError)?;
            written.push(rel.clone());
        }
    }

    let (ddl_count, dml_count) = INIT_RULE_SCRIPTS
        .iter()
        .fold((0usize, 0usize), |(d, m), (_, t, _)| {
            if *t == "ddl" {
                (d + 1, m)
            } else {
                (d, m + 1)
            }
        });

    if skipped.is_empty() {
        println!(
            "Initialized SqlGuard configuration in {}",
            target_dir.display()
        );
        println!("  - sqlguard.toml          # 主配置（结构/分类/输出/扫描/文件检查）");
        println!("  - sqlguard.rules.toml    # 规则配置（[[rules]] 单独拆分，避免文件过长）");
        println!("  - config/rules/ddl/ ({} rule files)", ddl_count);
        println!("  - config/rules/dml/ ({} rule files)", dml_count);
    } else {
        println!(
            "Initialized SqlGuard configuration in {} (幂等模式：保留已存在文件)",
            target_dir.display()
        );
        for rel in &written {
            println!("  written: {}", rel);
        }
        for rel in &skipped {
            println!(
                "  skipped: {} (already exists; rerun with --force to overwrite)",
                rel
            );
        }
    }
    println!();
    println!("Run: sqlguard check <project_path>");
    Ok(())
}

fn generate_default_config() -> Config {
    Config {
        structure: sqlguard::config::StructureConfig {
            paths: vec![
                "sql/ddl".to_string(),
                "sql/dml".to_string(),
                "sql/others".to_string(),
            ],
            strict: true,
            allow_extra: vec![".gitkeep".to_string(), "config/".to_string()],
        },
        classification: sqlguard::config::ClassificationConfig {
            rules: vec![
                sqlguard::config::ClassificationRule {
                    name: "ddl-by-dir".to_string(),
                    pattern: "**/ddl/**".to_string(),
                    script_type: "ddl".to_string(),
                    priority: 10,
                },
                sqlguard::config::ClassificationRule {
                    name: "dml-by-dir".to_string(),
                    pattern: "**/dml/**".to_string(),
                    script_type: "dml".to_string(),
                    priority: 10,
                },
            ],
            default_type: "other".to_string(),
        },
        rules: vec![
            // ===== P0 规则：默认启用 =====
            sqlguard::config::RuleConfig {
                id: "DDL001".to_string(),
                name: "no_drop_table".to_string(),
                group: Some("ddl-safety".to_string()),
                description: Some("Disallow DROP TABLE in DDL scripts".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/no_drop_table.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DDL002".to_string(),
                name: "primary_key_required".to_string(),
                group: Some("ddl-safety".to_string()),
                description: Some("CREATE TABLE must have a PRIMARY KEY".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/primary_key_required.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DDL003".to_string(),
                name: "no_reserved_keyword_naming".to_string(),
                group: Some("ddl-safety".to_string()),
                description: Some("Database object names must not use SQL reserved keywords".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/no_reserved_keyword_naming.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DDL004".to_string(),
                name: "backup_table_naming".to_string(),
                group: Some("ddl-convention".to_string()),
                description: Some("Backup tables created via CREATE TABLE AS SELECT must be prefixed with 'bks_'".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/backup_table_naming.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DDL005".to_string(),
                name: "index_naming_convention".to_string(),
                group: Some("ddl-convention".to_string()),
                description: Some("Indexes follow idx_/uk_/pk_ naming convention based on type and columns".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/index_naming_convention.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DDL006".to_string(),
                name: "no_redundant_index".to_string(),
                group: Some("ddl-performance".to_string()),
                description: Some("Avoid redundant indexes (duplicate PK indexes and leftmost-prefix duplicates)".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/no_redundant_index.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DDL007".to_string(),
                name: "table_name_naming".to_string(),
                group: Some("ddl-convention".to_string()),
                description: Some("Table names must contain only lowercase letters, digits, and underscores, and must not start with a digit".to_string()),
                enabled: true,
                script_path: "config/rules/ddl/table_name_naming.rhai".into(),
                applies_to: vec!["ddl".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML001".to_string(),
                name: "no_select_all".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("Disallow SELECT * in DML scripts".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_select_all.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML002".to_string(),
                name: "no_delete_update_without_where".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("DELETE/UPDATE must have a WHERE clause".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_delete_update_without_where.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML003".to_string(),
                name: "insert_columns_required".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("INSERT must specify target columns".to_string()),
                enabled: true,
                script_path: "config/rules/dml/insert_columns_required.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML004".to_string(),
                name: "subquery_alias_required".to_string(),
                group: Some("dml-style".to_string()),
                description: Some("Subqueries in FROM must have an alias".to_string()),
                enabled: true,
                script_path: "config/rules/dml/subquery_alias_required.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML005".to_string(),
                name: "column_references_qualified".to_string(),
                group: Some("dml-style".to_string()),
                description: Some("Qualify column references with table name in multi-table queries".to_string()),
                enabled: true,
                script_path: "config/rules/dml/column_references_qualified.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML006".to_string(),
                name: "no_join_without_condition".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("JOIN must have ON or USING condition".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_join_without_condition.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML007".to_string(),
                name: "order_by_required_for_pagination".to_string(),
                group: Some("dml-safety".to_string()),
                description: Some("Pagination queries (LIMIT/OFFSET/FETCH) must have ORDER BY for deterministic results".to_string()),
                enabled: true,
                script_path: "config/rules/dml/order_by_required_for_pagination.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            // ===== P1 规则：默认禁用，建议评估后启用 =====
            sqlguard::config::RuleConfig {
                id: "DML101".to_string(),
                name: "no_unused_join".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Detect potentially unused JOINs".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_unused_join.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML102".to_string(),
                name: "no_unused_cte".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Detect unused CTEs (WITH clauses)".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_unused_cte.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML103".to_string(),
                name: "use_is_null".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Use IS NULL instead of = NULL".to_string()),
                enabled: false,
                script_path: "config/rules/dml/use_is_null.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "error".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML104".to_string(),
                name: "use_coalesce".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Use standard COALESCE instead of NVL/ISNULL".to_string()),
                enabled: false,
                script_path: "config/rules/dml/use_coalesce.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML105".to_string(),
                name: "no_order_by_in_subquery".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("ORDER BY in subquery is typically ignored".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_order_by_in_subquery.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML106".to_string(),
                name: "union_all_preferred".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Prefer UNION ALL over UNION unless dedup needed".to_string()),
                enabled: false,
                script_path: "config/rules/dml/union_all_preferred.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML107".to_string(),
                name: "no_nested_case".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Avoid nested CASE expressions".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_nested_case.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML108".to_string(),
                name: "no_constant_where".to_string(),
                group: Some("dml-convention".to_string()),
                description: Some("Avoid constant conditions in WHERE clause".to_string()),
                enabled: false,
                script_path: "config/rules/dml/no_constant_where.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
            sqlguard::config::RuleConfig {
                id: "DML111".to_string(),
                name: "no_or_in_where".to_string(),
                group: Some("dml-performance".to_string()),
                description: Some("Do not use OR to combine conditions in WHERE; prefer IN, UNION ALL, or splitting the query".to_string()),
                enabled: true,
                script_path: "config/rules/dml/no_or_in_where.rhai".into(),
                applies_to: vec!["dml".to_string()],
                severity: "warning".to_string(),
                params: None,
            },
        ],
        rules_file: None,
        rules_dir: PathBuf::new(),
        pack: None,
        rule_packs: Default::default(),
        resolved_packs: Vec::new(),
        rule_pack_notices: Vec::new(),
        rule_pack_id_conflicts: Vec::new(),
        output: sqlguard::config::OutputConfig {
            formats: vec!["plain".to_string()],
            output_dir: None,
        },
        mapper: sqlguard::config::MapperConfig::default(),
        scan: sqlguard::config::ScanConfig::default(),
        file_check: sqlguard::config::FileCheckConfig::default(),
        rollback: sqlguard::config::RollbackConfig::default(),
        cache: sqlguard::config::CacheConfig::default(),
        dialect: sqlguard::config::CheckDialect::default(),
        dialect_fallback: None,
        trust_dynamic_substitution: true,
    }
}

/// 默认主配置内容——单一事实来源为仓库根的 `sqlguard.toml.example`，
/// 编译期嵌入；修改默认配置请直接编辑该文件（勿在本文件内再维护一份副本）。
fn get_default_config_content() -> &'static str {
    include_str!("../sqlguard.toml.example")
}

/// 默认规则配置内容（独立文件 sqlguard.rules.toml）——单一事实来源为
/// 仓库根的 `sqlguard.rules.toml.example`，编译期嵌入。
/// 仅含 `[[rules]]` 数组；脚本路径（script_path）相对本文件所在目录解析。
fn get_default_rules_content() -> &'static str {
    include_str!("../sqlguard.rules.toml.example")
}

/// 默认规则包清单（`config/rules-pack.toml`）——单一事实来源，编译期嵌入。
/// 使仓库 `config/` 目录同时是一个合规的规则包根（M4）。
fn get_default_pack_manifest() -> &'static str {
    include_str!("../config/rules-pack.toml")
}

/// 默认包在项目内的 vendor 目录（相对配置文件目录）。
const DEFAULT_PACK_VENDOR_DIR: &str = "vendor/rules/rules-core";

/// `init --with-default-pack` 追加到主配置末尾的规则包声明。
///
/// 追加在文件末尾：`[rule_packs]` 是顶层表头，无论原配置以什么内容结尾都合法，
/// 因此无需改写既有配置文本（保留注释与排版）。
const DEFAULT_PACK_CONFIG_BLOCK: &str = r#"
# ================================================================================
# 规则包 [rule_packs]（由 `sqlguard init --with-default-pack` 生成）
# ================================================================================
# 规则来自 vendor/rules/rules-core/ 下的默认规则包，版本由 sqlguard.lock 固定
# （CI 建议加 --locked 强制校验）。项目自定义规则写在 sqlguard.rules.toml，
# 优先级高于包内规则；也可用 [[rule_packs.overrides]] 只改级别/阈值/开关。
[rule_packs]
search_paths = ["vendor/rules"]

[[rule_packs.packs]]
name = "rules-core"
"#;

/// `init --with-default-pack` 写出的 `sqlguard.rules.toml`（仅承载本地自定义规则）。
const DEFAULT_PACK_RULES_TEMPLATE: &str = r#"# ================================================================================
# 项目本地规则（可选）
# ================================================================================
# 规则由规则包提供（见 sqlguard.toml 的 [rule_packs]）；本文件只放项目自定义规则。
# 本地规则优先级高于包内规则，可与包内规则同 id 覆盖之。
#
# 示例：
# [[rules]]
# id = "LOCAL001"
# name = "my_rule"
# script_path = "config/rules/local/my_rule.rhai"
# applies_to = ["dml"]
# severity = "warning"
"#;

/// `init --with-default-pack`：把默认规则包 vendor 到项目内，并在主配置里声明
/// `[rule_packs]`，最后生成 `sqlguard.lock`（M4）。
///
/// 与默认 `init`（写 `config/rules/` 脚本 + 完整 `sqlguard.rules.toml`）互不影响，
/// 后者行为保持不变。
fn run_init_with_default_pack(target_dir: &Path, force: bool) -> Result<(), SqlGuardError> {
    let pack_root = target_dir.join(DEFAULT_PACK_VENDOR_DIR);
    for sub in ["ddl", "dml"] {
        fs::create_dir_all(pack_root.join("rules").join(sub)).map_err(SqlGuardError::IoError)?;
    }

    // 主配置 = 默认配置 + 末尾追加的 [rule_packs] 声明
    let mut config_content = get_default_config_content().to_string();
    if !config_content.ends_with('\n') {
        config_content.push('\n');
    }
    config_content.push_str(DEFAULT_PACK_CONFIG_BLOCK);

    let mut files: Vec<(String, String)> = vec![
        ("sqlguard.toml".to_string(), config_content),
        (
            "sqlguard.rules.toml".to_string(),
            DEFAULT_PACK_RULES_TEMPLATE.to_string(),
        ),
        (
            format!("{DEFAULT_PACK_VENDOR_DIR}/rules-pack.toml"),
            get_default_pack_manifest().to_string(),
        ),
    ];
    for (name, subdir, content) in INIT_RULE_SCRIPTS {
        files.push((
            format!("{DEFAULT_PACK_VENDOR_DIR}/rules/{subdir}/{name}.rhai"),
            (*content).to_string(),
        ));
    }

    let mut written = 0usize;
    let mut skipped = 0usize;
    for (rel, content) in &files {
        let path = target_dir.join(rel);
        if path.exists() && !force {
            skipped += 1;
        } else {
            fs::write(&path, content).map_err(SqlGuardError::IoError)?;
            written += 1;
        }
    }

    // 生成锁文件：直接复用配置加载 + 解析结果，保证与实际生效包一致。
    let cfg_path = target_dir.join("sqlguard.toml");
    let (cfg, cfg_dir) = load_config(&cfg_path, true)?;
    let lock_file = lock::build_lock(&cfg, &cfg_dir)?;
    let lock_path = lock::write_lock(&lock_file, &cfg_dir)?;

    println!(
        "Initialized SqlGuard with the default rule pack in {}",
        target_dir.display()
    );
    println!("  - sqlguard.toml                      # 主配置（含 [rule_packs]）");
    println!("  - sqlguard.rules.toml                # 仅本地自定义规则（默认为空）");
    println!(
        "  - {DEFAULT_PACK_VENDOR_DIR}/     # 默认规则包（{} 个脚本）",
        INIT_RULE_SCRIPTS.len()
    );
    println!(
        "  - {}   ({} pack(s), {} rule(s) effective)",
        lock_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| lock::LOCK_FILE_NAME.to_string()),
        cfg.resolved_packs.len(),
        cfg.rules.len()
    );
    if skipped > 0 {
        println!("  （幂等模式：保留已存在文件 {skipped} 个，写入 {written} 个）");
    }
    println!();
    println!("Next: sqlguard check .        # CI 中加 --locked 强制校验锁文件");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlguard::git_diff::FileDiff;

    // === 默认规则包（M4） ===

    #[derive(serde::Deserialize)]
    struct MiniManifest {
        #[serde(default)]
        pack: Option<MiniPack>,
        #[serde(default)]
        rules: Vec<MiniRule>,
    }

    #[derive(serde::Deserialize)]
    struct MiniPack {
        name: String,
        version: String,
        api_version: u32,
    }

    #[derive(serde::Deserialize)]
    struct MiniRule {
        id: String,
        script_path: String,
        #[serde(default)]
        severity: String,
        #[serde(default)]
        enabled: bool,
    }

    #[test]
    fn default_pack_manifest_matches_rules_example() {
        let pack: MiniManifest = toml::from_str(get_default_pack_manifest())
            .expect("config/rules-pack.toml 必须是合法 TOML");
        let example: MiniManifest = toml::from_str(get_default_rules_content())
            .expect("sqlguard.rules.toml.example 必须是合法 TOML");

        let meta = pack.pack.as_ref().expect("包清单必须声明 [pack]");
        assert_eq!(meta.name, "rules-core");
        assert_eq!(meta.api_version, 1);
        assert!(!meta.version.is_empty(), "包版本不能为空");
        assert!(!pack.rules.is_empty());

        // 两处必须声明同一套规则（id / 脚本 / 级别 / 开关），防止清单与示例漂移
        let normalize = |rules: &[MiniRule]| -> Vec<(String, String, String, bool)> {
            rules
                .iter()
                .map(|r| {
                    (
                        r.id.clone(),
                        r.script_path.replace("config/rules/", "rules/"),
                        r.severity.clone(),
                        r.enabled,
                    )
                })
                .collect()
        };
        assert_eq!(
            normalize(&pack.rules),
            normalize(&example.rules),
            "config/rules-pack.toml 与 sqlguard.rules.toml.example 的规则集不一致"
        );

        // 每条包内脚本都必须有对应的内置脚本，否则 init --with-default-pack 会写出坏包
        for rule in &pack.rules {
            let rel = rule
                .script_path
                .strip_prefix("rules/")
                .unwrap_or(&rule.script_path);
            assert!(
                INIT_RULE_SCRIPTS
                    .iter()
                    .any(|(n, t, _)| format!("{t}/{n}.rhai") == rel),
                "规则 {} 引用 '{}'，但不在 INIT_RULE_SCRIPTS 中",
                rule.id,
                rule.script_path
            );
        }
    }

    #[test]
    fn init_with_default_pack_writes_self_contained_project() {
        let dir = tempfile::tempdir().unwrap();
        run_init_with_default_pack(dir.path(), false).unwrap();

        let cfg_path = dir.path().join("sqlguard.toml");
        let rules_path = dir.path().join("sqlguard.rules.toml");
        let lock_path = dir.path().join("sqlguard.lock");
        assert!(cfg_path.exists() && rules_path.exists());
        assert!(
            dir.path()
                .join(DEFAULT_PACK_VENDOR_DIR)
                .join("rules-pack.toml")
                .exists(),
            "默认规则包必须被 vendor 到项目内"
        );
        assert!(lock_path.exists(), "必须生成 sqlguard.lock");
        // 脚本模式的产物不应出现（两种 init 互不干扰）
        assert!(!dir.path().join("config/rules/ddl").exists());

        let cfg = Config::load(&cfg_path).expect("生成的配置必须可加载");
        assert_eq!(cfg.resolved_packs.len(), 1);
        assert_eq!(cfg.resolved_packs[0].name, "rules-core");

        let declared: MiniManifest = toml::from_str(get_default_pack_manifest()).unwrap();
        assert_eq!(cfg.rules.len(), declared.rules.len());

        // 锁文件必须与实际解析结果一致（Strict 模式亦通过）
        lock::verify_lock(&cfg, cfg_path.parent().unwrap(), lock::LockMode::Strict)
            .expect("生成的锁文件必须通过严格校验");

        // 幂等：重跑保留已存在文件
        std::fs::write(&rules_path, "# customized").unwrap();
        run_init_with_default_pack(dir.path(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rules_path).unwrap(),
            "# customized"
        );
    }

    // === run_init（幂等 init / --force） ===

    #[test]
    fn init_fresh_writes_all_files() {
        let dir = tempfile::tempdir().unwrap();
        run_init(dir.path(), false).unwrap();
        assert!(dir.path().join("sqlguard.toml").exists());
        assert!(dir.path().join("sqlguard.rules.toml").exists());
        let ddl = dir.path().join("config/rules/ddl");
        let dml = dir.path().join("config/rules/dml");
        let ddl_n = std::fs::read_dir(&ddl).unwrap().count();
        let dml_n = std::fs::read_dir(&dml).unwrap().count();
        assert_eq!(
            ddl_n,
            INIT_RULE_SCRIPTS.iter().filter(|(_, t, _)| *t == "ddl").count()
        );
        assert_eq!(
            dml_n,
            INIT_RULE_SCRIPTS.iter().filter(|(_, t, _)| *t == "dml").count()
        );
    }

    #[test]
    fn init_is_idempotent_keeps_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        run_init(dir.path(), false).unwrap();
        let rules_path = dir.path().join("sqlguard.rules.toml");
        // 模拟用户/工具链（如 gates-toolkit 模板）定制过的配置
        std::fs::write(&rules_path, "# customized").unwrap();
        let script = dir.path().join("config/rules/dml/no_select_all.rhai");
        std::fs::write(&script, "// customized").unwrap();

        // 无 --force 重跑：已存在文件原样保留
        run_init(dir.path(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rules_path).unwrap(),
            "# customized",
            "重跑 init 不得覆盖已存在的规则文件"
        );
        assert_eq!(
            std::fs::read_to_string(&script).unwrap(),
            "// customized",
            "重跑 init 不得覆盖已存在的规则脚本"
        );
        // 缺失文件仍会被补齐
        std::fs::remove_file(&script).unwrap();
        run_init(dir.path(), false).unwrap();
        assert!(script.exists(), "缺失的脚本应被补写");
    }

    #[test]
    fn init_force_overwrites_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        run_init(dir.path(), false).unwrap();
        let rules_path = dir.path().join("sqlguard.rules.toml");
        std::fs::write(&rules_path, "# customized").unwrap();
        run_init(dir.path(), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rules_path).unwrap(),
            get_default_rules_content(),
            "--force 应恢复为内嵌默认内容"
        );
    }

    #[test]
    fn default_contents_parse_as_toml() {
        let cfg: toml::Value = toml::from_str(get_default_config_content())
            .expect("sqlguard.toml.example 必须是合法 TOML");
        let rules: toml::Value = toml::from_str(get_default_rules_content())
            .expect("sqlguard.rules.toml.example 必须是合法 TOML");
        assert!(cfg.get("structure").is_some());
        assert!(rules.get("rules").and_then(|r| r.as_array()).map(|a| !a.is_empty()).unwrap_or(false));
    }

    #[test]
    fn init_rule_scripts_cover_example_declarations() {
        // example 声明的每个 script_path 必须能由 init 落盘，否则 init 出的
        // 规则文件会引用不存在的脚本（表现为运行时 "Rule script not found"）。
        for line in get_default_rules_content().lines() {
            if let Some(rest) = line.trim().strip_prefix("script_path = ") {
                let declared = rest.trim().trim_matches('"');
                let covered = INIT_RULE_SCRIPTS
                    .iter()
                    .any(|(name, ty, _)| format!("config/rules/{}/{}.rhai", ty, name) == declared);
                assert!(
                    covered,
                    "sqlguard.rules.toml.example 声明的脚本 {} 不在 init 写入清单中",
                    declared
                );
            }
        }
    }

    // === progress_line ===

    #[test]
    fn progress_line_half_way() {
        assert_eq!(
            progress_line(3, 6, "a.sql"),
            Some("Exporting SQL manifest: 50% [==========----------] 3/6 files  a.sql".to_string())
        );
    }

    #[test]
    fn progress_line_truncates_long_filename() {
        let long = "very-very-long-mapper-file-name-that-exceeds-24-chars.xml";
        let line = progress_line(1, 2, long).unwrap();
        assert!(
            line.ends_with("1/2 files  …hat-exceeds-24-chars.xml"),
            "应保留右侧 24 字符，实际: {}",
            line
        );
    }

    #[test]
    fn progress_line_total_zero_is_none() {
        assert_eq!(progress_line(0, 0, "a.sql"), None);
    }

    fn make_violation(rule_id: &str, line: Option<usize>) -> Violation {
        Violation {
            rule_id: rule_id.to_string(),
            rule_name: "test".to_string(),
            rule_group: None,
            severity: "error".to_string(),
            message: "test".to_string(),
            file_path: PathBuf::from("test.sql"),
            script_type: "dml".to_string(),
            line,
            end_line: line,
            column: None,
        }
    }

    // === resolve_absolute_path ===

    #[test]
    fn resolve_absolute_path_keeps_absolute() {
        // Windows 上 `/tmp/test` 是「根相对」而非绝对（仅 `\` 前缀算绝对），
        // join 后保留当前盘符成为 `C:/tmp/test`；Unix 上原样保留。
        let abs = Path::new("/tmp/test");
        let result = resolve_absolute_path(abs);
        assert!(result.is_absolute());
        assert!(
            result.ends_with(Path::new("tmp/test")),
            "不应拼接 cwd 目录，实际: {:?}",
            result
        );
        #[cfg(not(windows))]
        assert_eq!(result, PathBuf::from("/tmp/test"));
    }

    #[test]
    fn resolve_absolute_path_joins_cwd_for_relative() {
        let rel = Path::new("foo/bar.sql");
        let result = resolve_absolute_path(rel);
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(result, cwd.join("foo/bar.sql"));
    }

    // === resolve_output_dir ===

    #[test]
    fn resolve_output_dir_none_falls_back_to_target() {
        let target = Path::new("/tmp/target");
        let result = resolve_output_dir(None, target);
        assert_eq!(result, PathBuf::from("/tmp/target"));
    }

    #[test]
    fn resolve_output_dir_absolute_kept() {
        let result = resolve_output_dir(Some(Path::new("/tmp/out")), Path::new("/tmp/target"));
        assert!(result.is_absolute());
        assert!(
            result.ends_with(Path::new("tmp/out")),
            "不应拼接 cwd 目录，实际: {:?}",
            result
        );
        #[cfg(not(windows))]
        assert_eq!(result, PathBuf::from("/tmp/out"));
    }

    #[test]
    fn resolve_output_dir_relative_joined_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let result = resolve_output_dir(Some(Path::new("out")), Path::new("/tmp/target"));
        assert_eq!(result, cwd.join("out"));
    }

    // === parse_formats ===

    #[test]
    fn parse_formats_all() {
        let result = parse_formats("all");
        assert_eq!(result, vec!["plain", "json", "html", "sarif"]);
    }

    #[test]
    fn parse_formats_single() {
        let result = parse_formats("json");
        assert_eq!(result, vec!["json"]);
    }

    #[test]
    fn parse_formats_csv() {
        let result = parse_formats("json, html, sarif");
        assert_eq!(result, vec!["json", "html", "sarif"]);
    }

    // === filter_violations_by_diff ===

    #[test]
    fn filter_violations_new_file_keeps_all() {
        let violations = vec![make_violation("R1", Some(1)), make_violation("R2", Some(5))];
        let diff = FileDiff {
            path: PathBuf::from("test.sql"),
            hunks: vec![],
            old_hunks: vec![],
            is_new: true,
        };
        let result = filter_violations_by_diff(violations, &diff);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_violations_in_hunk_kept() {
        let violations = vec![make_violation("R1", Some(10))];
        let diff = FileDiff {
            path: PathBuf::from("test.sql"),
            hunks: vec![(8, 15)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = filter_violations_by_diff(violations, &diff);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_violations_outside_hunk_filtered() {
        let violations = vec![make_violation("R1", Some(100))];
        let diff = FileDiff {
            path: PathBuf::from("test.sql"),
            hunks: vec![(8, 15)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = filter_violations_by_diff(violations, &diff);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn filter_violations_no_line_kept() {
        let violations = vec![make_violation("R1", None)];
        let diff = FileDiff {
            path: PathBuf::from("test.sql"),
            hunks: vec![(8, 15)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = filter_violations_by_diff(violations, &diff);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_violations_range_overlaps_hunk() {
        // violation spans lines 10-20, hunk is 15-25 → overlap
        let mut v = make_violation("R1", Some(10));
        v.end_line = Some(20);
        let diff = FileDiff {
            path: PathBuf::from("test.sql"),
            hunks: vec![(15, 25)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = filter_violations_by_diff(vec![v], &diff);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_violations_line_no_endline_kept() {
        // P1-2：只有 line、无 end_line 的违规保守保留——无法确定语句行范围，
        // 「语句跨多行但只带起始行」时按单点判断会误删，宁可误报。
        let mut v = make_violation("R1", Some(100));
        v.end_line = None;
        let diff = FileDiff {
            path: PathBuf::from("test.sql"),
            hunks: vec![(8, 15)],
            old_hunks: vec![],
            is_new: false,
        };
        let result = filter_violations_by_diff(vec![v], &diff);
        assert_eq!(result.len(), 1);
    }

    // === extract_sql_lines ===

    #[test]
    fn extract_sql_lines_basic() {
        let content = "line1\nline2\nline3\nline4";
        let result = extract_sql_lines(content, 2, 3);
        assert_eq!(result, "line2\nline3");
    }

    #[test]
    fn extract_sql_lines_single() {
        let content = "a\nb\nc";
        let result = extract_sql_lines(content, 1, 1);
        assert_eq!(result, "a");
    }

    #[test]
    fn extract_sql_lines_full() {
        let content = "a\nb\nc";
        let result = extract_sql_lines(content, 1, 3);
        assert_eq!(result, "a\nb\nc");
    }

    #[test]
    fn extract_sql_lines_out_of_range() {
        let content = "a\nb";
        let result = extract_sql_lines(content, 5, 10);
        assert_eq!(result, "");
    }
}
