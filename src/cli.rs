use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[clap(name = "sqlguard", version, about = "SQL script checking tool with customizable rules engine")]
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
        #[clap(short, long, default_value = "sqlguard.toml")]
        config: PathBuf,

        /// Output format(s): plain, json, html, all
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
        #[clap(short, long, default_value = "sqlguard.toml")]
        config: PathBuf,

        /// Output directory for sql-manifest.json.
        #[clap(short, long, default_value = ".")]
        output_dir: PathBuf,

        /// Only export statements whose type matches (comma-separated:
        /// select,insert,update,delete,merge,ddl,other). Empty = all.
        #[clap(long)]
        types: Option<String>,
    },
    /// Initialize default configuration in the current directory
    Init {
        /// Target directory
        #[clap(default_value = ".")]
        path: PathBuf,
    },
    /// Only check SQL statements changed since a git baseline (incremental mode).
    ///
    /// Runs `git diff --unified=0 <base>...HEAD` to get changed hunks, then
    /// checks each changed file and keeps only violations whose statement
    /// range [line, end_line] intersects a hunk. Useful for CI.
    CheckDiff {
        /// Git baseline: commit / branch / tag, e.g. `origin/main`, `HEAD~1`.
        #[clap(long)]
        base: String,

        /// Project path (default: current directory).
        #[clap(default_value = ".")]
        path: PathBuf,

        /// Path to configuration file.
        #[clap(short, long, default_value = "sqlguard.toml")]
        config: PathBuf,

        /// Output format(s): plain, json, html, all.
        #[clap(short, long, default_value = "plain")]
        format: String,

        /// Output directory for reports (required for json/html).
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
    },
}
