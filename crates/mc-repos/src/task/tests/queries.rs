use super::super::queries::WorkingAgentFilter;
use super::super::store::NewTask;
use super::*;
use mc_core::{Id, Timestamp};
use mc_task::retry::{FailureReason, RetryBudget};
use mc_task::state::{ColumnWrite, TaskEventKind, TaskTransition};
use mc_task::status::TaskStatus;
use mc_task::usage::TaskUsageRow;
// ---------------------------------------------------------------------------
// 查询面：task runs / messages / usage / working agents / 家族读

// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn retry_child_links_parent_and_source() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let parent = issue_task(&fx, None);
    repo.create_task(&parent).await.unwrap();
    repo.commit_transition(
        parent.id,
        &TaskTransition {
            from: Some(TaskStatus::Queued),
            to: TaskStatus::Failed,
            event: TaskEventKind::Failed,
            status_changed: true,
            writes: vec![
                ColumnWrite::CompletedAt(Timestamp::now()),
                ColumnWrite::FailureReason(FailureReason::Timeout),
            ],
        },
    )
    .await
    .unwrap();

    let mut child = issue_task(&fx, None);
    child.parent_task_id = Some(parent.id);
    child.retry_of_task_id = Some(parent.id);
    child.budget = RetryBudget::new(2, 2);
    let row = repo.create_task(&child).await.expect("retry child");

    assert_eq!(row.parent_task_id, Some(parent.id.0));
    assert_eq!(row.retry_of_task_id, Some(parent.id.0));
    assert_eq!(row.attempt, 2);
    assert_eq!(row.task_status().unwrap(), TaskStatus::Queued);

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 4. 消息流水分页
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn task_messages_paginate_by_seq() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let task = issue_task(&fx, None);
    repo.create_task(&task).await.unwrap();
    for seq in 1..=3_i32 {
        sqlx::query(
            "INSERT INTO task_message (task_id, seq, type, content) VALUES ($1, $2, 'text', $3)",
        )
        .bind(task.id.0)
        .bind(seq)
        .bind(format!("m{seq}"))
        .execute(fx.db.pool())
        .await
        .unwrap();
    }

    let all = repo.list_task_messages(task.id, None).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(all[0].content.as_deref(), Some("m1"));

    let incremental = repo.list_task_messages(task.id, Some(1)).await.unwrap();
    assert_eq!(
        incremental.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(repo.max_task_message_seq(task.id).await.unwrap(), Some(3));

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 5. usage 汇总 / 覆盖不上报
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn issue_usage_summary_reports_metered_and_unreported() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let metered = issue_task(&fx, None);
    let unmetered = issue_task(&fx, Some(Id::from(Uuid::now_v7())));
    repo.create_task(&metered).await.unwrap();
    repo.create_task(&unmetered).await.unwrap();
    force_terminal(&fx, metered.id, "completed", true, true).await;
    force_terminal(&fx, unmetered.id, "failed", true, true).await;
    // `started_at` 还没落地的终态行不计入分母（上游 `terminal_runs` 的两个 IS NOT NULL）。
    let midflight = issue_task(&fx, Some(Id::from(Uuid::now_v7())));
    repo.create_task(&midflight).await.unwrap();
    force_terminal(&fx, midflight.id, "completed", false, false).await;

    let now = Timestamp::now();
    for (model, input, cost) in [("opus", 100_i64, Some(7_i64)), ("sonnet", 50, None)] {
        repo.upsert_task_usage(&TaskUsageRow {
            task_id: metered.id,
            provider: "anthropic".to_owned(),
            model: model.to_owned(),
            input_tokens: input,
            output_tokens: input * 2,
            cache_read_tokens: 5,
            cache_write_tokens: 1,
            cost_usd_ticks: cost,
            created_at: now,
            updated_at: Some(now),
        })
        .await
        .unwrap();
    }
    // 同一自然键二次写入 = 覆盖，不是累加。
    repo.upsert_task_usage(&TaskUsageRow {
        task_id: metered.id,
        provider: "anthropic".to_owned(),
        model: "opus".to_owned(),
        input_tokens: 100,
        output_tokens: 200,
        cache_read_tokens: 5,
        cache_write_tokens: 1,
        cost_usd_ticks: Some(7),
        created_at: now,
        updated_at: Some(now),
    })
    .await
    .unwrap();

    let summary = repo.issue_usage_summary(fx.issue_id).await.unwrap();
    assert_eq!(summary.total_input_tokens, 150);
    assert_eq!(summary.total_output_tokens, 300);
    assert_eq!(summary.total_cost_usd_ticks, 7);
    assert_eq!(
        summary.uncosted_input_tokens, 50,
        "sonnet 没有定价 ⇒ 进未计费桶"
    );
    assert_eq!(summary.task_count, 1);
    assert_eq!(summary.terminal_task_count, 2);
    assert_eq!(summary.metered_task_count, 1);
    assert_eq!(summary.unreported_task_count, 1);

    let rows = repo.list_task_usage(metered.id).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.model.as_str()).collect::<Vec<_>>(),
        vec!["opus", "sonnet"],
        "按 model 排序"
    );

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 6. 人工取消的三路 CASE 重算
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn quick_create_retry_copies_issue_less_failed_task() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let mut source = NewTask::queued(Id::from(Uuid::now_v7()), fx.agent_id);
    source.runtime_id = Some(fx.runtime_id);
    source.context = Some(json!({"prompt": "make an issue"}));
    source.priority = 3;
    repo.create_task(&source).await.unwrap();
    force_terminal(&fx, source.id, "failed", true, true).await;

    // pending 的 source context 要跟着转过去。
    sqlx::query(
        "INSERT INTO issue_source_context (id, workspace_id, source_issue_id, anchor_comment_id, \
             captured_by_user_id, snapshot_version, snapshot, capture_digest, state, origin_task_id) \
         VALUES ($1, $2, $3, $4, $5, 1, '{}'::jsonb, 'digest', 'pending', $6)",
    )
    .bind(Uuid::now_v7())
    .bind(fx.workspace_id.0)
    .bind(fx.issue_id.0)
    .bind(Uuid::now_v7())
    .bind(fx.user_id.0)
    .bind(source.id.0)
    .execute(fx.db.pool())
    .await
    .unwrap();

    let row = repo
        .create_quick_create_retry(fx.workspace_id, source.id, fx.user_id)
        .await
        .expect("retry")
        .expect("源任务满足条件");
    assert_ne!(row.id(), source.id);
    assert_eq!(row.task_status().unwrap(), TaskStatus::Queued);
    assert_eq!(row.priority, 3);
    assert_eq!(row.rerun_of_task_id, Some(source.id.0));
    assert!(row.force_fresh_session);
    assert_eq!(row.issue_id, None, "quick-create 重试仍不带 issue");
    let origin: Option<String> =
        sqlx::query_scalar("SELECT originator_source FROM agent_task_queue WHERE id = $1")
            .bind(row.id().0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert_eq!(origin.as_deref(), Some("direct_human"));
    let reattached: Uuid = sqlx::query_scalar(
        "SELECT origin_task_id FROM issue_source_context WHERE workspace_id = $1",
    )
    .bind(fx.workspace_id.0)
    .fetch_one(fx.db.pool())
    .await
    .unwrap();
    assert_eq!(reattached, row.id().0);

    // 带 issue 的任务不能走这条路。
    let with_issue = issue_task(&fx, None);
    repo.create_task(&with_issue).await.unwrap();
    force_terminal(&fx, with_issue.id, "failed", true, true).await;
    assert!(repo
        .create_quick_create_retry(fx.workspace_id, with_issue.id, fx.user_id)
        .await
        .unwrap()
        .is_none());

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 10. 工作区在跑的 agent
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn working_agents_only_counts_running_issue_tasks() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let running = issue_task(&fx, None);
    let queued = issue_task(&fx, Some(Id::from(Uuid::now_v7())));
    repo.create_task(&running).await.unwrap();
    repo.create_task(&queued).await.unwrap();
    force_terminal(&fx, running.id, "running", true, false).await;

    let filter = WorkingAgentFilter::default();
    let rows = repo
        .list_working_agents(fx.workspace_id, &filter)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "queued 不算 working");
    assert_eq!(rows[0].id, fx.agent_id.0);
    assert_eq!(rows[0].running_task_count, 1);
    assert_eq!(rows[0].issue_ids, vec![fx.issue_id.0]);

    let chat_only = WorkingAgentFilter {
        work_type: "chat".to_owned(),
        ..WorkingAgentFilter::default()
    };
    assert!(repo
        .list_working_agents(fx.workspace_id, &chat_only)
        .await
        .unwrap()
        .is_empty());
    let mine = WorkingAgentFilter {
        work_type: "issue".to_owned(),
        mine_relation: "assigned".to_owned(),
        member_id: Some(fx.user_id),
        parent_issue_id: None,
    };
    assert!(
        repo.list_working_agents(fx.workspace_id, &mine)
            .await
            .unwrap()
            .is_empty(),
        "issue 没分配给该成员 ⇒ mine 过滤为空"
    );

    force_terminal(&fx, running.id, "completed", true, true).await;
    assert!(repo
        .list_working_agents(fx.workspace_id, &filter)
        .await
        .unwrap()
        .is_empty());

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 11. issue 作用域列表 + 可见性收敛
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn issue_lists_hide_unstarted_escalation_placeholders() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let live = issue_task(&fx, None);
    repo.create_task(&live).await.unwrap();
    let placeholder = issue_task(&fx, Some(Id::from(Uuid::now_v7())));
    repo.create_task(&placeholder).await.unwrap();
    sqlx::query(
        "UPDATE agent_task_queue SET escalation_for_task_id = $2, status = 'deferred' \
         WHERE id = $1",
    )
    .bind(placeholder.id.0)
    .bind(live.id.0)
    .execute(fx.db.pool())
    .await
    .unwrap();

    let active = repo.list_active_tasks_by_issue(fx.issue_id).await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id(), live.id);

    let history = repo.list_tasks_by_issue(fx.issue_id).await.unwrap();
    assert!(
        history.iter().all(|r| r.id() != placeholder.id),
        "未启动的 deferred 升级占位行不外泄"
    );
    assert!(history.iter().any(|r| r.id() == live.id));

    // 一旦启动过，占位行就是历史的一部分。
    sqlx::query("UPDATE agent_task_queue SET started_at = now() WHERE id = $1")
        .bind(placeholder.id.0)
        .execute(fx.db.pool())
        .await
        .unwrap();
    let history = repo.list_tasks_by_issue(fx.issue_id).await.unwrap();
    assert!(history.iter().any(|r| r.id() == placeholder.id));

    // issue 归属校验（workspace 级租户隔离）。
    assert!(repo
        .issue_for_workspace(fx.issue_id, fx.workspace_id)
        .await
        .unwrap()
        .is_some());
    assert!(repo
        .issue_for_workspace(fx.issue_id, Id::from(Uuid::now_v7()))
        .await
        .unwrap()
        .is_none());

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 12. client usage upsert
// ---------------------------------------------------------------------------

/// `client_usage_daily` 的一行（避免测试里出现六元组）。
#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn family_read_spans_parent_and_direct_children_only() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let pool = fx.db.pool();

    // `issue` 有 `UNIQUE (workspace_id, number)`：fixture 的根 issue 已占 0。
    let new_issue = |parent: Option<Uuid>, number: i32| {
        let pool = pool.clone();
        let ws = fx.workspace_id.0;
        let user = fx.user_id.0;
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO issue (workspace_id, title, creator_type, creator_id, \
                     parent_issue_id, number) \
                 VALUES ($1, 'itest-family', 'member', $2, $3, $4) RETURNING id",
            )
            .bind(ws)
            .bind(user)
            .bind(parent)
            .bind(number)
            .fetch_one(&pool)
            .await
            .expect("insert issue")
        }
    };
    let child = new_issue(Some(fx.issue_id.0), 1).await;
    let grandchild = new_issue(Some(child), 2).await;

    // 父 issue：running；子 issue：queued；孙 issue：queued（必须被排除）。
    let root_task = repo.create_task(&issue_task(&fx, None)).await.unwrap();
    sqlx::query(
        "UPDATE agent_task_queue SET status = 'running', started_at = now() \
                 WHERE id = $1",
    )
    .bind(root_task.id)
    .execute(pool)
    .await
    .unwrap();
    for issue in [child, grandchild] {
        let mut task = issue_task(&fx, None);
        task.issue_id = Some(Id::from(issue));
        repo.create_task(&task).await.unwrap();
    }

    let rows = repo
        .list_active_tasks_by_issue_family(fx.workspace_id, fx.issue_id, 21)
        .await
        .expect("family read");
    assert_eq!(rows.len(), 2, "父 + 直接子，孙不在族里");
    assert_eq!(rows[0].status, "running", "running 优先排序");
    assert!(rows.iter().all(|r| r.issue_id != grandchild));
    assert_eq!(
        rows[0].issue_title, "itest-issue",
        "首行是根 issue 上的 running 任务"
    );
    assert_eq!(
        rows[1].issue_title, "itest-family",
        "次行是子 issue 上的 queued 任务"
    );

    // 行数上限照样生效（上游用 cap+1 区分「满页」与「被截断」）。
    let capped = repo
        .list_active_tasks_by_issue_family(fx.workspace_id, fx.issue_id, 1)
        .await
        .unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].status, "running");

    // workspace 作用域：别的 workspace 看不到任何一行。
    let rows = repo
        .list_active_tasks_by_issue_family(Id::from(Uuid::new_v4()), fx.issue_id, 21)
        .await
        .unwrap();
    assert!(rows.is_empty());

    // 未知族根 ⇒ 空集，而不是报错。
    let rows = repo
        .list_active_tasks_by_issue_family(fx.workspace_id, Id::from(Uuid::new_v4()), 21)
        .await
        .unwrap();
    assert!(rows.is_empty());
}
