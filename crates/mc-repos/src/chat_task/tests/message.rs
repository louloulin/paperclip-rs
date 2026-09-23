//! `chat_message` 仓储分页的真库语义（M4-4 / LUM-1601，`docs/45` §3 G9）。
//!
//! 两条查询的 SQL 里各有一处只有真库能判的细节：
//!
//! * `list_for_session` 的 `VISIBLE_HEAD_FILTER` 是**列存在性 + 子查询**的组合，别名写错
//!   `message` 就会在运行期报 `no column found for name`；
//! * `list_page` 的游标是 `(created_at, id)` **元组**比较 —— 同一毫秒落的多行里 id 没有单调性，
//!   只比时间会丢行或重复行。

use uuid::Uuid;

use super::{insert_message, insert_task, setup, teardown, MessageSeed, TaskSeed};
use crate::chat_message::ChatMessageRepo;

/// 消息时间戳全部显式给：同毫秒内生成的 v7 id 之间没有顺序保证。
const T1: &str = "2026-03-01 00:00:01+00";
const T2: &str = "2026-03-01 00:00:02+00";
const T3: &str = "2026-03-01 00:00:03+00";
const T4: &str = "2026-03-01 00:00:04+00";

/// `list_for_session` 返回的是升序**且**已经滤掉可见头隐藏的行。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn list_for_session_is_ascending_and_applies_the_visible_head_filter() {
    let Some(fixture) = setup().await else {
        println!("skip list_for_session_is_ascending_and_applies_the_visible_head_filter: no env");
        return;
    };
    let repo = ChatMessageRepo::new(fixture.db.clone());
    let session = fixture.session_id;

    let opener = insert_message(&fixture, session, MessageSeed::user("opener").at(T1)).await;
    let head = insert_task(&fixture, session, TaskSeed::queued().at(T1)).await;
    let head_input = insert_message(
        &fixture,
        session,
        MessageSeed::user("head turn").on(head).at(T2),
    )
    .await;
    let answer = insert_message(&fixture, session, MessageSeed::assistant("answer").at(T3)).await;
    let follow = insert_task(&fixture, session, TaskSeed::queued().at(T3)).await;
    let follow_input = insert_message(
        &fixture,
        session,
        MessageSeed::user("queued follow up").on(follow).at(T4),
    )
    .await;
    let control = insert_message(
        &fixture,
        session,
        MessageSeed::user("[control]")
            .kind("channel_command")
            .at(T4),
    )
    .await;

    let rows = repo
        .list_for_session(session)
        .await
        .expect("list for session");
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let contents: Vec<&str> = rows.iter().map(|row| row.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["opener", "head turn", "answer"],
        "时间升序；排队中的后续轮输入与渠道控制记录都不在"
    );
    assert_eq!(ids, vec![opener, head_input, answer]);
    assert!(!ids.contains(&follow_input), "排队中的后续轮输入被隐藏");
    assert!(!ids.contains(&control), "渠道控制记录不进用户面历史");

    teardown(&fixture).await;
}

/// `list_page` 的游标是 `(created_at, id)` 元组：同毫秒行既不丢也不重。
#[allow(clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn list_page_walks_the_transcript_backwards_and_breaks_timestamp_ties_by_id() {
    let Some(fixture) = setup().await else {
        println!(
            "skip list_page_walks_the_transcript_backwards_and_breaks_timestamp_ties_by_id: no env"
        );
        return;
    };
    let repo = ChatMessageRepo::new(fixture.db.clone());
    let session = fixture.session_id;

    let first = insert_message(&fixture, session, MessageSeed::user("first").at(T1)).await;
    let second = insert_message(&fixture, session, MessageSeed::assistant("second").at(T2)).await;
    // 同一时间戳的两行：靠元组里的 id 分量定序。
    let tied_a = insert_message(&fixture, session, MessageSeed::user("tied a").at(T3)).await;
    let tied_b = insert_message(&fixture, session, MessageSeed::assistant("tied b").at(T3)).await;

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor = None;
    for _ in 0..4 {
        let page = repo.list_page(session, 2, cursor).await.expect("list page");
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 2, "limit 生效");
        for window in page.windows(2) {
            assert!(
                window[0].created_at >= window[1].created_at,
                "页内按 `created_at DESC, id DESC`"
            );
        }
        cursor = page.last().map(|row| (row.created_at, row.id));
        seen.extend(page.iter().map(|row| row.id));
    }

    seen.sort();
    let mut expected = vec![first, second, tied_a, tied_b];
    expected.sort();
    assert_eq!(seen, expected, "倒着走完整个会话：不丢行、不重复");
    assert_eq!(cursor.map(|(_, id)| id), Some(first));

    // 最后一页之后的空页：游标停在最老一行上不会把任何东西再吐一遍。
    let empty = repo
        .list_page(session, 2, cursor)
        .await
        .expect("empty page");
    assert!(empty.is_empty());

    teardown(&fixture).await;
}
