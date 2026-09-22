//! `/api/agents*` 的 CRUD / 归档 / 取消任务端到端测试（M3-5 / LUM-1428）。
//!
//! 每条用例都打真 PostgreSQL（`MULTICA_TEST_DATABASE_URL`），走真实
//! `mc_http::routes::router()` —— 手写 INSERT 只用于铺 runtime / label / task
//! 这些**非本片所有**的表。

use axum::http::StatusCode;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::support::{
    call, cleanup, connect, create_agent, error_code, error_message, id_of, new_agent_body,
    seed_runtime, seed_task, seed_user, seed_workspace,
};

/// 建 agent → GET 回读 → 列表包含，且列默认值与上游逐列一致。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn create_get_list_roundtrip_applies_upstream_defaults() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    let created = create_agent(&app, ws, user, new_agent_body("chief", runtime_id)).await;
    let agent_id = id_of(&created);
    assert_eq!(created["name"], "chief");
    assert_eq!(created["workspace_id"], ws.to_string());
    assert_eq!(created["runtime_id"], runtime_id.to_string());
    assert_eq!(created["runtime_bound"], true);
    assert_eq!(created["owner_id"], user.to_string());
    // 上游列默认值（`max_concurrent_tasks` / `visibility` / `permission_mode`）
    assert_eq!(created["max_concurrent_tasks"], 6);
    assert_eq!(created["visibility"], "private");
    assert_eq!(created["permission_mode"], "private");
    assert_eq!(created["status"], "offline");
    assert_eq!(created["runtime_mode"], "local");
    assert_eq!(created["has_custom_env"], false);
    assert_eq!(created["custom_env_key_count"], 0);
    assert_eq!(created["skills"], json!([]));
    assert_eq!(created["conversation_starters"], json!([]));
    assert_eq!(created["invocation_targets"], json!([]));
    assert!(created["archived_at"].is_null());

    let (status, fetched) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["id"], created["id"]);

    let (status, list) = call(&app, "GET", "/api/agents/", ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = list
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert!(ids.contains(&created["id"].as_str().unwrap()));

    cleanup(&pool, ws, &[user]).await;
}

/// 创建期的校验分支：`name` / `runtime_id` / `max_concurrent_tasks` / 私有 runtime / 重名。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn create_validation_and_runtime_binding_errors() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let admin = seed_user(&pool, ws, "admin").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    // name 必填
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(json!({ "runtime_id": runtime_id.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "name is required");

    // runtime_id 必填
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(json!({ "name": "no-runtime" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "runtime_id is required");

    // runtime_id 形态非法 / 不存在 / 属于别的 workspace → 都是 400 invalid runtime_id
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(json!({ "name": "bad-uuid", "runtime_id": "not-a-uuid" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "validation_error");

    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(new_agent_body("missing-runtime", Uuid::new_v4())),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "invalid runtime_id");

    // 别人的 private runtime → 403（admin 也不能越权借用）
    let foreign = seed_user(&pool, ws, "member").await;
    let private_rt = seed_runtime(&pool, ws, Some(foreign), "private").await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(new_agent_body("borrower", private_rt)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(error_message(&body).contains("this runtime is private"));

    // 无主 runtime（`canUseRuntimeForAgent` 的 owner_id 空守卫）→ 403
    let orphan_rt = seed_runtime(&pool, ws, None, "public").await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(new_agent_body("orphan-host", orphan_rt)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // max_concurrent_tasks 越界 / 非数字
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        user,
        Some(json!({
            "name": "too-many",
            "runtime_id": runtime_id.to_string(),
            "max_concurrent_tasks": 51
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_message(&body),
        "max_concurrent_tasks must be between 1 and 50"
    );

    // 显式 null → 落默认值 6
    let null_max = create_agent(
        &app,
        ws,
        user,
        json!({
            "name": "null-max",
            "runtime_id": runtime_id.to_string(),
            "max_concurrent_tasks": null
        }),
    )
    .await;
    assert_eq!(null_max["max_concurrent_tasks"], 6);

    // 重名 → 409
    let _ = create_agent(&app, ws, user, new_agent_body("dup", runtime_id)).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/",
        ws,
        admin,
        Some(new_agent_body("dup", runtime_id)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "conflict");

    cleanup(&pool, ws, &[user, admin, foreign]).await;
}

/// PUT：owner 可改；非 owner 一律 403；admin 能改普通字段但**不能**改权限。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn update_permission_gates_match_upstream() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let admin = seed_user(&pool, ws, "admin").await;
    let other = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let created = create_agent(&app, ws, user, new_agent_body("editable", runtime_id)).await;
    let agent_id = id_of(&created);
    let path = format!("/api/agents/{agent_id}/");

    // owner 改普通字段
    let (status, updated) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({
            "description": "runs the show",
            "instructions": "be brief",
            "max_concurrent_tasks": 12,
            "model": "claude-sonnet",
            "conversation_starters": [{"label": "  hi  ", "prompt": " say hi "}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["description"], "runs the show");
    assert_eq!(updated["instructions"], "be brief");
    assert_eq!(updated["max_concurrent_tasks"], 12);
    assert_eq!(updated["model"], "claude-sonnet");
    assert_eq!(updated["conversation_starters"][0]["label"], "hi");

    // 非 owner 的 member：连描述都改不了（上游 `canManageAgent` 先拦）
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        other,
        Some(json!({ "description": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        error_message(&body),
        "only the agent owner can manage this agent"
    );

    // admin 可以改普通字段（不是 owner 也算 canManage）
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        admin,
        Some(json!({ "description": "admin edited" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "admin edited");

    // admin 改权限 → 403（owner-only）
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        admin,
        Some(json!({ "permission_mode": "public_to" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(error_message(&body).contains("only the agent owner can change access"));

    // owner 把 agent 开给某个成员：非 owner 立刻可见，未列名的成员仍 403
    let (status, opened) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({
            "permission_mode": "public_to",
            "invocation_targets": [{"target_type": "member", "target_id": other.to_string()}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["permission_mode"], "public_to");
    // `public_to` 只给 member 目标 → legacy visibility 仍是 private
    assert_eq!(opened["visibility"], "private");
    assert_eq!(opened["invocation_targets"].as_array().unwrap().len(), 1);

    let (status, _) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GET", &path, ws, admin, None).await;
    assert_eq!(status, StatusCode::OK);

    // `workspace` 目标 → legacy visibility 升为 workspace
    let (status, shared) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({
            "permission_mode": "public_to",
            "invocation_targets": [{"target_type": "workspace"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shared["visibility"], "workspace");

    // 注意：`canManageAgent` 在权限判定**之前**（上游 `agent.go:1885`），所以普通
    // member 连 PUT 都进不来；`permissionInputChangesAgent` 的「无改动重放放行」
    // 只对**非 owner 的 admin** 生效。
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        admin,
        Some(json!({
            "permission_mode": "public_to",
            "invocation_targets": [{ "target_type": "workspace" }]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin 原样重放同一份权限 → 放行: {body}"
    );
    assert_eq!(body["permission_mode"], "public_to");
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        admin,
        Some(json!({ "permission_mode": "private" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(error_message(&body).contains("only the agent owner can change access"));
    // legacy-only 形态的重放也按「派生 visibility」比较
    let (status, _) = call(
        &app,
        "PUT",
        &path,
        ws,
        admin,
        Some(json!({ "visibility": "workspace" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        admin,
        Some(json!({ "visibility": "private" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // `custom_env` 在 PUT 上被硬拒（防静默丢密钥）
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "custom_env": { "A": "1" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_message(&body).contains("custom_env is no longer accepted"));

    cleanup(&pool, ws, &[user, admin, other]).await;
}

/// 归档 / 恢复 / 取消任务 / 任务列表的完整生命周期。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn archive_restore_cancel_tasks_and_task_list() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let outsider = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let created = create_agent(&app, ws, user, new_agent_body("dutiful", runtime_id)).await;
    let agent_id = id_of(&created);

    let _queued = seed_task(&pool, runtime_id, agent_id, "queued", None).await;
    let _running = seed_task(&pool, runtime_id, agent_id, "running", None).await;
    let _done = seed_task(&pool, runtime_id, agent_id, "completed", Some("1 hour")).await;

    // 任务列表：3 条，`created_at DESC`，窄投影字段齐
    let (status, tasks) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/tasks"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tasks = tasks.as_array().expect("array").clone();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0]["agent_id"], agent_id.to_string());
    assert!(tasks.iter().any(|t| t["status"] == "completed"));
    assert!(
        tasks[0].get("workspace_id").is_none(),
        "窄投影不带 workspace_id"
    );
    assert!(tasks[0]["runtime_id"].is_string());

    // 取消在飞任务 → `{cancelled: 2}`，终态不受影响
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/cancel-tasks"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cancelled"], 2);
    let (_, body) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/cancel-tasks"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(body["cancelled"], 0);

    let (_, tasks) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/tasks"),
        ws,
        user,
        None,
    )
    .await;
    let statuses: Vec<&str> = tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses.iter().filter(|s| **s == "cancelled").count(), 2);
    assert_eq!(statuses.iter().filter(|s| **s == "completed").count(), 1);

    // 取消任务需要 canManage：别人 403
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/cancel-tasks"),
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 归档 → archived_at 落值；重复归档 409
    let (status, archived) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/archive"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert!(archived["archived_at"].is_string());
    assert_eq!(archived["archived_by"], user.to_string());
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/archive"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_message(&body), "agent is already archived");

    // 默认列表不含归档；`include_archived=true` 含
    let (_, list) = call(&app, "GET", "/api/agents/", ws, user, None).await;
    assert!(list.as_array().unwrap().is_empty());
    let (_, list) = call(
        &app,
        "GET",
        "/api/agents/?include_archived=true",
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 恢复 → archived_at 清空；重复恢复 409
    let (status, restored) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/restore"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(restored["archived_at"].is_null());
    assert!(restored["archived_by"].is_null());
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/restore"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_message(&body), "agent is not archived");

    cleanup(&pool, ws, &[user, outsider]).await;
}

/// 系统 agent（`system_key` 非空）不可归档；未知 id / 别的 workspace 的 id → 404。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn system_agent_is_unarchivable_and_missing_ids_are_404() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let created = create_agent(&app, ws, user, new_agent_body("builtin", runtime_id)).await;
    let agent_id = id_of(&created);
    sqlx::query("UPDATE agent SET system_key = 'chief_of_staff' WHERE id = $1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .expect("set system_key");

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/archive"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_message(&body),
        "this agent is built into Multica and cannot be archived"
    );

    // 不存在的 id / 形态非法的 id → 404 agent
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{}/", Uuid::new_v4()),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call(&app, "GET", "/api/agents/not-a-uuid/", ws, user, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");
    // 上游裸文案是 `agent not found`；本仓统一走 `not found: <resource>`（docs/40 §5）。
    assert_eq!(error_message(&body), "agent");

    cleanup(&pool, ws, &[user]).await;
}

/// 空 body / 非对象 body → 400；`null` body 退化为「字段全缺」（上游同行为）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn malformed_bodies_are_rejected() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let created = create_agent(&app, ws, user, new_agent_body("bodies", runtime_id)).await;
    let path = format!("/api/agents/{}/", id_of(&created));

    // 空 body（`Body::empty()`）→ 400
    let (status, _) = call(&app, "PUT", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(&app, "PUT", &path, ws, user, Some(json!([1, 2]))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // `null` body → 所有字段缺失，等于空 patch（保持原值）
    let (status, body) = call(&app, "PUT", &path, ws, user, Some(Value::Null)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "bodies");

    cleanup(&pool, ws, &[user]).await;
}
