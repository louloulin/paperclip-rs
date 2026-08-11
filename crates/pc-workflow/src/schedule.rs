//! 调度类型：支持 cron 表达式解析（5 字段标准 cron）+ 间隔 + 手动/事件触发。

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CronError {
    #[error("cron: empty expression")]
    Empty,
    #[error("cron: expected 5 fields, got {0}")]
    FieldCount(usize),
    #[error("cron: invalid field `{0}`: {1}")]
    Field(String, String),
    #[error("cron: range `start-end` must have start <= end (got {0}..{1})")]
    Range(u32, u32),
    #[error("cron: invalid timezone `{0}`")]
    InvalidTimeZone(String),
}

/// 调度方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleKind {
    Cron(String),
    IntervalSeconds(u64),
    Manual,
    Event { kind: String, selector: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSpec {
    pub kind: ScheduleKind,
}

impl ScheduleSpec {
    #[must_use]
    pub fn cron(expr: impl Into<String>) -> Self {
        Self {
            kind: ScheduleKind::Cron(expr.into()),
        }
    }
    #[must_use]
    pub fn interval_secs(secs: u64) -> Self {
        Self {
            kind: ScheduleKind::IntervalSeconds(secs),
        }
    }
    #[must_use]
    pub fn manual() -> Self {
        Self {
            kind: ScheduleKind::Manual,
        }
    }
    #[must_use]
    pub fn event(kind: impl Into<String>, selector: impl Into<String>) -> Self {
        Self {
            kind: ScheduleKind::Event {
                kind: kind.into(),
                selector: selector.into(),
            },
        }
    }
}

/// Cron field: supports `*`, `a`, `a-b`, `a-b/n`, `*/n`, `a,b,c`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CronField {
    bits: [bool; 64],
}

impl CronField {
    fn new() -> Self {
        Self { bits: [false; 64] }
    }
    fn set(&mut self, v: u32) {
        if (v as usize) < self.bits.len() {
            self.bits[v as usize] = true;
        }
    }
    fn matches(&self, v: u32) -> bool {
        self.bits.get(v as usize).copied().unwrap_or(false)
    }
    fn parse(s: &str, min: u32, max: u32) -> Result<Self, CronError> {
        let mut field = Self::new();
        for part in s.split(',') {
            if let Some((range, step)) = part.split_once('/') {
                let step: u32 = step
                    .parse()
                    .map_err(|_| CronError::Field(s.into(), "step not number".into()))?;
                if step == 0 {
                    return Err(CronError::Field(s.into(), "step = 0".into()));
                }
                let (lo, hi) = parse_range(range, min, max)?;
                let mut v = lo;
                while v <= hi {
                    field.set(v);
                    v += step;
                }
            } else {
                let (lo, hi) = parse_range(part, min, max)?;
                let mut v = lo;
                while v <= hi {
                    field.set(v);
                    v += 1;
                }
            }
        }
        Ok(field)
    }
}

fn parse_range(s: &str, min: u32, max: u32) -> Result<(u32, u32), CronError> {
    if s == "*" {
        return Ok((min, max));
    }
    if let Some((a, b)) = s.split_once('-') {
        let lo: u32 = a
            .parse()
            .map_err(|_| CronError::Field(s.into(), "start not number".into()))?;
        let hi: u32 = b
            .parse()
            .map_err(|_| CronError::Field(s.into(), "end not number".into()))?;
        if lo < min || hi > max || lo > hi {
            return Err(CronError::Range(lo, hi));
        }
        Ok((lo, hi))
    } else {
        let v: u32 = s
            .parse()
            .map_err(|_| CronError::Field(s.into(), "not a number or range".into()))?;
        if v < min || v > max {
            return Err(CronError::Field(s.into(), format!("out of {min}..{max}")));
        }
        Ok((v, v))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCron {
    minute: CronField,
    hour: CronField,
    dom: CronField,
    month: CronField,
    dow: CronField,
}

impl ParsedCron {
    fn parse(expr: &str) -> Result<Self, CronError> {
        let trimmed = expr.trim();
        if trimmed.is_empty() {
            return Err(CronError::Empty);
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(CronError::FieldCount(parts.len()));
        }
        let minute = CronField::parse(parts[0], 0, 59)?;
        let hour = CronField::parse(parts[1], 0, 23)?;
        let dom = CronField::parse(parts[2], 1, 31)?;
        let month = CronField::parse(parts[3], 1, 12)?;
        let dow = CronField::parse(parts[4], 0, 6)?;
        Ok(Self {
            minute,
            hour,
            dom,
            month,
            dow,
        })
    }

    fn matches(&self, t: DateTime<Utc>) -> bool {
        self.minute.matches(t.minute())
            && self.hour.matches(t.hour())
            && self.month.matches(t.month())
            && (self.dow.matches(t.weekday().num_days_from_sunday()) || self.dom.matches(t.day()))
    }

    /// 下一次匹配的时间，now 起算（不含 now）。
    fn next_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        // 5-minute resolution search window is fine for typical cron usage.
        let mut t = now + Duration::minutes(1);
        let end = now + Duration::days(366);
        while t < end {
            if self.matches(t) {
                return Some(t);
            }
            t += Duration::minutes(1);
        }
        None
    }
}

impl FromStr for ScheduleSpec {
    type Err = CronError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let inner = s.trim();
        if inner.eq_ignore_ascii_case("manual") {
            return Ok(Self::manual());
        }
        if let Some(stripped) = inner.strip_prefix("every:") {
            let secs: u64 = stripped
                .trim()
                .parse()
                .map_err(|_| CronError::Field(inner.into(), "every:<secs>".into()))?;
            return Ok(Self::interval_secs(secs));
        }
        // Validate cron
        ParsedCron::parse(inner)?;
        Ok(Self::cron(inner))
    }
}

impl ScheduleSpec {
    /// 下次触发时间（用于 polling）。
    /// - Manual/Event 返回 None。
    /// - Cron 返回 computed time。
    /// - Interval 返回 now + interval（粗略）。
    #[must_use]
    pub fn next_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match &self.kind {
            ScheduleKind::Cron(expr) => {
                ParsedCron::parse(expr).ok().and_then(|p| p.next_after(now))
            }
            ScheduleKind::IntervalSeconds(s) => {
                Some(now + Duration::seconds(i64::from(u32::try_from(*s).unwrap_or(u32::MAX))))
            }
            ScheduleKind::Manual | ScheduleKind::Event { .. } => None,
        }
    }
}

/// 计算 cron 表达式在指定时区内的下一次触发时间（返回 UTC）。
///
/// 与 Node `nextCronTickInTimeZone(expression, timeZone, after)` 1:1 对齐：
/// - 校验时区合法（`chrono-tz` 解析）
/// - 解析 cron 表达式为 `ParsedCron`
/// - 从 `after + 1 minute` 起逐步扫描（最多 ~5 年）
/// - 在目标时区下逐字段匹配；首个匹配时间返回 UTC
///
/// 高内聚：纯函数；无 IO、无外部状态。
/// 低耦合：仅依赖 `chrono` / `chrono-tz` / 现有 `ParsedCron`。
///
/// # Errors
/// - `CronError::InvalidTimeZone`：时区字符串不被 `chrono-tz` 识别
/// - `CronError::Empty` / `FieldCount` / `Field` / `Range`：cron 解析失败
pub fn next_cron_tick_in_timezone(
    expression: &str,
    time_zone: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, CronError> {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Err(CronError::Empty);
    }
    let tz: chrono_tz::Tz = time_zone
        .parse()
        .map_err(|_| CronError::InvalidTimeZone(time_zone.to_string()))?;
    let cron = ParsedCron::parse(trimmed)?;
    // 与 Node 一致：从 `after` 的下一整分钟开始搜索
    let mut cursor = after
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(after)
        + Duration::minutes(1);
    let end = after + Duration::days(366 * 5);
    while cursor < end {
        let parts = cursor.with_timezone(&tz);
        let weekday_from_sunday = parts.weekday().num_days_from_sunday();
        let matches = cron.minute.matches(parts.minute())
            && cron.hour.matches(parts.hour())
            && cron.dom.matches(parts.day())
            && cron.month.matches(parts.month())
            && cron.dow.matches(weekday_from_sunday);
        if matches {
            return Ok(Some(cursor));
        }
        cursor += Duration::minutes(1);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parse_cron_wildcards() {
        assert!(ParsedCron::parse("* * * * *").is_ok());
        assert!(ParsedCron::parse("0 0 * * *").is_ok());
        assert!(ParsedCron::parse("*/15 * * * *").is_ok());
        assert!(ParsedCron::parse("0,15,30,45 * * * *").is_ok());
    }

    #[test]
    fn parse_cron_rejects_empty() {
        assert_eq!(ParsedCron::parse(""), Err(CronError::Empty));
    }

    #[test]
    fn parse_cron_rejects_wrong_field_count() {
        assert!(matches!(
            ParsedCron::parse("* * * *"),
            Err(CronError::FieldCount(4))
        ));
    }

    #[test]
    fn parse_cron_rejects_out_of_range() {
        assert!(ParsedCron::parse("60 * * * *").is_err()); // minute out of range
        assert!(ParsedCron::parse("* 24 * * *").is_err());
    }

    #[test]
    fn schedule_spec_parsing() {
        assert_eq!(
            "manual".parse::<ScheduleSpec>().unwrap(),
            ScheduleSpec::manual()
        );
        let s = "every:30".parse::<ScheduleSpec>().unwrap();
        assert_eq!(s, ScheduleSpec::interval_secs(30));
        let s = "0 9 * * 1-5".parse::<ScheduleSpec>().unwrap();
        assert!(matches!(s.kind, ScheduleKind::Cron(_)));
    }

    #[test]
    fn cron_next_match_known_anchor() {
        let s = "0 9 * * *".parse::<ScheduleSpec>().unwrap();
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 8, 30, 0).unwrap();
        let next = s.next_after(from).unwrap();
        assert_eq!(next.hour(), 9);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn manual_and_event_have_no_next() {
        let s = ScheduleSpec::manual();
        let now = Utc::now();
        assert!(s.next_after(now).is_none());

        let s = ScheduleSpec::event("issue.created", "company=acme");
        assert!(s.next_after(now).is_none());
    }

    #[test]
    fn every_seconds_returns_now_plus_interval() {
        let s = ScheduleSpec::interval_secs(45);
        let now = Utc::now();
        let next = s.next_after(now).unwrap();
        let diff = (next - now).num_seconds();
        assert_eq!(diff, 45);
    }

    // ============ next_cron_tick_in_timezone 测试 ============

    #[test]
    fn next_cron_tick_in_timezone_utc_basic() {
        // 每天 UTC 09:00；after = 2026-01-01T08:30:00Z → 应返回 09:00
        use chrono::TimeZone;
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 8, 30, 0).unwrap();
        let next = next_cron_tick_in_timezone("0 9 * * *", "UTC", after).unwrap();
        let next = next.expect("should find a match");
        assert_eq!(next.hour(), 9);
        assert_eq!(next.minute(), 0);
        assert_eq!(next.day(), 1);
    }

    #[test]
    fn next_cron_tick_in_timezone_shanghai() {
        // 上海 +08:00；UTC 01:00 = 上海 09:00
        use chrono::TimeZone;
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 30, 0).unwrap();
        let next = next_cron_tick_in_timezone("0 9 * * *", "Asia/Shanghai", after)
            .unwrap()
            .expect("should find a match");
        // 上海 09:00 = UTC 01:00
        assert_eq!(next.hour(), 1);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn next_cron_tick_in_timezone_new_york_dst() {
        // 美东 -05:00 (EST) / -04:00 (EDT)；cron "0 9 * * *" 09:00 local
        use chrono::TimeZone;
        // 2026-03-10（EDT 阶段，DST 于 2026-03-08 生效）
        let after = Utc.with_ymd_and_hms(2026, 3, 10, 12, 0, 0).unwrap();
        let next = next_cron_tick_in_timezone("0 9 * * *", "America/New_York", after)
            .unwrap()
            .expect("should find a match");
        // 09:00 EDT = 13:00 UTC（after = 12:00，next 应是同日 13:00）
        assert_eq!(next.hour(), 13);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn next_cron_tick_in_timezone_new_york_est() {
        // 美东 EST 阶段（1 月，无 DST）；09:00 EST = 14:00 UTC
        use chrono::TimeZone;
        let after = Utc.with_ymd_and_hms(2026, 1, 15, 13, 0, 0).unwrap();
        let next = next_cron_tick_in_timezone("0 9 * * *", "America/New_York", after)
            .unwrap()
            .expect("should find a match");
        // 09:00 EST = 14:00 UTC
        assert_eq!(next.hour(), 14);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn next_cron_tick_in_timezone_weekday_match() {
        // 周一至周五 09:00 (Asia/Shanghai)
        use chrono::TimeZone;
        // 2026-01-03 是周六；after 周六 22:00 UTC = 周日 06:00 SH，下个匹配应是周一
        let after = Utc.with_ymd_and_hms(2026, 1, 3, 22, 0, 0).unwrap();
        let next = next_cron_tick_in_timezone("0 9 * * 1-5", "Asia/Shanghai", after)
            .unwrap()
            .expect("should find a match");
        // 应到 2026-01-05 周一 09:00 SH = 2026-01-05 01:00 UTC
        assert_eq!(next.day(), 5);
        assert_eq!(next.hour(), 1);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn next_cron_tick_in_timezone_invalid_timezone() {
        use chrono::TimeZone;
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let err = next_cron_tick_in_timezone("0 9 * * *", "Mars/Olympus", after).unwrap_err();
        assert!(matches!(err, CronError::InvalidTimeZone(_)));
    }

    #[test]
    fn next_cron_tick_in_timezone_invalid_cron() {
        use chrono::TimeZone;
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let err = next_cron_tick_in_timezone("not a cron", "UTC", after).unwrap_err();
        assert!(matches!(
            err,
            CronError::FieldCount(_) | CronError::Field(_, _)
        ));
    }

    #[test]
    fn next_cron_tick_in_timezone_skips_current_minute() {
        // cron 每分钟；after = 12:00:30；next 必须是 12:01:00（跳过 after 本身所在的分钟）
        use chrono::TimeZone;
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 30).unwrap();
        let next = next_cron_tick_in_timezone("* * * * *", "UTC", after)
            .unwrap()
            .expect("should find a match");
        assert_eq!(next.hour(), 12);
        assert_eq!(next.minute(), 1);
        assert_eq!(next.second(), 0);
    }
}
