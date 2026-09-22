//! `cancel` 的测试。

use super::*;
use mc_core::Timestamp;

fn t(secs: i64) -> Timestamp {
    Timestamp::from_unix(1_700_000_000 + secs)
}

fn user() -> Cancellation {
    Cancellation::by_user(Some(mc_core::Id::nil()), Some("louloulin".to_owned()))
}

fn ack(
    branch: Option<&str>,
    dir: Option<&str>,
    error: Option<&str>,
    reason: Option<&str>,
) -> CancelAck {
    CancelAck {
        branch_name: branch.map(str::to_owned),
        durable_work_dir: dir.map(str::to_owned),
        error_message: error.map(str::to_owned),
        failure_reason: reason.map(str::to_owned),
    }
}

fn target(status: TaskStatus) -> CancelAckTarget {
    CancelAckTarget {
        status,
        branch_name: None,
        durable_work_dir: None,
        error: None,
    }
}

// --- 取消 ---------------------------------------------------------------

#[test]
fn user_cancel_writes_the_three_columns_and_never_the_reason() {
    let state = running(t(0));
    let delivered = delivered_comments_plan(true, None, &[], false);
    let outcome = plan_cancellation(&state, &user(), delivered, t(10)).unwrap();

    let CancelOutcome::Applied {
        transition,
        acknowledges_delegated_failure,
        ..
    } = &outcome
    else {
        panic!("应当取消成功");
    };
    assert_eq!(transition.from, Some(TaskStatus::Running));
    assert_eq!(transition.to, TaskStatus::Cancelled);
    assert!(transition.status_changed);
    // 写集 = CancelAgentTaskByUser 的 SET 列表（不含 error/failure_reason）。
    assert_eq!(
        transition.writes,
        vec![
            ColumnWrite::CompletedAt(t(10)),
            ColumnWrite::CancelledBy(CancelledBy::User {
                id: Some(mc_core::Id::nil()),
                name: Some("louloulin".to_owned()),
            }),
            ColumnWrite::PrepareLeaseExpiresAt(None),
        ]
    );
    assert!(transition.writes_column(crate::state::Column::CompletedAt));
    assert!(!transition.writes_column(crate::state::Column::FailureReason));
    assert!(!transition.writes_column(crate::state::Column::ErrorMessage));
    assert!(
        acknowledges_delegated_failure,
        "人工取消 = 终态确认恢复信号"
    );
}

#[test]
fn cancellation_does_not_mutate_the_row_it_was_given() {
    let state = running(t(0));
    let _ = plan_cancellation(&state, &user(), DeliveredCommentsPlan::KeepUnchanged, t(9)).unwrap();
    assert_eq!(state.status, TaskStatus::Running);
    assert_eq!(state.completed_at, None);
    assert_eq!(state.cancelled_by, None);
}

#[test]
fn system_cancel_with_reason_persists_the_explanation() {
    let state = queued();
    let cancellation = Cancellation::by_system_with_reason(
        "  daemon 过旧，worktree 声明被拒 ",
        FailureReason::Timeout,
    )
    .unwrap();
    let outcome = plan_cancellation(
        &state,
        &cancellation,
        DeliveredCommentsPlan::Untouched,
        t(5),
    )
    .unwrap();
    let CancelOutcome::Applied { transition, .. } = &outcome else {
        panic!("应当取消成功");
    };
    assert_eq!(
        transition.writes.last(),
        Some(&ColumnWrite::FailureReason(FailureReason::Timeout))
    );
    assert!(transition.writes_column(crate::state::Column::ErrorMessage));
    assert!(transition.writes_column(crate::state::Column::CancelledBy));
    // 解释被 trim 过。
    assert!(transition.writes.contains(&ColumnWrite::ErrorMessage(Some(
        "daemon 过旧，worktree 声明被拒".to_owned()
    ))));
}

#[test]
fn blank_system_explanation_is_rejected() {
    assert_eq!(
        Cancellation::by_system_with_reason("   ", FailureReason::Timeout).unwrap_err(),
        TaskError::MalformedEvent {
            detail: "系统取消的解释不能是空白（CancelAgentTaskWithReason 的 error 列）"
        }
    );
    // 绕过构造器也要被 plan 拦住。
    let mut cancellation = Cancellation::by_system();
    cancellation.explanation = Some(CancelExplanation {
        message: "  ".to_owned(),
        reason: FailureReason::Timeout,
    });
    assert!(plan_cancellation(
        &queued(),
        &cancellation,
        DeliveredCommentsPlan::Untouched,
        t(0)
    )
    .is_err());
}

#[test]
fn user_cancel_with_explanation_is_rejected() {
    let mut cancellation = user();
    cancellation.explanation = Some(CancelExplanation {
        message: "用户不该带理由".to_owned(),
        reason: FailureReason::Timeout,
    });
    assert_eq!(
        plan_cancellation(
            &queued(),
            &cancellation,
            DeliveredCommentsPlan::KeepUnchanged,
            t(0)
        )
        .unwrap_err(),
        TaskError::MalformedEvent {
            detail: "人工取消不写 error/failure_reason（CancelAgentTaskByUser 的注释）"
        }
    );
}

#[test]
fn cancelling_a_terminal_row_writes_nothing_and_is_not_an_error() {
    let mut state = running(t(0));
    state.apply(TaskEvent::Completed, t(5)).unwrap();

    let outcome =
        plan_cancellation(&state, &user(), DeliveredCommentsPlan::KeepUnchanged, t(9)).unwrap();
    assert_eq!(
        outcome,
        CancelOutcome::AlreadyTerminal {
            status: TaskStatus::Completed
        }
    );
    assert!(!outcome.is_applied());
    assert!(
        !outcome.is_idempotent_replay(),
        "已完成的取消是冲突，不是重放"
    );
    assert!(outcome.writes().is_empty());
}

#[test]
fn double_cancel_is_an_idempotent_replay() {
    let mut state = running(t(0));
    let first =
        plan_cancellation(&state, &user(), DeliveredCommentsPlan::KeepUnchanged, t(5)).unwrap();
    assert!(first.is_applied());

    state
        .apply(
            TaskEvent::Cancelled {
                by: CancelledBy::System,
            },
            t(6),
        )
        .unwrap();
    let second =
        plan_cancellation(&state, &user(), DeliveredCommentsPlan::KeepUnchanged, t(7)).unwrap();
    assert!(second.is_idempotent_replay(), "重复取消要能被当成功处理");
    assert!(second.writes().is_empty(), "第二次不能再写列");
}

#[test]
fn every_non_terminal_status_is_cancellable() {
    for (at, status) in TaskStatus::ALL.into_iter().enumerate() {
        let mut state = queued();
        state.status = status;
        let outcome = plan_cancellation(
            &state,
            &user(),
            DeliveredCommentsPlan::KeepUnchanged,
            t(i64::try_from(at).expect("状态数小")),
        )
        .unwrap();
        if status.is_terminal() {
            assert_eq!(
                outcome,
                CancelOutcome::AlreadyTerminal { status },
                "{status} 是终态"
            );
        } else {
            assert!(
                outcome.is_applied(),
                "{status} 必须可取消（CancelAgentTask 的 5 状态集合）"
            );
            assert!(status.is_cancellable());
        }
    }
}

// --- delivered_comment_ids 分支 ------------------------------------------

#[test]
fn system_cancellation_never_touches_delivered_comments() {
    let id = mc_core::Id::nil();
    assert_eq!(
        delivered_comments_plan(false, Some(id), &[id], true),
        DeliveredCommentsPlan::Untouched
    );
}

#[test]
fn cheap_shape_probe_short_circuits_the_join() {
    // 没有 trigger_comment_id 也没有合并评论 ⇒ 连探测结果都不看。
    assert_eq!(
        delivered_comments_plan(true, None, &[], true),
        DeliveredCommentsPlan::KeepUnchanged
    );
}

#[test]
fn recovery_signal_only_forces_recompute_when_the_probe_hits() {
    let id = mc_core::Id::nil();
    assert_eq!(
        delivered_comments_plan(true, Some(id), &[id], false),
        DeliveredCommentsPlan::KeepUnchanged
    );
    assert_eq!(
        delivered_comments_plan(true, Some(id), &[id], true),
        DeliveredCommentsPlan::RecomputeRecoverySignalReceipts
    );
    assert_eq!(
        delivered_comments_plan(true, None, &[id], true),
        DeliveredCommentsPlan::RecomputeRecoverySignalReceipts
    );
}

// --- cancel-ack ---------------------------------------------------------

#[test]
fn ack_writes_durable_dir_branch_and_error_in_upstream_order() {
    let plan = plan_cancel_ack(
        &ack(
            Some("agent/fix-1"),
            Some("/work/dir"),
            Some("Finalize 中止"),
            Some("timeout"),
        ),
        &target(TaskStatus::Cancelled),
    );
    assert_eq!(
        plan.writes,
        vec![
            CancelAckWrite::DurableWorkDir("/work/dir".to_owned()),
            CancelAckWrite::BranchName("agent/fix-1".to_owned()),
            CancelAckWrite::Error {
                message: "Finalize 中止".to_owned(),
                failure_reason: Some("timeout".to_owned()),
            },
        ]
    );
    assert!(plan.rebroadcast);
    assert!(plan.writes_column(CancelAckColumn::Error));
}

#[test]
fn empty_ack_is_a_noop_even_on_a_cancelled_row() {
    let plan = plan_cancel_ack(&CancelAck::empty(), &target(TaskStatus::Cancelled));
    assert!(plan.is_noop());
    assert!(!plan.rebroadcast);
    assert!(!CancelAck::empty().has_payload());
}

#[test]
fn whitespace_only_fields_are_not_delivered() {
    let plan = plan_cancel_ack(
        &ack(Some("  "), Some("\t"), Some(" \n "), Some("timeout")),
        &target(TaskStatus::Cancelled),
    );
    assert!(plan.is_noop(), "trim 后为空的字段等于没带");
    assert!(ack(Some("  "), None, Some("x"), None).has_payload());
}

#[test]
fn ack_is_refused_on_every_non_cancelled_status() {
    for status in TaskStatus::ALL {
        let plan = plan_cancel_ack(
            &ack(Some("agent/x"), Some("/w"), Some("boom"), None),
            &target(status),
        );
        if status == TaskStatus::Cancelled {
            assert!(!plan.is_noop(), "只有 cancelled 行接受 ack");
        } else {
            assert!(plan.is_noop(), "{status} 行必须拒掉迟到的 ack");
            assert!(!plan.rebroadcast);
        }
    }
}

#[test]
fn ack_never_overwrites_existing_values() {
    let existing = CancelAckTarget {
        status: TaskStatus::Cancelled,
        branch_name: Some("agent/old".to_owned()),
        durable_work_dir: Some("/old/dir".to_owned()),
        error: Some("旧的 error".to_owned()),
    };
    let plan = plan_cancel_ack(
        &ack(
            Some("agent/new"),
            Some("/new/dir"),
            Some("新的 error"),
            Some("timeout"),
        ),
        &existing,
    );
    assert!(
        plan.is_noop(),
        "COALESCE + (error IS NULL OR error='') 都不许覆盖"
    );
}

#[test]
fn ack_replay_is_idempotent() {
    let first = plan_cancel_ack(
        &ack(Some("agent/x"), Some("/w"), Some("boom"), Some("timeout")),
        &target(TaskStatus::Cancelled),
    );
    assert!(first.rebroadcast);

    // 第一次写完之后再 ack 一次：现值都有了 ⇒ 一条都不写、也不重播。
    let after = plan_cancel_ack(
        &ack(Some("agent/x"), Some("/w"), Some("boom"), Some("timeout")),
        &CancelAckTarget {
            status: TaskStatus::Cancelled,
            branch_name: Some("agent/x".to_owned()),
            durable_work_dir: Some("/w".to_owned()),
            error: Some("boom".to_owned()),
        },
    );
    assert!(after.is_noop());
    assert!(!after.rebroadcast, "没写任何列就不该重播 task:cancelled");
}

#[test]
fn error_write_is_all_or_nothing_like_the_sql_predicate() {
    // error 已存在 ⇒ 连 failure_reason 也不写（同一条 UPDATE 的条件）。
    let target = CancelAckTarget {
        status: TaskStatus::Cancelled,
        branch_name: None,
        durable_work_dir: None,
        error: Some("已有 error".to_owned()),
    };
    let plan = plan_cancel_ack(&ack(None, None, Some("新 error"), Some("timeout")), &target);
    assert!(plan.is_noop());

    // 空串算「没写过」（上游是 `(error IS NULL OR error = '')`）。
    let empty = CancelAckTarget {
        error: Some(String::new()),
        ..target.clone()
    };
    assert!(empty.error_is_unset());
    let plan = plan_cancel_ack(&ack(None, None, Some("新 error"), None), &empty);
    assert_eq!(
        plan.writes,
        vec![CancelAckWrite::Error {
            message: "新 error".to_owned(),
            failure_reason: None,
        }]
    );
}

#[test]
fn error_only_ack_still_rebroadcasts() {
    let plan = plan_cancel_ack(
        &ack(None, None, Some("boom"), Some("timeout")),
        &target(TaskStatus::Cancelled),
    );
    assert!(plan.rebroadcast);
    assert!(!plan.is_noop());
    assert!(plan.writes_column(CancelAckColumn::Error));
    assert!(!plan.writes_column(CancelAckColumn::BranchName));
    assert!(!plan.writes_column(CancelAckColumn::DurableWorkDir));
}

#[test]
fn ack_target_reads_error_from_the_state() {
    let mut state = queued();
    state
        .apply(
            TaskEvent::Cancelled {
                by: CancelledBy::System,
            },
            t(0),
        )
        .unwrap();
    let target = CancelAckTarget::from_state(&state, Some("agent/x".to_owned()), None);
    assert!(target.is_cancelled());
    assert!(target.error_is_unset(), "取消本身不写 error");
    assert_eq!(target.branch_name.as_deref(), Some("agent/x"));
}

#[test]
fn ack_failure_reason_is_normalized_but_not_validated() {
    let padded = ack(None, None, Some("boom"), Some("  runtime_offline "));
    assert_eq!(
        normalize_ack_failure_reason(&padded),
        Some("runtime_offline")
    );
    let blank = ack(None, None, Some("boom"), Some("   "));
    assert_eq!(normalize_ack_failure_reason(&blank), None);
    // 自由文本不做枚举校验（上游亦然）。
    let odd = ack(None, None, Some("boom"), Some("daemon 自造原因"));
    assert_eq!(normalize_ack_failure_reason(&odd), Some("daemon 自造原因"));
}

#[test]
fn cancel_ack_serde_round_trips_and_skips_empty_fields() {
    let ack = ack(Some("agent/x"), None, None, None);
    let json = serde_json::to_string(&ack).unwrap();
    assert_eq!(json, r#"{"branch_name":"agent/x"}"#);
    assert_eq!(serde_json::from_str::<CancelAck>(&json).unwrap(), ack);
    assert_eq!(
        serde_json::from_str::<CancelAck>("{}").unwrap(),
        CancelAck::empty()
    );
}
