//! `ChatTaskRepo::send_direct_chat_message` 的真库语义（M4-4 / LUM-1601）。
//!
//! 这是 chat 面**唯一**把 12 条上游 query 收进一个事务的方法（`docs/45` §2 的顺序表），
//! 也正是本片优先级最高的对象：锁顺序、锁内重读、标题 CAS 的两级回退、附件「只绑无主行」、
//! 归属栅栏，全在只有 PostgreSQL 能回答的范围里。
//!
//! HTTP 侧的形状（400/403/409 的映射、`attachment_ids` 的 `null` vs `[]`）由
//! `crates/mc-http/tests/chat/broadcast.rs` 与 `chat.rs` 覆盖；这里只钉落库语义。

use std::time::Duration;

use uuid::Uuid;

use super::{
    insert_message, message_row, new_session, raw_session, raw_task, setup, stub_derive_title,
    teardown, Fixture, MessageSeed, TaskSeed,
};
use crate::chat_task::{ChatSendError, DirectChatSend, PRIORITY_CHAT};

/// 本文件用的 send 入参（每个用例只换 `content` / `attachment_ids`）。
fn direct<'a>(
    fixture: &'a Fixture,
    content: &'a str,
    attachment_ids: &'a [Uuid],
) -> DirectChatSend<'a> {
    DirectChatSend {
        session_id: fixture.session_id,
        agent_id: fixture.agent_id,
        initiator_user_id: fixture.user_id,
        content,
        attachment_ids,
        uploader_type: "member",
        uploader_id: fixture.user_id,
        derive_title: stub_derive_title,
    }
}

/// 再插一台 runtime（rebind 用例要两台不同的）。
async fn new_runtime(fixture: &Fixture, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
         VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(name)
    .bind(fixture.user_id)
    .fetch_one(fixture.pool())
    .await
    .expect("insert agent_runtime")
}

/// 插一条附件。绑定的全部条件由调用方通过这两个可空列 + `uploader` 控制。
async fn new_attachment(
    fixture: &Fixture,
    uploader: Uuid,
    filename: &str,
    chat_session_id: Option<Uuid>,
    chat_message_id: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO attachment(workspace_id, uploader_type, uploader_id, filename, url, \
                                content_type, size_bytes, chat_session_id, chat_message_id) \
         VALUES ($1, 'member', $2, $3, $4, 'image/png', 1024, $5, $6) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(uploader)
    .bind(filename)
    .bind(format!("https://files.test/{filename}"))
    .bind(chat_session_id)
    .bind(chat_message_id)
    .fetch_one(fixture.pool())
    .await
    .expect("insert attachment")
}

async fn attachment_owner(fixture: &Fixture, attachment_id: Uuid) -> (Option<Uuid>, Option<Uuid>) {
    sqlx::query_as("SELECT chat_message_id, chat_session_id FROM attachment WHERE id = $1")
        .bind(attachment_id)
        .fetch_one(fixture.pool())
        .await
        .expect("load attachment row")
}

// ---------------------------------------------------------------------------
// 一个事务写完整轮
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)] // 12 步事务的投影各要一组断言，拆开会丢掉「同一事务」这个前提
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_writes_the_task_message_and_session_touch_in_one_transaction() {
    let Some(fixture) = setup().await else {
        println!("skip send_writes_the_task_message_and_session_touch_in_one_transaction: no env");
        return;
    };
    let before = raw_session(&fixture, fixture.session_id).await;

    let sent = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "hello world", &[]))
        .await
        .expect("send");

    // 产物一：任务行（`CreateChatTask` + `SetChatTaskInputOwnerSelf`）。
    assert!(!sent.queued, "首个回合不是追问");
    assert_eq!(sent.task.status, "queued");
    assert_eq!(sent.task.priority, PRIORITY_CHAT);
    assert_eq!(sent.task.runtime_id, Some(fixture.runtime_id));
    assert_eq!(sent.task.chat_session_id, Some(fixture.session_id));
    assert_eq!(
        sent.task.chat_input_task_id,
        Some(sent.task.id),
        "第 6 步必须把输入批次归给自己"
    );
    assert!(sent.task.completed_at.is_none());

    // 产物一（旁证）：归属三元组与两个「恒 NULL」的列。
    let task = raw_task(&fixture, sent.task.id).await;
    assert_eq!(
        task.chat_input_task_id,
        Some(sent.task.id),
        "第 6 步的落库值"
    );
    assert!(
        task.regenerate_quick_actions_for.is_none(),
        "直聊轮不是背景重生成"
    );
    assert_eq!(task.originator_source.as_deref(), Some("direct_human"));
    assert_eq!(task.trigger_evidence_kind.as_deref(), Some("chat"));
    assert_eq!(task.trigger_evidence_ref_id, Some(fixture.session_id));
    // `buildRuntimeMCPOverlay` 不可达（本仓无 Composio）⇒ 恒 NULL；`fire_at` 只给 deferred 排程。
    assert!(
        task.runtime_mcp_overlay.is_none(),
        "runtime_mcp_overlay 必须恒 NULL"
    );
    assert!(task.fire_at.is_none());
    assert!(task.channel_context_revision.is_none(), "直聊不带代际");

    // 产物二：user 消息（kind 固定 'message'，落到本任务的输入批次）。
    assert_eq!(sent.message.role, "user");
    assert_eq!(sent.message.message_kind, "message");
    assert_eq!(sent.message.content, "hello world");
    assert_eq!(sent.message.task_id, Some(sent.task.id));
    assert_eq!(sent.message.chat_session_id, fixture.session_id);
    assert!(!sent.message.channel_ingested);
    assert_eq!(sent.message.quick_actions, serde_json::json!([]));

    // 产物三：标题 CAS（空标题 + 无公开 user 行 ⇒ 命中）。
    assert_eq!(sent.initial_title, "hello world");
    // 产物四：会话 touch。
    let after = raw_session(&fixture, fixture.session_id).await;
    assert_eq!(after.title, "hello world");
    assert!(
        after.updated_at > before.updated_at,
        "第 12 步必须 touch 会话：{} -> {}",
        before.updated_at,
        after.updated_at
    );
    assert_eq!(after.status, "active");

    // 公开 user 消息的判定（handler 用它决定是否首轮）。
    assert!(fixture
        .repo()
        .session_has_public_user_message(fixture.session_id)
        .await
        .expect("has public"));
    assert!(fixture
        .repo()
        .session_is_channel_backed(fixture.session_id)
        .await
        .expect("channel backed")
        .is_none());

    teardown(&fixture).await;
}

/// 第二个回合起 `queued = true`，且**不**覆盖标题 —— 同时钉住「本 surface 没有客户端 id
/// 去重」这一事实：同样的正文发两次会落两个任务两行消息，幂等性只存在于收编 / CAS /
/// 附件绑定三处（见下面的用例）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn second_send_reports_queued_and_does_not_rewrite_the_title() {
    let Some(fixture) = setup().await else {
        println!("skip second_send_reports_queued_and_does_not_rewrite_the_title: no env");
        return;
    };
    let repo = fixture.repo();
    let first = repo
        .send_direct_chat_message(direct(&fixture, "first turn", &[]))
        .await
        .expect("first send");
    let second = repo
        .send_direct_chat_message(direct(&fixture, "first turn", &[]))
        .await
        .expect("second send");

    assert!(!first.queued);
    assert!(second.queued, "插入前已有一条可见轮 ⇒ 位置语义是追问");
    assert_ne!(
        first.task.id, second.task.id,
        "没有客户端 id 去重：两轮两个任务"
    );
    assert_ne!(first.message.id, second.message.id);
    assert_eq!(second.initial_title, "", "CAS 已经用过一次，第二次不命中");
    assert_eq!(
        raw_session(&fixture, fixture.session_id).await.title,
        "first turn"
    );

    // 两行 user 消息都属于各自的任务（可见头不变式要求 follow-up 先藏起来）。
    let rows = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM chat_message WHERE chat_session_id = $1 AND role = 'user'",
    )
    .bind(fixture.session_id)
    .fetch_one(fixture.pool())
    .await
    .expect("count user messages");
    assert_eq!(rows, 2);

    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// 锁内重读（rebind）
// ---------------------------------------------------------------------------

/// 任务挂的 runtime 取自**锁内重读的 agent 行**，不是会话快照，也不是调用方缓存。
///
/// 形态：会话的 `runtime_id` 故意留成陈旧值 A，agent 指向 B ⇒ 任务必须挂 B；再把 agent
/// 重绑到 C，第二轮必须挂 C。上游 `LockChatSessionForRuntimeBind` 存在的理由就是这条。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_rereads_the_agent_runtime_instead_of_the_session_snapshot() {
    let Some(fixture) = setup().await else {
        println!("skip send_rereads_the_agent_runtime_instead_of_the_session_snapshot: no env");
        return;
    };
    let stale = new_runtime(&fixture, "itest-stale").await;
    let rebound = new_runtime(&fixture, "itest-rebound").await;
    // 会话快照停在 stale；agent 是权威。
    sqlx::query("UPDATE chat_session SET runtime_id = $2 WHERE id = $1")
        .bind(fixture.session_id)
        .bind(stale)
        .execute(fixture.pool())
        .await
        .expect("stale session runtime");

    let repo = fixture.repo();
    let first = repo
        .send_direct_chat_message(direct(&fixture, "turn one", &[]))
        .await
        .expect("first send");
    assert_eq!(
        first.task.runtime_id,
        Some(fixture.runtime_id),
        "必须用 agent 行（锁内重读）而不是会话里那个陈旧 runtime"
    );

    // 并发 rebind 之后发下一轮：新任务必须跟着新 runtime 走。
    sqlx::query("UPDATE agent SET runtime_id = $2 WHERE id = $1")
        .bind(fixture.agent_id)
        .bind(rebound)
        .execute(fixture.pool())
        .await
        .expect("rebind agent");
    let second = repo
        .send_direct_chat_message(direct(&fixture, "turn two", &[]))
        .await
        .expect("second send");
    assert_eq!(second.task.runtime_id, Some(rebound));
    assert_eq!(
        raw_session(&fixture, fixture.session_id).await.runtime_id,
        Some(stale),
        "发送不改会话快照那一列 —— 权威在 agent"
    );

    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// 三个 409 与一个 500
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_reports_archived_session_archived_agent_and_missing_runtime() {
    let Some(fixture) = setup().await else {
        println!("skip send_reports_archived_session_archived_agent_and_missing_runtime: no env");
        return;
    };
    let repo = fixture.repo();

    // 不存在的会话：第 1 步 `FOR UPDATE` 取不到行 ⇒ 500（不是 404，上游同款）。
    let err = repo
        .send_direct_chat_message(DirectChatSend {
            session_id: Uuid::new_v4(),
            ..direct(&fixture, "gone", &[])
        })
        .await
        .expect_err("missing session");
    assert!(
        matches!(err, ChatSendError::Repo(crate::RepoError::NotFound)),
        "{err}"
    );

    // 归档会话：锁内重读 `status`（调用方只给了 id，所以这条能证明确实在锁内重读）。
    let archived = new_session(&fixture, fixture.agent_id, "archived").await;
    let err = repo
        .send_direct_chat_message(DirectChatSend {
            session_id: archived,
            ..direct(&fixture, "archived session", &[])
        })
        .await
        .expect_err("archived session");
    assert!(matches!(err, ChatSendError::SessionArchived), "{err}");
    assert_eq!(
        count_messages(&fixture, archived).await,
        0,
        "拒绝必须发生在任何写入之前（事务里无残留行）"
    );

    // 归档 agent：第 3 步 `archived_at IS NOT NULL`。
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(fixture.agent_id)
        .execute(fixture.pool())
        .await
        .expect("archive agent");
    let err = repo
        .send_direct_chat_message(direct(&fixture, "archived agent", &[]))
        .await
        .expect_err("archived agent");
    assert!(matches!(err, ChatSendError::AgentArchived), "{err}");

    // 没有 runtime：`agent.runtime_id IS NULL`（先解开归档）。
    sqlx::query("UPDATE agent SET archived_at = NULL, runtime_id = NULL WHERE id = $1")
        .bind(fixture.agent_id)
        .execute(fixture.pool())
        .await
        .expect("clear runtime");
    let err = repo
        .send_direct_chat_message(direct(&fixture, "no runtime", &[]))
        .await
        .expect_err("no runtime");
    assert!(matches!(err, ChatSendError::NoRuntime), "{err}");

    assert_eq!(
        count_messages(&fixture, fixture.session_id).await,
        0,
        "四条拒绝路径都不许留下孤儿 user 消息（上游注释：过期客户端先落消息再 500 是缺陷）"
    );
    teardown(&fixture).await;
}

async fn count_messages(fixture: &Fixture, session_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM chat_message WHERE chat_session_id = $1")
        .bind(session_id)
        .fetch_one(fixture.pool())
        .await
        .expect("count messages")
}

// ---------------------------------------------------------------------------
// 附件绑定（第 10 / 11 步）
// ---------------------------------------------------------------------------

/// `LinkAttachmentsToChatMessage` 只绑「无主」的行，且必须同 workspace + 同上传者：
/// 别人的附件、已经挂过消息的、已经归属别的会话的都要被排除，且返回的是**实际绑上**的 id。
#[allow(clippy::too_many_lines)] // 六个附件各是一种拒绝理由
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_binds_only_ownerless_attachments_from_the_same_uploader() {
    let Some(fixture) = setup().await else {
        println!("skip send_binds_only_ownerless_attachments_from_the_same_uploader: no env");
        return;
    };
    let other_user: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-other', $1) RETURNING id"#,
    )
    .bind(format!("itest-other-{}@example.com", Uuid::new_v4()))
    .fetch_one(fixture.pool())
    .await
    .expect("insert other user");
    let other_session = new_session(&fixture, fixture.agent_id, "active").await;
    let bound_elsewhere = insert_message(
        &fixture,
        fixture.session_id,
        MessageSeed::user("earlier turn"),
    )
    .await;

    let clean = new_attachment(&fixture, fixture.user_id, "a.png", None, None).await;
    let already_linked = new_attachment(
        &fixture,
        fixture.user_id,
        "b.png",
        None,
        Some(bound_elsewhere),
    )
    .await;
    let foreign_uploader = new_attachment(&fixture, other_user, "c.png", None, None).await;
    let foreign_session = new_attachment(
        &fixture,
        fixture.user_id,
        "d.png",
        Some(other_session),
        None,
    )
    .await;
    let requested = [
        clean,
        already_linked,
        foreign_uploader,
        foreign_session,
        Uuid::new_v4(), // 不存在的 id
    ];

    let sent = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "with files", &requested))
        .await
        .expect("send with attachments");

    assert_eq!(
        sent.bound_attachment_ids,
        vec![clean],
        "只有无主 + 同上传者的那一条能被绑上"
    );
    assert_eq!(
        attachment_owner(&fixture, clean).await,
        (Some(sent.message.id), Some(fixture.session_id)),
        "绑定时要同时回填 message 与会话两列"
    );
    // 其余四条保持原样（别人的 / 已挂的 / 别的会话的 / 不存在的）。
    assert_eq!(
        attachment_owner(&fixture, already_linked).await,
        (Some(bound_elsewhere), None)
    );
    assert_eq!(
        attachment_owner(&fixture, foreign_uploader).await,
        (None, None)
    );
    assert_eq!(
        attachment_owner(&fixture, foreign_session).await,
        (None, Some(other_session))
    );

    // 幂等：同一条附件再发一次不会再被绑（`chat_message_id IS NULL` 已经不成立）。
    let again = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "with files again", &requested))
        .await
        .expect("second send");
    assert!(again.bound_attachment_ids.is_empty(), "重复请求不重复绑定");

    teardown(&fixture).await;
}

/// 纯附件轮的标题取自第一个绑上的附件名（`InitializeChatSessionMediaTitle`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_uses_the_attachment_filename_for_a_media_only_turn() {
    let Some(fixture) = setup().await else {
        println!("skip send_uses_the_attachment_filename_for_a_media_only_turn: no env");
        return;
    };
    let shot = new_attachment(&fixture, fixture.user_id, "diagram.png", None, None).await;
    let sent = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "   ", &[shot]))
        .await
        .expect("media only send");

    assert_eq!(sent.bound_attachment_ids, vec![shot]);
    assert_eq!(sent.initial_title, "diagram.png", "正文空白 ⇒ 退回附件名");
    assert_eq!(
        raw_session(&fixture, fixture.session_id).await.title,
        "diagram.png"
    );

    // 正文非空白时不走附件分支（正文的 derive 已经赢过）。
    let second = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "look at this", &[]))
        .await
        .expect("text send");
    assert_eq!(second.initial_title, "", "标题已被上一轮占用");
    assert_eq!(
        raw_session(&fixture, fixture.session_id).await.title,
        "diagram.png"
    );

    teardown(&fixture).await;
}

/// 标题 CAS 的两道闸：`title = ''` 与「没有别的公开 user 行」（`channel_command` 不算）；
/// 媒体标题 CAS 多一道「本轮是唯一公开 user 行」。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_skips_the_title_cas_when_the_title_or_the_turn_is_taken() {
    let Some(fixture) = setup().await else {
        println!("skip send_skips_the_title_cas_when_the_title_or_the_turn_is_taken: no env");
        return;
    };
    let repo = fixture.repo();

    // (a) 手工改过名 ⇒ 永不被覆盖。
    let manual = new_session(&fixture, fixture.agent_id, "active").await;
    sqlx::query("UPDATE chat_session SET title = 'manual name' WHERE id = $1")
        .bind(manual)
        .execute(fixture.pool())
        .await
        .expect("set manual title");
    let sent = repo
        .send_direct_chat_message(DirectChatSend {
            session_id: manual,
            ..direct(&fixture, "should not rename", &[])
        })
        .await
        .expect("send into manual session");
    assert_eq!(sent.initial_title, "");
    assert_eq!(raw_session(&fixture, manual).await.title, "manual name");

    // (b) 已有公开 user 行（不是第一轮）⇒ 空标题也不初始化。
    let second = new_session(&fixture, fixture.agent_id, "active").await;
    insert_message(&fixture, second, MessageSeed::user("earlier")).await;
    let sent = repo
        .send_direct_chat_message(DirectChatSend {
            session_id: second,
            ..direct(&fixture, "second turn", &[])
        })
        .await
        .expect("send into started session");
    assert_eq!(sent.initial_title, "");
    assert_eq!(raw_session(&fixture, second).await.title, "");

    // (c) 只有渠道控制面记录的行 ⇒ 不算「第一轮已过」，CAS 命中。
    let channel_only = new_session(&fixture, fixture.agent_id, "active").await;
    insert_message(
        &fixture,
        channel_only,
        MessageSeed::user("[control]").kind("channel_command"),
    )
    .await;
    let sent = repo
        .send_direct_chat_message(DirectChatSend {
            session_id: channel_only,
            ..direct(&fixture, "first public turn", &[])
        })
        .await
        .expect("send into channel-only session");
    assert_eq!(sent.initial_title, "first public turn");
    assert_eq!(
        raw_session(&fixture, channel_only).await.title,
        "first public turn"
    );

    // (d) 超长正文 ⇒ 会话名按与产品同源的截断规则（`TITLE_LIMIT = 30`，第 30 个字符换成 `…`）。
    let long = new_session(&fixture, fixture.agent_id, "active").await;
    let body = "0123456789abcdefghijklmnopqrstuvwxyz";
    let sent = repo
        .send_direct_chat_message(DirectChatSend {
            session_id: long,
            ..direct(&fixture, body, &[])
        })
        .await
        .expect("send a long first turn");
    assert_eq!(sent.initial_title, "0123456789abcdefghijklmnopqrs…");
    assert_eq!(raw_session(&fixture, long).await.title, sent.initial_title);
    assert_eq!(sent.initial_title.chars().count(), 30);

    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// 孤儿 kickoff 收编（第 7 步）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_adopts_only_the_orphan_onboarding_kickoff() {
    let Some(fixture) = setup().await else {
        println!("skip send_adopts_only_the_orphan_onboarding_kickoff: no env");
        return;
    };
    let earlier_task = super::insert_task(&fixture, fixture.session_id, TaskSeed::queued()).await;
    let orphan = insert_message(
        &fixture,
        fixture.session_id,
        MessageSeed::user("hello mika").kind("onboarding_kickoff"),
    )
    .await;
    let already_owned = insert_message(
        &fixture,
        fixture.session_id,
        MessageSeed::user("owned kickoff")
            .kind("onboarding_kickoff")
            .on(earlier_task),
    )
    .await;
    let plain_orphan =
        insert_message(&fixture, fixture.session_id, MessageSeed::user("no knd")).await;

    let sent = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "first real turn", &[]))
        .await
        .expect("send");

    assert_eq!(
        message_row(&fixture, orphan).await.task_id,
        Some(sent.task.id),
        "无主 kickoff 要被本任务收编（幂等：再来一轮也只是同一 UPDATE）"
    );
    assert_eq!(
        message_row(&fixture, already_owned).await.task_id,
        Some(earlier_task),
        "已经被收养的 kickoff 不许改嫁"
    );
    assert_eq!(
        message_row(&fixture, plain_orphan).await.task_id,
        None,
        "只有 message_kind = 'onboarding_kickoff' 的无主行才被收编"
    );

    // 再发一轮：kickoff 仍指向第一个任务，不重复收编。
    let second = fixture
        .repo()
        .send_direct_chat_message(direct(&fixture, "second turn", &[]))
        .await
        .expect("second send");
    assert_eq!(
        message_row(&fixture, orphan).await.task_id,
        Some(sent.task.id)
    );
    assert_ne!(second.task.id, sent.task.id);

    teardown(&fixture).await;
}

// ---------------------------------------------------------------------------
// 归属栅栏 / 锁顺序
// ---------------------------------------------------------------------------

/// `lock_task_owner_rows` 真的在**写入语句自己**的 WHERE 里：持锁方不放手，发送就一直等。
///
/// 两段各自对应上游 `284_task_owner_row_fence` 的一个窗口：workspace 的 `FOR UPDATE`
/// （工作区拆除）与 runtime 的 `FOR UPDATE`（legacy runtime 合并）。栅栏用 `FOR KEY SHARE`
/// 与之相撞，所以这里用「超时即证明被挡住」来观测 —— 不靠时序猜。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_blocks_on_the_owner_fence_until_the_delete_side_lets_go() {
    let Some(fixture) = setup().await else {
        println!("skip send_blocks_on_the_owner_fence_until_the_delete_side_lets_go: no env");
        return;
    };
    let repo = fixture.repo();

    for (label, statement) in [
        (
            "workspace teardown",
            "SELECT id FROM workspace WHERE id = $1 FOR UPDATE",
        ),
        (
            "runtime merge",
            "SELECT id FROM agent_runtime WHERE id = $1 FOR UPDATE",
        ),
    ] {
        let lock_id = if label == "workspace teardown" {
            fixture.workspace_id
        } else {
            fixture.runtime_id
        };
        let mut holder = fixture.pool().begin().await.expect("holder tx");
        sqlx::query(statement)
            .bind(lock_id)
            .fetch_optional(&mut *holder)
            .await
            .expect("take owner lock");

        let blocked = tokio::time::timeout(
            Duration::from_millis(500),
            repo.send_direct_chat_message(direct(&fixture, "races the delete", &[])),
        )
        .await;
        assert!(blocked.is_err(), "{label}: 发送没被归属栅栏挡住");

        holder.rollback().await.expect("release owner lock");
        // 锁一放，同一笔发送就能落库（栅栏返回 true）。
        let sent = tokio::time::timeout(
            Duration::from_secs(15),
            repo.send_direct_chat_message(direct(&fixture, "after the delete", &[])),
        )
        .await
        .expect("send after unlock")
        .expect("send after unlock");
        assert_eq!(
            raw_task(&fixture, sent.task.id).await.runtime_id,
            Some(fixture.runtime_id)
        );
    }

    teardown(&fixture).await;
}

/// 锁顺序 `chat_session` → `agent`：在 agent 行锁被别人拿着时，发送**已经持有会话行锁**。
///
/// 观测手法：占住 agent 行的 `FOR UPDATE`，让发送停在第三步；然后用 `FOR UPDATE NOWAIT`
/// 探会话行 —— 若发送还没持锁，探针会抢到；一旦抢不到（55P03）就证明会话锁在前。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn send_takes_the_session_lock_before_the_agent_lock() {
    let Some(fixture) = setup().await else {
        println!("skip send_takes_the_session_lock_before_the_agent_lock: no env");
        return;
    };
    let pool = fixture.pool().clone();

    let mut holder = pool.begin().await.expect("holder tx");
    sqlx::query("SELECT id FROM agent WHERE id = $1 FOR UPDATE")
        .bind(fixture.agent_id)
        .fetch_optional(&mut *holder)
        .await
        .expect("take agent lock");

    let repo = fixture.repo();
    let session_id = fixture.session_id;
    let agent_id = fixture.agent_id;
    let user_id = fixture.user_id;
    let sender = tokio::spawn(async move {
        repo.send_direct_chat_message(DirectChatSend {
            session_id,
            agent_id,
            initiator_user_id: user_id,
            content: "blocked on the agent lock",
            attachment_ids: &[],
            uploader_type: "member",
            uploader_id: user_id,
            derive_title: stub_derive_title,
        })
        .await
    });

    let mut probe = pool.begin().await.expect("probe tx");
    let mut session_lock_held_by_send = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let got = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM chat_session WHERE id = $1 FOR UPDATE NOWAIT",
        )
        .bind(session_id)
        .fetch_one(&mut *probe)
        .await;
        if got.is_err() {
            session_lock_held_by_send = true;
            break;
        }
        // 探针抢到了 ⇒ 发送还没走到第一步，放掉再等。
        probe.rollback().await.expect("probe rollback");
        probe = pool.begin().await.expect("probe tx");
    }
    assert!(
        session_lock_held_by_send,
        "发送停在 agent 锁上时必须已经持有 chat_session 行锁（锁顺序相反会与 delete 路径死锁）"
    );
    probe.rollback().await.expect("probe rollback");
    holder.rollback().await.expect("release agent lock");

    let sent = tokio::time::timeout(Duration::from_secs(15), sender)
        .await
        .expect("sender finished")
        .expect("join")
        .expect("send");
    assert_eq!(sent.task.chat_session_id, Some(fixture.session_id));

    teardown(&fixture).await;
}
