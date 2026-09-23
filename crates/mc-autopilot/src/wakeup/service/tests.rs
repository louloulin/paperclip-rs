//! `service` 的纯逻辑单测（validate / 截断 / 冲突短路）。
//!
//! 需要真库的部分在 `crates/mc-http/tests/issues/wakeups/**` 与 mc-repos 的 `--ignored` e2e。

use super::{task_event, validate, WakeupError, WakeupInput, WAKEUP_EVENT_TYPES};
use crate::wakeup::dispatch::truncate_for_summary;
use chrono::{DateTime, Utc};

fn now() -> DateTime<Utc> {
    "2026-09-23T12:00:00Z".parse().unwrap()
}

fn base_event() -> WakeupInput {
    WakeupInput {
        agent_id: "01930000-0000-7000-8000-000000000001".to_string(),
        instruction: "  look at it  ".to_string(),
        kind: "event".to_string(),
        event_types: vec!["comment.created".to_string()],
        ..WakeupInput::default()
    }
}

fn err(input: &mut WakeupInput) -> String {
    match validate(input, now()) {
        Ok(_) => panic!("expected validation error for {input:?}"),
        Err(WakeupError::Input(msg)) => msg,
        Err(other) => panic!("unexpected error: {other}"),
    }
}

#[test]
fn event_wakeup_trims_instruction_defaults_and_has_no_schedule() {
    let mut input = base_event();
    assert_eq!(validate(&mut input, now()).unwrap(), None);
    assert_eq!(input.instruction, "look at it");
    assert_eq!(input.timezone, "UTC");
    assert_eq!(input.mode, "once");
}

#[test]
fn every_and_cron_default_to_continuous_mode() {
    let mut every = WakeupInput {
        kind: "every".to_string(),
        interval_seconds: 60,
        ..WakeupInput::default()
    };
    every.instruction = "x".to_string();
    assert_eq!(
        validate(&mut every, now()).unwrap(),
        Some(now() + chrono::Duration::seconds(60))
    );
    assert_eq!(every.mode, "continuous");

    let mut cron = WakeupInput {
        kind: "cron".to_string(),
        cron_expression: "0 9 * * 1-5".to_string(),
        timezone: "Asia/Shanghai".to_string(),
        ..WakeupInput::default()
    };
    cron.instruction = "x".to_string();
    let next = validate(&mut cron, now()).unwrap().unwrap();
    assert!(next > now());
    assert_eq!(cron.mode, "continuous");
}

#[test]
fn at_requires_exactly_one_of_at_or_after_seconds() {
    let mut input = WakeupInput {
        kind: "at".to_string(),
        instruction: "x".to_string(),
        after_seconds: 3600,
        ..WakeupInput::default()
    };
    assert_eq!(
        validate(&mut input, now()).unwrap(),
        Some(now() + chrono::Duration::seconds(3600))
    );

    let mut both = WakeupInput {
        at: Some(now()),
        after_seconds: 5,
        ..input.clone()
    };
    assert_eq!(err(&mut both), "provide at or after_seconds (1–31536000)");

    let mut neither = WakeupInput {
        after_seconds: 0,
        ..input.clone()
    };
    assert_eq!(
        err(&mut neither),
        "provide at or after_seconds (1–31536000)"
    );

    let mut too_far = WakeupInput {
        after_seconds: 31_536_001,
        ..input
    };
    assert_eq!(
        err(&mut too_far),
        "provide at or after_seconds (1–31536000)"
    );
}

#[test]
fn lifecycle_and_unsupported_events_are_rejected_verbatim() {
    let mut lifecycle = WakeupInput {
        event_types: vec!["issue.created".to_string()],
        ..base_event()
    };
    assert_eq!(
        err(&mut lifecycle),
        "issue.created cannot wake its own issue; subscriptions require an existing, open issue"
    );

    let mut bogus = WakeupInput {
        event_types: vec!["nope.happened".to_string()],
        ..base_event()
    };
    assert_eq!(err(&mut bogus), "unsupported event: nope.happened");
}

#[test]
fn actor_filter_rules_and_agent_alias() {
    let mut wrong_kind = WakeupInput {
        kind: "every".to_string(),
        interval_seconds: 60,
        filter_actor_type: "member".to_string(),
        filter_actor_id: "01930000-0000-7000-8000-000000000002".to_string(),
        ..base_event()
    };
    assert_eq!(
        err(&mut wrong_kind),
        "actor filter requires an event, member or agent type, and actor ID"
    );

    let mut task_event_with_actor = WakeupInput {
        event_types: vec!["task.completed".to_string()],
        filter_actor_type: "agent".to_string(),
        filter_actor_id: "01930000-0000-7000-8000-000000000002".to_string(),
        ..base_event()
    };
    assert_eq!(
        err(&mut task_event_with_actor),
        "actor filters apply to issue, comment, reaction and attachment changes; use agent/run filters for task events"
    );

    // 只有变更事件 + `filter_agent_id` ⇒ 别名成 actor=agent（字段就地改写）。
    let mut alias = WakeupInput {
        filter_agent_id: "01930000-0000-7000-8000-000000000003".to_string(),
        ..base_event()
    };
    assert_eq!(validate(&mut alias, now()).unwrap(), None);
    assert_eq!(alias.filter_actor_type, "agent");
    assert_eq!(
        alias.filter_actor_id,
        "01930000-0000-7000-8000-000000000003"
    );
    assert!(alias.filter_agent_id.is_empty());

    // 混了 `task.*` 就不改写（已有客户端依赖这个形状）。
    let mut mixed = WakeupInput {
        event_types: vec!["task.completed".to_string(), "comment.created".to_string()],
        filter_agent_id: "01930000-0000-7000-8000-000000000003".to_string(),
        ..base_event()
    };
    assert_eq!(validate(&mut mixed, now()).unwrap(), None);
    assert_eq!(
        mixed.filter_agent_id,
        "01930000-0000-7000-8000-000000000003"
    );
    assert!(mixed.filter_actor_type.is_empty());
}

#[test]
fn schedule_ban_and_kind_specific_rules() {
    let mut event_with_schedule = WakeupInput {
        after_seconds: 10,
        ..base_event()
    };
    assert_eq!(
        err(&mut event_with_schedule),
        "event wakeups cannot contain a schedule"
    );

    let mut six_field_cron = WakeupInput {
        kind: "cron".to_string(),
        cron_expression: "0 0 0 0 0 0".to_string(),
        ..base_event()
    };
    assert_eq!(
        err(&mut six_field_cron),
        "cron must have a future occurrence"
    );

    let mut bad_kind = WakeupInput {
        kind: "sometimes".to_string(),
        ..base_event()
    };
    assert_eq!(err(&mut bad_kind), "kind must be event, at, every or cron");

    let mut bad_mode = WakeupInput {
        mode: "forever".to_string(),
        ..base_event()
    };
    assert_eq!(err(&mut bad_mode), "mode must be once or continuous");

    let mut bad_tz = WakeupInput {
        timezone: "Mars/Olympus".to_string(),
        ..base_event()
    };
    assert_eq!(err(&mut bad_tz), "invalid timezone");

    let mut long = WakeupInput {
        instruction: "x".repeat(12_001),
        ..base_event()
    };
    assert_eq!(err(&mut long), "instruction must contain 1–12000 bytes");

    let mut blank = WakeupInput {
        instruction: "   ".to_string(),
        ..base_event()
    };
    assert_eq!(err(&mut blank), "instruction must contain 1–12000 bytes");
}

#[test]
fn severity_and_event_tables_match_upstream() {
    assert_eq!(WAKEUP_EVENT_TYPES.len(), 25);
    assert!(WAKEUP_EVENT_TYPES.contains(&"task.waiting_local_directory"));
    assert_eq!(task_event("running"), "task.started");
    assert_eq!(task_event("completed"), "task.completed");
}

#[test]
fn truncate_for_summary_flattens_whitespace_and_counts_runes() {
    assert_eq!(truncate_for_summary("  a\nb\tc  ", 100), "a b c");
    assert_eq!(truncate_for_summary("中文字", 2), "中文…");
    assert_eq!(truncate_for_summary("abc", 3), "abc");
}
