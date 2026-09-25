//! `channel_reply_delivery` / `channel_task_delivery` 的 PG 集成测试。
//!
//! 全部 `#[ignore]`：靠 `MULTICA_TEST_DATABASE_URL` 触发（门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! 从 `delivery.rs` 的内联 `mod db_tests` 搬过来的（门 ⑩ 的 800 行硬限：M7-6 追加了投递
//! 状态机的六条语句，内联用例会把文件顶过上限）。**一条断言都没改**。

use super::*;

/// 建库连接（没有 `MULTICA_TEST_DATABASE_URL` ⇒ 用例自己跳过）。
async fn setup() -> Option<(Db, ChannelDeliveryRepo)> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    Some((db.clone(), ChannelDeliveryRepo::new(db)))
}

macro_rules! fixture {
    () => {
        match setup().await {
            Some(v) => v,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

fn delivery(turn_id: Id, owner: Id, expiry: DateTime<Utc>) -> NewReplyDelivery {
    NewReplyDelivery {
        turn_id,
        task_id: Id::new(),
        binding_id: Id::new(),
        installation_id: Id::new(),
        kind: ChannelKind::Telegram,
        chat_id: "chat-1".into(),
        owner_token: owner,
        owner_expires_at: expiry,
    }
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn a_live_owner_cannot_be_stolen_but_an_expired_one_can() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    let live = repo
        .claim_reply_delivery(delivery(
            turn,
            owner,
            Utc::now() + chrono::Duration::minutes(5),
        ))
        .await
        .expect("claim")
        .expect("第一次拿到");
    assert_eq!(live.phase, "streaming");
    assert_eq!(live.send_state, "none");
    assert_eq!(live.attempt_depth, 0);
    assert_eq!(live.kind(), Some(ChannelKind::Telegram));

    // 活跃持有者不被抢。
    let stolen = repo
        .claim_reply_delivery(delivery(
            turn,
            Id::new(),
            Utc::now() + chrono::Duration::minutes(5),
        ))
        .await
        .expect("claim");
    assert!(stolen.is_none(), "活跃租约不能被抢");

    // 租约过期（持有者死在投递中途）⇒ 可被接管，且 `attempt_depth` 只前进。
    sqlx::query(
        "UPDATE channel_reply_delivery \
         SET owner_expires_at = now() - interval '1 second' WHERE turn_id = $1",
    )
    .bind(turn.0)
    .execute(db.pool())
    .await
    .expect("expire lease");
    let expired_owner = Id::new();
    let taken = repo
        .claim_reply_delivery(delivery(
            turn,
            expired_owner,
            Utc::now() + chrono::Duration::minutes(5),
        ))
        .await
        .expect("claim")
        .expect("过期可接管");
    assert_eq!(taken.attempt_depth, 1);
    assert_eq!(taken.owner_token, Some(expired_owner.0));
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn progress_writes_are_token_fenced_and_unknown_never_beats_known() {
    let (_db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    repo.claim_reply_delivery(delivery(
        turn,
        owner,
        Utc::now() + chrono::Duration::minutes(5),
    ))
    .await
    .expect("claim");

    // 别人的令牌写不动。
    assert!(repo
        .update_reply_progress(turn, Id::new(), Id::new(), "known", "m1", 1)
        .await
        .expect("wrong token")
        .is_none());

    let known = repo
        .update_reply_progress(turn, Id::new(), owner, "known", "m1", 2)
        .await
        .expect("progress")
        .expect("row");
    assert_eq!(known.send_state, "known");
    assert_eq!(known.message_id, "m1");
    assert_eq!(known.chunks_sent, 2);

    // `known` 之后不能被 `unknown` 覆盖（已知 message id 是更强信息）。
    assert!(repo
        .update_reply_progress(turn, Id::new(), owner, "unknown", "", 0)
        .await
        .expect("unknown")
        .is_none());
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn terminal_and_settled_freeze_the_turn() {
    let (_db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    repo.claim_reply_delivery(delivery(
        turn,
        owner,
        Utc::now() + chrono::Duration::minutes(5),
    ))
    .await
    .expect("claim");

    let terminal = repo
        .mark_delivery_terminal(turn, owner)
        .await
        .expect("terminal")
        .expect("row");
    assert_eq!(terminal.phase, "terminal");
    // 已经 terminal ⇒ 不再是 streaming，重复切是 no-op。
    assert!(repo
        .mark_delivery_terminal(turn, owner)
        .await
        .expect("again")
        .is_none());

    let settled = repo
        .settle_reply_delivery(turn, "delivered")
        .await
        .expect("settle")
        .expect("row");
    assert!(settled.is_settled());
    assert_eq!(settled.settled_reason, "delivered");
    assert!(settled.owner_token.is_none());
    assert!(!settled.is_send_unknown());
    // 收口之后谁都不能再接管。
    assert!(repo
        .claim_reply_delivery(delivery(
            turn,
            Id::new(),
            Utc::now() + chrono::Duration::minutes(5)
        ))
        .await
        .expect("claim")
        .is_none());
    assert!(repo
        .settle_reply_delivery(turn, "again")
        .await
        .expect("settle")
        .is_none());
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn task_delivery_keeps_the_first_route() {
    let (_db, repo) = fixture!();
    let task = Id::new();
    let new = NewTaskDelivery {
        task_id: task,
        binding_id: Id::new(),
        installation_id: Id::new(),
        kind: ChannelKind::WeCom,
        channel_chat_id: "chat-1".into(),
        chat_type: "group".into(),
        channel_message_id: Some("m1".into()),
        channel_thread_id: None,
        route_revision: 1,
        config: serde_json::json!({ "bot_id": "b1" }),
    };
    let first = repo.upsert_task_delivery(new.clone()).await.expect("first");
    assert_eq!(first.channel_type, "wecom");
    let again = repo
        .upsert_task_delivery(NewTaskDelivery {
            route_revision: 9,
            channel_chat_id: "chat-2".into(),
            ..new
        })
        .await
        .expect("again");
    assert_eq!(again.task_id, first.task_id);
    assert_eq!(again.channel_chat_id, "chat-1", "首行不被后续上报改写");
    assert_eq!(again.route_revision, 1);
    assert!(repo.get_task_delivery(task).await.expect("get").is_some());
}

// =====================================================================
// M7-6（`LUM-1771`）追加：投递状态机的六条语句
// =====================================================================

/// 造一条取租约的入参（深度与 phase 由用例给，这两个正是本组语句的判据）。
fn attempt(
    turn_id: Id,
    task_id: Id,
    depth: i32,
    phase: &str,
    owner: Id,
    lease_seconds: f64,
) -> NewReplyDeliveryAttempt {
    NewReplyDeliveryAttempt {
        turn_id,
        task_id,
        attempt_depth: depth,
        binding_id: Id::new(),
        installation_id: Id::new(),
        kind: ChannelKind::Telegram,
        chat_id: "chat-1".into(),
        phase: phase.into(),
        owner_token: owner,
        lease_seconds,
    }
}

/// 把租约**当场过期**（模拟"持有者死在投递中途"）—— 过期是库侧时间决的，用例只能改库。
async fn expire_lease(db: &Db, turn_id: Id) {
    sqlx::query(
        "UPDATE channel_reply_delivery \
         SET owner_expires_at = now() - interval '1 second' WHERE turn_id = $1",
    )
    .bind(turn_id.0)
    .execute(db.pool())
    .await
    .expect("expire lease");
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn acquire_guards_settled_phase_and_retry_depth() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let first_task = Id::new();
    let owner = Id::new();

    let streaming = repo
        .acquire_reply_delivery(&attempt(turn, first_task, 0, "streaming", owner, 30.0))
        .await
        .expect("acquire")
        .expect("首建");
    assert_eq!(streaming.phase, "streaming");
    assert_eq!(streaming.attempt_depth, 0);

    // 1) 活着的持有者不被抢。
    assert!(repo
        .acquire_reply_delivery(&attempt(turn, Id::new(), 0, "streaming", Id::new(), 30.0))
        .await
        .expect("live")
        .is_none());

    // 2) `terminal` 一旦接管，`streaming` 再也拿不回来（最终答案已经接管，占位消息不许重开）。
    expire_lease(&db, turn).await;
    let terminal = repo
        .acquire_reply_delivery(&attempt(turn, first_task, 1, "terminal", Id::new(), 30.0))
        .await
        .expect("terminal")
        .expect("接管");
    assert_eq!(terminal.phase, "terminal", "phase 只前进");
    expire_lease(&db, turn).await;
    assert!(repo
        .acquire_reply_delivery(&attempt(turn, first_task, 1, "streaming", Id::new(), 30.0))
        .await
        .expect("streaming after terminal")
        .is_none());

    // 3) 深度只前进：被重试链甩下的旧尝试抢不回 turn。
    expire_lease(&db, turn).await;
    let deeper = repo
        .acquire_reply_delivery(&attempt(turn, Id::new(), 2, "terminal", Id::new(), 30.0))
        .await
        .expect("deeper")
        .expect("更深的重试可接管");
    assert_eq!(deeper.attempt_depth, 2);
    expire_lease(&db, turn).await;
    assert!(
        repo.acquire_reply_delivery(&attempt(turn, first_task, 1, "terminal", Id::new(), 30.0))
            .await
            .expect("shallower")
            .is_none(),
        "深度不许倒退"
    );

    // 4) 收口之后谁都不许再接管。
    repo.settle_reply_delivery(turn, "delivered")
        .await
        .expect("settle");
    assert!(repo
        .acquire_reply_delivery(&attempt(turn, Id::new(), 9, "terminal", Id::new(), 30.0))
        .await
        .expect("after settle")
        .is_none());
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn acquire_never_resets_an_outstanding_send() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    repo.acquire_reply_delivery(&attempt(turn, Id::new(), 0, "terminal", owner, 30.0))
        .await
        .expect("acquire");

    // 前一个持有者在发送中途死掉：`send_state` 停在 `in_flight`。
    sqlx::query("UPDATE channel_reply_delivery SET send_state = 'in_flight' WHERE turn_id = $1")
        .bind(turn.0)
        .execute(db.pool())
        .await
        .expect("mark in flight");
    expire_lease(&db, turn).await;

    let successor = repo
        .acquire_reply_delivery(&attempt(turn, Id::new(), 1, "terminal", Id::new(), 30.0))
        .await
        .expect("takeover")
        .expect("过期可接管");
    assert_eq!(
        successor.send_state, "in_flight",
        "接管者拿到的不是白纸：在飞的发送仍然在飞"
    );
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn release_and_renew_are_token_fenced() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    repo.acquire_reply_delivery(&attempt(turn, Id::new(), 0, "terminal", owner, 30.0))
        .await
        .expect("acquire");

    // 别人的令牌续不动，也放不掉。
    assert!(!repo
        .renew_reply_delivery(turn, Id::new(), 30.0)
        .await
        .expect("wrong token renew"));
    assert!(!repo
        .release_reply_delivery(turn, Id::new())
        .await
        .expect("wrong token release"));
    assert!(repo
        .renew_reply_delivery(turn, owner, 30.0)
        .await
        .expect("renew"));

    // 交还之后，下一条路径**立刻**能拿到（不必等租约到期）。
    assert!(repo
        .release_reply_delivery(turn, owner)
        .await
        .expect("release"));
    let next_owner = Id::new();
    let taken = repo
        .acquire_reply_delivery(&attempt(turn, Id::new(), 0, "terminal", next_owner, 30.0))
        .await
        .expect("acquire")
        .expect("交还后可立刻接管");
    assert_eq!(taken.owner_token, Some(next_owner.0));
    let _ = db;
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn unknown_send_is_recorded_without_the_owner_check() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    repo.acquire_reply_delivery(&attempt(turn, Id::new(), 0, "terminal", owner, 30.0))
        .await
        .expect("acquire");
    sqlx::query("UPDATE channel_reply_delivery SET send_state = 'in_flight' WHERE turn_id = $1")
        .bind(turn.0)
        .execute(db.pool())
        .await
        .expect("in flight");

    // 租约已经过期（持有者的请求挂死了）—— 这一条写仍然必须落地。
    expire_lease(&db, turn).await;
    sqlx::query("UPDATE channel_reply_delivery SET owner_token = NULL WHERE turn_id = $1")
        .bind(turn.0)
        .execute(db.pool())
        .await
        .expect("drop owner");
    assert!(
        repo.mark_reply_delivery_send_unknown(turn)
            .await
            .expect("unknown"),
        "不围栏在 owner 上"
    );
    let row = repo
        .get_reply_delivery(turn)
        .await
        .expect("get")
        .expect("row");
    assert!(row.is_send_unknown());

    // 只有"在飞"能变成"未知"：已经 known 的行不会被改写。
    sqlx::query("UPDATE channel_reply_delivery SET send_state = 'known' WHERE turn_id = $1")
        .bind(turn.0)
        .execute(db.pool())
        .await
        .expect("known");
    assert!(!repo
        .mark_reply_delivery_send_unknown(turn)
        .await
        .expect("again"));
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn close_turn_creates_a_settled_row_and_respects_the_retry_depth() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let close = |depth: i32, reason: &str| CloseReplyDeliveryTurn {
        turn_id: turn,
        task_id: Id::new(),
        attempt_depth: depth,
        binding_id: Id::new(),
        installation_id: Id::new(),
        kind: ChannelKind::Telegram,
        chat_id: "chat-1".into(),
        settled_reason: reason.into(),
    };

    // 从没碰过平台的 turn：这一条 insert 就是"取消之后才到的那一帧不会开出占位消息"的保证。
    let created = repo
        .close_reply_delivery_turn(&close(0, "cancelled"))
        .await
        .expect("close")
        .expect("建行");
    assert!(created.is_settled());
    assert_eq!(created.settled_reason, "cancelled");
    assert_eq!(created.send_state, "none");

    // 已收口 ⇒ 重复收口是 no-op（不会改原因）。
    assert!(repo
        .close_reply_delivery_turn(&close(0, "again"))
        .await
        .expect("again")
        .is_none());

    // 深度守卫：更浅的尝试不许收口一个被更深重试接管的 turn。
    let second = Id::new();
    sqlx::query(
        "UPDATE channel_reply_delivery \
         SET phase = 'streaming', settled_reason = '', attempt_depth = 3 WHERE turn_id = $1",
    )
    .bind(turn.0)
    .execute(db.pool())
    .await
    .expect("reset to streaming");
    assert!(repo
        .close_reply_delivery_turn(&close(1, "stale"))
        .await
        .expect("stale close")
        .is_none());
    assert!(repo
        .close_reply_delivery_turn(&close(3, "cancelled"))
        .await
        .expect("current close")
        .is_some());
    let _ = second;
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn get_reply_turn_walks_the_automatic_retry_chain() {
    let (db, repo) = fixture!();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let workspace = Id::new();
    let agent = Id::new();
    let issue = Id::new();
    let root = Id::new();
    let retry = Id::new();

    sqlx::query("INSERT INTO workspace (id, name, slug) VALUES ($1, 'delivery', $2)")
        .bind(workspace.0)
        .bind(format!("delivery-{suffix}"))
        .execute(db.pool())
        .await
        .expect("workspace");
    let owner = Id::new();
    sqlx::query(r#"INSERT INTO "user" (id, name, email) VALUES ($1, 'delivery', $2)"#)
        .bind(owner.0)
        .bind(format!("delivery-{suffix}@example.test"))
        .execute(db.pool())
        .await
        .expect("user");
    let runtime = Id::new();
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, name, runtime_mode, provider, status, owner_id) \
         VALUES ($1, $2, 'delivery-rt', 'local', 'claude_code', 'online', $3)",
    )
    .bind(runtime.0)
    .bind(workspace.0)
    .bind(owner.0)
    .execute(db.pool())
    .await
    .expect("runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'delivery', 'local', $3, $4, 'user')",
    )
    .bind(agent.0)
    .bind(workspace.0)
    .bind(runtime.0)
    .bind(owner.0)
    .execute(db.pool())
    .await
    .expect("agent");
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, number, identifier, title, creator_type, creator_id) \
         VALUES ($1, $2, 1, $3, 'delivery', 'member', $4)",
    )
    .bind(issue.0)
    .bind(workspace.0)
    .bind(format!("DEL-{suffix}"))
    .bind(owner.0)
    .execute(db.pool())
    .await
    .expect("issue");

    // 两次 insert 分离（不能一条 VALUES 里自引用）。
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, agent_id, issue_id, status, runtime_id, completed_at, retry_of_task_id) \
         VALUES ($1, $2, $3, 'completed', $4, now(), NULL)",
    )
    .bind(root.0)
    .bind(agent.0)
    .bind(issue.0)
    .bind(runtime.0)
    .execute(db.pool())
    .await
    .expect("root task");
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, agent_id, issue_id, status, runtime_id, completed_at, retry_of_task_id) \
         VALUES ($1, $2, $3, 'completed', $4, now(), $5)",
    )
    .bind(retry.0)
    .bind(agent.0)
    .bind(issue.0)
    .bind(runtime.0)
    .bind(root.0)
    .execute(db.pool())
    .await
    .expect("retry task");

    // 根：turn 是它自己，深度 0。
    let root_turn = repo.get_reply_turn(root).await.expect("root").expect("row");
    assert_eq!(root_turn.turn_id, Some(root.0));
    assert_eq!(root_turn.attempt_depth, 0);

    // 重试：turn 是**根**（不是自己），深度 1 ⇒ 自动重试接着前一次开始的投递。
    let retry_turn = repo
        .get_reply_turn(retry)
        .await
        .expect("retry")
        .expect("row");
    assert_eq!(retry_turn.turn_id, Some(root.0), "ownership 按链的根");
    assert_eq!(retry_turn.attempt_depth, 1);

    // 库里没有这个任务 ⇒ 查询仍然回**一行**，但 `turn_id` 是 NULL（标量子查询的形状决定的，
    // 上游同）：调用方按"它就是自己的 turn、深度 0"处理（`DeliveryLedger::turn_for`）。
    let missing = repo
        .get_reply_turn(Id::new())
        .await
        .expect("missing")
        .expect("标量子查询恒回一行");
    assert!(missing.turn_id.is_none(), "没有队列行 ⇒ 没有根");
    assert_eq!(missing.attempt_depth, 0);
}

#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_recording_keeps_placeholder_and_progress_apart() {
    let (db, repo) = fixture!();
    let turn = Id::new();
    let owner = Id::new();
    repo.acquire_reply_delivery(&attempt(turn, Id::new(), 0, "terminal", owner, 30.0))
        .await
        .expect("acquire");

    // 没有令牌就发不出去（claim 是围栏的第一步）。
    assert!(!repo
        .mark_reply_delivery_sending(turn, Id::new())
        .await
        .expect("wrong token"));
    assert!(repo
        .mark_reply_delivery_sending(turn, owner)
        .await
        .expect("claim"));
    // 已经 `in_flight` ⇒ 不再重复公开。
    assert!(!repo
        .mark_reply_delivery_sending(turn, owner)
        .await
        .expect("already in flight"));

    // 占位落地：有了可编辑消息，但 `chunks_sent` **不动**（占位不是进度）。
    assert!(repo
        .record_reply_delivery_placeholder(turn, owner, "c1:m1")
        .await
        .expect("placeholder"));
    let row = repo
        .get_reply_delivery(turn)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.message_id, "c1:m1");
    assert_eq!(row.send_state, "known");
    assert_eq!(row.chunks_sent, 0);

    // 第一片：`message_id` 已经是 `c1:m1` ⇒ **不**改指向；chunks_sent 前进。
    assert!(repo
        .mark_reply_delivery_sending(turn, owner)
        .await
        .expect("recommitted"));
    assert!(repo
        .record_reply_delivery_chunk(turn, owner, "c1:m9", 1)
        .await
        .expect("chunk 1"));
    let row = repo
        .get_reply_delivery(turn)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.message_id, "c1:m1", "后续片不改编辑目标");
    assert_eq!(row.chunks_sent, 1);
    assert_eq!(row.phase, "terminal", "取租约时给的就是 terminal");

    // `chunks_sent` 只增不减（GREATEST）。
    assert!(repo
        .record_reply_delivery_chunk(turn, owner, "c1:m9", 0)
        .await
        .expect("stale chunk"));
    assert_eq!(
        repo.get_reply_delivery(turn)
            .await
            .expect("get")
            .expect("row")
            .chunks_sent,
        1
    );

    // 平台拒绝 ⇒ 回到可再试的状态；已有占位消息 ⇒ `known` 而不是 `none`。
    assert!(repo
        .mark_reply_delivery_sending(turn, owner)
        .await
        .expect("claim again"));
    assert!(repo
        .reset_reply_delivery_send(turn, owner)
        .await
        .expect("refused"));
    let row = repo
        .get_reply_delivery(turn)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.send_state, "known", "占位消息还在，不能假装不存在");
    let _ = db;
}
