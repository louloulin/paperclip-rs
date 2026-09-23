//! `mc_autopilot::cron` 的用例（M5-1 专属：5 字段语义 + 无下次触发）。
//!
//! 这些用例是**语义钉**：每条断言对应上游 `service/cron.go` + robfig v3
//! `Minute|Hour|Dom|Month|Dow` 的一个可观察行为，改动语义必然打红。

use super::*;

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn next(expr: &str, timezone: &str, after: &str) -> Option<DateTime<Utc>> {
    next_occurrence_after_utc(expr, timezone, at(after)).expect("parse")
}

#[test]
fn requires_exactly_five_fields() {
    let err = CronSpec::parse("0 0 0 * * *").expect_err("6 fields must be rejected");
    assert!(matches!(err, CronError::FieldCount { found: 6, .. }));
    assert_eq!(err.code(), CODE_INVALID_CRON);
    // 4 字段 / 空表达式同样拒绝。
    assert!(CronSpec::parse("0 0 * *").is_err());
    assert!(CronSpec::parse("").is_err());
    // 多余空白不会被当成字段（robfig 用 strings.Fields）。
    assert!(CronSpec::parse("  0   9  *  *  * ").is_ok());
}

#[test]
fn dow_numbering_is_zero_for_sunday() {
    // 2024-01-01 是周一；下一个周日 00:00 = 2024-01-07。
    assert_eq!(
        next("0 0 * * 0", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-07T00:00:00Z"))
    );
    // 6 = 周六（chrono 的 Weekday 是 Mon=1..Sun=0，必须转成 num_days_from_sunday）。
    assert_eq!(
        next("0 0 * * 6", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-06T00:00:00Z"))
    );
    // `7` 不在 robfig 的 0-6 区间内 ⇒ 拒绝（不是「周日」的别名）。
    assert!(CronSpec::parse("0 0 * * 7").is_err());
}

#[test]
fn dom_and_dow_are_ored_when_both_restricted() {
    // 2024-01-05 是周五，2024-01-13 是周六：OR 语义下先命中周五。
    assert_eq!(
        next("0 0 13 * 5", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-05T00:00:00Z"))
    );
    // 一方是 `*` ⇒ AND（等价只看另一边）。
    assert_eq!(
        next("0 0 13 * *", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-13T00:00:00Z"))
    );
    // `*/2` 的 dom **不算** `*`（robfig 的 `if step > 1 { extra = 0 }`）⇒ 仍走 OR：
    // 命中 1/3/5/… 的奇数日或周一，1/1 是周一但必须严格晚于锚点 ⇒ 1/3。
    assert_eq!(
        next("0 0 */2 * 1", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-03T00:00:00Z"))
    );
}

#[test]
fn n_slash_step_means_n_to_max() {
    // `5/10` = `5-31/10` ⇒ 5、15、25 日。
    let days = next_occurrences_after_utc("0 0 5/10 * *", "UTC", at("2024-01-01T00:00:00Z"), 3)
        .expect("parse");
    assert_eq!(
        days,
        vec![
            at("2024-01-05T00:00:00Z"),
            at("2024-01-15T00:00:00Z"),
            at("2024-01-25T00:00:00Z"),
        ]
    );
}

#[test]
fn names_are_case_insensitive() {
    assert_eq!(
        next("0 9 * JAN Mon", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-01T09:00:00Z"))
    );
    assert_eq!(
        next("0 9 * jan mon", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-01T09:00:00Z"))
    );
}

#[test]
fn question_mark_is_star() {
    assert_eq!(
        next("0 0 ? * 1", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-08T00:00:00Z"))
    );
    // `?` 的星号位同样在 `/step>1` 时被清掉。
    assert_eq!(
        next("0 0 ? * 1", "UTC", "2024-01-01T00:00:00Z"),
        next("0 0 * * 1", "UTC", "2024-01-01T00:00:00Z")
    );
}

#[test]
fn never_fires_is_none_not_error() {
    // 2 月没有 30 日 ⇒ 5 年视界内零次（上游返回零值时间，本地是 None）。
    assert_eq!(next("0 0 30 2 *", "UTC", "2024-01-01T00:00:00Z"), None);
    let empty = next_occurrences_after_utc("0 0 30 2 *", "UTC", at("2024-01-01T00:00:00Z"), 3)
        .expect("parse");
    assert!(empty.is_empty(), "short slice, not an error: {empty:?}");
}

#[test]
fn occurrences_are_ascending_and_capped_by_count() {
    let runs = next_occurrences_after_utc("0 0 * * *", "UTC", at("2024-01-01T12:00:00Z"), 3)
        .expect("parse");
    assert_eq!(
        runs,
        vec![
            at("2024-01-02T00:00:00Z"),
            at("2024-01-03T00:00:00Z"),
            at("2024-01-04T00:00:00Z"),
        ]
    );
    // 5 年视界**不截断** `count`：它只用来**终止**「根本不触发」的表达式
    //（robfig 的 `yearLimit`，零值时间 ⇒ 短切片）。所以年粒度表达式要 10 次就给 10 次。
    let yearly =
        next_occurrences_after_utc("0 0 1 1 *", "UTC", at("2024-01-01T00:00:00Z"), 10).expect("parse");
    assert_eq!(yearly.len(), 10);
    assert_eq!(yearly[0], at("2025-01-01T00:00:00Z"));
    assert_eq!(yearly[9], at("2034-01-01T00:00:00Z"));
}

#[test]
fn between_window_is_strictly_after_and_inclusive_until() {
    let runs = next_occurrences_between_utc(
        "*/15 * * * *",
        "UTC",
        at("2024-01-01T00:00:00Z"),
        at("2024-01-01T01:00:00Z"),
    )
    .expect("parse");
    assert_eq!(runs.len(), 4);
    assert_eq!(runs[0], at("2024-01-01T00:15:00Z"));
    assert_eq!(runs[3], at("2024-01-01T01:00:00Z"));
    assert!(runs.iter().all(|run| *run > at("2024-01-01T00:00:00Z")));
}

#[test]
fn timezone_decides_the_wall_clock() {
    // 09:00 Asia/Shanghai = 01:00Z。
    assert_eq!(
        next("0 9 * * *", "Asia/Shanghai", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-01T01:00:00Z"))
    );
    // 同一个表达式在 UTC 是 09:00Z。
    assert_eq!(
        next("0 9 * * *", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-01T09:00:00Z"))
    );
}

#[test]
fn tz_prefix_overrides_the_argument() {
    // 09:00 America/New_York = 14:00Z（1 月是 EST，UTC-5）。
    assert_eq!(
        next("TZ=America/New_York 0 9 * * *", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-01T14:00:00Z"))
    );
    assert_eq!(
        next("CRON_TZ=America/New_York 0 9 * * *", "UTC", "2024-01-01T00:00:00Z"),
        next("TZ=America/New_York 0 9 * * *", "UTC", "2024-01-01T00:00:00Z")
    );
}

#[test]
fn tz_prefix_without_schedule_is_a_400_not_a_panic() {
    // 上游 robfig 在这里 panic（parser.go:99），handler 必须把它拦成 invalid_cron。
    // 前缀**没有被剥离**的原文交给 5 字段解析器 ⇒ 它看到 6 个字段（`TZ=UTC` 也算一个），
    // 绝不会误当成合法表达式；前缀剥离只发生在 [`parse_with_timezone`] 里。
    let err = CronSpec::parse("TZ=UTC 0 9 * * *").expect_err("raw parser must reject");
    assert!(matches!(err, CronError::FieldCount { found: 6, .. }));
    let err = parse_with_timezone("TZ=UTC", "UTC").expect_err("no schedule");
    assert!(matches!(
        err,
        CronError::MissingScheduleAfterTimezonePrefix { .. }
    ));
    assert_eq!(err.code(), CODE_INVALID_CRON);
}

#[test]
fn timezone_errors_are_classified() {
    let err = resolve_timezone("Mars/Olympus").expect_err("unknown zone");
    assert_eq!(err.code(), CODE_INVALID_TIMEZONE);
    let err = parse_with_timezone("0 9 * * *", "Mars/Olympus").expect_err("unknown zone");
    assert_eq!(err.code(), CODE_INVALID_TIMEZONE);
    // 合法时区 + 非法表达式 ⇒ invalid_cron（两类不能混）。
    let err = parse_with_timezone("61 0 * * *", "Europe/Berlin").expect_err("minute 61");
    assert_eq!(err.code(), CODE_INVALID_CRON);
    // 表达式自带前缀里的坏时区 ⇒ 也算**表达式**的问题（invalid_cron），
    // 只有 `tz` 查询参数才走 invalid_timezone（上游 handler 的 `ValidateTimezone`）。
    let err = parse_with_timezone("TZ=Mars/Olympus 0 9 * * *", "UTC").expect_err("bad prefix");
    assert!(matches!(err, CronError::BadPrefixTimezone { .. }));
    assert_eq!(err.code(), CODE_INVALID_CRON);
    assert!(resolve_timezone("  Asia/Tokyo  ").is_ok(), "前后空白要容忍");
}

#[test]
fn field_validation_errors() {
    assert!(matches!(
        CronSpec::parse("60 0 * * *"),
        Err(CronError::AboveMax { .. })
    ));
    assert!(matches!(
        CronSpec::parse("0 24 * * *"),
        Err(CronError::AboveMax { .. })
    ));
    assert!(matches!(
        CronSpec::parse("0 0 0 * *"),
        Err(CronError::BelowMin { .. })
    ));
    assert!(matches!(
        CronSpec::parse("0 0 32 * *"),
        Err(CronError::AboveMax { .. })
    ));
    assert!(matches!(
        CronSpec::parse("0 0 13 * 5-1"),
        Err(CronError::InvertedRange { .. })
    ));
    assert!(matches!(
        CronSpec::parse("*/0 * * * *"),
        Err(CronError::ZeroStep { .. })
    ));
    assert!(matches!(
        CronSpec::parse("a * * * *"),
        Err(CronError::ParseInt { .. })
    ));
    assert!(matches!(
        CronSpec::parse("0 0 * foo *"),
        Err(CronError::ParseInt { .. })
    ));
}

#[test]
fn lists_and_ranges() {
    assert_eq!(
        next("15,45 9-10 * * *", "UTC", "2024-01-01T00:00:00Z"),
        Some(at("2024-01-01T09:15:00Z"))
    );
    assert_eq!(
        next("15,45 9-10 * * *", "UTC", "2024-01-01T09:15:00Z"),
        Some(at("2024-01-01T09:45:00Z"))
    );
    assert_eq!(
        next("15,45 9-10 * * *", "UTC", "2024-01-01T09:45:00Z"),
        Some(at("2024-01-01T10:15:00Z"))
    );
}

#[test]
fn dst_gap_skips_the_missing_local_minute() {
    // 2024-03-31 Europe/Berlin：02:00 → 03:00。02:30 那天不存在 ⇒ 顺延到 4 月 1 日。
    assert_eq!(
        next("30 2 * * *", "Europe/Berlin", "2024-03-30T12:00:00Z"),
        Some(at("2024-04-01T00:30:00Z"))
    );
}

#[test]
fn dst_fold_fires_the_repeated_local_minute_twice() {
    // 2024-11-03 America/New_York：本地 01:00–01:59 出现两次。01:30 先 EDT（05:30Z）
    // 后 EST（06:30Z）——回拨那一小时里的分钟**触发两次**（只有 gap 里的分钟才永不触发）。
    let first = next("30 1 * * *", "America/New_York", "2024-11-03T00:00:00Z");
    assert_eq!(first, Some(at("2024-11-03T05:30:00Z")));
    let second = next("30 1 * * *", "America/New_York", "2024-11-03T05:30:00Z");
    assert_eq!(second, Some(at("2024-11-03T06:30:00Z")));
    // 回拨之后的下一分钟（本地 01:30 EST 之后）是次日 01:30。
    let third = next("30 1 * * *", "America/New_York", "2024-11-03T06:30:00Z");
    assert_eq!(third, Some(at("2024-11-04T06:30:00Z")));
}

#[test]
fn compute_next_run_uses_now_and_validates() {
    let upcoming = compute_next_run("0 0 1 1 *", "UTC").expect("parse");
    assert!(upcoming.is_some(), "元旦总会有下一次");
    assert!(compute_next_run("* * * * *", "Mars/Olympus").is_err());
}

#[test]
fn fold_hour_jump_lands_after_the_repeated_hour() {
    // 2024-11-03 America/New_York：01:00–01:59 出现了两次。从 00:00 EDT 出发找当天的 02:00：
    // 整点跳必须经过 01:00 EST（本地再次回到 01:00）后停在 02:00 EST = 07:00Z，
    // 不能把重复的那一小时当成「已经走过」。
    assert_eq!(
        next("0 2 * * *", "America/New_York", "2024-11-03T04:00:00Z"),
        Some(at("2024-11-03T07:00:00Z"))
    );
}
