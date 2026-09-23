//! `pending_tasks_*` / `prioritize_queued_task` / `clear_queued_tasks` 的真库语义
//! （M4-4 / LUM-1601）。
//!
//! 这三族 SQL 共享同一份「可见头」排序（上游抄了七遍），也共享同一批坑：`priority DESC` 与
//! FIFO 的交互、背景重生成轮对 UI 不可见、取消时输入批次的两种结算（渠道 `"Stopped."` vs
//! 直聊删行）、以及 `Stopped.` 写完之后**必须**把下一个 queued 输入重锚到 1µs 之后。

use chrono::Duration as ChronoDuration;
use uuid::Uuid;

use super::{
    insert_message, insert_task, message_row, new_session, raw_task, setup, teardown, Fixture,
    MessageSeed, TaskSeed,
};
use crate::chat_task::{PriorityError, PriorityOutcome};

/// 固定时间戳：FIFO / 排序断言必须可控（`now()` 在同一个事务里是同一个值，靠它是瞎猜）。
const T1: &str = "2026-02-01 00:00:01+00";
const T2: &str = "2026-02-01 00:00:02+00";
const T3: &str = "2026-02-01 00:00:03+00";
const T4: &str = "2026-02-01 00:00:04+00";
const T5: &str = "2026-02-01 00:00:05+00";
const T6: &str = "2026-02-01 00:00:06+00";
const T7: &str = "2026-02-01 00:00:07+00";
const T8: &str = "2026-02-01 00:00:08+00";

async fn message_exists(fixture: &Fixture, message_id: Uuid) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM chat_message WHERE id = $1)")
        .bind(message_id)
        .fetch_one(fixture.pool())
        .await
        .expect("message exists")
}

/// 会话里的全部消息（时间升序；`clear` 的副作用要靠它看）。
async fn session_messages(
    fixture: &Fixture,
    session_id: Uuid,
) -> Vec<(String, String, Option<Uuid>)> {
    sqlx::query_as(
        "SELECT role, content, task_id FROM chat_message WHERE chat_session_id = $1 \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(session_id)
    .fetch_all(fixture.pool())
    .await
    .expect("session messages")
}

// ---------------------------------------------------------------------------
// pending_tasks_for_session
// ---------------------------------------------------------------------------

/// 排序 = 可见头 > deferred > (`priority DESC, created_at ASC, id ASC`)；lateral join 取的是
/// **输入批次归属任务**下最早的 user 行（auto-retry 克隆继承父任务的批次）。
#[allow(clippy::too_many_lines)] // 六条 pending 各是一种排序 / 归属形态，拆开会看不清序
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn pending_tasks_for_session_orders_the_visible_head_first_and_joins_the_input() {
    let Some(fixture) = setup().await else {
        println!("skip pending_tasks_for_session_orders_the_visible_head_first_and_joins_the_input: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;

    let head = insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("running").at(T4).priority(2),
    )
    .await;
    let deferred = insert_task(
        &fixture,
        session,
        TaskSeed::queued()
            .status("deferred")
            .at(T2)
            .priority(9)
            .no_input(),
    )
    .await;
    sqlx::query("UPDATE agent_task_queue SET wait_reason = 'waiting for folder' WHERE id = $1")
        .bind(deferred)
        .execute(fixture.pool())
        .await
        .expect("set wait_reason");
    let queued_high = insert_task(&fixture, session, TaskSeed::queued().at(T5).priority(4)).await;
    let queued_low = insert_task(&fixture, session, TaskSeed::queued().at(T3).priority(2)).await;
    // auto-retry 克隆：不认领输入，继承 queued_low 的批次。
    let clone = insert_task(
        &fixture,
        session,
        TaskSeed::queued()
            .at(T7)
            .priority(2)
            .input_owned_by(queued_low),
    )
    .await;
    let orphan = insert_task(
        &fixture,
        session,
        TaskSeed::queued().at(T8).priority(1).no_input(),
    )
    .await;
    // 背景 quick-actions 重生成：对 UI 不可见 ⇒ 永不出现在 pending 列表里。
    let hidden_reply = insert_message(&fixture, session, MessageSeed::assistant("answer")).await;
    let hidden = insert_task(
        &fixture,
        session,
        TaskSeed::queued()
            .at(T6)
            .priority(9)
            .regenerating(hidden_reply),
    )
    .await;

    for (task, content) in [
        (head, "head turn"),
        (deferred, "deferred turn"),
        (queued_high, "high turn"),
        (queued_low, "low turn"),
    ] {
        insert_message(
            &fixture,
            session,
            MessageSeed::user(content).on(task).at(T1),
        )
        .await;
    }

    let rows = fixture
        .repo()
        .pending_tasks_for_session(session)
        .await
        .expect("pending tasks");

    let ids: Vec<Uuid> = rows.iter().map(|row| row.task_id).collect();
    assert_eq!(
        ids,
        vec![head, deferred, queued_high, queued_low, clone, orphan],
        "可见头 → deferred → priority DESC → FIFO"
    );
    assert!(!ids.contains(&hidden), "背景重生成轮不进 pending 列表");

    assert_eq!(rows[0].content, "head turn");
    assert_eq!(rows[0].status, "running");
    assert_eq!(rows[1].content, "deferred turn");
    assert_eq!(rows[1].wait_reason.as_deref(), Some("waiting for folder"));
    assert_eq!(rows[2].content, "high turn");
    assert_eq!(rows[3].content, "low turn");
    assert_eq!(
        rows[4].content, "low turn",
        "克隆继承父任务的输入批次（lateral join 的 COALESCE）"
    );
    assert_eq!(rows[4].message_id, rows[3].message_id);
    assert_eq!(rows[5].content, "", "没有输入消息 ⇒ 空串而不是 NULL");
    assert_eq!(rows[5].message_id, None);

    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// prioritize_queued_task
// ---------------------------------------------------------------------------

/// CAS 成功：目标 → 4，其余 `priority >= 4` 的 queued → 3，并回报要取消的活跃头。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn prioritize_promotes_the_target_and_demotes_the_previous_winner() {
    let Some(fixture) = setup().await else {
        println!("skip prioritize_promotes_the_target_and_demotes_the_previous_winner: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let head = insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("running").at(T1).priority(2),
    )
    .await;
    // 两台活跃：`active_task_id` 取最早创建的那台（顺序键里的 created_at ASC）。
    let also_active = insert_task(
        &fixture,
        session,
        TaskSeed::queued()
            .status("waiting_local_directory")
            .at(T5)
            .priority(2),
    )
    .await;
    let previous = insert_task(&fixture, session, TaskSeed::queued().at(T2).priority(4)).await;
    let target = insert_task(&fixture, session, TaskSeed::queued().at(T3).priority(2)).await;
    let done = insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("completed").at(T4).priority(9),
    )
    .await;
    // 别的会话：无论优先级如何都不许被本会话的事务碰到。
    let other_session = new_session(&fixture, fixture.agent_id, "active").await;
    let foreign = insert_task(
        &fixture,
        other_session,
        TaskSeed::queued().at(T2).priority(4),
    )
    .await;

    let outcome = fixture
        .repo()
        .prioritize_queued_task(session, fixture.agent_id, target)
        .await
        .expect("prioritize");
    match outcome {
        PriorityOutcome::Prioritized(row) => {
            assert_eq!(row.task_id, target);
            assert_eq!(row.agent_id, fixture.agent_id, "广播载荷取被提升行的 agent");
            assert_eq!(row.active_task_id, Some(head), "最早创建的活跃轮");
        }
        other => panic!("expected Prioritized, got {other:?}"),
    }

    assert_eq!(raw_task(&fixture, target).await.priority, 4);
    assert_eq!(
        raw_task(&fixture, previous).await.priority,
        3,
        "旧的 priority 4 被降回 3（不是 2：只降一档）"
    );
    assert_eq!(raw_task(&fixture, head).await.priority, 2);
    assert_eq!(raw_task(&fixture, also_active).await.priority, 2);
    assert_eq!(raw_task(&fixture, done).await.priority, 9, "终态行不动");
    assert_eq!(
        raw_task(&fixture, foreign).await.priority,
        4,
        "别的会话不动"
    );
    assert_eq!(raw_task(&fixture, target).await.status, "queued");

    teardown(&fixture).await;
}

/// 有 queued 目标但没有被认领的活跃头 ⇒ 409 `"there is no active reply to replace"`
/// （`NoActiveReply`），且**不做任何写入**。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn prioritize_without_a_claimed_head_reports_no_active_reply() {
    let Some(fixture) = setup().await else {
        println!("skip prioritize_without_a_claimed_head_reports_no_active_reply: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let previous = insert_task(&fixture, session, TaskSeed::queued().at(T1).priority(4)).await;
    let target = insert_task(&fixture, session, TaskSeed::queued().at(T2).priority(2)).await;

    let outcome = fixture
        .repo()
        .prioritize_queued_task(session, fixture.agent_id, target)
        .await
        .expect("prioritize");
    assert!(
        matches!(outcome, PriorityOutcome::NoActiveReply),
        "只有 queued 头 ⇒ 无可替换的活跃回复"
    );
    assert_eq!(
        raw_task(&fixture, target).await.priority,
        2,
        "CAS 没命中就不许写"
    );
    assert_eq!(raw_task(&fixture, previous).await.priority, 4);

    teardown(&fixture).await;
}

/// 目标不在队列（不存在 / 别的会话 / 已终态）⇒ 409 `"task is no longer queued"`
/// （`NotQueued`），哪怕活跃头存在。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn prioritize_reports_not_queued_for_a_stale_target() {
    let Some(fixture) = setup().await else {
        println!("skip prioritize_reports_not_queued_for_a_stale_target: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("running").at(T1).priority(2),
    )
    .await;
    let other_session = new_session(&fixture, fixture.agent_id, "active").await;
    let foreign = insert_task(
        &fixture,
        other_session,
        TaskSeed::queued().at(T2).priority(2),
    )
    .await;
    let done = insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("cancelled").at(T3).priority(2),
    )
    .await;
    let repo = fixture.repo();

    for (label, candidate) in [
        ("不存在", Uuid::new_v4()),
        ("别的会话", foreign),
        ("已终态", done),
    ] {
        let outcome = repo
            .prioritize_queued_task(session, fixture.agent_id, candidate)
            .await
            .expect("prioritize");
        assert!(
            matches!(outcome, PriorityOutcome::NotQueued),
            "{label}: expected NotQueued, got {outcome:?}"
        );
    }
    assert_eq!(raw_task(&fixture, foreign).await.priority, 2);
    assert_eq!(raw_task(&fixture, done).await.priority, 2);

    teardown(&fixture).await;
}

/// 先锁 agent 再动队列：agent 不存在（或锁不到）⇒ `PriorityError::LockAgent`，一个字节都没写。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn prioritize_fails_before_writing_when_the_agent_cannot_be_locked() {
    let Some(fixture) = setup().await else {
        println!("skip prioritize_fails_before_writing_when_the_agent_cannot_be_locked: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("running").at(T1).priority(2),
    )
    .await;
    let target = insert_task(&fixture, session, TaskSeed::queued().at(T2).priority(2)).await;

    let err = fixture
        .repo()
        .prioritize_queued_task(session, Uuid::new_v4(), target)
        .await
        .expect_err("agent is missing");
    assert!(
        matches!(err, PriorityError::LockAgent(crate::RepoError::NotFound)),
        "{err}"
    );
    assert_eq!(raw_task(&fixture, target).await.priority, 2);

    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// clear_queued_tasks
// ---------------------------------------------------------------------------

/// 可见头即使是 queued 也必须保住；其余 queued 全部取消，并清掉 `prepare_lease_expires_at`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn clear_keeps_the_visible_head_and_cancels_every_other_queued_turn() {
    let Some(fixture) = setup().await else {
        println!("skip clear_keeps_the_visible_head_and_cancels_every_other_queued_turn: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    // 头是 queued（没有任何被认领的轮）：`clear` 依旧不许取消它。
    let head = insert_task(&fixture, session, TaskSeed::queued().at(T1).priority(2)).await;
    let second = insert_task(&fixture, session, TaskSeed::queued().at(T2).priority(2)).await;
    let third = insert_task(&fixture, session, TaskSeed::queued().at(T3).priority(2)).await;
    let background = insert_task(
        &fixture,
        session,
        TaskSeed::queued()
            .at(T4)
            .priority(2)
            .regenerating(Uuid::new_v4()),
    )
    .await;
    let done = insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("completed").at(T5).priority(2),
    )
    .await;
    // 取消路径会清掉准备租约（上游 `prepare_lease_expires_at = NULL`）。
    sqlx::query("UPDATE agent_task_queue SET prepare_lease_expires_at = now() WHERE id = $1")
        .bind(second)
        .execute(fixture.pool())
        .await
        .expect("set prepare lease");

    let cancelled = fixture
        .repo()
        .clear_queued_tasks(session, fixture.agent_id)
        .await
        .expect("clear queued");

    let mut ids: Vec<Uuid> = cancelled.iter().map(|task| task.id).collect();
    ids.sort();
    let mut expected = vec![second, third, background];
    expected.sort();
    assert_eq!(
        ids, expected,
        "背景重生成轮不算可见头（head CTE 过滤 `regenerate_quick_actions_for IS NULL`）⇒ \
         它保不住自己，但也不会把真正的头挤掉"
    );
    for task in &cancelled {
        assert_eq!(task.status, "cancelled", "RETURNING 已是终态");
        assert!(task.completed_at.is_some());
    }

    let head_row = raw_task(&fixture, head).await;
    assert_eq!(head_row.status, "queued", "可见头不许被取消");
    assert!(head_row.completed_at.is_none());
    let second_row = raw_task(&fixture, second).await;
    assert_eq!(second_row.status, "cancelled");
    assert!(
        second_row.prepare_lease_expires_at.is_none(),
        "取消必须清准备租约，否则 daemon 会按旧租约执行已取消的轮"
    );
    assert_eq!(raw_task(&fixture, background).await.status, "cancelled");
    assert_eq!(raw_task(&fixture, done).await.status, "completed");

    teardown(&fixture).await;
}

/// 渠道入站的输入批次：不删消息，落一条 `"Stopped."`，并把**下一个 queued 直聊输入**重锚到
/// `Stopped.` 之后 1µs（读者要么看到旧头无回复、要么看到回复 + 新头，不会看到回复却没有头）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn clear_settles_a_channel_input_with_a_stopped_row_and_reanchors_the_next_head() {
    let Some(fixture) = setup().await else {
        println!("skip clear_settles_a_channel_input_with_a_stopped_row_and_reanchors_the_next_head: no env");
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let head = insert_task(&fixture, session, TaskSeed::queued().at(T1).priority(2)).await;
    let channel_turn = insert_task(&fixture, session, TaskSeed::queued().at(T2).priority(2)).await;
    let head_input = insert_message(
        &fixture,
        session,
        MessageSeed::user("head turn").on(head).at(T1),
    )
    .await;
    let channel_input = insert_message(
        &fixture,
        session,
        MessageSeed::user("from the channel")
            .on(channel_turn)
            .at(T2)
            .ingested(),
    )
    .await;

    let cancelled = fixture
        .repo()
        .clear_queued_tasks(session, fixture.agent_id)
        .await
        .expect("clear queued");
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].id, channel_turn);

    let messages = session_messages(&fixture, session).await;
    assert!(
        messages
            .iter()
            .any(|(role, content, _)| role == "user" && content == "from the channel"),
        "渠道消息不能删：发送者没有 Multica 输入框可以恢复草稿"
    );
    let stopped_row = sqlx::query_as::<_, (chrono::DateTime<chrono::Utc>, Option<i64>)>(
        "SELECT created_at, elapsed_ms FROM chat_message WHERE chat_session_id = $1 \
         AND content = 'Stopped.' AND task_id = $2",
    )
    .bind(session)
    .bind(channel_turn)
    .fetch_one(fixture.pool())
    .await
    .expect("load stopped row");
    assert!(stopped_row.1.is_some_and(|ms| ms >= 0), "elapsed_ms 非负");

    let head_input_row = message_row(&fixture, head_input).await;
    assert_eq!(
        head_input_row.created_at,
        stopped_row.0 + ChronoDuration::microseconds(1),
        "新头必须紧跟在 Stopped. 之后 1µs"
    );
    assert_eq!(raw_task(&fixture, head).await.status, "queued");
    assert!(message_exists(&fixture, channel_input).await);

    teardown(&fixture).await;
}

/// 直聊输入批次：删掉 user 行，但先把被它收养的 onboarding kickoff 交给下一个 queued 轮
/// （顺序是契约：先交 kickoff 再删输入）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn clear_settles_a_direct_input_by_deleting_it_and_reassigning_the_kickoff() {
    let Some(fixture) = setup().await else {
        println!(
            "skip clear_settles_a_direct_input_by_deleting_it_and_reassigning_the_kickoff: no env"
        );
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let head = insert_task(&fixture, session, TaskSeed::queued().at(T1).priority(2)).await;
    let cancelled_turn =
        insert_task(&fixture, session, TaskSeed::queued().at(T2).priority(2)).await;
    let kickoff = insert_message(
        &fixture,
        session,
        MessageSeed::user("hello mika")
            .kind("onboarding_kickoff")
            .on(cancelled_turn)
            .at(T1),
    )
    .await;
    let dropped = insert_message(
        &fixture,
        session,
        MessageSeed::user("second turn").on(cancelled_turn).at(T2),
    )
    .await;
    let kept_reply = insert_message(
        &fixture,
        session,
        MessageSeed::assistant("answer").on(cancelled_turn).at(T2),
    )
    .await;

    let cancelled = fixture
        .repo()
        .clear_queued_tasks(session, fixture.agent_id)
        .await
        .expect("clear queued");
    assert_eq!(cancelled.len(), 1);

    assert_eq!(
        message_row(&fixture, kickoff).await.task_id,
        Some(head),
        "kickoff 要交给下一个 queued 直聊轮，否则会挂在一个永不运行的任务上"
    );
    assert!(!message_exists(&fixture, dropped).await, "直聊 user 行被删");
    assert!(
        message_exists(&fixture, kept_reply).await,
        "assistant 行不是输入批次的一部分，不许跟着删"
    );

    teardown(&fixture).await;
}

/// 会话已经不存在 ⇒ 整体 no-op（handler 仍 204）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn clear_is_a_noop_for_a_missing_session() {
    let Some(fixture) = setup().await else {
        println!("skip clear_is_a_noop_for_a_missing_session: no env");
        return;
    };
    let cancelled = fixture
        .repo()
        .clear_queued_tasks(Uuid::new_v4(), fixture.agent_id)
        .await
        .expect("clear queued");
    assert!(cancelled.is_empty());
    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// pending_tasks_by_creator / has_pending_tasks_by_creator
// ---------------------------------------------------------------------------

/// 创建者过滤 + agent 可见性烘进 SQL + 背景重生成轮不算「在飞」。
#[allow(clippy::too_many_lines)] // 三会话 × 四断言的矩阵
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn creator_scoped_reads_filter_by_workspace_creator_and_visible_agents() {
    let Some(fixture) = setup().await else {
        println!(
            "skip creator_scoped_reads_filter_by_workspace_creator_and_visible_agents: no env"
        );
        return;
    };
    let other_user: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-other', $1) RETURNING id"#,
    )
    .bind(format!("itest-other-{}@example.com", Uuid::new_v4()))
    .fetch_one(fixture.pool())
    .await
    .expect("insert other user");
    let other_agent: Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(format!("itest-other-agent-{}", Uuid::new_v4()))
    .bind(fixture.runtime_id)
    .bind(other_user)
    .fetch_one(fixture.pool())
    .await
    .expect("insert other agent");

    // S1：本用户 + 本 agent，两轮在飞 + 一条终态 + 一条背景重生成。
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let running = insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("running").at(T1),
    )
    .await;
    let queued = insert_task(&fixture, session, TaskSeed::queued().at(T2)).await;
    insert_task(
        &fixture,
        session,
        TaskSeed::queued().status("completed").at(T3),
    )
    .await;
    insert_task(
        &fixture,
        session,
        TaskSeed::queued().at(T4).regenerating(Uuid::new_v4()),
    )
    .await;

    // S2：别的创建者，只有一条背景重生成 ⇒ 两个查询都必须为空 / false。
    let foreign_session = sqlx::query_scalar(
        "INSERT INTO chat_session(workspace_id, agent_id, creator_id, title, runtime_id, \
                                  explicitly_created_at) \
         VALUES ($1, $2, $3, '', $4, now()) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.agent_id)
    .bind(other_user)
    .bind(fixture.runtime_id)
    .fetch_one(fixture.pool())
    .await
    .expect("insert foreign session");
    insert_task(
        &fixture,
        foreign_session,
        TaskSeed::queued().at(T5).regenerating(Uuid::new_v4()),
    )
    .await;

    // S3：本用户但**别的 agent**（私有 agent 收回后这类行必须从列表里掉出去）。
    let other_agent_session = new_session(&fixture, other_agent, "active").await;
    let other_agent_task =
        insert_task(&fixture, other_agent_session, TaskSeed::queued().at(T6)).await;

    let repo = fixture.repo();
    let mine = repo
        .pending_tasks_by_creator(fixture.workspace_id, fixture.user_id)
        .await
        .expect("pending by creator");
    let ids: Vec<Uuid> = mine.iter().map(|row| row.task_id).collect();
    assert_eq!(
        ids,
        vec![other_agent_task, queued, running],
        "created_at DESC：S3 最新在最前，终态 / 背景轮都不算"
    );
    assert!(mine.iter().all(|row| row.chat_session_id.is_some()));
    assert_eq!(mine[0].agent_id, other_agent);
    assert_eq!(mine[2].agent_id, fixture.agent_id);

    assert!(
        repo.pending_tasks_by_creator(fixture.workspace_id, other_user)
            .await
            .expect("pending by other creator")
            .is_empty(),
        "只有背景重生成轮的创建者没有在飞任务"
    );
    assert!(repo
        .pending_tasks_by_creator(Uuid::new_v4(), fixture.user_id)
        .await
        .expect("pending by other workspace")
        .is_empty());

    assert!(
        repo.has_pending_tasks_by_creator(
            fixture.workspace_id,
            fixture.user_id,
            &[fixture.agent_id]
        )
        .await
        .expect("has any"),
        "S1 有一轮 running"
    );
    assert!(
        repo.has_pending_tasks_by_creator(fixture.workspace_id, fixture.user_id, &[other_agent])
            .await
            .expect("has any other agent"),
        "S3 属于另一个 agent"
    );
    assert!(
        !repo
            .has_pending_tasks_by_creator(fixture.workspace_id, other_user, &[fixture.agent_id])
            .await
            .expect("has any background only"),
        "背景重生成轮不算在飞"
    );
    assert!(
        !repo
            .has_pending_tasks_by_creator(fixture.workspace_id, fixture.user_id, &[])
            .await
            .expect("has any empty agent set"),
        "空 agent 集合恒 false（handler 侧短路同款）"
    );

    teardown(&fixture).await;
}
