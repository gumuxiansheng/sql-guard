use std::path::{Path, PathBuf};
use std::fs;

use clap::Parser;

// 原 binary 以 crate-root `mod` 形式直接引用各模块；提升为 lib 后，
// 用 glob 导入把顶层模块重新带入本 bin 的作用域（等价旧行为）。
use sqlguard::*;
use sqlguard::cli::{Cli, Commands};
use sqlguard::error::{SqlGuardError, Violation};
use sqlguard::config::Config;
use sqlguard::checker::directory;
use sqlguard::checker::classification;
use sqlguard::checker::encoding;
use sqlguard::rule::engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        } => {
            let config_path = config.clone().unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
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
            )?;
        }
        Commands::Init { path } => {
            run_init(&path)?;
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
        } => {
            let config_path = config.clone().unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
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
            )?;
        }
        Commands::ReplayExport {
            path,
            config,
            output_dir,
            types,
        } => {
            let config_path = config.clone().unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
            let explicit_config = config.is_some();
            run_replay_export(&path, &config_path, explicit_config, &output_dir, &types)?;
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
        } => {
            let config_path = config.clone().unwrap_or_else(|| PathBuf::from("sqlguard.toml"));
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
            )?;
            std::process::exit(code);
        }
    }

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
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| config_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf());
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
        let classification_result = classification::classify_file(file_path, &config.classification)?;
        // 编码 / 换行符检查属于轻量的文件属性检查，不进缓存（每次都跑）
        all_violations.extend(encoding::check_file(file_path, &classification_result.script_type, &config.file_check, filter));

        // 查缓存：命中则跳过读文件 + 解析 + 规则执行
        if let Some(cached) = cache.get(file_path) {
            all_violations.extend(cached);
            continue;
        }

        let sql_content = fs::read_to_string(file_path)
            .map_err(|e| SqlGuardError::CheckError(format!("Failed to read '{}': {}", file_path.display(), e)))?;
        let violations = engine::run_rules_for_file(engine_instance, file_path, &sql_content, &classification_result.script_type, config, config_dir, filter, 0)?;
        // 写入缓存（clone 一份，原始 violations 用于本次输出）
        cache.insert(file_path, violations.clone());
        all_violations.extend(violations);
    }

    // Mapper 模式
    if config.mapper.enabled {
        for file_path in mapper_files {
            let mapper_script_type = classification::classify_file(file_path, &config.classification)
                .map(|r| r.script_type)
                .unwrap_or_else(|_| "mapper".to_string());
            all_violations.extend(encoding::check_file(file_path, &mapper_script_type, &config.file_check, filter));

            // 查缓存：命中则跳过 XML 解析 + 逐条规则执行
            if let Some(cached) = cache.get(file_path) {
                all_violations.extend(cached);
                continue;
            }

            let extracted = match mapper::extract_sql_from_xml(file_path) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Warning: failed to parse mapper XML '{}': {}", file_path.display(), e);
                    continue;
                }
            };
            let mut file_violations = Vec::new();
            for sql in extracted {
                let script_type = mapper::map_statement_type(&sql.statement_type, &config.mapper.statement_type_mapping);
                let line_offset = sql.raw_xml_line.saturating_sub(1);
                let violations = engine::run_rules_for_file(engine_instance, file_path, &sql.processed_sql, script_type, config, config_dir, filter, line_offset)?;
                file_violations.extend(violations);
            }
            cache.insert(file_path, file_violations.clone());
            all_violations.extend(file_violations);
        }
    }

    Ok((all_violations, files_checked))
}

// ===== run_check =====

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
) -> Result<(), SqlGuardError> {
    if cache_flag && no_cache_flag {
        return Err(SqlGuardError::CheckError(
            "--cache and --no-cache are mutually exclusive".to_string(),
        ));
    }
    let (mut config, config_dir) = load_config(config_path, explicit_config)?;
    if let Some(d) = dialect_override {
        config.dialect = sqlguard::config::CheckDialect::from_str(d);
        eprintln!("Dialect override: {} → {}", config.dialect.as_str(), d);
    }
    // 解析方言回退链：CLI --dialect-fallback > 配置 dialect_fallback > 主方言默认回退。
    let fallback = if let Some(fb) = dialect_fallback_override {
        Some(sqlguard::config::CheckDialect::from_str(fb))
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

    let sql_files = classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
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
    let (all_violations, files_checked) = check_files(&config, &config_dir, &filter, &engine_instance, &sql_files, &mapper_files, &mut file_cache)?;

    // 写回缓存（仅在启用且有变更时实际落盘）
    file_cache.flush();

    let formats = parse_formats(format);
    let output_dir_path = resolve_output_dir(output_dir, &absolute_target);
    reporter::output_reports(&all_violations, &missing, &unexpected, files_checked, &formats, &output_dir_path)
        .map_err(SqlGuardError::CheckError)?;

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    let has_missing = !missing.is_empty();

    if has_errors || has_missing {
        Err(SqlGuardError::CheckError("Checks failed".to_string()))
    } else {
        Ok(())
    }
}

// ===== run_replay_export =====

fn run_replay_export(
    target_dir: &Path,
    config_path: &Path,
    explicit_config: bool,
    output_dir: &Path,
    types: &Option<String>,
) -> Result<(), SqlGuardError> {
    let (config, _config_dir) = load_config(config_path, explicit_config)?;

    let absolute_target = resolve_absolute_path(target_dir);

    let effective_scan_paths: Vec<String> = if !config.scan.paths.is_empty() {
        config.scan.paths.clone()
    } else {
        config.structure.paths.clone()
    };
    let exclude_dirs: &[String] = &config.scan.exclude_dirs;

    let sql_files = classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
    let mapper_files = mapper::collect_mapper_files(&absolute_target, &config.mapper, exclude_dirs);

    let type_filter = replay_export::parse_type_filter(types);

    let manifest = replay_export::build_manifest(
        &absolute_target,
        &sql_files,
        &mapper_files,
        &type_filter,
    )?;

    let json = replay_export::manifest_to_json(&manifest)?;

    let output_dir_abs = resolve_absolute_path(output_dir);
    if !output_dir_abs.exists() {
        fs::create_dir_all(&output_dir_abs).map_err(SqlGuardError::IoError)?;
    }
    let manifest_path = output_dir_abs.join("sql-manifest.json");
    fs::write(&manifest_path, &json).map_err(SqlGuardError::IoError)?;

    eprintln!(
        "Replay manifest exported: {} ({} statements, {} sql files, {} mapper files)",
        manifest_path.display(),
        manifest.statement_count,
        sql_files.len(),
        mapper_files.len(),
    );

    Ok(())
}

// ===== run_check_diff =====

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
) -> Result<(), SqlGuardError> {
    let (mut config, config_dir) = load_config(config_path, explicit_config)?;
    if let Some(d) = dialect_override {
        config.dialect = sqlguard::config::CheckDialect::from_str(d);
        eprintln!("Dialect override: {} → {}", config.dialect.as_str(), d);
    }
    // 解析方言回退链：CLI --dialect-fallback > 配置 dialect_fallback > 主方言默认回退。
    let fallback = if let Some(fb) = dialect_fallback_override {
        Some(sqlguard::config::CheckDialect::from_str(fb))
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

    eprintln!("Computing diff: {}...HEAD", base);
    let diffs = git_diff::get_diff(base, &patterns)?;

    if diffs.is_empty() {
        eprintln!("No SQL changes detected since {}", base);
        let formats = parse_formats(format);
        let output_dir_path = resolve_output_dir(output_dir, &std::env::current_dir().unwrap_or_default());
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
            ));
        }

        let is_xml = file_path
            .extension()
            .map_or(false, |e| e.eq_ignore_ascii_case("xml"));

        if is_xml {
            // mapper 模式：提取所有片段，逐条跑 + 过滤
            let extracted = match mapper::extract_sql_from_xml(file_path) {
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
                let script_type = mapper::map_statement_type(&sql.statement_type, &config.mapper.statement_type_mapping);
                let line_offset = sql.raw_xml_line.saturating_sub(1);
                let violations = engine::run_rules_for_file(
                    &engine_instance,
                    file_path,
                    &sql.processed_sql,
                    script_type,
                    &config,
                    &config_dir,
                    &filter,
                    line_offset,
                )?;
                let filtered = filter_violations_by_diff(violations, file_diff);
                all_violations.extend(filtered);
            }
        } else {
            // 脚本模式
            let classification_result =
                classification::classify_file(file_path, &config.classification)?;
            let sql_content = fs::read_to_string(file_path).map_err(|e| {
                SqlGuardError::CheckError(format!(
                    "Failed to read '{}': {}",
                    file_path.display(),
                    e
                ))
            })?;
            let violations = engine::run_rules_for_file(
                &engine_instance,
                file_path,
                &sql_content,
                &classification_result.script_type,
                &config,
                &config_dir,
                &filter,
                0,
            )?;
            let filtered = filter_violations_by_diff(violations, file_diff);
            all_violations.extend(filtered);
        }
    }

    // 3. 输出报告
    let formats = parse_formats(format);
    let output_dir_path = resolve_output_dir(output_dir, &absolute_target);
    reporter::output_reports(&all_violations, &[], &[], files_checked, &formats, &output_dir_path)
        .map_err(SqlGuardError::CheckError)?;

    let has_errors = all_violations.iter().any(|v| v.severity == "error");
    if has_errors {
        Err(SqlGuardError::CheckError("Incremental checks failed".to_string()))
    } else {
        Ok(())
    }
}

/// 语句级交集过滤：保留 violation 的 [line, end_line] 与任一 hunk [s, e] 有交集的违规。
/// - 新增文件：全保留
/// - violation 无 line：保守保留（可能是规则脚本错误，宁可误报）
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
            let end_line = v.end_line.unwrap_or(line);
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
) -> Result<i32, SqlGuardError> {
    let (mut config, _config_dir) = load_config(config_path, explicit_config)?;

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
    let dialect: sqlguard::rollback::Dialect = rc
        .dialect
        .parse()
        .map_err(|e: String| {
            SqlGuardError::ConfigError(format!("Invalid rollback.dialect '{}': {}", rc.dialect, e))
        })?;
    let renderer = sqlguard::rollback::renderer_for(dialect);

    // R2 预检：lock_scope=table 必须显式确认
    let prereq_errors = sqlguard::rollback::render::validate_render_prerequisites(&[], &config.rollback);
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

    let sql_files = classification::collect_sql_files(&absolute_target, &effective_scan_paths, exclude_dirs);
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

    let mut generator = sqlguard::rollback::RollbackGenerator::new(&config, &config.rollback, &*renderer);
    let mut pairs: Vec<sqlguard::rollback::BackupRollbackPair> = Vec::new();

    // 脚本模式：解析每个 SQL 文件为 SqlAst，遍历 StmtInfo
    for file_path in &sql_files {
        let content = fs::read_to_string(file_path).map_err(|e| {
            SqlGuardError::CheckError(format!("Failed to read '{}': {}", file_path.display(), e))
        })?;
        let ast = engine::parser::parse_sql_to_ast_fb(&content, config.dialect, config.dialect_fallback);

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
            let extracted = match mapper::extract_sql_from_xml(file_path) {
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
                if !config.rollback.include_select && sql.statement_type.eq_ignore_ascii_case("select") {
                    continue;
                }
                let ast = engine::parser::parse_sql_to_ast_fb(&sql.processed_sql, config.dialect, config.dialect_fallback);
                for stmt in &ast.statements {
                    if stmt.kind == "PARSE_ERROR" {
                        continue;
                    }
                    let original = extract_sql_lines(&sql.processed_sql, stmt.line as usize, stmt.end_line as usize);
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
        let errs = sqlguard::rollback::render::validate_render_prerequisites(group, &config.rollback);
        prereq_errors.extend(errs);
    }
    if !prereq_errors.is_empty() {
        for e in &prereq_errors {
            eprintln!("Prerequisite error: {}", e);
        }
        return Ok(2);
    }

    // ★ per-file 渲染：在 output_dir 下按输入目录结构镜像，每个源文件生成
    // <stem>.backup.sql / <stem>.rollback.sql（backup_file/rollback_file 作为后缀）
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
        let stem = rel_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let out_dir = absolute_output.join(parent_dir);
        fs::create_dir_all(&out_dir).map_err(SqlGuardError::IoError)?;

        let backup_sql =
            sqlguard::rollback::render::render_backup(group, &config.rollback, &*renderer);
        let rollback_sql =
            sqlguard::rollback::render::render_rollback(group, &config.rollback, &*renderer);

        let backup_out = out_dir.join(format!("{}.{}", stem, config.rollback.backup_file));
        let rollback_out = out_dir.join(format!("{}.{}", stem, config.rollback.rollback_file));

        fs::write(&backup_out, &backup_sql).map_err(SqlGuardError::IoError)?;
        fs::write(&rollback_out, &rollback_sql).map_err(SqlGuardError::IoError)?;

        eprintln!("Wrote {}", backup_out.display());
        eprintln!("Wrote {}", rollback_out.display());
    }

    // 全局 cleanup + manifest（跨文件汇总，写在 output_dir 根目录）
    let all_pairs: Vec<sqlguard::rollback::BackupRollbackPair> = file_groups
        .into_iter()
        .flat_map(|(_, g)| g)
        .collect();

    let cleanup_sql =
        sqlguard::rollback::render::render_cleanup(&all_pairs, &config.rollback, &*renderer);
    let cleanup_path = absolute_output.join(&config.rollback.cleanup_file);
    fs::write(&cleanup_path, &cleanup_sql).map_err(SqlGuardError::IoError)?;

    // 构建并写 manifest（★ P1-2：传入 rc 以透出 assert_on_schema_mismatch / on_partitioned_table）
    let manifest =
        sqlguard::rollback::Manifest::from_pairs(&all_pairs, dialect.as_str(), &config.rollback, Vec::new());
    let manifest_json = sqlguard::rollback::serialize_manifest(&manifest)
        .map_err(|e| SqlGuardError::CheckError(format!("Failed to serialize manifest: {}", e)))?;
    let manifest_path = absolute_output.join(&config.rollback.manifest_file);
    fs::write(&manifest_path, &manifest_json).map_err(SqlGuardError::IoError)?;

    eprintln!("Wrote {}", cleanup_path.display());
    eprintln!("Wrote {}", manifest_path.display());

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

fn run_init(target_dir: &Path) -> Result<(), SqlGuardError> {
    let rules_dir = target_dir.join("config").join("rules");
    for dir in &[rules_dir.join("ddl"), rules_dir.join("dml")] {
        fs::create_dir_all(dir).map_err(SqlGuardError::IoError)?;
    }

    let config_content = get_default_config_content();
    fs::write(target_dir.join("sqlguard.toml"), config_content)
        .map_err(SqlGuardError::IoError)?;

    let rules_content = get_default_rules_content();
    fs::write(target_dir.join("sqlguard.rules.toml"), rules_content)
        .map_err(SqlGuardError::IoError)?;

    // (name, subdir, content) — content 通过 include_str! 编译时嵌入
    let rules: &[(&str, &str, &str)] = &[
        ("no_drop_table", "ddl", include_str!("../config/rules/ddl/no_drop_table.rhai")),
        ("primary_key_required", "ddl", include_str!("../config/rules/ddl/primary_key_required.rhai")),
        ("no_reserved_keyword_naming", "ddl", include_str!("../config/rules/ddl/no_reserved_keyword_naming.rhai")),
        ("backup_table_naming", "ddl", include_str!("../config/rules/ddl/backup_table_naming.rhai")),
        ("index_naming_convention", "ddl", include_str!("../config/rules/ddl/index_naming_convention.rhai")),
        ("no_redundant_index", "ddl", include_str!("../config/rules/ddl/no_redundant_index.rhai")),
        ("no_select_all", "dml", include_str!("../config/rules/dml/no_select_all.rhai")),
        ("no_delete_update_without_where", "dml", include_str!("../config/rules/dml/no_delete_update_without_where.rhai")),
        ("insert_columns_required", "dml", include_str!("../config/rules/dml/insert_columns_required.rhai")),
        ("subquery_alias_required", "dml", include_str!("../config/rules/dml/subquery_alias_required.rhai")),
        ("column_references_qualified", "dml", include_str!("../config/rules/dml/column_references_qualified.rhai")),
        ("no_join_without_condition", "dml", include_str!("../config/rules/dml/no_join_without_condition.rhai")),
        ("no_unused_join", "dml", include_str!("../config/rules/dml/no_unused_join.rhai")),
        ("no_unused_cte", "dml", include_str!("../config/rules/dml/no_unused_cte.rhai")),
        ("use_is_null", "dml", include_str!("../config/rules/dml/use_is_null.rhai")),
        ("use_coalesce", "dml", include_str!("../config/rules/dml/use_coalesce.rhai")),
        ("no_order_by_in_subquery", "dml", include_str!("../config/rules/dml/no_order_by_in_subquery.rhai")),
        ("union_all_preferred", "dml", include_str!("../config/rules/dml/union_all_preferred.rhai")),
        ("no_nested_case", "dml", include_str!("../config/rules/dml/no_nested_case.rhai")),
        ("no_constant_where", "dml", include_str!("../config/rules/dml/no_constant_where.rhai")),
        ("order_by_required_for_pagination", "dml", include_str!("../config/rules/dml/order_by_required_for_pagination.rhai")),
    ];

    for (name, rule_type, content) in rules {
        let path = target_dir.join("config").join("rules").join(rule_type).join(format!("{}.rhai", name));
        fs::write(&path, content).map_err(SqlGuardError::IoError)?;
    }

    println!("Initialized SqlGuard configuration in {}", target_dir.display());
    println!("  - sqlguard.toml          # 主配置（结构/分类/输出/扫描/文件检查）");
    println!("  - sqlguard.rules.toml    # 规则配置（[[rules]] 单独拆分，避免文件过长）");
    println!("  - config/rules/ddl/ (6 rule files)");
    println!("  - config/rules/dml/ (15 rule files)");
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
            },
        ],
        rules_file: None,
        rules_dir: PathBuf::new(),
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
    }
}

fn get_default_config_content() -> &'static str {
    r#"[structure]
paths = [
  "sql/ddl",
  "sql/dml",
  "sql/others",
]
strict = true
allow_extra = [".gitkeep", "config/"]

[classification]
default_type = "other"

[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10

[[classification.rules]]
name = "dml-by-dir"
pattern = "**/dml/**"
type = "dml"
priority = 10

[[classification.rules]]
name = "other-by-others-dir"
pattern = "**/others/**"
type = "other"
priority = 10

[[classification.rules]]
name = "sql-by-ext"
pattern = "*.sql"
type = "sql"
priority = 0

# ================================================================================
# 规则配置：单独拆分到 sqlguard.rules.toml，避免主配置文件随规则增多而过长。
# 不写本行时，工具也会自动在同目录查找 sqlguard.rules.toml。
# ================================================================================
rules_file = "sqlguard.rules.toml"

[output]
# 可用格式：plain（控制台）/ json / html / sarif（接 GitHub/Azure/GitLab code scanning）
formats = ["plain", "json", "html", "sarif"]

# MyBatis Mapper 模式：扫描 XML 中的 <select>/<insert>/<update>/<delete>。
# 缺省或 enabled = false 时完全保持现有行为（仅扫描 .sql/.ddl/.dml）。
# [mapper]
# enabled = true
# paths = ["src/main/resources/mapper"]
# patterns = ["**/*Mapper.xml", "**/*.xml"]

# ================================================================================
# 文件扫描行为配置 [scan]
# ================================================================================
# 控制白名单扫描根与黑名单跳过目录，避免递归进入 .git/target/node_modules 等大目录。
#
# exclude_dirs：递归扫描时跳过的目录名（按名称匹配，任意层级生效）。
#   默认值见下，未配置 [scan] 段时也按默认黑名单生效。
#   适用于：SQL 脚本扫描、Mapper XML 扫描、目录结构校验三个场景。
#
# paths：SQL 脚本扫描白名单（相对配置文件目录或绝对路径）。
#   为空时回退到 [structure].paths，仍为空则扫描整个 target_dir（兜底）。
#   指定后只扫描这些目录下的 .sql/.ddl/.dml，散落在白名单外的 SQL 会被忽略。
#
# [scan]
# paths = []
# exclude_dirs = [
#   ".git", ".svn", ".hg", ".bzr",       # 版本控制元数据
#   "target", "node_modules", "build", "dist", "out",  # 构建产物
#   ".idea", ".vscode",                   # IDE 配置
# ]

# ================================================================================
# 文件格式检查 [file_check]
# ================================================================================
# 对扫描到的每个文件做字节级检查（独立于 SQL 语法规则）：
#   FILE001  编码必须为 UTF-8 且不带 BOM（severity = error，必须）
#   FILE002  换行符应为 LF（severity = warning，提示）
# 两条检查归入 file-format 分组，可用 --exclude-rules FILE001,FILE002
# 或 --exclude-groups file-format 临时关闭。
# 缺省（未写 [file_check] 段）时按下方默认值启用。

[file_check]
enabled = true
check_encoding = true               # UTF-8 无 BOM 检查（FILE001）
check_line_ending = true            # 换行符 LF 检查（FILE002）
encoding_severity = "error"         # 编码违规级别（必须）
line_ending_severity = "warning"    # 换行符违规级别（提示）
"#
}

/// 默认规则配置内容（独立文件 sqlguard.rules.toml）。
///
/// 仅含 `[[rules]]` 数组；脚本路径（script_path）相对本文件所在目录解析。
fn get_default_rules_content() -> &'static str {
    r#"# ================================================================================
# 规则配置（独立文件）
#
# 每条 [[rules]] 对应一个 Rhai 脚本。脚本路径（script_path）相对本文件
# 所在目录解析，也支持绝对路径。
#
# 在 sqlguard.toml 中用 rules_file = "sqlguard.rules.toml" 引用本文件；
# 不写该行时，工具也会自动在同目录查找 sqlguard.rules.toml。
# ================================================================================

# ================================================================================
# P0 规则：默认启用，建议 CI 中保持开启
# ================================================================================

[[rules]]
id = "DDL001"
name = "no_drop_table"
group = "ddl-safety"
description = "Disallow DROP TABLE in DDL scripts"
enabled = true
script_path = "config/rules/ddl/no_drop_table.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "DDL002"
name = "primary_key_required"
group = "ddl-safety"
description = "CREATE TABLE must have a PRIMARY KEY"
enabled = true
script_path = "config/rules/ddl/primary_key_required.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL003"
name = "no_reserved_keyword_naming"
group = "ddl-safety"
description = "Database object names must not use SQL reserved keywords"
enabled = true
script_path = "config/rules/ddl/no_reserved_keyword_naming.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "DDL004"
name = "backup_table_naming"
group = "ddl-convention"
description = "Backup tables created via CREATE TABLE AS SELECT must be prefixed with 'bks_'"
enabled = true
script_path = "config/rules/ddl/backup_table_naming.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL005"
name = "index_naming_convention"
group = "ddl-convention"
description = "Indexes follow idx_/uk_/pk_ naming convention based on type and columns"
enabled = true
script_path = "config/rules/ddl/index_naming_convention.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL006"
name = "no_redundant_index"
group = "ddl-performance"
description = "Avoid redundant indexes (duplicate PK indexes and leftmost-prefix duplicates)"
enabled = true
script_path = "config/rules/ddl/no_redundant_index.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DML001"
name = "no_select_all"
group = "dml-safety"
description = "Disallow SELECT * in DML scripts"
enabled = true
script_path = "config/rules/dml/no_select_all.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML002"
name = "no_delete_update_without_where"
group = "dml-safety"
description = "DELETE/UPDATE must have a WHERE clause"
enabled = true
script_path = "config/rules/dml/no_delete_update_without_where.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML003"
name = "insert_columns_required"
group = "dml-safety"
description = "INSERT must specify target columns"
enabled = true
script_path = "config/rules/dml/insert_columns_required.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML004"
name = "subquery_alias_required"
group = "dml-style"
description = "Subqueries in FROM must have an alias"
enabled = true
script_path = "config/rules/dml/subquery_alias_required.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML005"
name = "column_references_qualified"
group = "dml-style"
description = "Qualify column references with table name in multi-table queries"
enabled = true
script_path = "config/rules/dml/column_references_qualified.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML006"
name = "no_join_without_condition"
group = "dml-safety"
description = "JOIN must have ON or USING condition"
enabled = true
script_path = "config/rules/dml/no_join_without_condition.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML007"
name = "order_by_required_for_pagination"
group = "dml-safety"
description = "Pagination queries (LIMIT/OFFSET/FETCH) must have ORDER BY for deterministic results"
enabled = true
script_path = "config/rules/dml/order_by_required_for_pagination.rhai"
applies_to = ["dml"]
severity = "error"

# ================================================================================
# P1 规则：默认禁用，建议评估后启用
# 在 sqlguard.rules.toml 中将 enabled = false 改为 true 即可启用
# ================================================================================

[[rules]]
id = "DML101"
name = "no_unused_join"
group = "dml-performance"
description = "Detect potentially unused JOINs"
enabled = false
script_path = "config/rules/dml/no_unused_join.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML102"
name = "no_unused_cte"
group = "dml-performance"
description = "Detect unused CTEs (WITH clauses)"
enabled = false
script_path = "config/rules/dml/no_unused_cte.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML103"
name = "use_is_null"
group = "dml-convention"
description = "Use IS NULL instead of = NULL"
enabled = false
script_path = "config/rules/dml/use_is_null.rhai"
applies_to = ["dml"]
severity = "error"

[[rules]]
id = "DML104"
name = "use_coalesce"
group = "dml-convention"
description = "Use standard COALESCE instead of NVL/ISNULL"
enabled = false
script_path = "config/rules/dml/use_coalesce.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML105"
name = "no_order_by_in_subquery"
group = "dml-performance"
description = "ORDER BY in subquery is typically ignored"
enabled = false
script_path = "config/rules/dml/no_order_by_in_subquery.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML106"
name = "union_all_preferred"
group = "dml-performance"
description = "Prefer UNION ALL over UNION unless dedup needed"
enabled = false
script_path = "config/rules/dml/union_all_preferred.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML107"
name = "no_nested_case"
group = "dml-convention"
description = "Avoid nested CASE expressions"
enabled = false
script_path = "config/rules/dml/no_nested_case.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML108"
name = "no_constant_where"
group = "dml-convention"
description = "Avoid constant conditions in WHERE clause"
enabled = false
script_path = "config/rules/dml/no_constant_where.rhai"
applies_to = ["dml"]
severity = "warning"
"#
}
