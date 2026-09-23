//! `crate::autopilot::delivery` 的真库语义：replay 行的唯一槽位与去重旁路。
//!
//! 从 `run.rs` 拆出来的：门 ⑩ 的单文件 800 行上限；夹具在 `run.rs` 里以 `pub(super)` 暴露。

use serde_json::json;
use uuid::Uuid;

use super::run::{cleanup, seed};
use super::setup;
use crate::autopilot::delivery::{self as delivery_sql, NewReplayDelivery, WebhookDeliverySlimRow};
use crate::RepoError;

/// 投递面：瘦行读**不含**详情三列、`(原投递, 幂等键)` 的部分唯一索引、replay 行 `dedupe_key` 恒 NULL、
/// `signature_failed()` 只看 `rejected` / `invalid`、列表 workspace 收窄 + newest first。
#[allow(clippy::too_many_lines)] // 121 行：原投递 + 两条 replay + 三条查询（列表/详情/幂等键）
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn replay_rows_are_idempotent_per_key_and_bypass_provider_dedupe() {
    let Some(fixture) = setup().await else {
        println!("skip replay_rows_are_idempotent_per_key_and_bypass_provider_dedupe: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let seeded = seed(pool, ws).await;

    // 原投递：普通 webhook 投递（有 dedupe_key），直接 SQL 铺（`create_replay` 是 replay 专用）。
    let original_id: Uuid = sqlx::query_scalar(
        "INSERT INTO webhook_delivery (workspace_id, autopilot_id, trigger_id, provider, event, \
             dedupe_key, dedupe_source, signature_status, status, selected_headers, content_type, \
             raw_body) \
         VALUES ($1, $2, $3, 'github', 'issues.opened', 'gh:1', 'header', 'valid', 'queued', \
             '{\"x-github-event\": \"issues\"}', 'application/json', $4) RETURNING id",
    )
    .bind(ws)
    .bind(seeded.autopilot_id)
    .bind(seeded.trigger_id)
    .bind(br#"{"action":"opened"}"#.to_vec())
    .fetch_one(pool)
    .await
    .expect("insert original delivery");

    // replay：绕过去重（`dedupe_key` NULL）、`signature_status='not_required'`、初始 `queued`。
    let mut replay = NewReplayDelivery {
        id: Uuid::new_v4(),
        workspace_id: ws,
        autopilot_id: seeded.autopilot_id,
        trigger_id: seeded.trigger_id,
        provider: "generic".to_string(),
        event: "issues.opened".to_string(),
        selected_headers: json!({}),
        content_type: None,
        raw_body: br#"{"action":"opened"}"#.to_vec(),
        replayed_from_delivery_id: original_id,
        replay_idempotency_key: "replay-key".to_string(),
    };
    let created = delivery_sql::create_replay(pool, &replay)
        .await
        .expect("first replay");
    assert_eq!(created.status, "queued");
    assert_eq!(created.signature_status, "not_required");
    assert!(created.dedupe_key.is_none(), "replay 绕过 provider 去重");
    assert_eq!(created.dedupe_source, None);
    assert_eq!(
        created.replay_idempotency_key.as_deref(),
        Some("replay-key")
    );
    assert_eq!(created.replayed_from_delivery_id, Some(original_id));
    assert!(!created.signature_failed());

    // 同键再插 ⇒ 唯一索引冲突；`find_replay` 取回已有那行（handler 的 202 幂等）。
    replay.id = Uuid::new_v4();
    assert!(matches!(
        delivery_sql::create_replay(pool, &replay).await,
        Err(RepoError::Conflict)
    ));
    let found = delivery_sql::find_replay(pool, original_id, "replay-key")
        .await
        .expect("find_replay")
        .expect("idempotency hit");
    assert_eq!(found.id, created.id);
    assert!(delivery_sql::find_replay(pool, original_id, "another-key")
        .await
        .expect("find_replay")
        .is_none());

    // 瘦行列表：结构里没有详情三列（列被裁剪）+ newest first。
    let slim: Vec<WebhookDeliverySlimRow> =
        delivery_sql::list(pool, seeded.autopilot_id, ws, 10, 0)
            .await
            .expect("list");
    assert_eq!(slim.len(), 2);
    assert_eq!(slim[0].id, created.id, "newest first");
    assert_eq!(slim[1].id, original_id);
    let paged = delivery_sql::list(pool, seeded.autopilot_id, ws, 1, 1)
        .await
        .expect("list paged");
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].id, original_id);

    // 详情读有那三列；workspace 限定，跨工作区一律 NotFound。
    let detail = delivery_sql::get_in_workspace(pool, created.id, ws)
        .await
        .expect("detail");
    assert_eq!(detail.selected_headers, json!({}));
    assert_eq!(
        detail.raw_body.as_deref(),
        Some(&br#"{"action":"opened"}"#[..])
    );
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-delivery-other', $1) RETURNING id",
    )
    .bind(format!("itest-delivery-other-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("other workspace");
    assert!(matches!(
        delivery_sql::get_in_workspace(pool, created.id, other).await,
        Err(RepoError::NotFound)
    ));
    assert!(delivery_sql::list(pool, seeded.autopilot_id, other, 10, 0)
        .await
        .expect("cross-workspace list")
        .is_empty());

    // 签名没过（`rejected` / `signature_status='invalid'`）⇒ `signature_failed()`，replay 必须被拒。
    sqlx::query("UPDATE webhook_delivery SET status = 'rejected' WHERE id = $1")
        .bind(original_id)
        .execute(pool)
        .await
        .expect("reject original");
    assert!(delivery_sql::get_in_workspace(pool, original_id, ws)
        .await
        .expect("original")
        .signature_failed());
    sqlx::query(
        "UPDATE webhook_delivery SET status = 'queued', signature_status = 'invalid' WHERE id = $1",
    )
    .bind(original_id)
    .execute(pool)
    .await
    .expect("invalidate original");
    assert!(delivery_sql::get_in_workspace(pool, original_id, ws)
        .await
        .expect("original")
        .signature_failed());

    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other)
        .execute(pool)
        .await;
    cleanup(&fixture, &seeded).await;
}
