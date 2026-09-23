//! `chat_draft_restore` 仓储的真库语义（M4-4 / LUM-1601，`docs/45` §3 G9）。
//!
//! 这张表**没有外键**，所以「删会话顺手带走草稿」这件事只能靠显式剪枝；而 `consume` 的
//! `WHERE` 里那个 `chat_session_id` 是**授权**条件（草稿只能被它所属的会话消费），不是可有可无的
//! 冗余过滤 —— 两件事都只有真库能验。

use uuid::Uuid;

use super::{insert_task, new_draft_restore, new_session, setup, teardown, TaskSeed};
use crate::chat_draft_restore::ChatDraftRestoreRepo;

/// 全部显式给时间戳（同一毫秒内的 v7 id 之间没有顺序保证）。
const T1: &str = "2026-03-01 00:00:01+00";
const T2: &str = "2026-03-01 00:00:02+00";
const T3: &str = "2026-03-01 00:00:03+00";

/// 列表按 `created_at ASC`、`consume` 幂等且带会话授权、`prune_by_session` 只清自己那一份。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn consume_is_session_scoped_and_prune_only_touches_its_own_session() {
    let Some(fixture) = setup().await else {
        println!("skip consume_is_session_scoped_and_prune_only_touches_its_own_session: no env");
        return;
    };
    let repo = ChatDraftRestoreRepo::new(fixture.db.clone());

    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let neighbour = new_session(&fixture, fixture.agent_id, "active").await;
    let task = insert_task(&fixture, session, TaskSeed::queued()).await;

    // 故意乱序插入，验的是 `ORDER BY created_at ASC` 而不是插入顺序。
    let late = new_draft_restore(&fixture, session, task, "second draft", Some(T3)).await;
    let early = new_draft_restore(&fixture, session, task, "first draft", Some(T1)).await;
    let other = new_draft_restore(&fixture, neighbour, task, "other session", Some(T2)).await;

    let rows = repo.list_by_session(session).await.expect("list drafts");
    assert_eq!(
        rows.iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>(),
        vec!["first draft", "second draft"]
    );
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![early, late]
    );
    assert_eq!(rows[0].chat_session_id, session);
    assert_eq!(rows[0].task_id, task);
    assert_eq!(rows[0].attachment_ids, Vec::<Uuid>::new());
    assert_eq!(rows[0].id(), mc_core::Id::from(early));
    assert_eq!(rows[0].chat_session_id(), mc_core::Id::from(session));
    assert!(rows[0].created_at < rows[1].created_at);
    assert_eq!(
        repo.list_by_session(neighbour).await.expect("list").len(),
        1,
        "列表按会话隔离"
    );

    // 用**别的会话**消费 ⇒ 0 行，草稿还在（授权条件在 SQL 里，不是 handler 的礼貌）。
    assert_eq!(
        repo.consume(early, neighbour).await.expect("wrong session"),
        0
    );
    assert_eq!(repo.list_by_session(session).await.expect("list").len(), 2);
    // 不存在的 id 同样 0 行（调用方仍回 204）。
    assert_eq!(
        repo.consume(Uuid::new_v4(), session)
            .await
            .expect("unknown id"),
        0
    );
    // 正着来：1 行，且第二次是 0 行（幂等）。
    assert_eq!(repo.consume(early, session).await.expect("consume"), 1);
    assert_eq!(
        repo.consume(early, session).await.expect("consume again"),
        0
    );

    // 剪枝只清自己的那一个会话。
    assert_eq!(repo.prune_by_session(session).await.expect("prune"), 1);
    assert_eq!(
        repo.prune_by_session(session).await.expect("prune again"),
        0
    );
    assert!(repo
        .list_by_session(session)
        .await
        .expect("list")
        .is_empty());
    let neighbour_rows = repo.list_by_session(neighbour).await.expect("list");
    assert_eq!(neighbour_rows.len(), 1);
    assert_eq!(neighbour_rows[0].id, other);

    teardown(&fixture).await;
}
