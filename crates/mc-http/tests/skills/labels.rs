//! `/api/skills/:id/labels` 三条的端到端测试（M6-2 / 上游 `label.go` skill 段）。
//!
//! 上游口径里最容易写错的两条：
//! 1. 标签**目录**是 `issue_label`（`resource_type='skill'`），连接行是 `skill_to_label`
//!    —— POST 是「挂一个已存在的 `label_id`」，不是「新建标签」；
//! 2. `skill_to_label` **没有外键**（迁移 173 把 162 建的级联去掉了，「cleanup 由应用事务负责」），
//!    所以删 skill 必须在事务里显式删连接行 —— 靠级联会静默留下 orphan。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, cleanup, connect, create_skill, error_message, id_of, linked_label_count, seed_label,
    seed_user, seed_workspace,
};

/// 挂 → 列表 → 摘的往返，含幂等、类型闸门与坏 id。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 同上：钩子—列表—摘除的顺序本身就是被测语义
async fn label_lifecycle_is_idempotent_and_typed() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let skill = create_skill(&app, ws, user, json!({ "name": "tagged" })).await;
    let skill_id = id_of(&skill);
    let path = format!("/api/skills/{skill_id}/labels");

    let skill_label = seed_label(&pool, ws, "skill").await;
    let agent_label = seed_label(&pool, ws, "agent").await;

    // 初始为空：`{"labels":[]}`（`labelsToResponse` 保证非 nil）
    let (status, body) = call(&app, "GET", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"], json!([]));

    // 挂 ⇒ 回读全量
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": skill_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["labels"].as_array().unwrap().len(), 1);
    assert_eq!(body["labels"][0]["id"], skill_label.to_string());
    assert_eq!(body["labels"][0]["workspace_id"], ws.to_string());
    assert_eq!(body["labels"][0]["resource_type"], "skill");
    // 上游 `labelToResponse` 不算用量 ⇒ 恒 0
    assert_eq!(body["labels"][0]["usage_count"], 0);

    // 幂等：再挂一次仍只有一行（`ON CONFLICT DO NOTHING`，不是 409）
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": skill_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["labels"].as_array().unwrap().len(), 1);
    assert_eq!(linked_label_count(&pool, skill_id).await, 1);

    // 别的资源类型的标签 ⇒ 404（上游把「不存在 / 别的 workspace / 类型不对」折成一条）
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": agent_label.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "skill label");

    // 不存在的 label id ⇒ 同一个 404
    let (status, _) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 缺 label_id / 空串 / 坏 uuid
    for body in [json!({}), json!({ "label_id": "" }), json!(null)] {
        let (status, res) = call(&app, "POST", &path, ws, user, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{res}");
        assert_eq!(error_message(&res), "label_id is required");
    }
    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        user,
        Some(json!({ "label_id": "not-a-uuid" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "label_id must be a valid uuid");

    // 摘 ⇒ 回读全量；再摘一次也是成功（删 0 行不是错误）
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{path}/{skill_label}"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["labels"], json!([]));
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{path}/{skill_label}"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["labels"], json!([]));

    // 摘的路径参数是 `label id`（不是 `label_id`）—— 上游两处字段名不同
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{path}/not-a-uuid"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "label id must be a valid uuid");

    cleanup(&pool, ws, &[user]).await;
}

/// 删 skill 时必须显式清 `skill_to_label`（无外键 ⇒ 否则留下 orphan 行）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn deleting_a_skill_removes_its_label_links() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let skill = create_skill(&app, ws, user, json!({ "name": "linked" })).await;
    let skill_id = id_of(&skill);
    let label_id = seed_label(&pool, ws, "skill").await;

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/skills/{skill_id}/labels"),
        ws,
        user,
        Some(json!({ "label_id": label_id.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(linked_label_count(&pool, skill_id).await, 1);

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/skills/{skill_id}"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        linked_label_count(&pool, skill_id).await,
        0,
        "skill_to_label 迁移 173 起没有外键：必须在事务里显式删"
    );

    cleanup(&pool, ws, &[user]).await;
}

/// 挂 / 摘要过 `canManageSkill`；列只看 skill 是否可见（上游 `ListLabelsForSkill` 不查）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn label_writes_require_can_manage_but_reads_do_not() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, creator) = seed_workspace(&pool, "member").await;
    let other = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let skill = create_skill(&app, ws, creator, json!({ "name": "perm-labels" })).await;
    let skill_id = id_of(&skill);
    let path = format!("/api/skills/{skill_id}/labels");
    let label_id = seed_label(&pool, ws, "skill").await;

    let (status, _) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &app,
        "POST",
        &path,
        ws,
        other,
        Some(json!({ "label_id": label_id.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        error_message(&body),
        "only the skill creator can manage this skill"
    );

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("{path}/{label_id}"),
        ws,
        other,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(linked_label_count(&pool, skill_id).await, 0);

    cleanup(&pool, ws, &[creator, other]).await;
}
