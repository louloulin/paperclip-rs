//! cron 下次触发时间计算
//!
//! 算法：
//! - 从 `after` 的下一分钟开始，按"月份 → 日 → 小时 → 分钟"逐级跳跃
//! - 搜索窗口限制为约 4 年（366 × 24 × 60 × 4 ≈ 2.1M 步），防止不可能调度上死循环
//! - 时间统一用 UTC 计算（Node 用 UTC，Rust 也用 UTC）

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};

use super::ParsedCron;

/// 搜索窗口上限（年），防止不可能调度死循环。
const MAX_CRON_SEARCH_YEARS: i64 = 4;
/// 一年分钟数（含闰年 366 日）。
const MINUTES_PER_YEAR: i64 = 366 * 24 * 60;

/// 计算给定 cron 调度的下次触发时间。
///
/// 返回 `None` 表示搜索窗口内未找到匹配（不可能的调度）。
pub fn next_tick(cron: &ParsedCron, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    // 起点：after 的下一整分钟（秒和纳秒清零）
    let mut d = floor_to_next_minute(after);

    let max_iterations = MAX_CRON_SEARCH_YEARS * MINUTES_PER_YEAR;

    for _ in 0..max_iterations {
        let month = d.month();
        let day_of_month = d.day();
        let day_of_week = d.weekday().num_days_from_sunday();
        let hour = d.hour();
        let minute = d.minute();

        // 1. month
        if !cron.months.contains(&month) {
            advance_to_next_month(&mut d, &cron.months);
            continue;
        }

        // 2. day of month AND day of week (both must match)
        if !cron.days_of_month.contains(&day_of_month)
            || !cron.days_of_week.contains(&day_of_week)
        {
            // 后退到当日 00:00:00，再 +1 天
            d = floor_to_midnight(d) + chrono::Duration::days(1);
            continue;
        }

        // 3. hour
        if !cron.hours.contains(&hour) {
            if let Some(next_hour) = find_next(&cron.hours, hour) {
                d = set_hour_minute_second(d, next_hour, 0, 0, 0);
            } else {
                // 当日没有匹配小时了，跳到次日
                d = floor_to_midnight(d) + chrono::Duration::days(1);
            }
            continue;
        }

        // 4. minute
        if !cron.minutes.contains(&minute) {
            if let Some(next_min) = find_next(&cron.minutes, minute) {
                d = set_hour_minute_second(d, hour, next_min, 0, 0);
            } else {
                // 当小时没有匹配分钟了，跳到下一小时
                d = set_hour_minute_second(d, hour + 1, 0, 0, 0);
            }
            continue;
        }

        // 所有字段都匹配
        return Some(d);
    }

    None
}

// ============================================================================
// Helpers
// ============================================================================

/// 在有序数组中找到第一个严格大于 `current` 的值。
pub fn find_next(sorted_values: &[u32], current: u32) -> Option<u32> {
    for &v in sorted_values {
        if v > current {
            return Some(v);
        }
    }
    None
}

/// 把 `d` 原地推进到 `months` 中下一个匹配月份的第一天 00:00:00 UTC。
///
/// 最多走 48 个月（4 年）。
pub fn advance_to_next_month(d: &mut DateTime<Utc>, months: &[u32]) {
    let mut year = d.year();
    let mut month = d.month();

    for _ in 0..48 {
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
        if months.contains(&month) {
            // 设置到该月第一天 00:00:00
            *d = Utc
                .with_ymd_and_hms(year, month, 1, 0, 0, 0)
                .single()
                .unwrap_or(*d);
            return;
        }
    }
    // 兜底：理论上 48 步内必找到
}

fn floor_to_next_minute(d: DateTime<Utc>) -> DateTime<Utc> {
    // 用整 60 秒推进，依赖 chrono 正确处理日/月/年溢出
    let base = Utc
        .with_ymd_and_hms(d.year(), d.month(), d.day(), d.hour(), d.minute(), 0)
        .single()
        .unwrap_or(d);
    base + chrono::Duration::minutes(1)
}

fn floor_to_midnight(d: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(d.year(), d.month(), d.day(), 0, 0, 0)
        .single()
        .unwrap_or(d)
}

fn set_hour_minute_second(
    d: DateTime<Utc>,
    hour: u32,
    minute: u32,
    second: u32,
    _padding: u32,
) -> DateTime<Utc> {
    // 处理小时溢出（>23 表示次日 0 点）
    let (year, month, day) = (d.year(), d.month(), d.day());
    if hour >= 24 {
        // 推进一天
        let next = Utc
            .with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .unwrap_or(d)
            + chrono::Duration::days(1);
        return Utc
            .with_ymd_and_hms(next.year(), next.month(), next.day(), hour - 24, minute, second)
            .single()
            .unwrap_or(next);
    }
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .unwrap_or(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn find_next_returns_next_greater() {
        assert_eq!(find_next(&[1, 3, 5, 7], 0), Some(1));
        assert_eq!(find_next(&[1, 3, 5, 7], 3), Some(5));
        assert_eq!(find_next(&[1, 3, 5, 7], 7), None);
        assert_eq!(find_next(&[], 5), None);
    }

    #[test]
    fn next_tick_every_minute() {
        let cron = super::super::parse_cron("* * * * *").unwrap();
        let after = at(2025, 1, 1, 12, 30);
        let next = next_tick(&cron, after).unwrap();
        assert_eq!(next, at(2025, 1, 1, 12, 31));
    }

    #[test]
    fn next_tick_every_hour() {
        let cron = super::super::parse_cron("0 * * * *").unwrap();
        let after = at(2025, 1, 1, 12, 30);
        let next = next_tick(&cron, after).unwrap();
        assert_eq!(next, at(2025, 1, 1, 13, 0));
    }

    #[test]
    fn next_tick_every_day_midnight() {
        let cron = super::super::parse_cron("0 0 * * *").unwrap();
        let after = at(2025, 1, 1, 12, 30);
        let next = next_tick(&cron, after).unwrap();
        assert_eq!(next, at(2025, 1, 2, 0, 0));
    }

    #[test]
    fn next_tick_specific_minutes() {
        let cron = super::super::parse_cron("*/15 * * * *").unwrap();
        let after = at(2025, 1, 1, 12, 7);
        let next = next_tick(&cron, after).unwrap();
        assert_eq!(next, at(2025, 1, 1, 12, 15));
    }

    #[test]
    fn next_tick_skip_to_next_month() {
        // Only November: 0 0 1 11 *
        let cron = super::super::parse_cron("0 0 1 11 *").unwrap();
        let after = at(2025, 1, 15, 12, 30);
        let next = next_tick(&cron, after).unwrap();
        assert_eq!(next, at(2025, 11, 1, 0, 0));
    }

    #[test]
    fn next_tick_returns_none_for_impossible() {
        // Feb 30 doesn't exist
        let cron = super::super::parse_cron("0 0 30 2 *").unwrap();
        let after = at(2025, 1, 1, 0, 0);
        assert!(next_tick(&cron, after).is_none());
    }

    #[test]
    fn next_tick_from_expression_convenience() {
        let after = at(2025, 6, 15, 10, 30);
        let next = super::super::next_tick_from_expression("0 0 * * *", after).unwrap();
        assert_eq!(next, Some(at(2025, 6, 16, 0, 0)));
    }

    #[test]
    fn next_tick_from_expression_invalid() {
        let after = at(2025, 6, 15, 10, 30);
        let err = super::super::next_tick_from_expression("bad", after).unwrap_err();
        assert!(matches!(err, super::super::CronError::WrongFieldCount { .. }));
    }

    #[test]
    fn advance_to_next_month_finds_target() {
        let mut d = at(2025, 1, 15, 12, 30);
        let months = vec![3u32, 6, 9, 12];
        advance_to_next_month(&mut d, &months);
        assert_eq!(d, at(2025, 3, 1, 0, 0));
    }

    #[test]
    fn advance_to_next_month_wraps_year() {
        let mut d = at(2025, 11, 15, 12, 30);
        let months = vec![2u32];
        advance_to_next_month(&mut d, &months);
        assert_eq!(d, at(2026, 2, 1, 0, 0));
    }

    #[test]
    fn next_tick_day_of_week_filter() {
        // Only Sundays (day_of_week=0): 0 0 * * 0
        let cron = super::super::parse_cron("0 0 * * 0").unwrap();
        // 2025-01-04 is a Saturday, 2025-01-05 is Sunday
        let after = at(2025, 1, 4, 0, 0);
        let next = next_tick(&cron, after).unwrap();
        assert_eq!(next, at(2025, 1, 5, 0, 0));
    }
}
