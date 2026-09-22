//! `/api/agents/:id/labels` 端到端测试（上游 `label.go` L576-660）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, cleanup, connect, create_agent, error_code, error_message, id_of, new_agent_body,
    seed_label, seed_runtime, seed_user, seed_workspace,
};

/// attach → list → detach 的往返，含「非 agent 资源类型 → 404」与 owner 门槛。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn label_lifecycle_is_resource_type_guarded_and_owner_only() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let other = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let created = create_agent(&app, ws, user, new_agent_body("tagged", runtime_id)).await;
    let agent_id = id_of(&created);
    let path = format!("/api/agents/{agent_id}/labels");

    let agent_label = seed_label(&pool, ws, "agent").await;
    let issue_label = seed_label(&pool, ws, "issue").await;

    // 初始为空
    let (status, body) = call(&app, "GET", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"], json!([]));

    // attach：返回全量列表
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": agent_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["labels"].as_array().unwrap().len(), 1);
    assert_eq!(body["labels"][0]["id"], agent_label.to_string());
    assert_eq!(body["labels"][0]["resource_type"], "agent");
    // 上游 `labelToResponse` 不填 usage_count → 恒 0
    assert_eq!(body["labels"][0]["usage_count"], 0);

    // 幂等：再 attach 一次仍只有一条
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": agent_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"].as_array().unwrap().len(), 1);

    // issue 类型的 label → 404 `agent label not found`
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": issue_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");
    // 上游裸文案是 `agent label not found`；本仓统一 `not found: <resource>`（docs/40 §5）。
    assert_eq!(error_message(&body), "agent label");

    // 不存在的 label → 同一个 404
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_message(&body), "agent label");

    // 缺 label_id → 400
    let (status, body) = call(&app, "POST", &path, ws, user, Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "label_id is required");

    // 非 owner 不能 attach（GET 可以：只需能加载到 agent）
    let (status, _) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        &path,
        ws,
        other,
        Some(json!({ "label_id": agent_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // detach：返回剩余列表；再 detach 幂等
    let detach_path = format!("{path}/{agent_label}");
    let (status, body) = call(&app, "DELETE", &detach_path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"], json!([]));
    let (status, body) = call(&app, "DELETE", &detach_path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"], json!([]));

    // 别的 workspace 的同名 label 不可用（`get_label` 带 workspace 过滤）
    let (other_ws, other_user) = seed_workspace(&pool, "owner").await;
    let foreign_label = seed_label(&pool, other_ws, "agent").await;
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": foreign_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    cleanup(&pool, ws, &[user, other]).await;
    cleanup(&pool, other_ws, &[other_user]).await;
}
