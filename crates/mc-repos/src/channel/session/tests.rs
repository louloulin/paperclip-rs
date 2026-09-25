//! 会话绑定面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
//!
//! 覆盖本片 `DoD` 里**只有真 PostgreSQL 能判**的那几处：
//!
//! 1. `ensure_session` 的 find-or-create + 唯一约束仲裁（一个隔离键一个会话）；
//! 2. `/new`（`start_route`）退休当前路由行并**收口**老代的历史边界，新代从 1 开始；
//! 3. `append_message` 的**代际**语义：`/clear` 之后新消息进新代，**老代的行不会被后来的
//!    发起人覆盖**（三代的行分别按自己的 revision 读）；
//! 4. 两阶段幂等的**事务内**落定：令牌被抢走 ⇒ `ClaimLost` 且**一条消息都没留下**；
//! 5. 路由退休之后拿着老 binding 的 append ⇒ `RouteChanged`（不静默写进老会话）。
//!
//! 现场：一个 workspace + user + agent（`chat_session` 有三个外键，必须齐）。
//! 未设置 `MULTICA_TEST_DATABASE_URL` → 打印跳过并 `return`；**已设置但连不上 → panic**。

use chrono::Utc;
use mc_core::channel::message::{ChatType, MessageKind};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use sqlx::PgPool;
use uuid::Uuid;

use super::*;

struct Fixture {
    db: Db,
    pool: PgPool,
    workspace_id: Id,
    agent_id: Id,
    user_id: Id,
}

async fn setup() -> Option<Fixture> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let pool = db.pool().clone();
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-lum1767-chat', $1) RETURNING id",
    )
    .bind(format!("itest-lum1767-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("insert workspace");
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-lum1767', $1) RETURNING id"#,
    )
    .bind(format!("itest-lum1767-{}@example.com", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("insert user");
    let runtime_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
         VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-lum1767-rt-{}", Uuid::new_v4()))
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("insert agent_runtime");
    let agent_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-lum1767-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("insert agent");
    Some(Fixture {
        db,
        pool,
        workspace_id: Id(workspace_id),
        agent_id: Id(agent_id),
        user_id: Id(user_id),
    })
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

mod legacy;
mod media;

impl Fixture {
    fn repo(&self) -> ChannelChatSessionRepo {
        ChannelChatSessionRepo::new(self.db.clone())
    }

    fn ensure(&self, kind: ChannelKind, key: &str) -> NewEnsureSession {
        NewEnsureSession {
            workspace_id: self.workspace_id,
            agent_id: self.agent_id,
            installation_id: Id::new(),
            kind,
            chat_type: ChatType::P2p,
            binding_key: key.to_string(),
            binding_config: serde_json::json!({}),
            creator: self.user_id,
        }
    }

    async fn message_count(&self, session_id: Id) -> i64 {
        sqlx::query_scalar("SELECT count(*)::bigint FROM chat_message WHERE chat_session_id = $1")
            .bind(session_id.0)
            .fetch_one(&self.pool)
            .await
            .expect("count messages")
    }
}

fn key(tag: &str) -> String {
    format!("itest1767-{tag}-{}", Uuid::new_v4().simple())
}

/// 隔离键是**唯一**的会话判据：同一个键两次 ensure 拿到同一个会话，不同键各一个。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn ensure_session_is_idempotent_per_isolation_key() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let input = fixture.ensure(ChannelKind::Slack, &key("ensure"));

    let first = repo.ensure_session(&input).await.expect("first ensure");
    let again = repo.ensure_session(&input).await.expect("second ensure");
    assert_eq!(first, again, "同一个 (installation, key) 只建一次会话");

    let binding = repo
        .get_current_binding(input.installation_id, &input.binding_key)
        .await
        .expect("get binding")
        .expect("当前路由行");
    assert_eq!(binding.chat_session_id(), first);
    assert_eq!(binding.route_revision, 1);
    assert_eq!(binding.context_revision, 1);
    assert!(binding.is_current());
    assert_eq!(binding.kind(), Some(ChannelKind::Slack));
    assert_eq!(binding.chat_type(), Some(ChatType::P2p));
    // 绑定行的第 1 代在同一个事务里就建好了（`generation` CTE）。
    let generation = repo
        .get_generation(first, 1)
        .await
        .expect("get generation")
        .expect("第 1 代");
    assert_eq!(generation.chat_session_id(), first);
    assert_eq!(generation.initiator_user_id(), None);
}

/// `/new`：退休当前路由（老代收口）→ 建显式 Chat → 装下一代；首条正文落在**新**会话里。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn start_route_retires_the_old_generation_and_opens_the_next() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let mut input = fixture.ensure(ChannelKind::Lark, &key("start"));
    input.kind = ChannelKind::Lark;
    let first_session = repo.ensure_session(&input).await.expect("ensure");

    let outcome = repo
        .start_route(&NewStartRoute {
            session: input.clone(),
            initiator: fixture.user_id,
            body: "second life".to_string(),
            first_title: "second life".to_string(),
            message_id: "om-new".to_string(),
            thread_id: String::new(),
            sender_channel_id: "ou_1".to_string(),
            claim_token: None,
            media_pending_seconds: 0.0,
            persist_message: true,
            history_boundary_pending: false,
        })
        .await
        .expect("start route");
    let StartRouteOutcome::Started(started) = outcome else {
        panic!("expected Started, got {outcome:?}");
    };
    assert_ne!(started.session_id, first_session, "/new 造的是新 Chat");
    assert_eq!(started.route_revision, 2, "路由代际 +1");
    assert_eq!(started.context_revision, 1, "新 Chat 的上下文从第 1 代开始");
    assert_eq!(started.initial_title, "second life");
    assert!(started.first_message_id.is_some());
    assert_eq!(started.pending_contexts.len(), 1);
    assert_eq!(
        started.pending_contexts[0].initiator_user_id(),
        Some(fixture.user_id)
    );

    // 老路由被退休，历史边界收在第 2 条消息上；新路由是唯一当前行。
    let retired = repo
        .get_current_binding(input.installation_id, &input.binding_key)
        .await
        .expect("get current")
        .expect("新路由是当前行");
    assert_eq!(retired.chat_session_id(), started.session_id);
    assert_eq!(retired.route_revision, 2);
    let all = repo
        .list_bindings_by_session(first_session)
        .await
        .expect("list by old session");
    assert_eq!(all.len(), 1);
    assert!(all[0].retired_at.is_some(), "老路由必须被退休");
    assert_eq!(all[0].history_end_message_id.as_deref(), Some("om-new"));

    // 老会话里没有留下任何消息（`/new` 的正文写进新会话）。
    assert_eq!(fixture.message_count(first_session).await, 0);
    assert_eq!(fixture.message_count(started.session_id).await, 1);
}

/// append 的第一条消息：变可见、初始化标题、快照发起人与 reply target。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn append_records_provenance_and_the_reply_target() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let input = fixture.ensure(ChannelKind::Telegram, &key("append"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");

    let outcome = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: "hello there".to_string(),
            first_title: "hello there".to_string(),
            is_command: false,
            message_id: "tg-1".to_string(),
            thread_id: String::new(),
            sender_channel_id: "tg-user-1".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 0.0,
            force_fresh: false,
            has_media: false,
        })
        .await
        .expect("append");
    let AppendOutcome::Appended(appended) = outcome else {
        panic!("expected Appended, got {outcome:?}");
    };
    assert_eq!(appended.context_revision, 1);
    assert!(appended.became_visible, "隐式会话的第一条正文让它变可见");
    assert_eq!(appended.initial_title.as_deref(), Some("hello there"));
    assert!(!appended.dedup_marked, "没有认领令牌就没有 in-tx mark");
    assert_eq!(appended.route_revision, 1);
    assert_eq!(appended.pending_contexts.len(), 1);
    assert_eq!(appended.pending_contexts[0].revision, 1);

    let (title, rev, ingested): (String, Option<i64>, bool) = sqlx::query_as(
        "SELECT s.title, m.channel_context_revision, m.channel_ingested \
         FROM chat_session AS s JOIN chat_message AS m ON m.chat_session_id = s.id \
         WHERE m.id = $1",
    )
    .bind(appended.message_id.expect("message id").0)
    .fetch_one(&fixture.pool)
    .await
    .expect("read message");
    assert_eq!(title, "hello there");
    assert_eq!(rev, Some(1));
    assert!(ingested, "channel_ingested 是不可变出处戳");

    let generation = repo
        .get_generation(session_id, 1)
        .await
        .expect("get generation")
        .expect("第 1 代");
    assert_eq!(generation.initiator_user_id(), Some(fixture.user_id));
    assert_eq!(generation.last_message_id.as_deref(), Some("tg-1"));
    assert_eq!(generation.last_sender_id.as_deref(), Some("tg-user-1"));
    assert!(!generation.pending_fresh);
}

/// **代际语义（本片的硬项）**：`/clear` 之后的新消息进新代，**老代的行不被覆盖**，
/// 而且按老 revision 读到的永远只有老代自己的发起人 / reply target。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn clearing_advances_the_generation_and_never_rewrites_the_old_one() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let input = fixture.ensure(ChannelKind::WeCom, &key("generation"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");

    let AppendOutcome::Appended(first) = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: "before clear".to_string(),
            first_title: "before clear".to_string(),
            is_command: false,
            message_id: "wc-1".to_string(),
            thread_id: String::new(),
            sender_channel_id: "wc-user-1".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 0.0,
            force_fresh: false,
            has_media: false,
        })
        .await
        .expect("first append")
    else {
        panic!("expected Appended");
    };
    assert_eq!(first.context_revision, 1);

    let other_sender = Id::new();
    let AppendOutcome::Appended(second) = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: other_sender,
            installation_id: input.installation_id,
            body: "after clear".to_string(),
            first_title: String::new(),
            is_command: false,
            message_id: "wc-2".to_string(),
            thread_id: String::new(),
            sender_channel_id: "wc-user-2".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 0.0,
            force_fresh: true,
            has_media: false,
        })
        .await
        .expect("fresh append")
    else {
        panic!("expected Appended");
    };
    assert_eq!(second.context_revision, 2, "/clear 把上下文推进到第 2 代");
    assert!(!second.became_visible, "标题已经落过了，不再换");
    assert_eq!(second.initial_title, None);

    // 老代：边界收在触发它的那条消息上，发起人**仍是**第 1 代的那个人。
    let old = repo
        .get_generation(session_id, 1)
        .await
        .expect("get old")
        .expect("老代还在（不是删掉，是收口）");
    assert_eq!(old.initiator_user_id(), Some(fixture.user_id));
    assert_eq!(old.history_end_message_id.as_deref(), Some("wc-2"));
    assert_eq!(old.last_message_id.as_deref(), Some("wc-1"));
    // 新代：从触发消息开始，属于新的发起人。
    let new = repo
        .get_generation(session_id, 2)
        .await
        .expect("get new")
        .expect("第 2 代");
    assert_eq!(new.initiator_user_id(), Some(other_sender));
    assert_eq!(new.history_start_message_id.as_deref(), Some("wc-2"));
    assert_eq!(new.last_message_id.as_deref(), Some("wc-2"));
    assert!(new.pending_fresh, "新代带着 fresh 意图等下一次任务入队消费");
    assert_eq!(
        repo.binding_pending_fresh(session_id)
            .await
            .expect("binding pending fresh"),
        Some(true)
    );
    assert_eq!(
        repo.clear_pending_fresh_for_revision(session_id, 2)
            .await
            .expect("consume"),
        1
    );
    assert_eq!(
        repo.binding_pending_fresh(session_id)
            .await
            .expect("after consume"),
        Some(false)
    );
    // 两条消息各自钉在自己的代上（这正是"老代读不到新代上下文"的数据面）。
    let revisions: Vec<Option<i64>> = sqlx::query_scalar(
        "SELECT channel_context_revision FROM chat_message WHERE chat_session_id = $1 \
         ORDER BY created_at ASC",
    )
    .bind(session_id.0)
    .fetch_all(&fixture.pool)
    .await
    .expect("read revisions");
    assert_eq!(revisions, vec![Some(1), Some(2)]);
}

/// 裸 `/clear`：**不留消息行**，但代际推进、fresh 意图落在代际行与绑定行上。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn mark_pending_fresh_advances_without_writing_a_message() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let input = fixture.ensure(ChannelKind::DingTalk, &key("fresh"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");

    let outcome = repo
        .mark_pending_fresh(session_id, "dt-1", None)
        .await
        .expect("mark pending fresh");
    let AppendOutcome::Appended(appended) = outcome else {
        panic!("expected Appended, got {outcome:?}");
    };
    assert_eq!(appended.message_id, None, "裸命令没有正文行");
    assert_eq!(appended.context_revision, 2);
    assert_eq!(fixture.message_count(session_id).await, 0);

    let new = repo
        .get_generation(session_id, 2)
        .await
        .expect("get generation")
        .expect("第 2 代");
    assert!(new.pending_fresh);
    assert!(
        new.history_boundary_pending,
        "裸命令没有平台游标 ⇒ 边界待定"
    );
    assert_eq!(new.history_end_message_id.as_deref(), None, "新代还开着");
    let old = repo
        .get_generation(session_id, 1)
        .await
        .expect("get old")
        .expect("老代还在");
    assert!(
        !old.history_boundary_pending,
        "ensure 开出来的第 1 代没有待定边界（它是从会话第一次接触开始的）"
    );
    assert_eq!(
        old.history_end_message_id.as_deref(),
        Some("dt-1"),
        "老代收口"
    );
    assert_eq!(
        repo.binding_pending_fresh(session_id)
            .await
            .expect("binding pending fresh"),
        Some(true),
        "裸 /clear 也必须让绑定行带上 fresh 意图"
    );
}

/// 两阶段幂等**在事务里**：认领令牌有效 ⇒ 消息 + `processed_at` 一起提交；
/// 令牌被抢走 ⇒ `ClaimLost` 且**一条消息都没留下**（回滚，不是半条）。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn the_in_transaction_dedup_mark_commits_with_the_message_or_not_at_all() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let dedup = crate::channel::dedup::ChannelInboundDedupRepo::new(fixture.db.clone());
    let input = fixture.ensure(ChannelKind::Slack, &key("dedup"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");

    let claim = dedup
        .claim(input.installation_id, "sl-1")
        .await
        .expect("claim")
        .expect("claimed");
    let AppendOutcome::Appended(appended) = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: "with claim".to_string(),
            first_title: "with claim".to_string(),
            is_command: false,
            message_id: "sl-1".to_string(),
            thread_id: String::new(),
            sender_channel_id: "sl-user".to_string(),
            dedup_message_id: String::new(),
            claim_token: Some(claim.claim_token()),
            media_pending_seconds: 0.0,
            force_fresh: false,
            has_media: false,
        })
        .await
        .expect("append")
    else {
        panic!("expected Appended");
    };
    assert!(appended.dedup_marked, "binder 在自己的事务里落定了认领");
    let stored = dedup
        .get(input.installation_id, "sl-1")
        .await
        .expect("get")
        .expect("row");
    assert!(stored.is_terminal(), "processed_at 与消息同一次提交落下");

    // 令牌被抢走（这里用一个假令牌模拟）：整条流水线回滚，消息不留。
    let before = fixture.message_count(session_id).await;
    let outcome = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: "lost claim".to_string(),
            first_title: String::new(),
            is_command: false,
            message_id: "sl-2".to_string(),
            thread_id: String::new(),
            sender_channel_id: "sl-user".to_string(),
            dedup_message_id: String::new(),
            claim_token: Some(Id::new()),
            media_pending_seconds: 0.0,
            force_fresh: false,
            has_media: false,
        })
        .await
        .expect("append with a stolen token");
    assert_eq!(outcome, AppendOutcome::ClaimLost);
    assert_eq!(
        fixture.message_count(session_id).await,
        before,
        "ClaimLost 必须回滚自己的在途写入"
    );
}

/// 拿着**已被退休**的路由行 append ⇒ `RouteChanged`（Router 重新解析并重试），不静默写老会话。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn appending_against_a_retired_route_is_a_route_change() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let input = fixture.ensure(ChannelKind::Lark, &key("route-change"));
    let old_session = repo.ensure_session(&input).await.expect("ensure");
    let started = repo
        .start_route(&NewStartRoute {
            session: input.clone(),
            initiator: fixture.user_id,
            body: "/new".to_string(),
            first_title: "/new".to_string(),
            message_id: "om-rotate".to_string(),
            thread_id: String::new(),
            sender_channel_id: "ou_1".to_string(),
            claim_token: None,
            media_pending_seconds: 0.0,
            persist_message: false,
            history_boundary_pending: false,
        })
        .await
        .expect("start route");
    assert!(matches!(started, StartRouteOutcome::Started(_)));

    let outcome = repo
        .append_message(&NewChannelAppend {
            session_id: old_session,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: "late".to_string(),
            first_title: String::new(),
            is_command: false,
            message_id: "om-late".to_string(),
            thread_id: String::new(),
            sender_channel_id: "ou_1".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 0.0,
            force_fresh: false,
            has_media: false,
        })
        .await
        .expect("append against a retired route");
    assert_eq!(outcome, AppendOutcome::RouteChanged);
    assert_eq!(
        fixture.message_count(old_session).await,
        0,
        "老会话里一条都不许落"
    );
}

/// 未认领代际列表：还有没被任务认领的输入的代际（崩溃恢复靠它重建去抖窗口）。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn unowned_context_revisions_list_every_generation_with_pending_input() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let input = fixture.ensure(ChannelKind::Slack, &key("unowned"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");
    for (index, message_id) in ["sl-a", "sl-b"].iter().enumerate() {
        repo.append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: format!("turn {index}"),
            first_title: String::new(),
            is_command: false,
            message_id: (*message_id).to_string(),
            thread_id: String::new(),
            sender_channel_id: "sl-user".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 0.0,
            force_fresh: index == 1,
            has_media: false,
        })
        .await
        .expect("append");
    }
    let pending = repo
        .list_unowned_context_revisions(session_id)
        .await
        .expect("list unowned");
    assert_eq!(
        pending.iter().map(|row| row.revision).collect::<Vec<_>>(),
        vec![1, 2],
        "两代都有未认领输入 ⇒ 两代都要被恢复"
    );
    assert_eq!(pending[0].initiator_user_id(), Some(fixture.user_id));

    // 命令行不进列表（控制面轮次不是 agent 输入）。
    let _ = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: "/issue x".to_string(),
            first_title: String::new(),
            is_command: true,
            message_id: "sl-cmd".to_string(),
            thread_id: String::new(),
            sender_channel_id: "sl-user".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 0.0,
            force_fresh: false,
            has_media: false,
        })
        .await
        .expect("append command");
    assert_eq!(
        repo.list_unowned_context_revisions(session_id)
            .await
            .expect("list unowned")
            .len(),
        2,
        "命令行（channel_command）不计入未认领输入"
    );
}
