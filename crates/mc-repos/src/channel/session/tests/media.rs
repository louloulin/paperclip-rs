//! 媒体绑定的真库用例（`BindMediaRefs` 的归属面）。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session/tests.rs` 一度 867 行 > 800 硬限）。
//! 现场与共用助手来自父模块（`use super::*`，`macro_rules! fixture!` 的文本作用域覆盖本模块）。

use super::*;

/// 媒体绑定：附件行 + 挂到消息 + 占位替换 + 清 pending 标记；被对账器接管的 key 不落附件。
///
/// `too_many_lines`：一条路径要同时砟四个可观察结果（附件行、内联正文、pending 标记、意图行），
/// 拆开就没人看得出它们必须在**同一个事务**里。
#[allow(clippy::too_many_lines)]
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn bind_media_links_attachments_and_rewrites_the_inline_placeholder() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let media = ChannelMediaRepo::new(fixture.db.clone());
    let input = fixture.ensure(ChannelKind::WeCom, &key("media"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");

    let body = "[Image]\nlook".to_string();
    let AppendOutcome::Appended(appended) = repo
        .append_message(&NewChannelAppend {
            session_id,
            sender: fixture.user_id,
            installation_id: input.installation_id,
            body: body.clone(),
            first_title: String::new(),
            is_command: false,
            message_id: "wc-media-1".to_string(),
            thread_id: String::new(),
            sender_channel_id: "wc-user".to_string(),
            dedup_message_id: String::new(),
            claim_token: None,
            media_pending_seconds: 45.0,
            force_fresh: false,
            has_media: true,
        })
        .await
        .expect("append")
    else {
        panic!("expected Appended");
    };
    let message_id = appended.message_id.expect("message id");
    let storage_key = format!("itest1767/{}", Uuid::new_v4().simple());
    media
        .record_pending_object(
            &storage_key,
            fixture.workspace_id,
            message_id,
            "https://example.invalid/obj",
            Some(input.installation_id),
        )
        .await
        .expect("record intent")
        .expect("intent row");

    let result = repo
        .bind_media(&BindMediaRefsParams {
            message_id: Some(message_id),
            session_id,
            workspace_id: fixture.workspace_id,
            sender: fixture.user_id,
            issue_id: None,
            issue_description_base: None,
            issue_command_text: String::new(),
            body: body.clone(),
            media_refs: vec![MediaRef {
                message_kind: MessageKind::Image,
                storage_key: storage_key.clone(),
                storage_url: "https://example.invalid/obj".to_string(),
                filename: "shot.png".to_string(),
                mime_type: "image/png".to_string(),
                size_bytes: 12,
                inline_placeholder: "[Image]".to_string(),
                inline_index: 0,
            }],
            media_title: Some("shot.png".to_string()),
        })
        .await
        .expect("bind media");
    assert!(result.linked >= 1, "附件真的落库并挂上了");
    assert_eq!(result.initial_title.as_deref(), Some("shot.png"));

    let (content, pending, title): (String, Option<chrono::DateTime<Utc>>, String) =
        sqlx::query_as(
            "SELECT m.content, m.channel_media_pending_until, s.title \
         FROM chat_message AS m JOIN chat_session AS s ON s.id = m.chat_session_id \
         WHERE m.id = $1",
        )
        .bind(message_id.0)
        .fetch_one(&fixture.pool)
        .await
        .expect("read message");
    assert!(content.contains("/api/attachments/"), "占位被换成附件链接");
    assert!(!content.contains("[Image]"), "占位文本被替换掉了");
    assert_eq!(title, "shot.png", "媒体首轮用附件名做标题");
    assert!(pending.is_none(), "pending 标记被清掉");
    assert!(
        media.get(&storage_key).await.expect("get intent").is_none(),
        "意图行与附件同一次提交里被删掉"
    );

    // 对账器接管后的 key 不再落附件：先造一行 `deleting` 的意图。
    let taken_key = format!("itest1767/{}", Uuid::new_v4().simple());
    media
        .record_pending_object(
            &taken_key,
            fixture.workspace_id,
            message_id,
            "https://example.invalid/taken",
            None,
        )
        .await
        .expect("record intent")
        .expect("intent row");
    sqlx::query(
        "UPDATE channel_media_pending_object SET state = 'deleting' WHERE storage_key = $1",
    )
    .bind(&taken_key)
    .execute(&fixture.pool)
    .await
    .expect("hand the intent to the reconciler");
    let second = repo
        .bind_media(&BindMediaRefsParams {
            message_id: Some(message_id),
            session_id,
            workspace_id: fixture.workspace_id,
            sender: fixture.user_id,
            issue_id: None,
            issue_description_base: None,
            issue_command_text: String::new(),
            body: body.clone(),
            media_refs: vec![MediaRef {
                message_kind: MessageKind::Image,
                storage_key: taken_key.clone(),
                storage_url: "https://example.invalid/taken".to_string(),
                filename: "taken.png".to_string(),
                mime_type: "image/png".to_string(),
                size_bytes: 1,
                inline_placeholder: String::new(),
                inline_index: 0,
            }],
            media_title: None,
        })
        .await
        .expect("bind media after reconcile");
    assert_eq!(second.linked, 0, "被对账器接管的 key 不落附件");
    let attachments: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM attachment WHERE chat_message_id = $1")
            .bind(message_id.0)
            .fetch_one(&fixture.pool)
            .await
            .expect("count attachments");
    assert_eq!(attachments, 1, "taken 那个没有落附件");

    let _ = sqlx::query("DELETE FROM channel_media_pending_object WHERE storage_key = $1")
        .bind(&taken_key)
        .execute(&fixture.pool)
        .await;
}
