//! `lease` 的测试。

use super::*;
use crate::retry::RetryBudget;

fn t(secs: i64) -> Timestamp {
    Timestamp::from_unix(1_700_000_000 + secs)
}

fn policy() -> StalePolicy {
    StalePolicy::UPSTREAM_DEFAULT
}

fn queued() -> TaskState {
    let (state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    state
}

fn dispatched_state(dispatched_at: Timestamp) -> TaskState {
    let (mut state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    state.apply(TaskEvent::Dispatch, dispatched_at).unwrap();
    state
}

fn running_at(started_at: Timestamp) -> TaskState {
    let (mut state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    state.apply(TaskEvent::Dispatch, started_at).unwrap();
    state.apply(TaskEvent::Running, started_at).unwrap();
    state
}

#[test]
fn upstream_thresholds_are_the_go_ones() {
    assert_eq!(policy(), StalePolicy::default());
    assert_eq!(policy().prepare_lease_secs, 45);
    assert_eq!(policy().dispatch_timeout_secs, 300);
    assert_eq!(policy().running_timeout_secs, 9_000);
    assert_eq!(policy().runtime_stale_secs, 150);
    assert_eq!(policy().runtime_reconnect_grace_secs, 10_800);
    assert_eq!(policy().claim_recovery_secs, 90);
    assert_eq!(
        policy().queued_grace_secs,
        policy().runtime_reconnect_grace_secs
    );
}

#[test]
fn prepare_lease_null_is_not_permanent_guard() {
    let lease = Lease::new(Some(t(0)), None, None);
    assert!(!lease.prepare_is_active(t(1)), "NULL 租约不是「永久守护」");
    let active = Lease::new(Some(t(0)), Some(plus_secs(t(0), 45)), None);
    assert!(active.prepare_is_active(t(0)));
    assert!(active.prepare_is_active(t(45)), "边界：到期时刻仍算活着");
    assert!(!active.prepare_is_active(t(46)));
    assert_eq!(active.remaining_secs(t(0)), Some(45));
    assert_eq!(active.remaining_secs(t(60)), Some(-15));
    assert_eq!(lease.remaining_secs(t(0)), None);
    assert_eq!(Lease::next_deadline(t(0), &policy()), t(45));
}

#[test]
fn lease_extension_is_due_in_the_last_interval() {
    let lease = Lease::new(Some(t(0)), Some(t(45)), None);
    assert!(!lease.is_due_for_extension(t(0)), "剩余 45s 不用续");
    assert!(!lease.is_due_for_extension(t(29)), "剩余 16s 还不用续");
    assert!(lease.is_due_for_extension(t(30)), "剩余 15s 该续了");
    assert!(lease.is_due_for_extension(t(60)), "已过期更要续");
    assert!(!Lease::new(Some(t(0)), None, None).is_due_for_extension(t(60)));
}

#[test]
fn dispatched_is_killed_on_wall_clock_when_the_runtime_is_healthy() {
    let state = dispatched_state(t(0));
    let fresh = RuntimeLiveness::known(true, t(400));
    assert!(stale_task_verdict(&state, fresh, &policy(), t(299)).is_keep());
    assert_eq!(
        stale_task_verdict(&state, fresh, &policy(), t(301)),
        StaleVerdict::Fail {
            reason: FailureReason::Timeout,
            message: TIMEOUT_MESSAGE,
        }
    );
}

#[test]
fn a_live_prepare_lease_protects_a_dispatched_row() {
    let mut state = dispatched_state(t(0));
    state.prepare_lease_expires_at = Some(t(1_000));
    // 挂钟早就超了，租约还在 ⇒ 保住。
    assert!(stale_task_verdict(
        &state,
        RuntimeLiveness::known(true, t(400)),
        &policy(),
        t(500)
    )
    .is_keep());
}

#[test]
fn a_short_partition_does_not_kill_dispatched_work() {
    let state = dispatched_state(t(0));
    // 守护进程 10 分钟前掉线：挂钟超了、租约过期，但仍在 3h 宽限内。
    let just_dropped = RuntimeLiveness::known(false, t(600 - 1_800));
    assert!(stale_task_verdict(&state, just_dropped, &policy(), t(600)).is_keep());
    // 超出整个宽限 ⇒ 退场。
    let long_gone = RuntimeLiveness::known(false, t(600 - 10_801));
    assert!(matches!(
        stale_task_verdict(&state, long_gone, &policy(), t(600)),
        StaleVerdict::Fail { .. }
    ));
}

#[test]
fn running_work_survives_a_partition_even_when_the_wall_clock_elapsed() {
    let state = running_at(t(0));
    let fresh = RuntimeLiveness::known(true, t(9_000));
    // 9h 的挂钟上限到了，但 daemon 心跳新鲜 ⇒ 多小时的正常工作必须活下来。
    assert!(stale_task_verdict(&state, fresh, &policy(), t(9_001)).is_keep());
    // 心跳停在一个宽限之外 ⇒ 退场。
    let dead = RuntimeLiveness::known(true, t(9_001 - 10_801));
    assert!(matches!(
        stale_task_verdict(&state, dead, &policy(), t(9_001)),
        StaleVerdict::Fail { .. }
    ));
    // 时间没到 ⇒ 不管 runtime 死活都不动。
    assert!(stale_task_verdict(&state, RuntimeLiveness::Unbound, &policy(), t(100)).is_keep());
}

#[test]
fn waiting_local_directory_is_never_swept_by_wall_clock() {
    let mut state = dispatched_state(t(0));
    state
        .apply(
            TaskEvent::WaitingLocalDirectory {
                wait_reason: "/srv/repo".to_owned(),
            },
            t(0),
        )
        .unwrap();
    // 即使挂钟远超 dispatch/running 上限，本清扫也不碰它。
    assert!(
        stale_task_verdict(&state, RuntimeLiveness::Unbound, &policy(), t(1_000_000)).is_keep()
    );
    // 但 runtime 明确离线到超宽限时，`FailTasksForOfflineRuntimes` 会接管。
    let dead = RuntimeLiveness::known(false, t(1_000_000 - 20_000));
    assert_eq!(
        offline_runtime_verdict(&state, dead, &policy(), t(1_000_000)).failure(),
        Some((FailureReason::RuntimeOffline, RUNTIME_OFFLINE_MESSAGE))
    );
    // runtime 在线时该清扫不动它。
    assert!(offline_runtime_verdict(
        &state,
        RuntimeLiveness::known(true, t(1_000_000)),
        &policy(),
        t(1_000_000)
    )
    .is_keep());
}

#[test]
fn queued_expiry_needs_age_liveness_and_no_wakeup_gate() {
    let state = queued();
    // 心跳停在宽限之外（20_000 - 10_801 = 9_199）。
    let dead = RuntimeLiveness::known(true, t(9_199));
    let context = QueuedExpiryContext {
        created_at: t(0),
        wakeup_gated: false,
        runtime_offline_retry: false,
    };
    // 自身宽限没到 ⇒ 不动（哪怕 runtime 已死）。
    assert!(queued_expiry_verdict(&state, dead, &policy(), context, t(10_000)).is_keep());
    // 宽限到了 + 心跳超出宽限 ⇒ queued_expired。
    assert_eq!(
        queued_expiry_verdict(&state, dead, &policy(), context, t(20_000)).failure(),
        Some((FailureReason::QueuedExpired, QUEUED_EXPIRED_MESSAGE))
    );
    // 还在心跳的慢 runtime 只是忙 ⇒ 保住（MUL-6558）。
    let busy = RuntimeLiveness::known(true, t(19_999));
    assert!(queued_expiry_verdict(&state, busy, &policy(), context, t(20_000)).is_keep());
    // wakeup 门控的行不归它管。
    let gated = QueuedExpiryContext {
        wakeup_gated: true,
        ..context
    };
    assert!(queued_expiry_verdict(&state, dead, &policy(), gated, t(20_000)).is_keep());
    // runtime_offline 重试血脉有自己的出口。
    let retry_lineage = QueuedExpiryContext {
        runtime_offline_retry: true,
        ..context
    };
    assert!(queued_expiry_verdict(&state, dead, &policy(), retry_lineage, t(20_000)).is_keep());
}

#[test]
fn reconnect_retry_expires_after_one_full_grace() {
    let mut state = queued();
    state.status = TaskStatus::Deferred;
    state.fire_at = Some(t(0));
    let dead = RuntimeLiveness::known(false, t(20_000));
    assert!(reconnect_retry_verdict(&state, dead, &policy(), true, t(10_000)).is_keep());
    assert_eq!(
        reconnect_retry_verdict(&state, dead, &policy(), true, t(10_801)).failure(),
        Some((
            FailureReason::RuntimeReconnectTimeout,
            RECONNECT_TIMEOUT_MESSAGE
        ))
    );
    // runtime 抢在查询前恢复 ⇒ 继续 deferred。
    let back = RuntimeLiveness::known(true, t(10_800));
    assert!(reconnect_retry_verdict(&state, back, &policy(), true, t(10_801)).is_keep());
    // 血脉不对（父行不是 runtime_offline）⇒ 不归它管。
    assert!(reconnect_retry_verdict(&state, dead, &policy(), false, t(10_801)).is_keep());
    // 非 deferred 行不归它管。
    state.status = TaskStatus::Queued;
    assert!(reconnect_retry_verdict(&state, dead, &policy(), true, t(10_801)).is_keep());
}

#[test]
fn reclaim_requires_the_full_window_and_a_fresh_runtime() {
    let state = dispatched_state(t(0));
    let fresh = RuntimeLiveness::known(true, t(100));
    // 窗口没到。
    assert_eq!(
        reclaim_stale_dispatch(&state, fresh, &policy(), t(90)).unwrap_err(),
        TaskError::ReclaimWindowOpen {
            dispatched_at: t(0),
            claim_recovery_secs: 90,
        }
    );
    // 窗口到了 + 租约过期 + runtime 新鲜 ⇒ 可回收。
    assert_eq!(
        reclaim_stale_dispatch(&state, fresh, &policy(), t(91)).unwrap(),
        TaskEvent::Reclaim
    );
    // 租约还活着 ⇒ 拒绝（上游用例）。
    let mut leased = state.clone();
    leased.prepare_lease_expires_at = Some(t(200));
    assert_eq!(
        reclaim_stale_dispatch(&leased, fresh, &policy(), t(91)).unwrap_err(),
        TaskError::LeaseStillActive { expires_at: t(200) }
    );
    // runtime 不新鲜 ⇒ 拒绝。
    let stale_runtime = RuntimeLiveness::known(true, t(91 - 151));
    assert!(matches!(
        reclaim_stale_dispatch(&state, stale_runtime, &policy(), t(91)).unwrap_err(),
        TaskError::RuntimeNotEligible { .. }
    ));
    // 已开工 ⇒ 拒绝。
    let mut started = state;
    started.started_at = Some(t(10));
    assert_eq!(
        reclaim_stale_dispatch(&started, fresh, &policy(), t(91)).unwrap_err(),
        TaskError::AlreadyStarted { started_at: t(10) }
    );
}

#[test]
fn reclaim_is_refused_on_rows_that_are_not_dispatched() {
    let state = running_at(t(0));
    assert!(matches!(
        reclaim_stale_dispatch(
            &state,
            RuntimeLiveness::known(true, t(100)),
            &policy(),
            t(1_000)
        )
        .unwrap_err(),
        TaskError::IllegalTransition { .. }
    ));
}

#[test]
fn heartbeat_helpers_do_not_gate_on_online_for_the_reconnect_grace() {
    let offline_but_heartbeating = RuntimeLiveness::known(false, t(0));
    assert!(!offline_but_heartbeating.is_fresh(t(10), &policy()));
    assert!(offline_but_heartbeating.within_reconnect_grace(t(10), &policy()));
    assert!(!RuntimeLiveness::Unbound.within_reconnect_grace(t(10), &policy()));
    assert!(!RuntimeLiveness::Dangling.is_online());
    assert!(!RuntimeLiveness::Unbound.has_runtime_row());
    assert!(RuntimeLiveness::known(true, t(0)).has_runtime_row());
}
