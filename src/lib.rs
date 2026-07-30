//! SqlGuard 库入口。
//!
//! 同时提供二进制（`src/main.rs`）和库（本文件）两个 target，
//! 使 `benches/` 和 `tests/` 中的外部 crate 能通过 `sqlguard::` 路径引用内部模块。

pub mod cache;
pub mod checker;
pub mod cli;
pub mod config;
pub mod error;
pub mod git_diff;
pub mod mapper;
pub mod replay_export;
pub mod reporter;
pub mod rollback;
pub mod rule;
