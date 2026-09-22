//! 状态机矩阵用例：96 个 (事件 × 源状态) 组合逐条断言，34 条合法边 / 62 条非法边。

use super::*;
use crate::retry::{FailureReason, RetryBudget, RetryChild, DEFAULT_MAX_ATTEMPTS};
use mc_core::{Id, Timestamp};

fn at() -> Timestamp {
    Timestamp::from_unix(1_700_000_000)
}

const REASON: &str = "/srv/repos/contested-dir";

fn waiting_event() -> TaskEvent {
    TaskEvent::WaitingLocalDirectory {
        wait_reason: REASON.to_owned(),
    }
}

/// 造一个处于 `status` 的**合法**状态：一律从构造器沿合法边走到位。
fn state_in(status: TaskStatus) -> TaskState {
    if status == TaskStatus::Deferred {
        let (deferred, _) = TaskState::defer(RetryBudget::FIRST_RUN, None);
        return deferred;
    }
    let steps: &[TaskEvent] = match status {
        TaskStatus::Queued | TaskStatus::Deferred => &[],
        TaskStatus::Dispatched => &[TaskEvent::Dispatch],
        TaskStatus::Running => &[TaskEvent::Dispatch, TaskEvent::Running],
        TaskStatus::WaitingLocalDirectory => &[TaskEvent::Dispatch, waiting_event()],
        TaskStatus::Completed => &[
            TaskEvent::Dispatch,
            TaskEvent::Running,
            TaskEvent::Completed,
        ],
        TaskStatus::Failed => &[
            TaskEvent::Dispatch,
            TaskEvent::Running,
            TaskEvent::Failed {
                reason: FailureReason::Timeout,
                message: None,
            },
        ],
        TaskStatus::Cancelled => &[TaskEvent::Cancelled {
            by: CancelledBy::System,
        }],
    };
    let (mut state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    for event in steps {
        state.apply(event.clone(), at()).expect("夹具路径必须合法");
    }
    assert_eq!(state.status, status, "夹具没把状态走到位");
    state
}

fn event_of(kind: TaskEventKind) -> TaskEvent {
    match kind {
        TaskEventKind::Queued => TaskEvent::Queued,
        TaskEventKind::Dispatch => TaskEvent::Dispatch,
        TaskEventKind::Running => TaskEvent::Running,
        TaskEventKind::WaitingLocalDirectory => waiting_event(),
        TaskEventKind::Progress => TaskEvent::Progress,
        TaskEventKind::Message => TaskEvent::Message,
        TaskEventKind::Completed => TaskEvent::Completed,
        TaskEventKind::Failed => TaskEvent::Failed {
            reason: FailureReason::Timeout,
            message: Some("boom".to_owned()),
        },
        TaskEventKind::Cancelled => TaskEvent::Cancelled {
            by: CancelledBy::System,
        },
        TaskEventKind::Deferred => TaskEvent::Deferred,
        TaskEventKind::Reclaim => TaskEvent::Reclaim,
        TaskEventKind::RequeueAfterClaimFailure => TaskEvent::RequeueAfterClaimFailure,
    }
}

/// 权威转移表：(事件, 源状态) → 目标状态。合法边之外的组合**必须** `Err`。
const LEGAL_EDGES: &[(TaskEventKind, TaskStatus, TaskStatus)] = &[
    (
        TaskEventKind::Queued,
        TaskStatus::Deferred,
        TaskStatus::Queued,
    ),
    (
        TaskEventKind::Dispatch,
        TaskStatus::Queued,
        TaskStatus::Dispatched,
    ),
    (
        TaskEventKind::Running,
        TaskStatus::Dispatched,
        TaskStatus::Running,
    ),
    (
        TaskEventKind::Running,
        TaskStatus::WaitingLocalDirectory,
        TaskStatus::Running,
    ),
    (
        TaskEventKind::WaitingLocalDirectory,
        TaskStatus::Dispatched,
        TaskStatus::WaitingLocalDirectory,
    ),
    (
        TaskEventKind::Completed,
        TaskStatus::Running,
        TaskStatus::Completed,
    ),
    // events.go 只标注 running → failed；SQL 的 WHERE 更宽（见 state.rs 模块文档）。
    (
        TaskEventKind::Failed,
        TaskStatus::Queued,
        TaskStatus::Failed,
    ),
    (
        TaskEventKind::Failed,
        TaskStatus::Dispatched,
        TaskStatus::Failed,
    ),
    (
        TaskEventKind::Failed,
        TaskStatus::Running,
        TaskStatus::Failed,
    ),
    (
        TaskEventKind::Failed,
        TaskStatus::WaitingLocalDirectory,
        TaskStatus::Failed,
    ),
    (
        TaskEventKind::Failed,
        TaskStatus::Deferred,
        TaskStatus::Failed,
    ),
    // * → cancelled（全部非终态）。
    (
        TaskEventKind::Cancelled,
        TaskStatus::Queued,
        TaskStatus::Cancelled,
    ),
    (
        TaskEventKind::Cancelled,
        TaskStatus::Dispatched,
        TaskStatus::Cancelled,
    ),
    (
        TaskEventKind::Cancelled,
        TaskStatus::Running,
        TaskStatus::Cancelled,
    ),
    (
        TaskEventKind::Cancelled,
        TaskStatus::WaitingLocalDirectory,
        TaskStatus::Cancelled,
    ),
    (
        TaskEventKind::Cancelled,
        TaskStatus::Deferred,
        TaskStatus::Cancelled,
    ),
    // 通知类：8 个状态全部接受，状态不变。
    (
        TaskEventKind::Progress,
        TaskStatus::Queued,
        TaskStatus::Queued,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::Dispatched,
        TaskStatus::Dispatched,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::Running,
        TaskStatus::Running,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::WaitingLocalDirectory,
        TaskStatus::WaitingLocalDirectory,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::Completed,
        TaskStatus::Completed,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::Failed,
        TaskStatus::Failed,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::Cancelled,
        TaskStatus::Cancelled,
    ),
    (
        TaskEventKind::Progress,
        TaskStatus::Deferred,
        TaskStatus::Deferred,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Queued,
        TaskStatus::Queued,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Dispatched,
        TaskStatus::Dispatched,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Running,
        TaskStatus::Running,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::WaitingLocalDirectory,
        TaskStatus::WaitingLocalDirectory,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Completed,
        TaskStatus::Completed,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Failed,
        TaskStatus::Failed,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Cancelled,
        TaskStatus::Cancelled,
    ),
    (
        TaskEventKind::Message,
        TaskStatus::Deferred,
        TaskStatus::Deferred,
    ),
    // 服务端内部边（无 task: 常量）。
    (
        TaskEventKind::Reclaim,
        TaskStatus::Dispatched,
        TaskStatus::Dispatched,
    ),
    (
        TaskEventKind::RequeueAfterClaimFailure,
        TaskStatus::Dispatched,
        TaskStatus::Queued,
    ),
];

#[test]
fn transition_matrix_is_exhaustive_and_exact() {
    let mut legal_seen = 0_u32;
    for kind in TaskEventKind::ALL {
        for status in TaskStatus::ALL {
            let mut state = state_in(status);
            let result = state.apply(event_of(kind), at());
            let expected = LEGAL_EDGES
                .iter()
                .find(|(k, from, _)| *k == kind && *from == status);

            match (result, expected) {
                (Ok(transition), Some((_, _, to))) => {
                    legal_seen += 1;
                    assert_eq!(transition.from, Some(status));
                    assert_eq!(state.status, *to, "{kind:?} on {status} 目标状态不对");
                    assert_eq!(transition.to, *to);
                    assert_eq!(
                        transition.status_changed,
                        status != *to,
                        "{kind:?} on {status} 的 status_changed 不对"
                    );
                }
                (Err(TaskError::TerminalState { status: s, .. }), None) => {
                    assert!(
                        status.is_terminal(),
                        "{kind:?} 在非终态 {status} 上报了 TerminalState"
                    );
                    assert_eq!(s, status);
                }
                (Err(TaskError::IllegalTransition { from, .. }), None) => {
                    assert_eq!(from, status);
                }
                (Err(other), Some(_)) => panic!("{kind:?} on {status} 本应合法却报错：{other}"),
                (Err(other), None) => panic!("{kind:?} on {status} 出了意外错误：{other:?}"),
                (Ok(_), None) => panic!("{kind:?} on {status} 本应被拒绝，却成功了"),
            }
        }
    }
    assert_eq!(
        legal_seen,
        u32::try_from(LEGAL_EDGES.len()).unwrap(),
        "合法边没有被逐条走完"
    );
    assert_eq!(
        legal_seen, 34,
        "12 事件 × 8 状态 = 96 组合中应恰好 34 条合法"
    );
}

#[test]
fn terminal_states_are_absorbing_for_status_changing_events() {
    for terminal in TaskStatus::TERMINAL {
        for kind in TaskEventKind::ALL {
            if !kind.changes_status() || kind.is_creation_only() {
                continue;
            }
            let mut state = state_in(terminal);
            let err = state.apply(event_of(kind), at()).unwrap_err();
            assert_eq!(
                err,
                TaskError::TerminalState {
                    status: terminal,
                    event: event_of(kind),
                },
                "{kind:?} 在终态 {terminal} 上没有被吸收"
            );
            assert_eq!(state.status, terminal, "被拒的事件不得改动状态");
        }
    }
}

#[test]
fn terminal_states_still_accept_progress_notifications() {
    // Progress/Message 不是转移，是通知 —— 终态收到它们也必须无害。
    for terminal in TaskStatus::TERMINAL {
        let mut state = state_in(terminal);
        for event in [TaskEvent::Progress, TaskEvent::Message] {
            let transition = state.apply(event, at()).unwrap();
            assert!(!transition.status_changed);
            assert!(transition.writes.is_empty());
            assert_eq!(state.status, terminal);
        }
    }
}

#[test]
fn wire_event_names_are_the_nine_upstream_constants() {
    let upstream = [
        "task:queued",
        "task:dispatch",
        "task:running",
        "task:waiting_local_directory",
        "task:progress",
        "task:completed",
        "task:failed",
        "task:message",
        "task:cancelled",
    ];
    let ours: Vec<&str> = TaskEventKind::WIRE_EVENTS
        .iter()
        .map(|kind| kind.wire_name().expect("线上事件必须有名字"))
        .collect();
    assert_eq!(ours, upstream.to_vec());
    // 内部事件绝不自造线上名（上游没有对应的 task: 常量）。
    for kind in [
        TaskEventKind::Deferred,
        TaskEventKind::Reclaim,
        TaskEventKind::RequeueAfterClaimFailure,
    ] {
        assert!(!kind.is_wire());
        assert!(kind.wire_name().is_none());
    }
}

#[test]
fn event_serialization_uses_the_wire_names() {
    for kind in TaskEventKind::WIRE_EVENTS {
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{}\"", kind.wire_name().unwrap())
        );
    }
}

#[test]
fn happy_path_lands_every_state_with_consistent_columns() {
    let state = state_in(TaskStatus::Running);
    assert_eq!(state.started_at, Some(at()));
    assert_eq!(state.dispatched_at, Some(at()));
    // StartAgentTask 同时清 wait_reason 与 prepare lease。
    assert_eq!(state.prepare_lease_expires_at, None);
    assert_eq!(state.wait_reason, None);

    let completed = state_in(TaskStatus::Completed);
    assert_eq!(completed.completed_at, Some(at()));

    let mut waiting = state_in(TaskStatus::WaitingLocalDirectory);
    assert_eq!(waiting.wait_reason.as_deref(), Some(REASON));
    let transition = waiting.apply(TaskEvent::Running, at()).unwrap();
    assert_eq!(waiting.wait_reason, None);
    assert!(transition.writes_column(Column::WaitReason));
    assert!(transition.writes_column(Column::PrepareLeaseExpiresAt));

    // 只有 dispatched → waiting 合法。
    let mut already_waiting = state_in(TaskStatus::WaitingLocalDirectory);
    assert!(already_waiting.apply(waiting_event(), at()).is_err());
}

#[test]
fn failed_transition_records_reason_and_clears_lease() {
    let mut state = state_in(TaskStatus::Running);
    let transition = state
        .apply(
            TaskEvent::Failed {
                reason: FailureReason::RuntimeRecovery,
                message: Some("daemon restarted while task was in flight".to_owned()),
            },
            at(),
        )
        .unwrap();
    let failure = state.failure.clone().unwrap();
    assert_eq!(failure.reason, FailureReason::RuntimeRecovery);
    assert_eq!(
        failure.message.as_deref(),
        Some("daemon restarted while task was in flight")
    );
    assert_eq!(state.completed_at, Some(at()));
    assert_eq!(state.prepare_lease_expires_at, None);
    for column in [
        Column::CompletedAt,
        Column::FailureReason,
        Column::ErrorMessage,
        Column::WaitReason,
        Column::PrepareLeaseExpiresAt,
    ] {
        assert!(transition.writes_column(column), "缺少 {column:?} 写入");
    }
}

#[test]
fn manual_is_rejected_as_a_failure_reason() {
    let mut state = state_in(TaskStatus::Running);
    let err = state
        .apply(
            TaskEvent::Failed {
                reason: FailureReason::Manual,
                message: None,
            },
            at(),
        )
        .unwrap_err();
    assert!(matches!(err, TaskError::MalformedEvent { .. }));
    assert_eq!(state.status, TaskStatus::Running, "被拒的事件不得改动状态");
}

#[test]
fn empty_wait_reason_is_rejected() {
    let mut state = state_in(TaskStatus::Dispatched);
    for bad in ["", "   "] {
        let err = state
            .apply(
                TaskEvent::WaitingLocalDirectory {
                    wait_reason: bad.to_owned(),
                },
                at(),
            )
            .unwrap_err();
        assert!(matches!(err, TaskError::MalformedEvent { .. }));
    }
    assert_eq!(state.status, TaskStatus::Dispatched);
}

#[test]
fn deferred_events_are_creation_only() {
    for status in TaskStatus::ALL {
        if status.is_terminal() {
            // 终态连创建事件也拒（吸收态检查在前）。
            continue;
        }
        let mut state = state_in(status);
        assert_eq!(
            state.apply(TaskEvent::Deferred, at()).unwrap_err(),
            TaskError::IllegalTransition {
                from: status,
                event: TaskEvent::Deferred,
            }
        );
    }
}

#[test]
fn deferred_promotion_clears_fire_at_and_lease() {
    let fire_at = Timestamp::from_unix(at().as_unix() - 1);
    let (mut state, creation) = TaskState::defer(RetryBudget::FIRST_RUN, Some(fire_at));
    assert_eq!(creation.from, None);
    assert_eq!(creation.to, TaskStatus::Deferred);
    assert!(creation.writes_column(Column::FireAt));
    assert!(state.is_promotable(at()), "fire_at 已到应当可提升");

    let future = Timestamp::from_unix(at().as_unix() + 100);
    let (not_yet, _) = TaskState::defer(RetryBudget::FIRST_RUN, Some(future));
    assert!(!not_yet.is_promotable(at()), "fire_at 未到不得提升");
    assert!(TaskState::defer(RetryBudget::FIRST_RUN, None)
        .0
        .is_promotable(at()));

    let transition = state.apply(TaskEvent::Queued, at()).unwrap();
    assert_eq!(state.status, TaskStatus::Queued);
    assert_eq!(state.fire_at, None);
    assert!(transition.writes_column(Column::FireAt));
    assert!(transition.writes_column(Column::PrepareLeaseExpiresAt));
}

#[test]
fn reclaim_is_a_self_transition_and_refuses_started_rows() {
    let mut state = state_in(TaskStatus::Dispatched);
    let transition = state.apply(TaskEvent::Reclaim, at()).unwrap();
    assert!(
        !transition.status_changed,
        "dispatched → dispatched 不算状态变化"
    );
    assert_eq!(transition.from, Some(TaskStatus::Dispatched));
    assert_eq!(transition.to, TaskStatus::Dispatched);
    assert!(transition.writes_column(Column::DispatchedAt));
    assert_eq!(state.dispatched_at, Some(at()));

    // 已经开工过的行不能被重投递回收（上游只回收 started_at IS NULL）。
    let mut started = state_in(TaskStatus::Dispatched);
    started.started_at = Some(Timestamp::from_unix(1));
    let err = started.apply(TaskEvent::Reclaim, at()).unwrap_err();
    assert!(matches!(err, TaskError::AlreadyStarted { .. }));
}

#[test]
fn requeue_after_claim_failure_clears_the_dispatch_generation() {
    let mut state = state_in(TaskStatus::Dispatched);
    let transition = state
        .apply(TaskEvent::RequeueAfterClaimFailure, at())
        .unwrap();
    assert_eq!(state.status, TaskStatus::Queued);
    assert_eq!(state.dispatched_at, None);
    assert_eq!(state.prepare_lease_expires_at, None);
    assert_eq!(transition.from, Some(TaskStatus::Dispatched));
    assert!(transition.writes_column(Column::DispatchedAt));
    assert!(transition.status_changed);
}

#[test]
fn cancel_records_the_cancelled_by_columns() {
    let mut state = state_in(TaskStatus::Dispatched);
    let user = CancelledBy::User {
        id: Some(Id::new()),
        name: Some("louloulin".to_owned()),
    };
    let transition = state
        .apply(TaskEvent::Cancelled { by: user }, at())
        .unwrap();
    let recorded = state.cancelled_by.clone().unwrap();
    assert_eq!(recorded.type_str(), "user");
    assert_eq!(recorded.name(), Some("louloulin"));
    assert!(recorded.id().is_some());
    assert!(recorded.is_user());
    assert_eq!(state.completed_at, Some(at()));
    assert!(transition.writes_column(Column::CancelledBy));

    assert_eq!(CancelledBy::System.type_str(), "system");
    assert!(!CancelledBy::System.is_user());
    assert_eq!(CancelledBy::System.name(), None);
    assert_eq!(CancelledBy::System.id(), None);
}

#[test]
fn cancel_is_idempotent_at_the_domain_level() {
    // 终态吸收：重复取消不会二次写 cancelled_by（仓储层的幂等由 affected_rows = 0 表达）。
    let mut state = state_in(TaskStatus::Cancelled);
    let before = state.clone();
    let err = state
        .apply(
            TaskEvent::Cancelled {
                by: CancelledBy::User {
                    id: None,
                    name: None,
                },
            },
            at(),
        )
        .unwrap_err();
    assert!(matches!(err, TaskError::TerminalState { .. }));
    assert_eq!(state, before);
}

#[test]
fn delegated_failure_is_a_subclass_of_failed_not_a_status() {
    let mut state = state_in(TaskStatus::Failed);
    assert!(!state.is_delegated_failure());
    state.delegated_from_task_id = Some(Id::new());
    assert!(state.is_delegated_failure());
    assert_eq!(state.status, TaskStatus::Failed, "子类不改变状态列");
    assert!(!state.is_escalation_failure());
    state.escalation_for_task_id = Some(Id::new());
    assert!(state.is_escalation_failure());
    // 本地 CHECK 的 delegated_failure 不是上游状态值。
    assert!(TaskStatus::parse("delegated_failure").is_err());
}

#[test]
fn retry_child_creation_carries_parent_and_consistent_budget() {
    let parent = Id::new();
    let child = RetryChild {
        attempt: 2,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
        delay_secs: 0,
        status: TaskStatus::Queued,
        force_fresh_session: true,
    };
    let (state, transition) = TaskState::from_retry_child(child, parent, None);
    assert_eq!(state.status, TaskStatus::Queued);
    assert_eq!(state.parent_task_id, Some(parent));
    assert_eq!(state.budget.attempt, 2);
    assert_eq!(state.budget.max_attempts, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(transition.from, None);
    assert_eq!(state.budget, RetryBudget::new(2, DEFAULT_MAX_ATTEMPTS));

    let deferred = RetryChild {
        attempt: 2,
        max_attempts: 2,
        delay_secs: 1,
        status: TaskStatus::Deferred,
        force_fresh_session: false,
    };
    let fire_at = Some(Timestamp::from_unix(at().as_unix() + 1));
    let (state, _) = TaskState::from_retry_child(deferred, parent, fire_at);
    assert_eq!(state.status, TaskStatus::Deferred);
    assert_eq!(state.fire_at, fire_at);
    assert_eq!(state.parent_task_id, Some(parent));
}

#[test]
fn serialization_slot_membership_matches_the_partial_unique_index() {
    // 022 的部分唯一索引覆盖 `queued|dispatched`（037 收窄到 (issue, agent)）。
    for status in TaskStatus::ALL {
        let state = state_in(status);
        let expected = matches!(status, TaskStatus::Queued | TaskStatus::Dispatched);
        assert_eq!(
            state.occupies_serialization_slot(),
            expected,
            "{status} 的槽位归属不对"
        );
    }
}
