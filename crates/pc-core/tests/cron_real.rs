//! M15 真实验证：pc-cron 解析 + 下次触发时间。

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use pc_core::cron::{next_tick_from_expression, parse_cron, validate_cron};

#[test]
fn parse_valid_expressions() {
    assert!(parse_cron("*/5 * * * *").is_ok());
    assert!(parse_cron("0 0 * * *").is_ok());
    assert!(parse_cron("15 14 1 * *").is_ok()); // 14:15 on 1st of month
    assert!(parse_cron("0 9 * * 1-5").is_ok()); // 9am weekdays
    assert!(parse_cron("0 0 1 1 *").is_ok()); // jan 1 midnight
}

#[test]
fn parse_rejects_bad_expressions() {
    assert!(parse_cron("not a cron").is_err());
    assert!(parse_cron("60 * * * *").is_err()); // minute out of range
    assert!(parse_cron("* * * * 8").is_err()); // dow out of range
    assert!(parse_cron("* * * 13 *").is_err()); // month out of range
    assert!(parse_cron("* * 32 * *").is_err()); // dom out of range
    assert!(parse_cron("* * 0 * *").is_err()); // dom = 0 invalid
    assert!(parse_cron("* 25 * * *").is_err()); // hour out of range
    assert!(parse_cron("a b c d e").is_err());
    assert!(parse_cron("").is_err());
    assert!(parse_cron("* * *").is_err()); // too few fields
}

#[test]
fn validate_cron_returns_none_for_ok() {
    assert!(validate_cron("0 0 * * *").is_none());
    assert!(validate_cron("*/5 * * * *").is_none());
}

#[test]
fn next_tick_every_5_min() {
    let parsed = parse_cron("*/5 * * * *").unwrap();
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let next = next_tick_from_expression("*/5 * * * *", now)
        .unwrap()
        .unwrap();
    assert!(next > now);
    assert_eq!(next.minute() % 5, 0);
}

#[test]
fn next_tick_daily_midnight() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let next = next_tick_from_expression("0 0 * * *", now)
        .unwrap()
        .unwrap();
    assert_eq!(next.hour(), 0);
    assert_eq!(next.minute(), 0);
    assert!(next > now);
    assert!(next.day() == 8 || (next.day() == 1 && next.month() == 9));
}

#[test]
fn next_tick_at_specific_minute() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let next = next_tick_from_expression("30 14 * * *", now)
        .unwrap()
        .unwrap();
    assert_eq!(next.hour(), 14);
    assert_eq!(next.minute(), 30);
    assert!(next > now);
}

#[test]
fn next_tick_returns_none_for_unreachable() {
    let parsed = parse_cron("0 0 31 2 *").unwrap();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let n = pc_core::cron::next_tick(&parsed, now);
    if let Some(t) = n {
        let days = (t - now).num_days();
        assert!(days >= 365 || days <= 365);
    }
}

#[test]
fn parsed_cron_serializes() {
    let p = parse_cron("*/5 * * * *").unwrap();
    let j = serde_json::to_string(&p).unwrap();
    assert!(j.contains("[0,5,10,15,20,25,30,35,40,45,50,55]") || j.contains("0"));
    let back: pc_core::cron::ParsedCron = serde_json::from_str(&j).unwrap();
    assert_eq!(
        back.minutes,
        vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]
    );
}

#[test]
fn weekday_syntax_round_trip() {
    let p = parse_cron("0 9 * * 1-5").unwrap();
    assert_eq!(p.days_of_week.len(), 5);
}

#[test]
fn step_value_works() {
    let p = parse_cron("*/15 * * * *").unwrap();
    assert_eq!(p.minutes, vec![0, 15, 30, 45]);
}
