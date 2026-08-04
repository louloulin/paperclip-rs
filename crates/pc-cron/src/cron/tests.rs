//! `cron` 模块私有集成单测：覆盖跨子模块的行为组合

use chrono::{TimeZone, Utc};

use super::{next_tick, next_tick_from_expression, parse_cron, validate_cron};

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
}

#[test]
fn end_to_end_every_5_minutes() {
    let cron = parse_cron("*/5 * * * *").unwrap();
    let after = at(2025, 3, 15, 10, 7);
    assert_eq!(next_tick(&cron, after), Some(at(2025, 3, 15, 10, 10)));
}

#[test]
fn end_to_end_weekday_morning() {
    // 0 9 * * 1-5 (9 AM Mon-Fri)
    let cron = parse_cron("0 9 * * 1-5").unwrap();
    // 2025-01-04 is Saturday, 2025-01-06 is Monday
    let after = at(2025, 1, 4, 10, 0);
    let next = next_tick(&cron, after).unwrap();
    assert_eq!(next, at(2025, 1, 6, 9, 0));
}

#[test]
fn end_to_end_quarterly() {
    // 0 0 1 1,4,7,10 * (midnight on 1st of Jan/Apr/Jul/Oct)
    let cron = parse_cron("0 0 1 1,4,7,10 *").unwrap();
    let after = at(2025, 2, 1, 0, 0);
    let next = next_tick(&cron, after).unwrap();
    assert_eq!(next, at(2025, 4, 1, 0, 0));
}

#[test]
fn end_to_end_yearly_jan_first() {
    let cron = parse_cron("0 0 1 1 *").unwrap();
    let after = at(2025, 6, 15, 12, 0);
    let next = next_tick(&cron, after).unwrap();
    assert_eq!(next, at(2026, 1, 1, 0, 0));
}

#[test]
fn end_to_end_complex_expression() {
    // 0,30 9-17 * * 1-5 (every 30 min during business hours, weekdays)
    let cron = parse_cron("0,30 9-17 * * 1-5").unwrap();
    let after = at(2025, 1, 4, 8, 0); // Saturday 8:00
    let next = next_tick(&cron, after).unwrap();
    // Next Monday 9:00
    assert_eq!(next, at(2025, 1, 6, 9, 0));
}

#[test]
fn validate_simple_expressions() {
    assert!(validate_cron("* * * * *").is_none());
    assert!(validate_cron("0 0 * * *").is_none());
    assert!(validate_cron("0 12 * * MON").is_some()); // MON not numeric (only 0-6 supported)
    assert!(validate_cron("0 0 31 2 *").is_none()); // Feb 31 is impossible but parses
    assert!(validate_cron("60 * * * *").is_some()); // minute=60 out of range
}

#[test]
fn convenience_wrapper_parses_and_computes() {
    let after = at(2025, 12, 31, 23, 59);
    let next = next_tick_from_expression("* * * * *", after).unwrap();
    assert_eq!(next, Some(at(2026, 1, 1, 0, 0)));
}

#[test]
fn convenience_wrapper_returns_parse_error() {
    let after = at(2025, 1, 1, 0, 0);
    assert!(next_tick_from_expression("not a cron", after).is_err());
}

#[test]
fn impossible_schedule_returns_none() {
    // Feb 30 doesn't exist → next_tick should give up after 4-year search
    let cron = parse_cron("0 0 30 2 *").unwrap();
    let after = at(2025, 1, 1, 0, 0);
    assert!(next_tick(&cron, after).is_none());
}

#[test]
fn parses_round_trip_via_serde() {
    let cron = parse_cron("0,15,30,45 * * * *").unwrap();
    let json = serde_json::to_string(&cron).unwrap();
    let back: super::ParsedCron = serde_json::from_str(&json).unwrap();
    assert_eq!(cron, back);
}
