//! 用户面 8 条异步往返（`docs/16` §6.2 表 B）的端到端用例。
//!
//! 每条都是**完整闭环**：用户面入队 → daemon 心跳领走（`pending_*` 字段）→ daemon 回
//! 结果 → 用户面轮询看到终态。只测半边（入队后直接查状态）会漏掉「ack 里带没带工作」
//! 这一环，而那正是 daemon 拿不到活儿的唯一原因。

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support;

/// 建一台 `online` 的 runtime（有主，`daemon_id = "m1"`）。
async fn seed_online(pool: &PgPool, workspace_id: Uuid, owner: Uuid) -> Uuid {
    support::seed_runtime(pool, workspace_id, owner, "m1").await
}

/// `POST /api/daemon/heartbeat` 并返回 ack（WS 面的完整 `DaemonHeartbeatAckPayload`）。
async fn heartbeat(app: &axum::Router, user_id: Uuid, runtime_id: Uuid) -> Value {
    let (status, ack) = support::call(
        app,
        "POST",
        "/api/daemon/heartbeat",
        user_id,
        Some("m1"),
        Some(json!({ "runtime_id": runtime_id.to_string(), "supports_batch_import": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "heartbeat: {ack}");
    ack
}

/// update：入队 → 心跳领走 → 回结果 → 轮询终态；同时验证「在飞 ⇒ 409」。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn update_round_trip_and_in_flight_conflict() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, user_id).await;
    let update_path = format!("/api/runtimes/{runtime_id}/update");

    // 入队：200（上游是 200 不是 201），状态 pending。
    let (status, created) = support::call(
        &app,
        "POST",
        &update_path,
        user_id,
        None,
        Some(json!({ "target_version": "2.0.0" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "initiate update: {created}");
    assert_eq!(created["status"], json!("pending"));
    assert_eq!(created["target_version"], json!("2.0.0"));
    let update_id = created["id"].as_str().expect("update id").to_owned();

    // 同一条 runtime 再来一次 ⇒ 409 `an update is already in progress for this runtime`。
    let (status, conflict) = support::call(
        &app,
        "POST",
        &update_path,
        user_id,
        None,
        Some(json!({ "target_version": "2.1.0" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(
        support::error_message(&conflict),
        "an update is already in progress for this runtime"
    );

    // 心跳把请求领走：ack 里带 `pending_update`，台账转 running（`PopPending` 的副作用）。
    let ack = heartbeat(&app, user_id, runtime_id).await;
    assert_eq!(ack["pending_update"]["id"], json!(update_id));
    assert_eq!(ack["pending_update"]["target_version"], json!("2.0.0"));

    let (status, polled) = support::call(
        &app,
        "GET",
        &format!("{update_path}/{update_id}"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    assert_eq!(polled["status"], json!("running"));

    // 第二次心跳不再重复派发（已认领）。
    let ack = heartbeat(&app, user_id, runtime_id).await;
    assert_eq!(ack["pending_update"], Value::Null);

    // 回结果：`output` 带 omitempty 语义 —— 非空才上线。
    let (status, ok) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/update/{update_id}/result"),
        user_id,
        Some("m1"),
        Some(json!({ "status": "completed", "output": "upgraded to 2.0.0" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "report result: {ok}");
    assert_eq!(ok["status"], json!("ok"));

    let (_, done) = support::call(
        &app,
        "GET",
        &format!("{update_path}/{update_id}"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(done["status"], json!("completed"));
    assert_eq!(done["output"], json!("upgraded to 2.0.0"));

    // 终态后重复上报是**幂等** 200，不是 409。
    let (status, _) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/update/{update_id}/result"),
        user_id,
        Some("m1"),
        Some(json!({ "status": "completed", "output": "again" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 未知 update ⇒ 404。
    let (status, missing) = support::call(
        &app,
        "GET",
        &format!("{update_path}/00000000-0000-4000-8000-000000000000"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// models：入队 → 心跳 → 回结果 → 轮询。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn model_list_round_trip() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, user_id).await;

    let (status, created) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/models"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "initiate model list: {created}");
    assert_eq!(created["status"], json!("pending"));
    // 排队期就必须带上 `supported`（上游该字段没有 omitempty）。
    assert_eq!(created["supported"], json!(true));
    let request_id = created["id"].as_str().expect("request id").to_owned();

    let ack = heartbeat(&app, user_id, runtime_id).await;
    assert_eq!(ack["pending_model_list"]["id"], json!(request_id));

    let (status, ok) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/models/{request_id}/result"),
        user_id,
        Some("m1"),
        Some(json!({
            "status": "completed",
            "models": [{ "id": "claude-sonnet", "label": "Sonnet" }],
            "supported": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ok}");

    let (status, done) = support::call(
        &app,
        "GET",
        &format!("/api/runtimes/{runtime_id}/models/{request_id}"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["status"], json!("completed"));
    assert_eq!(done["models"][0]["id"], json!("claude-sonnet"));
    assert_eq!(done["supported"], json!(true));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// local-skills 列表：入队 → 心跳 → 回结果 → 轮询（`mcp_supported` 排队期即上线）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn local_skill_list_round_trip() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, user_id).await;

    let (status, created) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["mcp_supported"], json!(false));
    let request_id = created["id"].as_str().expect("request id").to_owned();

    let ack = heartbeat(&app, user_id, runtime_id).await;
    assert_eq!(ack["pending_local_skills"]["id"], json!(request_id));

    let (status, _) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/local-skills/{request_id}/result"),
        user_id,
        Some("m1"),
        Some(json!({
            "status": "completed",
            "skills": [{ "key": "code-review", "name": "Code Review" }],
            "mcp_supported": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, done) = support::call(
        &app,
        "GET",
        &format!("/api/runtimes/{runtime_id}/local-skills/{request_id}"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(done["status"], json!("completed"));
    assert_eq!(done["mcp_supported"], json!(true));
    assert_eq!(done["skills"][0]["key"], json!("code-review"));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 导入：create 口径不输出 `action`；未知 action ⇒ 400；回结果后轮询能看到 `skill`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn local_skill_import_round_trip() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, user_id).await;

    // 未知 action 在入队前就被拒。
    let (status, bad) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills/import"),
        user_id,
        None,
        Some(json!({ "skill_key": "k", "action": "bogus" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    assert_eq!(support::error_message(&bad), "invalid action");

    // 缺 skill_key ⇒ 400。
    let (status, bad) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills/import"),
        user_id,
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    assert_eq!(support::error_message(&bad), "skill_key is required");

    let (status, created) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills/import"),
        user_id,
        None,
        Some(json!({
            "skill_key": "code-review",
            "name": "Code Review",
            "description": "reviews code",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["skill_key"], json!("code-review"));
    // create 口径：`action` 为空串 ⇒ 线上没有这个键。
    assert!(created.get("action").is_none(), "{created}");
    let request_id = created["id"].as_str().expect("request id").to_owned();

    let ack = heartbeat(&app, user_id, runtime_id).await;
    assert_eq!(ack["pending_local_skill_import"]["id"], json!(request_id));
    // 支持批量导入是本地能力位，两个字段都要有。
    assert_eq!(
        ack["pending_local_skill_imports"].as_array().map(Vec::len),
        Some(1)
    );

    let (status, _) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/local-skills/import/{request_id}/result"),
        user_id,
        Some("m1"),
        Some(json!({ "status": "completed", "skill": { "id": "s-1", "key": "code-review" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, done) = support::call(
        &app,
        "GET",
        &format!("/api/runtimes/{runtime_id}/local-skills/import/{request_id}"),
        user_id,
        None,
        None,
    )
    .await;
    assert_eq!(done["status"], json!("completed"));
    // `skill` 是**服务端自己建的行**（上游 `LocalSkillImportStore.Complete(ctx, id, resp)`，
    // `resp` 来自 `createSkillWithFiles`）⇒ 新 uuid，不是 daemon 报的 `s-1`；
    // 名字也以用户面的 `name` 为准（上游 `if req.Name != nil { name = *req.Name }`）。
    let created_skill_id = done["skill"]["id"].as_str().expect("created skill id");
    assert_ne!(created_skill_id, "s-1");
    Uuid::parse_str(created_skill_id).expect("created skill id is a uuid");
    assert_eq!(done["skill"]["name"], json!("Code Review"));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 本地技能两条门：**列表**是能力读（`G_rc`），**导入**才是 owner-only（`G_ls`）。
///
/// 上游 `InitiateListLocalSkills` / `GetLocalSkillListRequest` 走
/// `requireRuntimeCapabilityReadAccess`（`runtime_local_skills.go:594`、`:617`），owner 门
/// 只加在导入两条上（`requireRuntimeLocalSkillAccess`，同文件 `:572`）—— 导入要读
/// **机器上的真实文件**，列表不读。所以同一位非 owner 成员：列表 200、导入 403。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn local_skill_list_is_readable_but_import_is_owner_only() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, owner) = support::seed_workspace(&pool, "owner").await;
    let member = support::seed_user(&pool, workspace_id, "member").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, owner).await;

    // `private`（上游默认）：非 owner 连读门都过不了 ⇒ 404 `runtime`（不是 403 ——
    // 一个已知但在别人名下的 runtime id 不能当存在性预言机）。
    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills"),
        member,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 翻成 `public`：读门（`canUseRuntimeForAgent`）放行，列表两条都 200。
    sqlx::query("UPDATE agent_runtime SET visibility = 'public' WHERE id = $1")
        .bind(runtime_id)
        .execute(&pool)
        .await
        .expect("publish runtime");
    let (status, created) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills"),
        member,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let request_id = created["id"].as_str().expect("request id").to_owned();
    let (status, polled) = support::call(
        &app,
        "GET",
        &format!("/api/runtimes/{runtime_id}/local-skills/{request_id}"),
        member,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{polled}");

    // 导入两条：owner-only ⇒ 403 `insufficient permissions`（读门先过，所以不是 404）。
    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/local-skills/import"),
        member,
        None,
        Some(json!({ "skill_key": "code-review" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(support::error_message(&body), "insufficient permissions");

    let (status, body) = support::call(
        &app,
        "GET",
        &format!("/api/runtimes/{runtime_id}/local-skills/import/{request_id}"),
        member,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(support::error_message(&body), "insufficient permissions");

    support::cleanup(&pool, workspace_id, &[owner, member]).await;
}

/// 离线 runtime 的**三个入队端点** ⇒ 503 `runtime is offline`（不是 422 —— 本地
/// `Error::RuntimeOffline` 的默认码是 422，daemon 面按上游口径改写）。
///
/// `POST .../update` **不在这组里**：上游 `InitiateUpdate` 没有离线门
/// （`runtime_update.go:213`），只有模型清单与两个技能端点有
/// （`runtime_models.go:353`、`runtime_local_skills.go:601`、`:642`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn enqueue_on_offline_runtime_is_503() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, user_id).await;
    sqlx::query("UPDATE agent_runtime SET status = 'offline' WHERE id = $1")
        .bind(runtime_id)
        .execute(&pool)
        .await
        .expect("mark offline");

    for (method, path, body) in [
        ("POST", format!("/api/runtimes/{runtime_id}/models"), None),
        (
            "POST",
            format!("/api/runtimes/{runtime_id}/local-skills"),
            None,
        ),
        (
            "POST",
            format!("/api/runtimes/{runtime_id}/local-skills/import"),
            Some(json!({ "skill_key": "k" })),
        ),
    ] {
        let (status, error) = support::call(&app, method, &path, user_id, None, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {error}");
        assert_eq!(
            support::error_message(&error),
            "runtime is offline",
            "{path}"
        );
    }

    // update 在离线 runtime 上照常入队（上游没有离线门）—— 回归：别顺手把 503 加上。
    let (status, created) = support::call(
        &app,
        "POST",
        &format!("/api/runtimes/{runtime_id}/update"),
        user_id,
        None,
        Some(json!({ "target_version": "2.0.0" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["status"], json!("pending"));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// `target_version` 为空串 ⇒ 400（上游 `req.TargetVersion == ""`，`runtime_update.go:229`），
/// 不落台账；纯空白**算合法输入**（上游不做 trim，本仓照抄，见模块文档末条）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn update_requires_target_version() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let runtime_id = seed_online(&pool, workspace_id, user_id).await;
    let path = format!("/api/runtimes/{runtime_id}/update");

    let (status, body) = support::call(
        &app,
        "POST",
        &path,
        user_id,
        None,
        Some(json!({ "target_version": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(support::error_message(&body), "target_version is required");

    let (status, accepted) = support::call(
        &app,
        "POST",
        &path,
        user_id,
        None,
        Some(json!({ "target_version": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["target_version"], json!("   "));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}
