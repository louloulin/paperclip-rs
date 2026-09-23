//! M5-7 内核的**纯逻辑**测试（不连库）：`plan_time` 取整、规格校验、退避表、
//! 错误分类、重试判据、作用域格式化。
//!
//! 真库用例在 `tests/lease_db.rs`（`#[ignore]`，要 `MULTICA_TEST_DATABASE_URL`）。
//! 本文件跑在门禁 ⑤（`cargo test --workspace`）里。

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use mc_repos::scheduler::{ExecutionStatus, LatestPlanInfo, Lease};
use mc_repos::RepoError;
use mc_scheduler::db_ops::{Claim, Claimed, ClaimKind};
use mc_scheduler::error::{ErrorClass, SchedulerError};
use mc_scheduler::spec::{
    floor_plan, global_scopes, stale_secs_f64, CatchUpMode, Handler, HandlerInput, HandlerResult,
    JobSpec, PlansHook, Scope, GLOBAL,
};

/// 恒成功的 handler（纯逻辑用例只检查规格与判据，不跑它）。
fn noop_handler() -> Handler {
    Arc::new(|_input: HandlerInput| Box::pin(async { Ok(HandlerResult::default()) }))
}

/// 空计划 hook（只用来验「有 hook 时 `cadence` / `max_plans_per_tick` 可以留零」）。
fn noop_hook() -> PlansHook {
    Arc::new(|_scope, _now, _info| Box::pin(async { Ok(Vec::new()) }))
}

/// 一份**合法**的规格：5 分钟桶 / 60s-300s-30s 时间预算 / 最多 3 次尝试。
fn valid_spec(name: &str) -> JobSpec {
    JobSpec::new(name, Duration::minutes(5), global_scopes(), noop_handler())
        .with_timing(
            StdDuration::from_secs(60),
            StdDuration::from_secs(300),
            StdDuration::from_secs(30),
        )
        .with_retry(3, vec![Duration::seconds(10), Duration::seconds(60)])
}

/// Unix 秒 → `DateTime<Utc>`（测试里的时间常量一律用秒，便于手算取整）。
fn ts(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid timestamp")
}

/// 断言规格不合法，且错误文案里出现 `needle`（文案是给人看的，必须点名字段）。
fn assert_invalid(spec: &JobSpec, needle: &str) {
    match spec.validate() {
        Err(SchedulerError::InvalidSpec(msg)) => {
            assert!(msg.contains(needle), "消息里应出现 {needle:?}：{msg}");
        }
        other => panic!("期望 InvalidSpec({needle:?})，实际 {other:?}"),
    }
}

/// 浮点相等（`clippy::float_cmp` 不喜欢 `==`）。
fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < f64::EPSILON
}

// ---------------------------------------------------------------- plan_time 取整

#[test]
fn floor_plan_truncates_since_go_zero_not_since_unix_epoch() {
    let epoch = ts(0);

    // 整除一天的 cadence（5m / 1h）⇒ 与「按 Unix 纪元取整」同结果。
    assert_eq!(floor_plan(epoch, Duration::minutes(5)), epoch);
    assert_eq!(
        floor_plan(epoch + Duration::minutes(4) + Duration::seconds(59), Duration::minutes(5)),
        epoch
    );
    assert_eq!(floor_plan(epoch, Duration::hours(1)), epoch);

    // 7h 不整除一天：Go 的 `Truncate` 从**零值**起算，网格点是 05:00/12:00/19:00…
    // 按 Unix 纪元取整会把 epoch 留在 epoch，差 2 小时 ⇒ 两实例的 plan_time 会漂移。
    assert_eq!(floor_plan(epoch, Duration::hours(7)), ts(-7_200));
    assert_eq!(floor_plan(ts(-7_200), Duration::hours(7)), ts(-7_200));
    assert_eq!(floor_plan(ts(-7_199), Duration::hours(7)), ts(-7_200));
    assert_eq!(floor_plan(ts(-7_201), Duration::hours(7)), ts(-32_400));
}

#[test]
fn floor_plan_handles_sub_second_and_zero_cadence() {
    let epoch = ts(0);
    // 1.5s：零值到纪元的秒数整除 1.5 ⇒ 纪元是一个网格点。
    assert_eq!(
        floor_plan(epoch + Duration::seconds(1), Duration::milliseconds(1_500)),
        epoch
    );
    // 非正 cadence 原样返回（上游同：`if c <= 0 { return eligible }`）。
    assert_eq!(floor_plan(epoch + Duration::seconds(3), Duration::zero()), ts(3));
    assert_eq!(floor_plan(epoch + Duration::seconds(3), Duration::seconds(-5)), ts(3));
    // 未来桶不会被造出来：调用方拿到的桶必须 <= 入参。
    let eligible = ts(1_700_000_123);
    assert!(floor_plan(eligible, Duration::minutes(15)) <= eligible);
}

// ---------------------------------------------------------------- 规格校验

#[test]
fn valid_spec_passes_validation() {
    assert!(valid_spec("itest-ok").validate().is_ok());
}

#[test]
fn validate_rejects_each_missing_budget_with_the_field_name() {
    assert_invalid(&valid_spec("   "), "job name is required");
    // `JobSpec::new` 故意把所有时间预算留零 ⇒ 漏填必须在这里炸，不能静默取默认值。
    assert_invalid(
        &JobSpec::new("itest-zero", Duration::minutes(5), global_scopes(), noop_handler()),
        "run_timeout",
    );
    assert_invalid(
        &valid_spec("itest-stale")
            .with_timing(StdDuration::from_secs(60), StdDuration::from_secs(60), StdDuration::from_secs(30)),
        "stale_timeout",
    );
    assert_invalid(
        &valid_spec("itest-hb-zero")
            .with_timing(StdDuration::from_secs(60), StdDuration::from_secs(300), StdDuration::ZERO),
        "heartbeat_interval",
    );
    assert_invalid(
        &valid_spec("itest-hb-too-big").with_timing(
            StdDuration::from_secs(60),
            StdDuration::from_secs(300),
            StdDuration::from_secs(300),
        ),
        "heartbeat_interval",
    );
    assert_invalid(&valid_spec("itest-attempts").with_retry(0, Vec::new()), "max_attempts");
    assert_invalid(
        &valid_spec("itest-every-plan")
            .with_catch_up(CatchUpMode::EveryPlan, Duration::hours(6), 0),
        "max_plans_per_tick",
    );
}

#[test]
fn validate_accepts_zero_cadence_and_zero_cap_when_a_plan_hook_is_set() {
    // 上游：`PlansForScope` 一设，`cadence` 与「every_plan 必须有上限」两条都不再适用。
    let spec = JobSpec::new("itest-hooked", Duration::zero(), global_scopes(), noop_handler())
        .with_plans_for_scope(noop_hook())
        .with_catch_up(CatchUpMode::EveryPlan, Duration::hours(6), 0)
        .with_timing(
            StdDuration::from_secs(60),
            StdDuration::from_secs(300),
            StdDuration::from_secs(30),
        )
        .with_retry(1, Vec::new());
    assert!(spec.validate().is_ok());
}

// ---------------------------------------------------------------- 退避与陈旧窗口

#[test]
fn retry_delay_clamps_both_ends_and_treats_empty_as_zero() {
    let spec = valid_spec("itest-backoff");
    assert_eq!(spec.retry_delay(1), Duration::seconds(10));
    assert_eq!(spec.retry_delay(2), Duration::seconds(60));
    assert_eq!(spec.retry_delay(3), Duration::seconds(60), "越界复用末项");
    assert_eq!(spec.retry_delay(9), Duration::seconds(60));
    assert_eq!(spec.retry_delay(0), Duration::seconds(10), "小于 1 取首项");
    assert_eq!(spec.retry_delay(-5), Duration::seconds(10));

    let no_backoff = valid_spec("itest-no-backoff").with_retry(3, Vec::new());
    assert_eq!(no_backoff.retry_delay(1), Duration::zero());
}

#[test]
fn stale_secs_truncates_to_whole_seconds_with_a_floor_of_one() {
    assert!(approx(stale_secs_f64(StdDuration::ZERO), 1.0));
    assert!(approx(stale_secs_f64(StdDuration::from_millis(1_500)), 1.0));
    assert!(approx(stale_secs_f64(StdDuration::from_millis(2_900)), 2.0));
    assert!(approx(stale_secs_f64(StdDuration::from_secs(300)), 300.0));
    assert!(approx(valid_spec("itest-stale-secs").stale_secs(), 300.0));
}

// ---------------------------------------------------------------- 错误分类

#[test]
fn error_codes_match_upstream_classify_error() {
    assert_eq!(SchedulerError::LeaseLost("heartbeat").code(), "lease_lost");
    assert_eq!(SchedulerError::RunTimeout.code(), "run_timeout");
    assert_eq!(SchedulerError::Canceled.code(), "canceled");
    assert_eq!(SchedulerError::HandlerPanic("boom".to_owned()).code(), "handler_panic");
    assert_eq!(SchedulerError::Handler("boom".to_owned()).code(), "handler_error");
    assert_eq!(
        SchedulerError::Permanent {
            code: "invalid_cron".to_owned(),
            message: "boom".to_owned(),
        }
        .code(),
        "invalid_cron",
        "Permanent 透传 handler 自己的审计码"
    );
    assert_eq!(SchedulerError::InvalidSpec("boom".to_owned()).code(), "invalid_spec");
    assert_eq!(SchedulerError::DuplicateJob("boom".to_owned()).code(), "duplicate_job");
    assert_eq!(SchedulerError::Repo(RepoError::Db("boom".to_owned())).code(), "db_error");
}

#[test]
fn error_classes_drive_the_three_dispositions() {
    assert_eq!(SchedulerError::LeaseLost("x").class(), ErrorClass::LeaseLost);
    for retryable in [
        SchedulerError::RunTimeout,
        SchedulerError::Canceled,
        SchedulerError::HandlerPanic("boom".to_owned()),
        SchedulerError::Handler("boom".to_owned()),
        // 一次网络抖动不能永久废掉一个计划桶 ⇒ 仓储错误按可重试处理（上游 default 分支）。
        SchedulerError::Repo(RepoError::Db("boom".to_owned())),
    ] {
        assert_eq!(retryable.class(), ErrorClass::Retryable, "{retryable}");
    }
    for permanent in [
        SchedulerError::Permanent {
            code: "invalid_cron".to_owned(),
            message: "boom".to_owned(),
        },
        SchedulerError::InvalidSpec("boom".to_owned()),
        SchedulerError::DuplicateJob("boom".to_owned()),
    ] {
        assert_eq!(permanent.class(), ErrorClass::Permanent, "{permanent}");
    }
}

// ---------------------------------------------------------------- 重试判据（在仓储里）

#[test]
fn retry_eligible_follows_upstream_order() {
    let now = ts(1_700_000_000);
    let failed = LatestPlanInfo {
        found: true,
        plan_time: ts(1_699_999_700),
        status: ExecutionStatus::Failed,
        attempt: 1,
        max_attempts: 3,
        next_retry_at: None,
    };
    assert!(failed.retry_eligible(now), "NULL next_retry_at 的语义是「尽快」= 现在");
    assert!(!LatestPlanInfo::empty().retry_eligible(now), "没有历史 ⇒ 不重试");
    assert!(!LatestPlanInfo { status: ExecutionStatus::Success, ..failed }.retry_eligible(now));
    assert!(!LatestPlanInfo { status: ExecutionStatus::Running, ..failed }.retry_eligible(now));
    assert!(!LatestPlanInfo { attempt: 3, ..failed }.retry_eligible(now), "预算用尽");
    assert!(!LatestPlanInfo { attempt: 4, ..failed }.retry_eligible(now));
    assert!(
        !LatestPlanInfo { next_retry_at: Some(ts(1_700_000_001)), ..failed }.retry_eligible(now)
    );
    assert!(
        LatestPlanInfo { next_retry_at: Some(now), ..failed }.retry_eligible(now),
        "退避正好到期（== now）就算到"
    );
}

// ---------------------------------------------------------------- 认领结果与作用域

#[test]
fn claim_exposes_kind_and_lease() {
    let lease = Lease {
        id: Uuid::nil(),
        lease_token: Uuid::nil(),
    };
    let conflicted = Claim::Conflicted;
    assert_eq!(conflicted.kind(), ClaimKind::Conflicted);
    assert!(conflicted.claimed().is_none());

    let won = Claim::Won(Claimed { lease, attempt: 1 });
    assert_eq!(won.kind(), ClaimKind::Won);
    assert_eq!(won.claimed().map(|claimed| claimed.attempt), Some(1));
    assert_eq!(won.claimed().map(|claimed| claimed.lease.id), Some(Uuid::nil()));

    let stole = Claim::Stole(Claimed { lease, attempt: 2 });
    assert_eq!(stole.kind(), ClaimKind::Stole);
    assert_eq!(stole.claimed().map(|claimed| claimed.attempt), Some(2));
}

#[test]
fn catch_up_mode_round_trips_and_rejects_unknown() {
    assert_eq!(CatchUpMode::default(), CatchUpMode::LatestOnly);
    assert_eq!(CatchUpMode::LatestOnly.as_str(), "latest_only");
    assert_eq!(CatchUpMode::EveryPlan.to_string(), "every_plan");
    assert_eq!(CatchUpMode::from_str("latest_only").expect("parse"), CatchUpMode::LatestOnly);
    assert_eq!(CatchUpMode::from_str("every_plan").expect("parse"), CatchUpMode::EveryPlan);
    match CatchUpMode::from_str("hourly") {
        Err(SchedulerError::InvalidSpec(msg)) => assert!(msg.contains("catch_up_mode"), "{msg}"),
        other => panic!("未知模式必须在注册期报错，实际 {other:?}"),
    }
}

#[test]
fn scope_formats_as_kind_slash_id() {
    assert_eq!(GLOBAL, "global");
    assert_eq!(Scope::global().to_string(), "global/global");
    assert_eq!(Scope::new("workspace", "ws-1").to_string(), "workspace/ws-1");
    assert_eq!(Scope::global().kind, GLOBAL);
}

#[tokio::test]
async fn global_scopes_yields_exactly_the_global_scope() {
    let scopes = global_scopes()(ts(0)).await.expect("scope provider");
    assert_eq!(scopes, vec![Scope::global()]);
}
