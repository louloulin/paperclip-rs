//! Schedule —— active hours 与 next-eval 计算。
//!
//! 与 Node `isWithinStatusCardActiveHours` / `nextStatusCardEvaluationAt` 1:1 对齐。

use chrono::{DateTime, Timelike, Utc};

use crate::types::{EngineError, EngineResult, StatusCardRefreshPolicy};
use crate::types::{
    DEFAULT_DAILY_TOKEN_CAP, DEFAULT_INTERVAL_MINUTES, DEFAULT_MAX_UPDATES_PER_HOUR,
    DEFAULT_REACTIVE_DEBOUNCE_SECONDS, REACTIVE_DEBOUNCE_MAX_SECONDS,
};

/// 判断给定时刻是否在 policy 的 activeHours 窗口内（与 Node `isWithinStatusCardActiveHours` 1:1 对齐）。
///
/// ## 行为
///
/// - `policy.active_hours == None` → 始终返回 `true`。
/// - 解析 `start` / `end` 为分钟数（"HH:MM"），与当前时区时间比较。
/// - `start <= end`：窗口 [start, end)（闭开区间）。
/// - `start > end`：窗口跨午夜 [start, 24:00) ∪ [00:00, end)。
pub fn is_within_status_card_active_hours(
    policy: &StatusCardRefreshPolicy,
    now: DateTime<Utc>,
) -> bool {
    let Some(active) = &policy.active_hours else {
        return true;
    };

    // 把 now 转到目标时区，提取 hour + minute
    let tz: chrono_tz::Tz = match active.timezone.parse() {
        Ok(tz) => tz,
        Err(_) => return true, // invalid timezone → 视作 always active
    };
    let local = now.with_timezone(&tz);
    let hour = local.hour() as i32;
    let minute = local.minute() as i32;
    let current = hour * 60 + minute;

    let (start, end) = match (parse_hhmm(&active.start), parse_hhmm(&active.end)) {
        (Some(s), Some(e)) => (s, e),
        _ => return true,
    };

    if start <= end {
        current >= start && current < end
    } else {
        current >= start || current < end
    }
}

/// 计算下一次 evaluation 时间（与 Node `nextStatusCardEvaluationAt` 1:1 对齐）。
///
/// - `mode == Manual` → `None`。
/// - `mode == Interval` → `now + interval_minutes * 60s`。
/// - `mode == Reactive` → `now + min(debounce_seconds, 60)s`。
pub fn next_status_card_evaluation_at(
    policy: &StatusCardRefreshPolicy,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    use crate::types::RefreshMode;
    match policy.mode {
        RefreshMode::Manual => None,
        RefreshMode::Interval => {
            let minutes = policy.interval_minutes.unwrap_or(DEFAULT_INTERVAL_MINUTES);
            Some(now + chrono::Duration::seconds((minutes as i64) * 60))
        }
        RefreshMode::Reactive => {
            let seconds = policy
                .debounce_seconds
                .unwrap_or(DEFAULT_REACTIVE_DEBOUNCE_SECONDS)
                .min(REACTIVE_DEBOUNCE_MAX_SECONDS);
            Some(now + chrono::Duration::seconds(seconds as i64))
        }
    }
}

fn parse_hhmm(s: &str) -> Option<i32> {
    let mut parts = s.split(':');
    let h: i32 = parts.next()?.parse().ok()?;
    let m: i32 = parts.next()?.parse().ok()?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

// 引用以避免 unused 警告
#[allow(dead_code)]
fn _engine_err() -> EngineResult<()> {
    Err(EngineError::InvalidTimeFormat("".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ActiveHours, RefreshMode};

    fn policy_with_hours(start: &str, end: &str, timezone: &str) -> StatusCardRefreshPolicy {
        StatusCardRefreshPolicy {
            mode: RefreshMode::Interval,
            interval_minutes: Some(15),
            debounce_seconds: None,
            max_updates_per_hour: None,
            triggers: Default::default(),
            active_hours: Some(ActiveHours {
                start: start.to_string(),
                end: end.to_string(),
                timezone: timezone.to_string(),
            }),
            daily_token_cap: None,
        }
    }

    #[test]
    fn r676_active_hours_returns_true_when_no_active_hours() {
        let p = StatusCardRefreshPolicy::default_manual();
        let now = Utc::now();
        assert!(is_within_status_card_active_hours(&p, now));
    }

    #[test]
    fn r676_active_hours_within_window() {
        let p = policy_with_hours("09:00", "17:00", "UTC");
        let at_16_59 = parse_utc("2026-07-23T16:59:00Z");
        assert!(is_within_status_card_active_hours(&p, at_16_59));
    }

    #[test]
    fn r676_active_hours_at_boundary_end_excluded() {
        let p = policy_with_hours("09:00", "17:00", "UTC");
        let at_17_00 = parse_utc("2026-07-23T17:00:00Z");
        // end 是 exclusive
        assert!(!is_within_status_card_active_hours(&p, at_17_00));
    }

    #[test]
    fn r676_active_hours_at_boundary_start_included() {
        let p = policy_with_hours("09:00", "17:00", "UTC");
        let at_09_00 = parse_utc("2026-07-23T09:00:00Z");
        assert!(is_within_status_card_active_hours(&p, at_09_00));
    }

    #[test]
    fn r676_active_hours_invalid_timezone_returns_true() {
        let p = policy_with_hours("09:00", "17:00", "Invalid/Timezone");
        let now = parse_utc("2026-07-23T12:00:00Z");
        assert!(is_within_status_card_active_hours(&p, now));
    }

    fn parse_utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn r676_next_eval_manual_returns_none() {
        let p = StatusCardRefreshPolicy::default_manual();
        assert_eq!(next_status_card_evaluation_at(&p, Utc::now()), None);
    }

    #[test]
    fn r676_next_eval_interval_15_minutes() {
        let p = StatusCardRefreshPolicy {
            mode: RefreshMode::Interval,
            interval_minutes: Some(15),
            ..StatusCardRefreshPolicy::default_manual()
        };
        let now = parse_utc("2026-07-23T14:00:00Z");
        let next = next_status_card_evaluation_at(&p, now).unwrap();
        assert_eq!(next, parse_utc("2026-07-23T14:15:00Z"));
    }

    #[test]
    fn r676_next_eval_reactive_debounce() {
        let p = StatusCardRefreshPolicy {
            mode: RefreshMode::Reactive,
            debounce_seconds: Some(60),
            ..StatusCardRefreshPolicy::default_manual()
        };
        let now = parse_utc("2026-07-23T14:00:00Z");
        let next = next_status_card_evaluation_at(&p, now).unwrap();
        assert_eq!(next, parse_utc("2026-07-23T14:01:00Z"));
    }

    #[test]
    fn r676_next_eval_reactive_clamps_debounce_to_60s() {
        let p = StatusCardRefreshPolicy {
            mode: RefreshMode::Reactive,
            debounce_seconds: Some(300), // 5 分钟
            ..StatusCardRefreshPolicy::default_manual()
        };
        let now = parse_utc("2026-07-23T14:00:00Z");
        let next = next_status_card_evaluation_at(&p, now).unwrap();
        // 应被 clamp 到 60s
        assert_eq!(next, parse_utc("2026-07-23T14:01:00Z"));
    }
}
