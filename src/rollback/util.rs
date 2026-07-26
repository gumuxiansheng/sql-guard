//! 共享日期/时间工具（不引入 chrono 依赖）。
//!
//! P2-3：naming.rs 和 manifest.rs 原各自手写闰年/月长逻辑，提取到此处复用。

/// 取当前日期，格式 `YYYYMMDD`（8 位数字）。失败时回退到 `19700101`。
pub fn current_date_yyyymmdd() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let (y, m, d) = ymd_from_days_since_epoch(days);
    format!("{:04}{:02}{:02}", y, m, d)
}

/// 取当前 UTC 时间，格式 `YYYY-MM-DDTHH:MM:SSZ`。失败时回退到 `1970-01-01T00:00:00Z`。
pub fn current_iso8601_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let sec_of_day = secs % 86_400;
    let hour = sec_of_day / 3600;
    let minute = (sec_of_day % 3600) / 60;
    let second = sec_of_day % 60;
    let (y, m, d) = ymd_from_days_since_epoch(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hour, minute, second)
}

/// 将"自 1970-01-01 起的天数"转换为 `(year, month, day)`。
/// 算法：从 1970 起逐年减去当年天数（闰年 366，平年 365）。
pub fn ymd_from_days_since_epoch(mut days: i64) -> (i64, i64, i64) {
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
    (year, month, days + 1)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_day_zero_is_1970_01_01() {
        assert_eq!(ymd_from_days_since_epoch(0), (1970, 1, 1));
    }

    #[test]
    fn known_date_2026_07_26() {
        // 2026-07-26 距 1970-01-01 共 20660 天
        // (1970→2025 共 56 年：14 闰 + 42 平 = 14*366 + 42*365 = 20454 天；
        //  2026-01-01 为当年第 0 天，7 月 26 日为当年第 206 天：31+28+31+30+31+30+26 = 207，
        //  但 1 月 1 日是 day 0，故 7 月 26 日是 day 206)
        let (y, m, d) = ymd_from_days_since_epoch(20660);
        assert_eq!(y, 2026);
        assert_eq!(m, 7);
        assert_eq!(d, 26);
    }

    #[test]
    fn current_date_is_8_digits() {
        let s = current_date_yyyymmdd();
        assert_eq!(s.len(), 8);
        assert!(s.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn current_iso8601_has_correct_format() {
        let s = current_iso8601_utc();
        assert!(s.ends_with('Z'));
        assert_eq!(s.len(), 20); // YYYY-MM-DDTHH:MM:SSZ
    }
}
