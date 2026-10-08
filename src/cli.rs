use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[clap(
    name = "sqlguard",
    version,
    about = "SQL script checking tool with customizable rules engine"
)]
pub struct Cli {
    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Check SQL scripts in the specified directory
    Check {
        /// Path to the project/sql directory to check
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Path to configuration file
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Output format(s): plain, json, html, sarif, all
        #[clap(short, long, default_value = "plain")]
        format: String,

        /// Output directory for reports (required for json/html)
        #[clap(short, long)]
        output_dir: Option<PathBuf>,

        /// Only run rules whose id matches (comma-separated, supports prefix wildcard `DDL*`)
        #[clap(long)]
        rules: Option<String>,

        /// Only run rules whose group matches (comma-separated, supports prefix wildcard)
        #[clap(long)]
        groups: Option<String>,

        /// Exclude rules whose id matches (comma-separated, supports prefix wildcard)
        #[clap(long)]
        exclude_rules: Option<String>,

        /// Only run rules whose group matches (comma-separated, supports prefix wildcard)
        #[clap(long)]
        exclude_groups: Option<String>,

        /// Override [dialect] in config: generic / mysql / postgresql / ansi / oracle / gaussdb
        #[clap(long)]
        dialect: Option<String>,

        /// Override [dialect_fallback] in config (second-choice dialect for
        /// per-statement fallback). generic / mysql / postgresql / ansi / oracle / gaussdb.
        /// Pass `generic` to disable fallback (chain collapses to "primary -> Generic").
        #[clap(long)]
        dialect_fallback: Option<String>,

        /// Force-enable file cache (overrides [cache].enabled = false).
        /// Mutually exclusive with --no-cache.
        #[clap(long)]
        cache: bool,

        /// Force-disable file cache (overrides [cache].enabled = true).
        /// Mutually exclusive with --cache.
        #[clap(long)]
        no_cache: bool,

        /// Override [scan] encoding for reading scanned files (default utf-8).
        ///
        /// Supported labels: utf-8, gbk, gb2312, gb18030, big5, shift_jis
        /// (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, utf-32le,
        /// utf-32be, windows-1252 (latin1, iso-8859-1), ascii, ...
        /// Files with a BOM are decoded per the BOM regardless of this value.
        #[clap(long)]
        encoding: Option<String>,

        /// Treat a rule-pack `engine` version mismatch as an error (default: warn).
        ///
        /// `[pack].api_version` above the engine's supported version is always a
        /// hard error; this flag only affects the non-fatal `[pack].engine`
        /// version-range check.
        #[clap(long)]
        strict_engine: bool,

        /// Treat duplicate rule ids across rule sources as an error
        /// (default: warn and let the higher-priority source win).
        #[clap(long)]
        strict_ids: bool,

        /// Disallow a missing/out-of-date `sqlguard.lock` (CI mode).
        ///
        /// Also verifies pack checksums (reads all pack files). Mutually
        /// exclusive with --no-lock.
        #[clap(long)]
        locked: bool,

        /// Skip `sqlguard.lock` verification entirely.
        ///
        /// Mutually exclusive with --locked.
        #[clap(long)]
        no_lock: bool,
    },
    /// Export a SQL manifest (sql-manifest.json) for dynamic replay.
    ///
    /// Reuses the existing SQL/Mapper collection. Each statement is exported
    /// with its type (select/insert/update/delete/merge/ddl/other) and source
    /// location, ready to be consumed by the Java-side `sqlguard-replay`.
    ReplayExport {
        /// Path to the project/sql directory to scan.
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Path to configuration file.
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Output directory for sql-manifest.json.
        #[clap(short, long, default_value = ".")]
        output_dir: PathBuf,

        /// Only export statements whose type matches (comma-separated:
        /// select,insert,update,delete,merge,ddl,other). Empty = all.
        #[clap(long)]
        types: Option<String>,

        /// Export only statements changed since a git baseline (incremental mode).
        ///
        /// Diffs from the merge-base of `<base>` and HEAD to the working tree
        /// (uncommitted changes and untracked new files included) and exports
        /// only statements whose line range intersects a hunk (new files are
        /// fully exported). Deleted statements are written to
        /// `sql-manifest-removed.json`. Omit for a full export (current behavior).
        #[clap(long)]
        base: Option<String>,

        /// Override [scan] encoding for reading scanned files (default utf-8).
        ///
        /// Supported labels: utf-8, gbk, gb2312, gb18030, big5, shift_jis
        /// (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, utf-32le,
        /// utf-32be, windows-1252 (latin1, iso-8859-1), ascii, ...
        #[clap(long)]
        encoding: Option<String>,
    },
    /// Initialize default configuration in the current directory
    Init {
        /// Target directory
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Overwrite existing files (sqlguard.toml / sqlguard.rules.toml / rule scripts).
        ///
        /// Default is idempotent init: only missing files are written, existing
        /// files are kept untouched and reported as skipped. Pass --force to
        /// restore the old overwrite-everything behavior.
        #[clap(long)]
        force: bool,

        /// Initialize from the default rule packs instead of copying rule scripts
        /// into `config/rules/`.
        ///
        /// Writes the default packs (`rules-core` + `rules-gaussdb`) to
        /// `vendor/rules/<pack>/`, declares `[rule_packs]` in `sqlguard.toml`,
        /// and generates `sqlguard.lock`. The generated project is self-contained
        /// and offline-capable.
        #[clap(long)]
        with_default_pack: bool,
    },
    /// Only check SQL statements changed since a git baseline (incremental mode).
    ///
    /// Diffs from the merge-base of `<base>` and HEAD to the **working tree**,
    /// so uncommitted changes are included: staged, unstaged, and untracked
    /// new files (checked in full). Checks each changed file and keeps only
    /// violations whose statement range [line, end_line] intersects a hunk.
    /// On a clean CI checkout this equals `git diff <base>...HEAD`.
    CheckDiff {
        /// Git baseline: commit / branch / tag, e.g. `origin/main`, `HEAD~1`.
        #[clap(long)]
        base: String,

        /// Project path (default: current directory).
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Path to configuration file.
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Output format(s): plain, json, html, sarif, all.
        #[clap(short, long, default_value = "plain")]
        format: String,

        /// Output directory for reports (required for json/html/sarif).
        #[clap(short, long)]
        output_dir: Option<PathBuf>,

        /// Only run rules whose id matches (comma-separated, supports prefix wildcard `DDL*`).
        #[clap(long)]
        rules: Option<String>,

        /// Only run rules whose group matches (comma-separated, supports prefix wildcard).
        #[clap(long)]
        groups: Option<String>,

        /// Exclude rules whose id matches (comma-separated, supports prefix wildcard).
        #[clap(long)]
        exclude_rules: Option<String>,

        /// Exclude rules whose group matches (comma-separated, supports prefix wildcard).
        #[clap(long)]
        exclude_groups: Option<String>,

        /// Override [dialect] in config: generic / mysql / postgresql / ansi / oracle / gaussdb
        #[clap(long)]
        dialect: Option<String>,

        /// Override [dialect_fallback] in config (second-choice dialect for
        /// per-statement fallback). generic / mysql / postgresql / ansi / oracle / gaussdb.
        /// Pass `generic` to disable fallback (chain collapses to "primary -> Generic").
        #[clap(long)]
        dialect_fallback: Option<String>,

        /// Override [scan] encoding for reading scanned files (default utf-8).
        ///
        /// Supported labels: utf-8, gbk, gb2312, gb18030, big5, shift_jis
        /// (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, utf-32le,
        /// utf-32be, windows-1252 (latin1, iso-8859-1), ascii, ...
        #[clap(long)]
        encoding: Option<String>,

        /// Treat a rule-pack `engine` version mismatch as an error (default: warn).
        ///
        /// See `check --strict-engine` for details.
        #[clap(long)]
        strict_engine: bool,

        /// Treat duplicate rule ids across rule sources as an error
        /// (default: warn and let the higher-priority source win).
        #[clap(long)]
        strict_ids: bool,

        /// Disallow a missing/out-of-date `sqlguard.lock` (CI mode).
        ///
        /// See `check --locked` for details. Mutually exclusive with --no-lock.
        #[clap(long)]
        locked: bool,

        /// Skip `sqlguard.lock` verification entirely.
        #[clap(long)]
        no_lock: bool,
    },
    /// Generate backup/rollback scripts for DDL/DML files.
    ///
    /// Produces backup.sql / rollback.sql / rollback-manifest.json / cleanup.sql
    /// in the output directory. Exit code follows the manifest:
    ///   0 = all reliable, 1 = warnings only, 2 = errors (irreversible/unreliable/partial)
    GenRollback {
        /// Path to the project/sql directory to scan.
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Path to configuration file.
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Output directory for backup.sql / rollback.sql / manifest / cleanup.
        #[clap(short, long, default_value = ".")]
        output_dir: PathBuf,

        /// Override [rollback].dialect: mysql / mariadb / postgresql / gaussdb.
        ///
        /// `postgres`/`pg` and `gauss`/`gaussdb` are accepted aliases.
        #[clap(long)]
        dialect: Option<String>,

        /// Override [rollback].lock_scope: auto / global / table / snapshot / none.
        #[clap(long)]
        lock_scope: Option<String>,

        /// Override [rollback].lock_timeout (seconds).
        #[clap(long)]
        lock_timeout: Option<u64>,

        /// Required when lock_scope=table (accept implicit-commit release risk).
        #[clap(long)]
        accept_table_lock_risk: bool,

        /// Treat warnings as errors (exit 2 instead of 1).
        #[clap(long)]
        fail_on_warning: bool,

        /// Allow partial rollback plans (do not exit 2 on safety.partial=true).
        #[clap(long)]
        allow_partial: bool,

        /// Generate `rollback-review-report.html` alongside the manifest,
        /// highlighting statements that require manual review (by risk level).
        #[clap(long)]
        review_report: bool,

        /// Override [scan] encoding for reading scanned files (default utf-8).
        ///
        /// Supported labels: utf-8, gbk, gb2312, gb18030, big5, shift_jis
        /// (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, utf-32le,
        /// utf-32be, windows-1252 (latin1, iso-8859-1), ascii, ...
        #[clap(long)]
        encoding: Option<String>,
    },
    /// Explain the AST structure of a SQL file or Mapper XML.
    ///
    /// Parses the file with the configured dialect and prints the AST in a
    /// human-readable format. Helps rule developers understand what fields and
    /// values are available when writing Rhai rule scripts.
    ///
    /// Examples:
    ///   sqlguard explain examples/sql/ddl/create_users.sql
    ///   sqlguard explain examples/mapper/UserMapper.xml --mapper
    ///   sqlguard explain query.sql --dialect mysql --json
    Explain {
        /// Path to the SQL file (or Mapper XML with --mapper).
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Path to configuration file (for dialect / mapper settings).
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Override [dialect] in config: generic / mysql / postgresql / ansi / oracle / gaussdb.
        #[clap(long)]
        dialect: Option<String>,

        /// Override [dialect_fallback] in config.
        #[clap(long)]
        dialect_fallback: Option<String>,

        /// Treat the input as a MyBatis Mapper XML file.
        #[clap(long)]
        mapper: bool,

        /// Output AST as JSON instead of human-readable text.
        #[clap(long)]
        json: bool,

        /// Override [scan] encoding for reading scanned files (default utf-8).
        ///
        /// Supported labels: utf-8, gbk, gb2312, gb18030, big5, shift_jis
        /// (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, utf-32le,
        /// utf-32be, windows-1252 (latin1, iso-8859-1), ascii, ...
        #[clap(long)]
        encoding: Option<String>,
    },
    /// Manage rule packs: list the effective rule set, generate/verify
    /// `sqlguard.lock`, vendor packs locally, and add a pack to the config.
    Rules {
        #[clap(subcommand)]
        action: RulesAction,
    },
}

/// `sqlguard rules` 的子命令。
#[derive(Subcommand)]
pub enum RulesAction {
    /// List the effective rule set (after pack merge + overrides).
    List {
        /// Path to configuration file (default: sqlguard.toml).
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Emit JSON instead of a table.
        #[clap(long)]
        json: bool,
    },
    /// Generate / update `sqlguard.lock` from the resolved rule packs.
    Lock {
        /// Path to configuration file (default: sqlguard.toml).
        #[clap(short, long)]
        config: Option<PathBuf>,
    },
    /// Verify `sqlguard.lock` and pack version compatibility (no checks run).
    Verify {
        /// Path to configuration file (default: sqlguard.toml).
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Also verify checksums (reads all pack files) and require the lock
        /// to be complete. Same semantics as `check --locked`.
        #[clap(long)]
        locked: bool,
    },
    /// Copy resolved rule packs into a local directory (offline / committable).
    Vendor {
        /// Path to configuration file (default: sqlguard.toml).
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Destination directory, relative to the config directory.
        /// Default: `vendor/rules`.
        #[clap(short, long)]
        output_dir: Option<PathBuf>,
    },
    /// Add a rule pack to the config and refresh `sqlguard.lock`.
    ///
    /// SPEC is `name`, `name@version`, or a path to a pack directory.
    /// The entry is appended to the config file; if the result no longer
    /// parses, the file is restored and the command fails.
    Add {
        /// Pack spec: `name`, `name@version`, or a pack directory path.
        spec: String,

        /// Path to configuration file (default: sqlguard.toml).
        #[clap(short, long)]
        config: Option<PathBuf>,
    },
}
