//! SqlGuard 库 crate。
//!
//! 原 binary crate 的模块在此提升为库，同时提供二进制（`src/main.rs`）与库两个 target，
//! 使 `benches/`、`tests/` 中的外部 crate 能通过 `sqlguard::` 路径引用内部模块，
//! 也供独立工具（如 `sqlguard-mine` 逻辑外键挖掘器）共享——挖掘逻辑不会被链入主二进制
//! （依赖 `[profile.release]` 的 `lto = "fat"` 做跨 crate 死代码消除）。

pub mod cache;
pub mod checker;
pub mod cli;
pub mod config;
pub mod encoding;
pub mod error;
pub mod explain;
pub mod git_diff;
pub mod mapper;
pub mod replay_export;
pub mod reporter;
pub mod rollback;
pub mod rule;

/// 逻辑外键挖掘（relations mining）。
///
/// 仅被 `sqlguard-mine` 等独立二进制引用；主 `sqlguard` 二进制不引用本模块，
/// 因此链接器（配合 `lto = "fat"`）不会将其编入主程序。
pub mod relation;
