use super::super::store::PgTaskStore;
use super::*;
use mc_core::{Id, Timestamp};
use mc_task::cancel::{CancelAck, CancelAckColumn, CancelOutcome, Cancellation};
use mc_task::error::TaskError;
use mc_task::lease::StalePolicy;
use mc_task::retry::{FailureReason, RetryBudget};
use mc_task::state::{ColumnWrite, TaskEventKind, TaskState, TaskTransition};
use mc_task::status::TaskStatus;
use mc_task::store::{ClaimRequest, CommitOutcome, TaskStore};
// ---------------------------------------------------------------------------
// 生命周期：入队 / 认领 / 取消 / 存储端口

// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn enqueue_claim_and_complete_round_trip() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let new = issue_task(&fx, None);
    let task_id = new.id;

    let row = repo.create_task(&new).await.expect("create_task");
    assert_eq!(row.task_status().unwrap(), TaskStatus::Queued);
    assert_eq!(row.attempt, 1);
    assert_eq!(row.max_attempts, 2);
    assert_eq!(row.priority, 0);

    let claim = repo
        .claim_next_task(&ClaimRequest::new(
            fx.agent_id,
            fx.runtime_id,
            Timestamp::now(),
            StalePolicy::UPSTREAM_DEFAULT,
        ))
        .await
        .expect("claim")
        .expect("claimed");
    assert_eq!(claim.task_id, task_id);
    assert_eq!(claim.state.status, TaskStatus::Dispatched);
    assert_eq!(claim.transition.from, Some(TaskStatus::Queued));
    assert!(claim.state.prepare_lease_expires_at.is_some());

    let applied = repo
        .commit_transition(
            task_id,
            &TaskTransition {
                from: Some(TaskStatus::Dispatched),
                to: TaskStatus::Running,
                event: TaskEventKind::Running,
                status_changed: true,
                writes: running_writes(),
            },
        )
        .await
        .expect("running");
    assert_eq!(applied, CommitOutcome::Applied);

    let applied = repo
        .commit_transition(
            task_id,
            &TaskTransition {
                from: Some(TaskStatus::Running),
                to: TaskStatus::Completed,
                event: TaskEventKind::Completed,
                status_changed: true,
                writes: vec![ColumnWrite::CompletedAt(Timestamp::now())],
            },
        )
        .await
        .expect("completed");
    assert_eq!(applied, CommitOutcome::Applied);

    let state = repo.read_state(task_id).await.unwrap().unwrap();
    assert_eq!(state.status, TaskStatus::Completed);
    // 第二次认领不该再拿到它。
    assert!(repo
        .claim_next_task(&ClaimRequest::new(
            fx.agent_id,
            fx.runtime_id,
            Timestamp::now(),
            StalePolicy::UPSTREAM_DEFAULT,
        ))
        .await
        .unwrap()
        .is_none());

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 2. 活的线程级部分唯一索引
// ---------------------------------------------------------------------------

/// 上游迁移 `037` 删掉了 `idx_one_pending_task_per_issue`，现在生效的是
/// `idx_one_pending_task_per_issue_agent_thread`（迁移 `452`）。这条测试既断言
/// 索引名，也真的把它撞红一次。
#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn live_thread_unique_index_rejects_second_pending_task() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);

    let unique: bool = sqlx::query_scalar(
        "SELECT indisunique FROM pg_index WHERE indexrelid = \
         'idx_one_pending_task_per_issue_agent_thread'::regclass",
    )
    .fetch_one(fx.db.pool())
    .await
    .expect("live thread index exists");
    assert!(unique, "线程级唯一索引必须是 UNIQUE");
    let stale: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class WHERE relname = 'idx_one_pending_task_per_issue'",
    )
    .fetch_one(fx.db.pool())
    .await
    .unwrap();
    assert_eq!(stale, 0, "上游 037 已删除该索引，本地不应复活它");

    let plain = repo.create_task(&issue_task(&fx, None)).await.unwrap();
    assert!(
        plain.comment_thread_id.is_none(),
        "既无 wakeup_id 也无 trigger_comment_id 时线程作用域为空"
    );
    // 同 (issue, agent, 无 thread) 的第二条未决任务必须被数据库拒绝。
    match repo.create_task(&issue_task(&fx, None)).await {
        Err(TaskError::Conflict { .. }) => {}
        other => panic!("期望 Conflict，实际 {other:?}"),
    }
    // 不同 thread 的键不同（`COALESCE(comment_thread_id, 全零)`），可以并存；
    // 且列值确实由 `context->>'wakeup_id'` 推导（触发器，非直写）。
    let wakeup = Id::from(Uuid::now_v7());
    let threaded = repo
        .create_task(&issue_task(&fx, Some(wakeup)))
        .await
        .expect("different thread is a different key");
    assert_eq!(
        threaded.comment_thread_id,
        Some(wakeup.0),
        "线程作用域由 context.wakeup_id 推导"
    );

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 3. 重试链
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn cancel_by_user_recomputes_delivered_comment_ids() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);

    // source → failed（委派出去的那一步）→ target（本次要取消的恢复任务）
    let source = issue_task(&fx, None);
    repo.create_task(&source).await.unwrap();
    let failed = issue_task(&fx, Some(Id::from(Uuid::now_v7())));
    repo.create_task(&failed).await.unwrap();
    sqlx::query("UPDATE agent_task_queue SET delegated_from_task_id = $2 WHERE id = $1")
        .bind(failed.id.0)
        .bind(source.id.0)
        .execute(fx.db.pool())
        .await
        .unwrap();
    force_terminal(&fx, failed.id, "failed", true, true).await;

    let target = issue_task(&fx, Some(Id::from(Uuid::now_v7())));
    repo.create_task(&target).await.unwrap();

    let receipt_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO comment (id, issue_id, workspace_id, author_type, author_id, content, type, \
             source_task_id) VALUES ($1, $2, $3, 'system', $4, 'progress', 'progress_update', $5)",
    )
    .bind(receipt_id)
    .bind(fx.issue_id.0)
    .bind(fx.workspace_id.0)
    .bind(fx.user_id.0)
    .bind(failed.id.0)
    .execute(fx.db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE agent_task_queue SET trigger_comment_id = $2 WHERE id = $1")
        .bind(target.id.0)
        .bind(receipt_id)
        .execute(fx.db.pool())
        .await
        .unwrap();

    let cancellation = Cancellation::by_user(Some(fx.user_id), Some("tester".to_owned()));
    let outcome = repo
        .cancel_task(target.id, &cancellation, Timestamp::now())
        .await
        .expect("cancel");
    let CancelOutcome::Applied {
        delivered_comments, ..
    } = outcome
    else {
        panic!("期望 Applied，实际 {outcome:?}");
    };
    assert_eq!(
        delivered_comments,
        mc_task::cancel::DeliveredCommentsPlan::RecomputeRecoverySignalReceipts
    );

    let delivered: Vec<Uuid> =
        sqlx::query_scalar("SELECT delivered_comment_ids FROM agent_task_queue WHERE id = $1")
            .bind(target.id.0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert_eq!(delivered, vec![receipt_id], "恢复信号回执要并进投递集");
    let cancelled_by: Option<String> =
        sqlx::query_scalar("SELECT cancelled_by_type FROM agent_task_queue WHERE id = $1")
            .bind(target.id.0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert_eq!(
        cancelled_by.as_deref(),
        Some("user"),
        "人工取消的 cancelled_by_type"
    );

    // 幂等：同一行再取消一次不再是 Applied。
    let again = repo
        .cancel_task(target.id, &cancellation, Timestamp::now())
        .await
        .unwrap();
    assert!(matches!(again, CancelOutcome::AlreadyTerminal { .. }));

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 7. 取消确认的 COALESCE 语义
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn cancel_ack_coalesces_branch_and_work_dir() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let task = issue_task(&fx, None);
    repo.create_task(&task).await.unwrap();
    repo.cancel_task(task.id, &Cancellation::by_system(), Timestamp::now())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_task_queue SET branch_name = 'feat/keep', error = NULL WHERE id = $1",
    )
    .bind(task.id.0)
    .execute(fx.db.pool())
    .await
    .unwrap();

    let ack = CancelAck {
        branch_name: Some("feat/other".to_owned()),
        durable_work_dir: Some("/tmp/wd".to_owned()),
        error_message: Some("daemon saw a failure".to_owned()),
        failure_reason: Some(FailureReason::Timeout.as_str().to_owned()),
    };
    let plan = repo.apply_cancel_ack_task(task.id, &ack).await.unwrap();
    // 已有 branch_name ⇒ 计划阶段就跳过（COALESCE 的上游语义：先到者赢）。
    assert!(!plan.writes_column(CancelAckColumn::BranchName));
    assert!(plan.writes_column(CancelAckColumn::DurableWorkDir));
    assert!(plan.writes_column(CancelAckColumn::Error));

    let row: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT branch_name, durable_work_dir, error, failure_reason FROM agent_task_queue \
         WHERE id = $1",
    )
    .bind(task.id.0)
    .fetch_one(fx.db.pool())
    .await
    .unwrap();
    assert_eq!(row.0.as_deref(), Some("feat/keep"), "已有分支名不被覆盖");
    assert_eq!(row.1.as_deref(), Some("/tmp/wd"));
    assert_eq!(row.2.as_deref(), Some("daemon saw a failure"));
    assert_eq!(row.3.as_deref(), Some(FailureReason::Timeout.as_str()));

    // 第二次 ack：三列都已有值 ⇒ 计划为空且不重播，落库形状不变。
    let replay = repo
        .apply_cancel_ack_task(
            task.id,
            &CancelAck {
                branch_name: Some("feat/late".to_owned()),
                durable_work_dir: Some("/tmp/late".to_owned()),
                error_message: Some("late".to_owned()),
                failure_reason: Some(FailureReason::IterationLimit.as_str().to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(replay.is_noop(), "已定型的列不再接受第二次 ack 写入");
    let again: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT branch_name, durable_work_dir, error FROM agent_task_queue WHERE id = $1",
    )
    .bind(task.id.0)
    .fetch_one(fx.db.pool())
    .await
    .unwrap();
    assert_eq!(again.0.as_deref(), Some("feat/keep"));
    assert_eq!(again.1.as_deref(), Some("/tmp/wd"));
    assert_eq!(again.2.as_deref(), Some("daemon saw a failure"));

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 8. agent-builder 会话
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn pg_task_store_adapter_fills_identity_gap() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let identity = issue_task(&fx, None);
    let task_id = identity.id;
    let store = PgTaskStore::with_pool(fx.db.pool().clone(), identity.clone());

    let budget = RetryBudget::FIRST_RUN;
    let (state, _) = TaskState::enqueue(budget);
    store.insert(task_id, &state).await.expect("insert");

    let read = store.get(task_id).await.unwrap().expect("row");
    assert_eq!(read.status, TaskStatus::Queued);
    assert_eq!(read.runtime_id, Some(fx.runtime_id));
    assert_eq!(read.parent_task_id, None);
    assert_eq!(read.budget.max_attempts, budget.max_attempts);

    teardown(&fx).await;
}
