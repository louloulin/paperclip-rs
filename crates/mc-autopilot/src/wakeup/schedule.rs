//! 最小 5 字段 cron —— 上游 `service/cron.go` 的 `NextOccurrenceAfterUTC` 等价实现。
//!
//! # 为什么在本片（而非常设的 `src/trigger.rs`）
//!
//! `service/cron.go` 的移植原定落在 M5-3 的 `src/trigger.rs`（C 波），但 `kind=cron` 的
//! `Validate` 分支必须能算出 `next_fire_at` 才能创建 ⇒ 本片自带最小实现，M5-3 落地后应改为
//! 复用 `trigger.rs` 并把本文件删掉。**这是本片登记的第 1 处偏离**。
//!
//! # 与上游的等价边界
//!
//! - 上游用 `cron.NewParser(cron.Minute|cron.Hour|cron.Dom|cron.Month|cron.Dow)` 解析 5 字段
//!   （分钟/小时/日/月/星期），语义是 Vixie cron：**「日」与「星期」都受限时取并集**，只一个受限时取它。
//! - 支持 `*`、`a`、`a-b`、`a-b/n`、`*/n`、逗号列表；月/星期支持 3 字母名字（大小写不敏感）。
//! - 星期允许 `0..=7`（`0` 与 `7` 都是周日；robfig 只收 `0..=6`，这里更宽松一点，登记为差异）。
//! - 时区用 `chrono-tz`（与 `Cargo.toml` 的既有依赖一致，**不引第三方 cron 库**）。
//! - **DST**：枚举本地时间后过滤「不存在的本地时刻」（春季跳表那一小时）——跳过而不是平移；
//!   robfig 的行为是继续向前找下一个可用时刻，两者在上述边界上一致（都是「当天没有就下一天」）。
//!   `LocalResult::Ambiguous`（秋季重复）取**较早**的那个（`earliest()`）。
//! - 5 年内找不到（如 `0 0 30 2 *`）⇒ `Ok(None)`；调用方（`Validate`）把它变成
//!   `invalid wakeup: cron must have a future occurrence`。

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use super::WakeupError;

/// 上游 `NextOccurrenceAfterUTC` 的搜索上限：5 年（≈ `366*5` 天）。
const SEARCH_DAYS: i64 = 366 * 5;

/// 一个 cron 字段（位图 + 是否受限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    bits: u64,
    restricted: bool,
}

impl Field {
    fn has(&self, value: u32) -> bool {
        self.bits & (1u64 << value) != 0
    }
}

/// 已解析的 5 字段表达式。
#[derive(Debug, Clone, Copy)]
pub struct CronSchedule {
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

impl CronSchedule {
    /// 解析（**不做**任何默认值填充：5 字段必须齐）。
    pub fn parse(expr: &str) -> Result<Self, WakeupError> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(WakeupError::input("cron must have 5 fields"));
        }
        let minute = parse_field(parts[0], &Bounds::minute())?;
        let hour = parse_field(parts[1], &Bounds::hour())?;
        let dom = parse_field(parts[2], &Bounds::dom())?;
        let month = parse_field(parts[3], &Bounds::month())?;
        let dow = parse_field(parts[4], &Bounds::dow())?;
        Ok(Self {
            minute,
            hour,
            dom,
            month,
            dow,
        })
    }

    /// 该日是否匹配（含 Vixie 的「日 / 星期」并集规则）。
    fn day_matches(&self, date: NaiveDate) -> bool {
        if !self.month.has(date.month()) {
            return false;
        }
        let month_day_match = self.dom.has(date.day());
        let week_day_match = self.dow.has(weekday_value(date));
        match (self.dom.restricted, self.dow.restricted) {
            (true, true) => month_day_match || week_day_match,
            (true, false) => month_day_match,
            (false, true) => week_day_match,
            (false, false) => true,
        }
    }

    /// 一天内的第 `minute_of_day` 分钟是否匹配。
    fn minute_matches(&self, minute_of_day: u32) -> bool {
        self.hour.has(minute_of_day / 60) && self.minute.has(minute_of_day % 60)
    }

    /// 严格晚于 `after` 的下一次触发（本地时区 `tz`），5 年内没有则 `None`。
    pub fn next_after(&self, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local_after = after.with_timezone(&tz);
        let mut date = local_after.date_naive();
        // 首日的起始分钟 = 当前分钟 + 1（`Next` 是严格晚于），越界即当天无候选。
        let mut first_minute = local_after.hour() * 60 + local_after.minute() + 1;
        for _ in 0..SEARCH_DAYS {
            if self.day_matches(date) {
                for minute_of_day in first_minute..1440 {
                    if !self.minute_matches(minute_of_day) {
                        continue;
                    }
                    let naive = NaiveDateTime::new(
                        date,
                        chrono::NaiveTime::from_hms_opt(minute_of_day / 60, minute_of_day % 60, 0)
                            // 1440 已被 range 排除，60/60 不会越界。
                            .expect("minute_of_day < 1440"),
                    );
                    // `None` = 本地时刻不存在（春季跳表），跳过；`Ambiguous` 取较早。
                    if let Some(local) = tz.from_local_datetime(&naive).earliest() {
                        let candidate = local.with_timezone(&Utc);
                        if candidate > after {
                            return Some(candidate);
                        }
                    }
                }
            }
            date = date.succ_opt()?;
            first_minute = 0;
        }
        None
    }
}

/// 上游 `NextOccurrenceAfterUTC(expr, tz, after)`：**严格晚于** `after` 的下一次触发。
///
/// - 时区解析失败 ⇒ `invalid wakeup: invalid timezone`；
/// - 表达式非法 ⇒ `invalid wakeup: invalid cron expression`；
/// - 5 年内无触发 ⇒ `Ok(None)`（调用方映射成 `cron must have a future occurrence`）。
pub fn next_occurrence_after_utc(
    expr: &str,
    tz_name: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, WakeupError> {
    let tz: Tz = tz_name
        .parse()
        .map_err(|_| WakeupError::input("invalid timezone"))?;
    let schedule =
        CronSchedule::parse(expr).map_err(|_| WakeupError::input("invalid cron expression"))?;
    Ok(schedule.next_after(tz, after))
}

/// 星期：cron 的 `0/7 = 周日`，`NaiveDate::weekday().num_days_from_sunday()` 已经是 0=周日。
fn weekday_value(date: NaiveDate) -> u32 {
    date.weekday().num_days_from_sunday()
}

/// 字段的取值范围与名字表。
struct Bounds {
    min: u32,
    max: u32,
    names: &'static [(&'static str, u32)],
}

impl Bounds {
    const fn minute() -> Self {
        Self {
            min: 0,
            max: 59,
            names: &[],
        }
    }
    const fn hour() -> Self {
        Self {
            min: 0,
            max: 23,
            names: &[],
        }
    }
    const fn dom() -> Self {
        Self {
            min: 1,
            max: 31,
            names: &[],
        }
    }
    const fn month() -> Self {
        Self {
            min: 1,
            max: 12,
            names: &[
                ("jan", 1),
                ("feb", 2),
                ("mar", 3),
                ("apr", 4),
                ("may", 5),
                ("jun", 6),
                ("jul", 7),
                ("aug", 8),
                ("sep", 9),
                ("oct", 10),
                ("nov", 11),
                ("dec", 12),
            ],
        }
    }
    const fn dow() -> Self {
        Self {
            min: 0,
            max: 7,
            names: &[
                ("sun", 0),
                ("mon", 1),
                ("tue", 2),
                ("wed", 3),
                ("thu", 4),
                ("fri", 5),
                ("sat", 6),
            ],
        }
    }

    fn value(&self, token: &str) -> Option<u32> {
        let lowered = token.to_ascii_lowercase();
        if let Some((_, value)) = self.names.iter().find(|(name, _)| *name == lowered) {
            return Some(*value);
        }
        let parsed: u32 = lowered.parse().ok()?;
        if parsed < self.min || parsed > self.max {
            return None;
        }
        // 星期 `7` 归一化到 `0`（周日）。
        if self.max == 7 && parsed == 7 {
            return Some(0);
        }
        Some(parsed)
    }
}

/// 解析单个字段：`*` / 列表 / 单值 / 区间 / 步长。
fn parse_field(token: &str, bounds: &Bounds) -> Result<Field, WakeupError> {
    let mut bits = 0u64;
    let mut restricted = false;
    for item in token.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(WakeupError::input("invalid cron expression"));
        }
        let (range_part, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| WakeupError::input("invalid cron expression"))?;
                if step == 0 || step > 59 {
                    return Err(WakeupError::input("invalid cron expression"));
                }
                (range, Some(step))
            }
            None => (item, None),
        };
        // 全范围 + 无步长 ⇒ 纯 `*`，不算受限（Vixie 的 dom/dow 并集规则要看这个）。
        if range_part == "*" && step.is_none() {
            bits |= mask(bounds.min, bounds.max, 1);
            continue;
        }
        restricted = true;
        let (start, end) = if range_part == "*" {
            (bounds.min, bounds.max)
        } else if let Some((low, high)) = range_part.split_once('-') {
            let low = bounds
                .value(low)
                .ok_or_else(|| WakeupError::input("invalid cron expression"))?;
            let high = bounds
                .value(high)
                .ok_or_else(|| WakeupError::input("invalid cron expression"))?;
            (low, high)
        } else {
            let single = bounds
                .value(range_part)
                .ok_or_else(|| WakeupError::input("invalid cron expression"))?;
            // 单值配步长（`5/10`）在 robfig 里等同 `5-max/10`；**不带**步长就是那一个值。
            match step {
                Some(_) => (single, bounds.max),
                None => (single, single),
            }
        };
        if start > end {
            return Err(WakeupError::input("invalid cron expression"));
        }
        bits |= mask(start, end, step.unwrap_or(1));
    }
    if bits == 0 {
        return Err(WakeupError::input("invalid cron expression"));
    }
    Ok(Field { bits, restricted })
}

fn mask(start: u32, end: u32, step: u32) -> u64 {
    let mut bits = 0u64;
    let mut value = start;
    while value <= end {
        bits |= 1u64 << value;
        value += step;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    fn next(expr: &str, tz: &str, from: &str) -> Option<DateTime<Utc>> {
        next_occurrence_after_utc(expr, tz, utc(from)).expect("parses")
    }

    #[test]
    fn step_expression_advances_within_the_hour() {
        assert_eq!(
            next("*/15 * * * *", "UTC", "2026-01-01T00:07:00Z").unwrap(),
            utc("2026-01-01T00:15:00Z")
        );
    }

    #[test]
    fn current_minute_is_excluded() {
        // `Next` 是严格晚于：00:15 出发要去 00:30。
        assert_eq!(
            next("*/15 * * * *", "UTC", "2026-01-01T00:15:00Z").unwrap(),
            utc("2026-01-01T00:30:00Z")
        );
    }

    #[test]
    fn weekday_names_and_ranges_work() {
        // 2026-01-03 是周六 ⇒ 下一个周一 09:00。
        assert_eq!(
            next("0 9 * * mon-fri", "UTC", "2026-01-03T00:00:00Z").unwrap(),
            utc("2026-01-05T09:00:00Z")
        );
    }

    #[test]
    fn day_of_month_and_weekday_are_unioned_when_both_restricted() {
        // 2026-01-08 是周四：下一个「13 号或周五」是 1/9（周五），不是 1/13。
        assert_eq!(
            next("0 0 13 * fri", "UTC", "2026-01-08T00:00:00Z").unwrap(),
            utc("2026-01-09T00:00:00Z")
        );
    }

    #[test]
    fn restricted_dow_with_star_dom_only_uses_weekday() {
        assert_eq!(
            next("30 6 * * sun", "UTC", "2026-01-01T00:00:00Z").unwrap(),
            utc("2026-01-04T06:30:00Z")
        );
    }

    #[test]
    fn month_names_and_single_day_of_month_work() {
        assert_eq!(
            next("0 6 1 feb *", "UTC", "2026-01-15T00:00:00Z").unwrap(),
            utc("2026-02-01T06:00:00Z")
        );
    }

    #[test]
    fn timezone_is_applied() {
        // 纽约 1 月是 EST（UTC-5）⇒ 本地 09:00 = 14:00Z。
        assert_eq!(
            next("0 9 * * *", "America/New_York", "2026-01-01T00:00:00Z").unwrap(),
            utc("2026-01-01T14:00:00Z")
        );
    }

    #[test]
    fn dst_gap_is_skipped_to_the_next_day() {
        // 2026-03-08 是美国夏令时起跳日：本地 02:00 不存在 ⇒ 顺延到 3/9 02:00 EDT（06:00Z）。
        assert_eq!(
            next("0 2 * * *", "America/New_York", "2026-03-08T05:00:00Z").unwrap(),
            utc("2026-03-09T06:00:00Z")
        );
    }

    #[test]
    fn impossible_calendar_date_has_no_occurrence() {
        assert_eq!(next("0 0 30 2 *", "UTC", "2026-01-01T00:00:00Z"), None);
    }

    #[test]
    fn field_count_and_ranges_are_validated() {
        assert!(CronSchedule::parse("* * * *").is_err());
        assert!(CronSchedule::parse("60 * * * *").is_err());
        assert!(CronSchedule::parse("* * * * 9").is_err());
        assert!(CronSchedule::parse("*/0 * * * *").is_err());
        assert!(CronSchedule::parse("5-1 * * * *").is_err());
        assert!(CronSchedule::parse("1,,2 * * * *").is_err());
        assert!(
            next_occurrence_after_utc("* * * * *", "Not/AZone", utc("2026-01-01T00:00:00Z"))
                .is_err()
        );
    }

    #[test]
    fn sunday_accepts_both_zero_and_seven() {
        assert_eq!(
            next("0 0 * * 7", "UTC", "2026-01-01T00:00:00Z").unwrap(),
            utc("2026-01-04T00:00:00Z")
        );
        assert_eq!(
            next("0 0 * * 0", "UTC", "2026-01-01T00:00:00Z").unwrap(),
            utc("2026-01-04T00:00:00Z")
        );
    }
}
