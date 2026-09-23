//! replay 面（#20 `POST …/deliveries/:deliveryId/replay`）的 e2e。
//!
//! 从 `deliveries.rs` 拆出来的：门 ⑩ 的单文件 800 行上限，而 #18/#19 的读面投影用例
//! 已经把那个文件顶到 833 行。拆法按「读面 / 写面」而不是按行数切：本文件只管 replay
//! 的**判负阶梯与幂等**，`deliveries.rs` 只剩两条读路由与共用夹具（`DeliverySpec` /
//! `seed_delivery` 在那里以 `pub(crate)` 暴露，不复制第二份）。
//!
//! 关注点：
//!
//! 1. **判负阶梯逐字对照上游**：签名失败 → 无 raw body → autopilot 未启用 →
//!    trigger 没了（404）/ 停用（400）→ body 解不开（400）→ `Idempotency-Key` 过长（400）；
//! 2. 成功与幂等命中都是 **202**，幂等命中**不新建第二行**、换键才新行，原投递行不被改写；
//! 3. replay 行不带 `dedupe_key`（上游刻意让重放绕开 provider 去重）。
//!
//! replay **不唤醒 worker**（本切片登记的 `known_gap`）= 新行落 `queued` 后没人取走，
//! 所以这里只断言「行落库 + 字段对」，不断言它被处理。

use axum::http::StatusCode;
use serde_json::{json, Value};
use uuid::Uuid;

use super::deliveries::{seed_delivery, DeliverySpec};
use super::support::{
    app_with_db, call, cleanup, connect, seed_autopilot, seed_schedule_trigger,
    seed_webhook_trigger, seed_workspace,
};
use super::triggers::upstream_message;

/// replay 的**判负阶梯**：五道闸按上游顺序，各自一句文案。
#[allow(clippy::too_many_lines)] // 127 行：五道闸各自要造自己的前置状态（raw_body / 状态 / trigger）
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn replay_gate_ladder_matches_upstream_order() {
    let Some((pool, db)) = connect().await else {
        println!("skip replay_gate_ladder_matches_upstream_order: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_ladder", None, None).await;
    // 停用的 schedule 触发器专供「trigger is disabled」那一档（webhook 触发器默认启用）。
    let disabled = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;
    sqlx::query("UPDATE autopilot_trigger SET enabled = false WHERE id = $1")
        .bind(disabled)
        .execute(&pool)
        .await
        .expect("disable trigger");
    let replay_uri =
        |delivery: Uuid| format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay");

    // ① 签名失败（`status='rejected'`）→ 400。
    let rejected = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            status: "rejected",
            signature_status: "invalid",
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;
    let (status, body) = call(&app, "POST", &replay_uri(rejected), ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "cannot replay a delivery that failed signature verification"
    );

    // ② 没有 raw body → 400（顺序在「autopilot 是否 active」之前）。
    let bodyless = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            raw_body: None,
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;
    let (status, body) = call(&app, "POST", &replay_uri(bodyless), ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "original delivery has no raw body to replay"
    );

    // ③ autopilot 不是 active → 400。
    let paused = seed_autopilot(&pool, ws, "paused", "member", owner).await;
    let paused_trigger = seed_webhook_trigger(&pool, paused, "awt_paused", None, None).await;
    let paused_delivery = seed_delivery(
        &pool,
        ws,
        paused,
        paused_trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{paused}/deliveries/{paused_delivery}/replay"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot is not active");

    // ④ trigger 停用 → 400。
    let disabled_delivery = seed_delivery(
        &pool,
        ws,
        autopilot,
        disabled,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &replay_uri(disabled_delivery),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "trigger is disabled");

    // ⑤ `raw_body` 不再是合法 JSON → 400（文案带 `stored body no longer parses: `）。
    let broken = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            raw_body: Some(b"{not json"),
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;
    let (status, body) = call(&app, "POST", &replay_uri(broken), ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert!(
        upstream_message(&body).starts_with("stored body no longer parses: "),
        "{body}"
    );

    // ⑥ `Idempotency-Key` 过长 → 400（在插入之前）。
    let long_key = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;
    let (status, body) =
        keyed_replay(&app, &replay_uri(long_key), ws, owner, &"k".repeat(256)).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "Idempotency-Key is too long");

    cleanup(&pool, ws, &[owner]).await;
}

/// replay 成功：202 + 新行（`queued` / `replayed_from_delivery_id` / 幂等键），
/// 同键重放**不新建第二行**、两次都 202 且 id 相同。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn replay_is_202_and_idempotent_per_key() {
    let Some((pool, db)) = connect().await else {
        println!("skip replay_is_202_and_idempotent_per_key: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_replay", None, None).await;
    let original = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            status: "failed",
            signature_status: "valid",
            raw_body: Some(br#"{"action":"opened","payload":{"n":1}}"#),
            response_body: Some("upstream 500"),
            selected_headers: json!({"content-type": "application/json"}),
        },
        "1 hour",
    )
    .await;
    let uri = format!("/api/autopilots/{autopilot}/deliveries/{original}/replay");

    let (status, body) = keyed_replay(&app, &uri, ws, owner, "key-1").await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["status"], "queued");
    assert_eq!(body["signature_status"], "not_required");
    assert_eq!(body["replayed_from_delivery_id"], original.to_string());
    assert_eq!(body["replay_idempotency_key"], "key-1");
    assert_eq!(body["raw_body"], r#"{"action":"opened","payload":{"n":1}}"#);
    assert_eq!(body["selected_headers"]["content-type"], "application/json");
    // replay 行不带去重键（上游刻意让重放绕开 provider 去重）。
    assert!(body["dedupe_key"].is_null(), "{body}");
    assert_eq!(body["provider"], "github");
    assert_eq!(body["event"], "push");
    assert!(body["autopilot_run_id"].is_null(), "{body}");
    let replay_id = body["id"].as_str().unwrap().to_string();

    // 同键：202 + 同一行（幂等命中走 `find_replay`，不插第二行）。
    let (status, body) = keyed_replay(&app, &uri, ws, owner, "key-1").await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["id"], replay_id);
    // 换键：新行。
    let (status, body) = keyed_replay(&app, &uri, ws, owner, "key-2").await;
    assert_eq!(status, 202, "{body}");
    assert_ne!(body["id"], replay_id);

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_delivery WHERE replayed_from_delivery_id = $1",
    )
    .bind(original)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 2, "同一个原投递 + 两个键 = 两行 replay");

    // 原投递没被动过（replay 不改原行）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries/{original}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "failed");
    assert_eq!(body["response_body"], "upstream 500");

    cleanup(&pool, ws, &[owner]).await;
}

/// 带 `Idempotency-Key` 的 replay 请求。
async fn keyed_replay(
    app: &axum::Router,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    key: &str,
) -> (StatusCode, Value) {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(super::support::USER_ID_HEADER, user_id.to_string())
        .header(super::support::WORKSPACE_HEADER, workspace_id.to_string())
        .header("Idempotency-Key", key)
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (status, super::support::body_json(res.into_body()).await)
}
