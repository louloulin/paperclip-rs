//! `mc_autopilot::trigger` 的用例（M5-3 专属：时区解包 / provider 闭集 / 事件过滤 / next_run_at）。
//!
//! 这些用例是**语义钉**：每条断言对应上游 `handler/autopilot.go` 的一个可观察行为
//! （`ValidateTimezone` / `isAllowedWebhookProvider` / `validateWebhookEventFilters` /
//! `encodeWebhookEventFilters*` / `eventFiltersMatch` / `computeNextRun`），
//! 改语义必然打红。纯计算，无 DB、无 HTTP。

use super::*;
use crate::cron::{CODE_INVALID_CRON, CODE_INVALID_TIMEZONE};

// ---------------------------------------------------------------------------
// Timezone
// ---------------------------------------------------------------------------

#[test]
fn empty_timezone_is_utc_not_an_error() {
    // 上游 `time.LoadLocation("")` 返回 UTC ⇒ 空串是合法输入。
    // 这条同时钉住「`autopilot_trigger.timezone` 可空」的读法：NULL / '' ⇒ UTC。
    let tz = Timezone::parse("").expect("empty is utc");
    assert_eq!(tz.name(), "UTC");
    assert_eq!(tz, Timezone::default());
}

#[test]
fn named_timezone_round_trips_through_iana() {
    let tz = Timezone::parse("Asia/Shanghai").expect("iana");
    assert_eq!(tz.name(), "Asia/Shanghai");
    assert_eq!(tz.to_string(), "Asia/Shanghai");
    // `+08:00` 固定偏移（无 DST）—— 用 `chrono-tz` 的真实规则验一次，避免 newtype 只做字符串搬运。
    assert_eq!(tz.as_tz().to_string(), "Asia/Shanghai");
}

#[test]
fn unknown_timezone_is_invalid_timezone_code() {
    let err = Timezone::parse("Not/AZone").expect_err("unknown zone must fail");
    assert!(matches!(err, CronError::InvalidTimezone { .. }));
    assert_eq!(err.code(), CODE_INVALID_TIMEZONE);
}

#[test]
fn from_column_treats_none_and_empty_as_utc() {
    // 可空列（`timezone TEXT NULL DEFAULT 'UTC'`）与 NOT NULL 的 `issue_wakeup.timezone`
    // 语义不同：这里 `None` **不是**错误。
    assert_eq!(Timezone::from_column(None).expect("none").name(), "UTC");
    assert_eq!(Timezone::from_column(Some("")).expect("empty").name(), "UTC");
    assert_eq!(
        Timezone::from_column(Some("America/New_York"))
            .expect("iana")
            .name(),
        "America/New_York"
    );
    // 存量脏数据（列里存了不认识的名字）要能被调用方识别出来，而不是静默 UTC。
    assert!(Timezone::from_column(Some("<script>")).is_err());
}

// ---------------------------------------------------------------------------
// provider
// ---------------------------------------------------------------------------

#[test]
fn provider_whitelist_is_closed() {
    assert!(is_allowed_webhook_provider(WEBHOOK_PROVIDER_GENERIC));
    assert!(is_allowed_webhook_provider(WEBHOOK_PROVIDER_GITHUB));
    // 大小写/空串/近义串都不在白名单（上游注释：拼错就静默退化成 generic = 绕过 provider 专属行为）。
    for bad in ["", "Generic", "GITHUB", "githubb", "slack", "generic "] {
        assert!(!is_allowed_webhook_provider(bad), "{bad:?}");
    }
}

// ---------------------------------------------------------------------------
// 事件过滤
// ---------------------------------------------------------------------------

fn filter(event: &str, actions: Option<&[&str]>) -> WebhookEventFilter {
    WebhookEventFilter {
        event: event.to_string(),
        actions: actions.map(|list| list.iter().map(|s| (*s).to_string()).collect()),
    }
}

#[test]
fn empty_filter_list_is_valid_and_accepts_everything() {
    let filters: Vec<WebhookEventFilter> = Vec::new();
    assert_eq!(validate_webhook_event_filters(&filters), Ok(()));
    assert!(webhook_event_filters_match(&filters, "anything", "at.all"));
}

#[test]
fn blank_event_or_action_is_rejected_with_index() {
    let err = validate_webhook_event_filters(&[filter("issues", None), filter("  ", None)])
        .expect_err("blank event");
    assert_eq!(err, EventFilterError::EmptyEvent { index: 1 });
    assert_eq!(err.to_string(), "event_filters[1].event must not be empty");

    let err = validate_webhook_event_filters(&[filter("issues", Some(&["opened", " "]))])
        .expect_err("blank action");
    assert_eq!(
        err,
        EventFilterError::EmptyAction {
            index: 0,
            action: 1
        }
    );
    assert_eq!(
        err.to_string(),
        "event_filters[0].actions[1] must not be empty"
    );
}

#[test]
fn create_path_encodes_empty_as_null_but_always_variant_as_empty_array() {
    // 上游 `encodeWebhookEventFilters`：nil/empty ⇒ 不写列（SQL NULL）。
    assert!(encode_webhook_event_filters(&[]).is_none());
    // 上游 `encodeWebhookEventFiltersAlways`：清除路径必须是**非 NULL** 的 `[]`
    //（SQL 里 `event_filters = COALESCE($n, event_filters)` ⇒ NULL 是「保留原值」）。
    assert_eq!(
        encode_webhook_event_filters_always(&[]),
        serde_json::json!([])
    );

    let encoded = encode_webhook_event_filters(&[filter("issues", None)]).expect("jsonb");
    assert_eq!(encoded, serde_json::json!([{ "event": "issues" }]));
    // `actions: []` 归一成「字段缺失」：`omitempty` 与缺失在 Go 里落同一份字节，
    // 统一成缺失才能让 update 的实质变更比对不被 `[]`/缺失的差异误报。
    let encoded = encode_webhook_event_filters(&[filter("issues", Some(&[]))]).expect("jsonb");
    assert_eq!(encoded, serde_json::json!([{ "event": "issues" }]));
    let encoded =
        encode_webhook_event_filters(&[filter("issues", Some(&["opened"]))]).expect("jsonb");
    assert_eq!(
        encoded,
        serde_json::json!([{ "event": "issues", "actions": ["opened"] }])
    );
}

#[test]
fn matcher_requires_event_equality_and_action_membership() {
    let open_only = filter("issues", Some(&["opened"]));
    assert!(webhook_event_filter_matches(&open_only, "issues", "opened"));
    assert!(!webhook_event_filter_matches(&open_only, "issues", "closed"));
    assert!(!webhook_event_filter_matches(&open_only, "pull_request", "opened"));
    // 事件相同但没写 actions ⇒ 该事件的全部动作都放行。
    assert!(webhook_event_filter_matches(
        &filter("push", None),
        "push",
        "any"
    ));
    // 整表：任一条命中即放行；`actions` 缺失与空数组等价。
    let filters = vec![open_only, filter("push", Some(&[]))];
    assert!(webhook_event_filters_match(&filters, "push", "anything"));
    assert!(webhook_event_filters_match(&filters, "issues", "opened"));
    assert!(!webhook_event_filters_match(&filters, "issues", "closed"));
}

// ---------------------------------------------------------------------------
// next_run_at（上游 computeNextRun）
// ---------------------------------------------------------------------------

#[test]
fn next_run_at_uses_the_given_timezone() {
    // `0 9 * * *`（每天 09:00）在 `Asia/Shanghai` 与 UTC 下都能算出下一次；两者相差 8 小时。
    let tz = Timezone::parse("Asia/Shanghai").expect("iana");
    let shanghai = next_run_at_for("0 9 * * *", &tz)
        .expect("parse")
        .expect("has next run");
    let utc = next_run_at_for("0 9 * * *", &Timezone::default())
        .expect("parse")
        .expect("has next run");
    assert_ne!(shanghai, utc);
    assert!(shanghai > Utc::now());
    // 本地时间必须正好 09:00。
    let local = shanghai.with_timezone(&tz.as_tz());
    assert_eq!(
        local.format("%H:%M").to_string(),
        "09:00",
        "本地时刻：{local}"
    );
}

#[test]
fn never_firing_expression_is_none_not_an_error() {
    // 语法合法但永不触发（2 月 30 日）⇒ `Ok(None)`，与「表达式写错了」靠状态码区分。
    assert_eq!(
        next_run_at_for("0 0 30 2 *", &Timezone::default()).expect("valid syntax"),
        None
    );
}

#[test]
fn bad_expression_or_bad_timezone_is_a_coded_error() {
    let err = next_run_at_for("not a cron", &Timezone::default()).expect_err("syntax");
    assert_eq!(err.code(), CODE_INVALID_CRON);
    // 表达式自带的 `TZ=` 前缀不认识 ⇒ 仍归 `invalid_cron`（robfig `Parse` 的错，不是 `ValidateTimezone`）。
    let err = next_run_at_for("TZ=Not/AZone 0 0 * * *", &Timezone::default()).expect_err("prefix");
    assert_eq!(err.code(), CODE_INVALID_CRON);
}
