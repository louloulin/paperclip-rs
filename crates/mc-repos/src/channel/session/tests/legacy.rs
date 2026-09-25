//! lark **遗留**绑定面的真库用例（`lark_chat_session_binding` 与泛化表并存）。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session/tests.rs` 一度 867 行 > 800 硬限）。

use super::*;

/// lark **遗留**绑定面：与泛化表并存，且 `installation_id` 的外键要求先有 `lark_installation` 行。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn the_lark_legacy_binding_repo_reads_and_writes_its_own_table() {
    let fixture = fixture!();
    let repo = fixture.repo();
    let lark = LarkChatSessionBindingRepo::new(fixture.db.clone());
    let input = fixture.ensure(ChannelKind::Lark, &key("lark-legacy"));
    let session_id = repo.ensure_session(&input).await.expect("ensure");

    // 遗留表的外键指向 `lark_installation` ⇒ 先造一行安装（本片不实现 lark 安装面）。
    let installation_id: Uuid = sqlx::query_scalar(
        "INSERT INTO lark_installation (workspace_id, agent_id, app_id, app_secret_encrypted, \
         bot_open_id, region, installer_user_id) \
         VALUES ($1, $2, $3, $4, 'ou_bot', 'feishu', $5) RETURNING id",
    )
    .bind(fixture.workspace_id.0)
    .bind(fixture.agent_id.0)
    .bind(format!("cli_itest1767_{}", Uuid::new_v4().simple()))
    .bind(vec![1_u8, 2, 3])
    .bind(fixture.user_id.0)
    .fetch_one(&fixture.pool)
    .await
    .expect("insert lark_installation");

    let legacy_id = Id(installation_id);
    let created = lark
        .insert(session_id, legacy_id, "oc_legacy", ChatType::Group)
        .await
        .expect("insert legacy binding");
    assert_eq!(created.chat_session_id(), session_id);
    assert_eq!(created.lark_chat_type, "group");

    let read = lark
        .get_by_chat(legacy_id, "oc_legacy")
        .await
        .expect("get by chat")
        .expect("row");
    assert_eq!(read.id(), created.id());
    assert_eq!(
        lark.get_by_session(session_id)
            .await
            .expect("get by session")
            .expect("row")
            .id(),
        created.id()
    );
    assert_eq!(
        lark.update_reply_target(session_id, Some("om_1"), Some("th_1"))
            .await
            .expect("update reply target"),
        1
    );
    let updated = lark
        .get_by_session(session_id)
        .await
        .expect("get by session")
        .expect("row");
    assert_eq!(updated.last_lark_message_id.as_deref(), Some("om_1"));
    assert_eq!(updated.last_lark_thread_id.as_deref(), Some("th_1"));

    // 泛化面**看不到**这一行（两套表并存、不互相去重/合并）。
    assert!(repo
        .get_current_binding(legacy_id, "oc_legacy")
        .await
        .expect("generalized lookup")
        .is_none());
}
