//! 备份表命名：`bks_<table>_<YYYYMMDD>_<NNNN>`。
//!
//! 对应设计文档 §4.6 / F9。每个 rollback 生成周期维护独立的 NamingAllocator，
//! 同一表多次出现时 NNNN 递增（1-indexed），跨表共用一个计数器以保持全局唯一。

use std::collections::HashSet;
use crate::config::RollbackConfig;
use super::strip_ident_quotes;

/// 备份表命名分配器。
///
/// 单次 `gen-rollback` 运行创建一个实例，按需 `alloc(table)` 分配 `bks_<table>_<date>_<seq>`，
/// 内部用 `HashSet` 防重，保证同一运行内所有 bks_ 表名唯一。
pub struct NamingAllocator<'a> {
    prefix: &'a str,
    with_date: bool,
    date_str: String,
    used: HashSet<String>,
    seq: u64,
}

impl<'a> NamingAllocator<'a> {
    pub fn new(rc: &'a RollbackConfig) -> Self {
        let date_str = current_date_yyyymmdd();
        NamingAllocator {
            prefix: rc.backup_table_prefix.as_str(),
            with_date: rc.backup_table_with_date,
            date_str,
            used: HashSet::new(),
            seq: 0,
        }
    }

    /// 为指定表分配一个唯一的 bks_ 备份表名。
    /// `table` 可带反引号/双引号，内部会剥离。
    pub fn alloc(&mut self, table: &str) -> String {
        let clean = strip_ident_quotes(table);
        let safe = sanitize_table_name(&clean);
        loop {
            self.seq += 1;
            let candidate = if self.with_date {
                format!("{}{}_{}_{:04}", self.prefix, safe, self.date_str, self.seq)
            } else {
                format!("{}{}_{:04}", self.prefix, safe, self.seq)
            };
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    /// 分配一个影子表名（用于 F12 原子 RENAME 切换）：`_rb_<seq>_<table>`。
    /// 与 bks_ 表共用同一序号空间（调用方在 alloc bks_ 之后调用 alloc_shadow 取对齐的 seq）。
    pub fn alloc_shadow(&mut self, seq: u64, table: &str) -> String {
        let clean = strip_ident_quotes(table);
        let safe = sanitize_table_name(&clean);
        format!("_rb_{:04}_{}", seq, safe)
    }

    /// 当前已分配的最大序号。
    pub fn current_seq(&self) -> u64 {
        self.seq
    }
}

/// 将表名中非 `[a-zA-Z0-9_]` 字符替换为 `_`，保证生成的 bks_ 表名合法。
fn sanitize_table_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// 取当前日期，格式 `YYYYMMDD`（8 位数字）。失败时回退到 `19700101`。
fn current_date_yyyymmdd() -> String {
    // 不引入 chrono 依赖，用 std::time + 简单天数计算。
    // Unix epoch 1970-01-01；以日为单位累加，反向计算年月日。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    yyyymmdd_from_days_since_epoch(days as i64)
}

/// 将"自 1970-01-01 起的天数"转换为 `YYYYMMDD`。
/// 算法：从 1970 起逐年减去当年天数（闰年 366，平年 365）。
fn yyyymmdd_from_days_since_epoch(mut days: i64) -> String {
    let mut year = 1970i64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let month_lengths: [i64; 12] = [31, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1i64;
    for &ml in &month_lengths {
        if days < ml {
            break;
        }
        days -= ml;
        month += 1;
    }
    let day = days + 1;
    format!("{:04}{:02}{:02}", year, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc() -> RollbackConfig {
        RollbackConfig::default()
    }

    #[test]
    fn alloc_produces_bks_prefix_with_date_and_seq() {
        let rc = rc();
        let mut n = NamingAllocator::new(&rc);
        let name = n.alloc("users");
        assert!(name.starts_with("bks_users_"));
        assert!(name.ends_with("_0001"));
        assert_eq!(name.len(), "bks_users_YYYYMMDD_0001".len());
    }

    #[test]
    fn alloc_strips_backticks() {
        let rc = rc();
        let mut n = NamingAllocator::new(&rc);
        let name = n.alloc("`users`");
        assert!(name.starts_with("bks_users_"));
    }

    #[test]
    fn alloc_increments_seq_per_call() {
        let rc = rc();
        let mut n = NamingAllocator::new(&rc);
        let a = n.alloc("users");
        let b = n.alloc("users");
        let c = n.alloc("orders");
        assert!(a.ends_with("_0001"));
        assert!(b.ends_with("_0002"));
        assert!(c.ends_with("_0003"));
    }

    #[test]
    fn alloc_sanitizes_special_chars() {
        let rc = rc();
        let mut n = NamingAllocator::new(&rc);
        let name = n.alloc("schema.table-name");
        assert!(name.starts_with("bks_schema_table_name_"));
    }

    #[test]
    fn alloc_shadow_uses_seq_prefix() {
        let rc = rc();
        let mut n = NamingAllocator::new(&rc);
        let _bks = n.alloc("users");
        let shadow = n.alloc_shadow(1, "users");
        assert_eq!(shadow, "_rb_0001_users");
    }

    #[test]
    fn alloc_without_date_omits_date_segment() {
        let mut rc = rc();
        rc.backup_table_with_date = false;
        let mut n = NamingAllocator::new(&rc);
        let name = n.alloc("users");
        assert_eq!(name, "bks_users_0001");
    }

    #[test]
    fn yyyymmdd_known_epoch() {
        assert_eq!(yyyymmdd_from_days_since_epoch(0), "19700101");
        // 2026-07-26 是 epoch + 20681 天（粗略验证）
        let s = yyyymmdd_from_days_since_epoch(20681);
        assert_eq!(s.len(), 8);
        assert!(s.starts_with("202"));
    }

    #[test]
    fn is_leap_handles_century_rules() {
        assert!(is_leap(2000));
        assert!(!is_leap(1900));
        assert!(is_leap(2024));
        assert!(!is_leap(2023));
    }
}
