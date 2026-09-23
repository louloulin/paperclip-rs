//! `chat_session` 仓储的真库语义（M4-4 / LUM-1601，`docs/45` §3 G9）。
//!
//! 本文件覆盖 `ChatSessionRepo` 的**全部 17 个公开方法**，重点在三处只有真 PostgreSQL 能判的地方：
//!
//! 1. `create_explicit` 的四步事务（锁 workspace `FOR KEY SHARE` → 可选锁 project → INSERT
//!    → 盖「显式创建」戳）与两个 404 分支**回滚不留行**；
//! 2. 两个列表查询的可见性判据（`explicitly_created_at IS NOT NULL OR 有非渠道控制消息`）、
//!    未读计数（`last_read_at` 之后的 assistant 行）与「归档强制未读 0」；
//! 3. `delete_cascade` 的三步事务：`chat_draft_restore` **没有外键** ⇒ 草稿只能靠显式剪枝
//!    清掉，而剪枝语句只按 `chat_session_id` 走、不看 `workspace_id`。
//!
//! 用例落在 `chat_task/tests/` 而不是 `chat_session.rs` 内联：真库现场只有一份（见兄弟模块
//! `mod.rs`），且 `chat_session.rs` 已贴着门 ⑩ 的 800 行上限。

use uuid::Uuid;

use super::{
    insert_message, insert_task, message_row, new_draft_restore, new_project, new_session,
    new_unmarked_session, raw_session, setup, teardown, Fixture, MessageSeed, TaskSeed,
};
use crate::chat_draft_restore::ChatDraftRestoreRepo;
use crate::chat_session::{
    ChatSessionRepo, CreateSessionOutcome, DeleteSessionOutcome, NewChatSession,
};

/// 消息时间戳：全部显式给（同一毫秒内生成的 v7 id 之间没有顺序保证）。
const T1: &str = "2026-03-01 00:00:01+00";
const T2: &str = "2026-03-01 00:00:02+00";
const T3: &str = "2026-03-01 00:00:03+00";

fn new_session_params(
    fixture: &Fixture,
    project_id: Option<Uuid>,
    workspace_id: Uuid,
) -> NewChatSession {
    NewChatSession {
        id: None,
        workspace_id,
        agent_id: fixture.agent_id,
        creator_id: fixture.user_id,
        title: "explicit".to_owned(),
        project_id,
        is_agent_intro: false,
    }
}

/// `create_explicit`：`runtime_id` 由 agent 子查询回填、戳只盖一次，两个 404 分支都不留行。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_explicit_backfills_the_runtime_and_reports_the_two_missing_parents() {
    let Some(fixture) = setup().await else {
        println!("skip create_explicit_backfills_the_runtime_and_reports_the_two_missing_parents: no env");
        return;
    };
    let repo = ChatSessionRepo::new(fixture.db.clone());
    let project = new_project(&fixture, "lum1601-project").await;

    let created = match repo
        .create_explicit(&new_session_params(
            &fixture,
            Some(project),
            fixture.workspace_id,
        ))
        .await
        .expect("create_explicit")
    {
        CreateSessionOutcome::Created(row) => row,
        other => panic!("expected Created, got {other:?}"),
    };
    assert_eq!(
        created.runtime_id,
        Some(fixture.runtime_id),
        "`runtime_id` 由 `(SELECT runtime_id FROM agent WHERE id = $2)` 回填，不由调用方传"
    );
    assert_eq!(created.project_id, Some(project));
    assert_eq!(created.creator_id, fixture.user_id);
    assert_eq!(created.title, "explicit");
    assert_eq!(created.status, "active");
    assert!(!created.is_archived());
    assert!(created.is_creator(mc_core::Id::from(fixture.user_id)));
    let stamped = created.explicitly_created_at.expect("显式创建戳已盖");

    // `MarkChatSessionExplicitlyCreated` 的 `COALESCE` ⇒ 第二次不改时间（幂等）。
    let again = repo
        .mark_explicitly_created(created.id)
        .await
        .expect("mark explicitly created");
    assert_eq!(again.explicitly_created_at, Some(stamped));

    // project 不存在 ⇒ ProjectNotFound（第二步的锁取不到行）。
    let outcome = repo
        .create_explicit(&new_session_params(
            &fixture,
            Some(Uuid::new_v4()),
            fixture.workspace_id,
        ))
        .await
        .expect("create_explicit");
    assert!(matches!(outcome, CreateSessionOutcome::ProjectNotFound));

    // workspace 不存在 ⇒ WorkspaceNotFound（先死在第一把锁上）。
    let outcome = repo
        .create_explicit(&new_session_params(&fixture, None, Uuid::new_v4()))
        .await
        .expect("create_explicit");
    assert!(matches!(outcome, CreateSessionOutcome::WorkspaceNotFound));

    let sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chat_session WHERE workspace_id = $1")
            .bind(fixture.workspace_id)
            .fetch_one(fixture.pool())
            .await
            .expect("count sessions");
    assert_eq!(
        sessions, 2,
        "两个 404 分支各自回滚，不留行（现场 1 + 本用例 1）"
    );

    // `create`（上游 `CreateChatSession`）：同样由 agent 子查询回填 `runtime_id`，但**不**盖戳
    // （「显式创建」只在 `create_explicit` 里落），也没有 404 结局（缺父行直接是外键错）。
    let plain = repo
        .create(&new_session_params(&fixture, None, fixture.workspace_id))
        .await
        .expect("create");
    assert_eq!(plain.runtime_id, Some(fixture.runtime_id));
    assert!(
        plain.explicitly_created_at.is_none(),
        "盖戳只在 `create_explicit` 里"
    );
    assert_eq!(plain.title, "explicit");

    // 租户护栏：workspace 是 SQL 层的一部分，换一个 id 就查不到。
    assert!(repo
        .is_public_in_workspace(created.id, fixture.workspace_id)
        .await
        .expect("public check"));
    assert!(!repo
        .is_public_in_workspace(created.id, Uuid::new_v4())
        .await
        .expect("public check"));
    assert!(repo
        .get_in_workspace(created.id, fixture.workspace_id)
        .await
        .expect("get")
        .is_some());
    assert!(repo
        .get_in_workspace(created.id, Uuid::new_v4())
        .await
        .expect("get")
        .is_none());

    teardown(&fixture).await;
}

/// 可见性 / 未读 / 置顶 / 归档：两个列表查询的三条判据。
#[allow(clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn creator_lists_apply_the_visibility_unread_and_archived_rules() {
    let Some(fixture) = setup().await else {
        println!("skip creator_lists_apply_the_visibility_unread_and_archived_rules: no env");
        return;
    };
    let repo = ChatSessionRepo::new(fixture.db.clone());

    // A：已盖「显式创建」戳，但一条消息都没有（刚建好的 Web Chat 也算公开会话）。
    // 没有可见消息的会话按 `updated_at` 参与「最近活动」排序 ⇒ 显式压到过去，
    // 让下面那条相对顺序断言不依赖机器时钟。
    let stamped = new_session(&fixture, fixture.agent_id, "active").await;
    sqlx::query("UPDATE chat_session SET updated_at = $2::timestamptz WHERE id = $1")
        .bind(stamped)
        .bind("2026-02-01 00:00:00+00")
        .execute(fixture.pool())
        .await
        .expect("age the stamped session");
    // B：没有戳，且只有渠道控制面记录 ⇒ 两个列表都看不见。
    let channel_only = new_unmarked_session(&fixture, fixture.agent_id, "active").await;
    insert_message(
        &fixture,
        channel_only,
        MessageSeed::user("[control]")
            .kind("channel_command")
            .at(T1),
    )
    .await;
    // C：没有戳，但有一条公开回复 ⇒ 可见；未读按 `last_read_at` 之后算。
    let replied = new_unmarked_session(&fixture, fixture.agent_id, "active").await;
    sqlx::query("UPDATE chat_session SET last_read_at = $2::timestamptz WHERE id = $1")
        .bind(replied)
        .bind("2026-03-01 00:00:00+00")
        .execute(fixture.pool())
        .await
        .expect("set last_read_at");
    let answer = insert_message(&fixture, replied, MessageSeed::assistant("answer").at(T1)).await;
    assert_eq!(message_row(&fixture, answer).await.content, "answer");
    insert_message(
        &fixture,
        replied,
        MessageSeed::user("[control 2]")
            .kind("channel_command")
            .at(T2),
    )
    .await;

    let listed = repo
        .list_by_creator(fixture.workspace_id, fixture.user_id)
        .await
        .expect("list active sessions");
    let ids: Vec<Uuid> = listed.iter().map(|row| row.id).collect();
    assert!(
        ids.contains(&stamped) && ids.contains(&replied),
        "有戳的与有公开消息的都在列表里"
    );
    assert!(
        !ids.contains(&channel_only),
        "只有渠道控制记录的会话不算公开会话"
    );

    // 相对顺序：按最近活动（没有可见消息就用 `updated_at`）倒序；B 不可见所以不进这个投影。
    let order: Vec<Uuid> = ids
        .iter()
        .copied()
        .filter(|id| *id == stamped || *id == replied)
        .collect();
    assert_eq!(
        order,
        vec![replied, stamped],
        "最近有回复的排在前面；没有可见消息的会话用 `updated_at` 参与排序"
    );

    let replied_row = listed.iter().find(|row| row.id == replied).expect("row C");
    assert_eq!(
        replied_row.unread_count, 1,
        "`last_read_at` 之后的 assistant 行"
    );
    assert!(replied_row.has_unread());
    assert_eq!(replied_row.last_message_content, "answer");
    assert_eq!(replied_row.last_message_role, "assistant");
    assert_eq!(
        replied_row.last_message_at.map(|at| at.to_rfc3339()),
        Some("2026-03-01T00:00:01+00:00".to_owned())
    );
    assert_eq!(
        replied_row.last_message_kind, "message",
        "渠道控制记录不进最近消息投影"
    );
    assert!(!replied_row.is_pinned());

    let stamped_row = listed.iter().find(|row| row.id == stamped).expect("row A");
    assert_eq!(stamped_row.unread_count, 0);
    assert_eq!(stamped_row.last_message_content, "");
    assert_eq!(stamped_row.last_message_kind, "");
    assert!(stamped_row.last_message_at.is_none());

    // 已读游标推到 `now()` ⇒ 未读归零（只写 `last_read_at`）。
    repo.mark_read(replied).await.expect("mark read");
    let listed = repo
        .list_by_creator(fixture.workspace_id, fixture.user_id)
        .await
        .expect("list after read");
    let replied_row = listed.iter().find(|row| row.id == replied).expect("row C");
    assert_eq!(replied_row.unread_count, 0);

    // 置顶压过「最近活动」：A 没有消息却排到第一。
    let pinned = repo.set_pinned(stamped, true).await.expect("pin");
    let pinned_at = pinned.pinned_at.expect("pinned_at 已盖戳");
    assert!(pinned.is_pinned());
    let again = repo.set_pinned(stamped, true).await.expect("pin twice");
    assert_eq!(again.pinned_at, Some(pinned_at), "重复置顶保持原置顶顺序");
    let listed = repo
        .list_by_creator(fixture.workspace_id, fixture.user_id)
        .await
        .expect("list after pin");
    assert_eq!(listed[0].id, stamped, "pin 优先于最近活动");
    let unpinned = repo.set_pinned(stamped, false).await.expect("unpin");
    assert!(!unpinned.is_pinned());

    // 归档：从 active 列表消失，且未读被 SQL 强制成 0（归档刻意不推进 `last_read_at`）。
    insert_message(
        &fixture,
        replied,
        MessageSeed::assistant("unread again").at(T3),
    )
    .await;
    let cursor_before = answered_cursor(&fixture, replied).await;
    let archived = repo.set_archived(replied, true).await.expect("archive");
    assert!(archived.is_archived());
    assert_eq!(
        archived.last_read_at, cursor_before,
        "归档不碰 `last_read_at`"
    );
    let active_ids: Vec<Uuid> = repo
        .list_by_creator(fixture.workspace_id, fixture.user_id)
        .await
        .expect("list active")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert!(!active_ids.contains(&replied), "归档会话不在 active 列表");
    let all = repo
        .list_all_by_creator(fixture.workspace_id, fixture.user_id)
        .await
        .expect("list all");
    let archived_row = all.iter().find(|row| row.id == replied).expect("row C");
    assert_eq!(archived_row.status, "archived");
    assert_eq!(archived_row.unread_count, 0, "归档强制未读 0");
    assert_eq!(archived_row.last_message_content, "unread again");
    assert_eq!(archived_row.unread_since, None);

    // `update_title` 推进 `updated_at`（改名算活动），`touch` 只推 `updated_at`。
    let before = raw_session(&fixture, stamped).await;
    let renamed = repo.update_title(stamped, "renamed").await.expect("rename");
    assert_eq!(renamed.title, "renamed");
    assert!(renamed.updated_at > before.updated_at);
    repo.touch(stamped).await.expect("touch");
    let after = raw_session(&fixture, stamped).await;
    assert!(after.updated_at > before.updated_at);
    assert_eq!(after.title, "renamed");
    assert_eq!(after.status, "active");
    assert_eq!(after.runtime_id, Some(fixture.runtime_id));

    teardown(&fixture).await;
}

/// 归档会话的 `last_read_at` 读回（证明归档没有偷偷推进已读游标）。
async fn answered_cursor(fixture: &Fixture, session_id: Uuid) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("SELECT last_read_at FROM chat_session WHERE id = $1")
        .bind(session_id)
        .fetch_one(fixture.pool())
        .await
        .expect("load last_read_at")
}

/// `project` 上下文：`update_project_locked` 必须先拿到父行的 `FOR KEY SHARE`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn project_context_updates_take_the_parent_lock_first() {
    let Some(fixture) = setup().await else {
        println!("skip project_context_updates_take_the_parent_lock_first: no env");
        return;
    };
    let repo = ChatSessionRepo::new(fixture.db.clone());
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let project = new_project(&fixture, "lum1601-project-lock").await;

    // project 不存在 ⇒ None（回滚，不写）。
    assert!(repo
        .update_project_locked(session, fixture.workspace_id, Some(Uuid::new_v4()))
        .await
        .expect("locked update")
        .is_none());
    // project 存在于别的 workspace ⇒ 同样 None（锁的 WHERE 带 workspace）。
    assert!(repo
        .update_project_locked(session, Uuid::new_v4(), Some(project))
        .await
        .expect("locked update")
        .is_none());
    let untouched = repo
        .get_in_workspace(session, fixture.workspace_id)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(untouched.project_id, None, "两次失败都不写库");

    let updated = repo
        .update_project_locked(session, fixture.workspace_id, Some(project))
        .await
        .expect("locked update")
        .expect("row");
    assert_eq!(updated.project_id, Some(project));

    // 清上下文不需要锁，且**不碰** `updated_at`（换上下文不是对话活动）。
    let cleared = repo
        .update_project(session, fixture.workspace_id, None)
        .await
        .expect("clear project");
    assert_eq!(cleared.project_id, None);
    assert_eq!(cleared.updated_at, updated.updated_at);

    // 租户护栏：`UPDATE` 打不到行 ⇒ `fetch_one` 的 RowNotFound ⇒ `RepoError::NotFound`。
    let err = repo
        .update_project(session, Uuid::new_v4(), Some(project))
        .await
        .expect_err("cross tenant update");
    assert!(matches!(err, crate::RepoError::NotFound));

    teardown(&fixture).await;
}

/// `delete` / `delete_cascade` / `lock_for_delete`：三步事务与幂等语义。
#[allow(clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn delete_cascade_prunes_the_drafts_before_it_deletes_the_session() {
    let Some(fixture) = setup().await else {
        println!("skip delete_cascade_prunes_the_drafts_before_it_deletes_the_session: no env");
        return;
    };
    let repo = ChatSessionRepo::new(fixture.db.clone());
    let drafts = ChatDraftRestoreRepo::new(fixture.db.clone());

    let doomed = new_session(&fixture, fixture.agent_id, "active").await;
    let task = insert_task(&fixture, doomed, TaskSeed::queued()).await;
    let draft = new_draft_restore(&fixture, doomed, task, "half typed prompt", Some(T1)).await;
    let message = insert_message(&fixture, doomed, MessageSeed::user("doomed turn").at(T2)).await;
    assert_eq!(message_row(&fixture, message).await.content, "doomed turn");

    // 首锁 `FOR UPDATE` 拿得到（没有人和它抢）。
    assert_eq!(
        repo.lock_for_delete(doomed).await.expect("lock"),
        Some(doomed)
    );

    // 跨租户：持到了行锁，但 `workspace_id` 不匹配 ⇒ 父行删不掉 ⇒ `AlreadyGone`。
    // ⚠️ 剪枝在**同一个事务里先跑**，而它只按 `chat_session_id` 过滤（上游同款）⇒
    // 这一次失败的删除**也**会带走草稿。这是可观测的真实语义，不是笔误。
    assert_eq!(
        repo.delete_cascade(doomed, Uuid::new_v4())
            .await
            .expect("cross tenant delete"),
        DeleteSessionOutcome::AlreadyGone
    );
    assert!(
        repo.get_in_workspace(doomed, fixture.workspace_id)
            .await
            .expect("still there")
            .is_some(),
        "跨租户调用删不掉会话"
    );
    assert!(
        drafts
            .list_by_session(doomed)
            .await
            .expect("list drafts")
            .is_empty(),
        "剪枝不看 workspace"
    );
    assert_eq!(
        drafts.consume(draft, doomed).await.expect("consume gone"),
        0
    );

    // 正常删除：草稿已空、`chat_message` 由外键级联带走、第二次调用幂等。
    let leftover = new_draft_restore(&fixture, doomed, task, "leftover", Some(T3)).await;
    assert_eq!(
        repo.delete_cascade(doomed, fixture.workspace_id)
            .await
            .expect("delete"),
        DeleteSessionOutcome::Deleted
    );
    assert!(repo
        .get_in_workspace(doomed, fixture.workspace_id)
        .await
        .expect("gone")
        .is_none());
    let messages: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chat_message WHERE chat_session_id = $1")
            .bind(doomed)
            .fetch_one(fixture.pool())
            .await
            .expect("count messages");
    assert_eq!(messages, 0, "`chat_message` 有外键，级联带走");
    assert_eq!(
        drafts
            .consume(leftover, doomed)
            .await
            .expect("draft pruned"),
        0,
        "没有外键 ⇒ 只能靠显式剪枝"
    );
    assert_eq!(
        repo.delete_cascade(doomed, fixture.workspace_id)
            .await
            .expect("idempotent delete"),
        DeleteSessionOutcome::AlreadyGone
    );
    assert_eq!(repo.lock_for_delete(doomed).await.expect("lock gone"), None);

    // `delete`（不带事务的那条）同样带租户护栏：workspace 不匹配 ⇒ 0 行。
    let plain = new_session(&fixture, fixture.agent_id, "active").await;
    assert_eq!(
        repo.delete(plain, Uuid::new_v4())
            .await
            .expect("cross tenant"),
        0
    );
    assert!(repo
        .get_in_workspace(plain, fixture.workspace_id)
        .await
        .expect("still there")
        .is_some());
    assert_eq!(
        repo.delete(plain, fixture.workspace_id)
            .await
            .expect("delete"),
        1
    );
    assert_eq!(
        repo.delete(plain, fixture.workspace_id)
            .await
            .expect("delete again"),
        0
    );

    teardown(&fixture).await;
}
