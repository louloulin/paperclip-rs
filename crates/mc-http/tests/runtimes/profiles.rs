//! `/api/workspaces/{id}/runtime-profiles` 6 条路由的端到端测试。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, call_anon, call_status, cleanup, connect, error_code, error_message, flat_code,
    profile_body, profile_path, seed_agent, seed_profile, seed_runtime, seed_user, seed_workspace,
    RuntimeSeed,
};

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn profile_reads_require_a_workspace_member() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, member) = seed_workspace(&pool, "member").await;
    let (other_workspace, outsider) = seed_workspace(&pool, "member").await;

    assert_eq!(
        call_anon(&app, "GET", &profile_path(workspace_id, "")).await,
        StatusCode::UNAUTHORIZED
    );

    // 非成员 → 404（不暴露 workspace 是否存在）。
    let (status, body) = call(
        &app,
        "GET",
        &profile_path(workspace_id, ""),
        workspace_id,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");

    // 非法 workspace 段 → 也是 404：成员检查先跑，查不出成员行（上游同序）。
    let (status, _) = call(
        &app,
        "GET",
        &profile_path(workspace_id, "").replace(&workspace_id.to_string(), "not-a-uuid"),
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(
        &app,
        "GET",
        &profile_path(workspace_id, ""),
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 上游回 `{"runtime_profiles":[...]}`，不是裸数组。
    assert_eq!(body["runtime_profiles"], json!([]));

    cleanup(&pool, workspace_id, &[member]).await;
    cleanup(&pool, other_workspace, &[outsider]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn profile_reads_and_writes_are_scoped_to_the_path_workspace() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let (other_workspace, other_admin) = seed_workspace(&pool, "admin").await;

    let profile_id = seed_profile(&pool, workspace_id, "in-house codex", "codex", "codex").await;

    // 同一个人是另一个 workspace 的 admin，也不能用那个 workspace 的路径读到本 workspace 的 profile。
    let (status, _) = call(
        &app,
        "GET",
        &profile_path(other_workspace, &format!("/{profile_id}")),
        other_workspace,
        other_admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(
        &app,
        "GET",
        &profile_path(workspace_id, &format!("/{profile_id}")),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["display_name"], "in-house codex");
    assert_eq!(body["workspace_id"], workspace_id.to_string());

    // 非法 profile id → 400（不是 404）。
    let (status, body) = call(
        &app,
        "GET",
        &profile_path(workspace_id, "/not-a-uuid"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "profile id must be a uuid");

    // 不存在但合法的 id → 404。
    let (status, _) = call(
        &app,
        "GET",
        &profile_path(workspace_id, &format!("/{}", Uuid::new_v4())),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, workspace_id, &[admin]).await;
    cleanup(&pool, other_workspace, &[other_admin]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn profile_create_requires_admin_and_validates_every_field() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let uri = profile_path(workspace_id, "");

    // 普通成员写 → 403（列表读得到，写不了）。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        member,
        Some(profile_body("member attempt", "claude")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "forbidden");

    let cases: Vec<(serde_json::Value, &str)> = vec![
        (
            json!({ "runtime_type": "claude", "command_name": "x" }),
            "display_name is required",
        ),
        (
            json!({ "display_name": "x", "runtime_type": "not-a-runtime", "command_name": "x" }),
            "unsupported runtime_type: not-a-runtime",
        ),
        (
            json!({
                "display_name": "x",
                "runtime_type": "claude",
                "protocol_family": "codex",
                "command_name": "x"
            }),
            "protocol_family does not match runtime_type",
        ),
        (
            json!({ "display_name": "x", "runtime_type": "claude" }),
            "command_name is required",
        ),
        (
            json!({
                "display_name": "x",
                "runtime_type": "claude",
                "command_name": "claude --dangerously"
            }),
            "command_name must be a single executable token; put arguments in fixed_args",
        ),
        (
            json!({
                "display_name": "x",
                "runtime_type": "claude",
                "command_name": "claude",
                "fixed_args": ["--ok", "  "]
            }),
            "fixed_args entries must be non-empty",
        ),
    ];
    for (body, expected) in cases {
        let (status, response) = call(&app, "POST", &uri, workspace_id, admin, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "case: {expected}");
        assert_eq!(error_message(&response), expected);
    }

    // happy path：201 + `visibility` 由服务端固定 `workspace`。
    let (status, created) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        admin,
        Some(profile_body("  in-house codex  ", "codex")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {created}");
    assert_eq!(created["display_name"], "in-house codex");
    assert_eq!(created["protocol_family"], "codex");
    assert_eq!(created["runtime_type"], "codex");
    assert_eq!(created["command_name"], "my-agent");
    assert_eq!(created["fixed_args"], json!(["--mode", "json"]));
    assert_eq!(created["visibility"], "workspace");
    assert_eq!(created["enabled"], true);
    assert_eq!(created["created_by"], json!(null));

    // 重名 → 409（前端按 code 分支）。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        admin,
        Some(profile_body("in-house codex", "codex")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "conflict");

    // `runtime_type` 缺省 → 回退到 protocol_family（老 profile 的形态）。
    let (status, created) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "display_name": "fallback", "protocol_family": "claude", "command_name": "cc" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["runtime_type"], "claude");
    assert_eq!(created["protocol_family"], "claude");

    // `omp` 是复用 `pi` 协议族的独立 CLI：protocol_family 派生为 `pi`，不是 `omp`。
    let (status, created) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        admin,
        Some(profile_body("oh my pi", "omp")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "omp create failed: {created}");
    assert_eq!(created["runtime_type"], "omp");
    assert_eq!(created["protocol_family"], "pi");

    cleanup(&pool, workspace_id, &[admin, member]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn profile_update_is_partial_and_keeps_the_backend_immutable() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let profile_id = seed_profile(&pool, workspace_id, "in-house codex", "codex", "codex").await;
    let uri = profile_path(workspace_id, &format!("/{profile_id}"));

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        member,
        Some(json!({ "display_name": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "forbidden");

    // 改后端会把已绑定的 agent 悄悄指向另一个 CLI → 一律 400，先建新 profile。
    for immutable in [
        json!({ "runtime_type": "claude" }),
        json!({ "protocol_family": "claude" }),
    ] {
        let (status, body) = call(&app, "PATCH", &uri, workspace_id, admin, Some(immutable)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error_message(&body),
            "runtime_type and protocol_family are immutable; create a new profile"
        );
    }

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "display_name": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "display_name cannot be empty");

    // 局部更新：只动给了的字段。
    let (status, updated) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        admin,
        Some(json!({
            "display_name": "  renamed  ",
            "description": "",
            "fixed_args": ["--json"],
            "enabled": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update failed: {updated}");
    assert_eq!(updated["display_name"], "renamed");
    assert_eq!(updated["description"], "");
    assert_eq!(updated["fixed_args"], json!(["--json"]));
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["protocol_family"], "codex");

    // PUT 与 PATCH 同一条 handler（装过的客户端用 PUT 做全量替换）。
    let (status, updated) = call(
        &app,
        "PUT",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "display_name": "renamed again" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["display_name"], "renamed again");
    assert_eq!(updated["enabled"], false);

    // 撞上另一个 profile 的名字 → 409。
    seed_profile(&pool, workspace_id, "taken", "claude", "claude").await;
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "display_name": "taken" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "conflict");

    let (status, _) = call(
        &app,
        "PATCH",
        &profile_path(workspace_id, &format!("/{}", Uuid::new_v4())),
        workspace_id,
        admin,
        Some(json!({ "display_name": "ghost" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, workspace_id, &[admin, member]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn profile_delete_is_204_and_blocked_while_agents_are_bound() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;

    let profile_id = seed_profile(&pool, workspace_id, "in-house codex", "codex", "codex").await;
    let instance = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed {
            daemon_id: Some("daemon-a"),
            provider: "codex",
            profile_id: Some(profile_id),
            ..Default::default()
        },
    )
    .await;
    let agent_id = seed_agent(&pool, workspace_id, instance, "codex-agent", None).await;
    let uri = profile_path(workspace_id, &format!("/{profile_id}"));

    let (status, _) = call(&app, "DELETE", &uri, workspace_id, member, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 活跃 agent 还在 → 409 扁平体，前端按 `code` 重开弹窗。
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(flat_code(&body), "runtime_profile_has_active_agents");
    assert_eq!(body["active_agent_count"], 1);
    assert_eq!(body["active_agents"][0]["name"], "codex-agent");
    assert_eq!(body["active_agents"][0]["runtime_status"], "online");
    assert!(
        body["error"].as_str().unwrap().contains("in-house codex"),
        "refusal should name the profile: {body}"
    );

    // 解绑后 204，并且 profile 名下的 runtime 实例行一起被清掉（迁移 120 之后
    // 这层清理只能在应用层做，正是本路径的职责）。
    sqlx::query("UPDATE agent SET runtime_id = NULL WHERE id = $1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .expect("unbind agent");

    assert_eq!(
        call_status(&app, "DELETE", &uri, workspace_id, admin, None).await,
        StatusCode::NO_CONTENT
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_runtime WHERE profile_id = $1")
            .bind(profile_id)
            .fetch_one(&pool)
            .await
            .expect("count runtime instances");
    assert_eq!(remaining, 0, "profile delete must not orphan instances");

    let (status, _) = call(&app, "DELETE", &uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, workspace_id, &[admin, member]).await;
}
