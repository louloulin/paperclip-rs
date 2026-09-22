//! `/api/runtimes*` 9 条台账路由的端到端测试。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, call_anon, call_raw, call_status, cleanup, connect, empty_body_req, error_message,
    flat_code, ids_of, seed_agent, seed_profile, seed_runtime, seed_task, seed_user,
    seed_workspace, user_only_req, RuntimeSeed,
};

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn runtime_list_scopes_by_role_and_owner_param() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let (other_workspace, outsider) = seed_workspace(&pool, "member").await;

    let rt_public = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "public",
        RuntimeSeed::default(),
    )
    .await;
    let rt_member = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let rt_admin_private = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed::default(),
    )
    .await;

    assert_eq!(
        call_anon(&app, "GET", "/api/runtimes/").await,
        StatusCode::UNAUTHORIZED
    );

    // 没有 workspace 选择器 → 400（header 与 query 都缺）。
    let (status, body) = call_raw(&app, user_only_req("GET", "/api/runtimes/", member)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_message(&body).contains("workspace_id"));

    // 非成员 → 404，不泄露 workspace 是否存在。
    let (status, _) = call(&app, "GET", "/api/runtimes/", workspace_id, outsider, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // admin：全部；成员：自己的 + public；`?owner=me`：只要自己的。
    let (status, body) = call(&app, "GET", "/api/runtimes/", workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array(), "list must be a bare array: {body}");
    let mut all = ids_of(&body);
    all.sort_unstable();
    let mut expected = vec![
        rt_public.to_string(),
        rt_member.to_string(),
        rt_admin_private.to_string(),
    ];
    expected.sort_unstable();
    assert_eq!(all, expected);

    let (_, body) = call(&app, "GET", "/api/runtimes/", workspace_id, member, None).await;
    let mut visible = ids_of(&body);
    visible.sort_unstable();
    let mut expected_visible = vec![rt_public.to_string(), rt_member.to_string()];
    expected_visible.sort_unstable();
    assert_eq!(visible, expected_visible);

    let (_, body) = call(
        &app,
        "GET",
        "/api/runtimes/?owner=me",
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(ids_of(&body), vec![rt_member.to_string()]);

    let (_, body) = call(
        &app,
        "GET",
        "/api/runtimes/?owner=me",
        workspace_id,
        admin,
        None,
    )
    .await;
    let mut mine = ids_of(&body);
    mine.sort_unstable();
    let mut expected_mine = vec![rt_public.to_string(), rt_admin_private.to_string()];
    expected_mine.sort_unstable();
    assert_eq!(mine, expected_mine);

    // 无斜杠别名与尾斜杠形态是同一条路由（⑦ 会把两者折叠，只有 e2e 看得见）。
    assert_eq!(
        call_status(&app, "GET", "/api/runtimes", workspace_id, admin, None).await,
        StatusCode::OK
    );

    cleanup(&pool, workspace_id, &[admin, member]).await;
    cleanup(&pool, other_workspace, &[outsider]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn runtime_patch_gates_visibility_and_validates_before_writing() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;

    let rt = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed {
            custom_name: Some("old name"),
            ..Default::default()
        },
    )
    .await;
    let uri = format!("/api/runtimes/{rt}/");

    // 普通成员改别人的机器 → 403（admin 在位也不放宽这条）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "visibility": "public" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        error_message(&body),
        "only the runtime owner can change its visibility"
    );

    let other_rt = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "public",
        RuntimeSeed::default(),
    )
    .await;
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/runtimes/{other_rt}/"),
        workspace_id,
        member,
        Some(json!({ "custom_name": "mine now" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_message(&body), "you can only edit your own runtimes");

    // 非法可见性 → 400；原样回显未变的可见性 → 200（PATCH-as-PUT 容错）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "visibility": "shared" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_message(&body),
        "visibility must be 'private' or 'public'"
    );

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        admin,
        Some(json!({ "visibility": "private" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "echoing visibility should pass: {body}"
    );
    assert_eq!(body["visibility"], "private");

    // 一个字段非法 → 整个 PATCH 不落地（先校验全部字段再写）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        member,
        Some(json!({
            "visibility": "public",
            "custom_name": "x".repeat(101)
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "custom name is too long");
    let (visibility, custom_name): (String, Option<String>) =
        sqlx::query_as("SELECT visibility, custom_name FROM agent_runtime WHERE id = $1")
            .bind(rt)
            .fetch_one(&pool)
            .await
            .expect("reload runtime");
    assert_eq!(visibility, "private", "failed PATCH must not half-apply");
    assert_eq!(custom_name.as_deref(), Some("old name"));

    // 100 字符（runes，不是字节）是合法上界。
    let long = "名".repeat(100);
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        workspace_id,
        member,
        Some(json!({ "visibility": "public", "custom_name": long })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "100-rune name should pass: {body}");
    assert_eq!(body["visibility"], "public");
    assert_eq!(body["custom_name"], "名".repeat(100));

    // 空串 = 清掉覆盖值（回落到 daemon 提的 `name`）。
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/runtimes/{rt}"),
        workspace_id,
        member,
        Some(json!({ "custom_name": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["custom_name"], json!(null));

    let (status, _) = call(
        &app,
        "PATCH",
        "/api/runtimes/not-a-uuid/",
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/runtimes/{}/", Uuid::new_v4()),
        workspace_id,
        member,
        Some(json!({ "visibility": "public" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, workspace_id, &[admin, member]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn runtime_patch_apply_to_machine_relabels_the_whole_daemon() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;

    // 同一台机器上的三条 runtime（provider 不同 —— `(workspace_id, daemon_id, provider)`
    // 是唯一键，provider 相同就得再区分 profile）。
    let mine_a = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed {
            daemon_id: Some("shared-machine"),
            provider: "claude",
            ..Default::default()
        },
    )
    .await;
    let mine_b = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed {
            daemon_id: Some("shared-machine"),
            provider: "codex",
            ..Default::default()
        },
    )
    .await;
    let theirs = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed {
            daemon_id: Some("shared-machine"),
            provider: "pi",
            ..Default::default()
        },
    )
    .await;

    // 普通成员只能改自己在同一 daemon 上的行（owner_filter）。
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/runtimes/{mine_a}/"),
        workspace_id,
        member,
        Some(json!({ "custom_name": "my laptop", "apply_to_machine": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // admin 打同样的旗标 → 整台机器都改名（owner_filter = None）。
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/runtimes/{theirs}/"),
        workspace_id,
        admin,
        Some(json!({ "custom_name": "team machine", "apply_to_machine": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["custom_name"], "team machine");

    let rows: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, custom_name FROM agent_runtime WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_all(&pool)
            .await
            .expect("reload runtimes");
    let by_id: std::collections::HashMap<Uuid, Option<String>> = rows.into_iter().collect();
    assert_eq!(
        by_id[&mine_a].as_deref(),
        Some("team machine"),
        "member's first row is on the same daemon → relabelled by the admin write"
    );
    assert_eq!(by_id[&mine_b].as_deref(), Some("team machine"));
    assert_eq!(by_id[&theirs].as_deref(), Some("team machine"));

    // `apply_to_machine` 只认 daemon 作用域：不带旗标时只有目标行改名。
    let solo = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed {
            daemon_id: Some("solo-machine"),
            provider: "claude",
            ..Default::default()
        },
    )
    .await;
    call(
        &app,
        "PATCH",
        &format!("/api/runtimes/{solo}/"),
        workspace_id,
        member,
        Some(json!({ "custom_name": "solo" })),
    )
    .await;
    let name: Option<String> =
        sqlx::query_scalar("SELECT custom_name FROM agent_runtime WHERE id = $1")
            .bind(solo)
            .fetch_one(&pool)
            .await
            .expect("reload solo");
    assert_eq!(name.as_deref(), Some("solo"));

    cleanup(&pool, workspace_id, &[admin, member]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn runtime_delete_strict_refuses_while_agents_are_bound() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let (other_workspace, outsider) = seed_workspace(&pool, "member").await;

    let rt = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let agent = seed_agent(&pool, workspace_id, rt, "worker", None).await;
    let uri = format!("/api/runtimes/{rt}/");

    let (status, _) = call(&app, "DELETE", &uri, workspace_id, outsider, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "non-member sees no oracle");

    // 活跃 agent 挡路 → 409 扁平体（前端按 `code` 提示「先解绑」）。
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(flat_code(&body), "runtime_has_active_agents");
    assert_eq!(body["active_agents"][0]["name"], "worker");
    assert_eq!(body["active_agents"][0]["runtime_status"], "online");

    // 归档后严格删除放行，并且归档的 agent 是**解绑保留**而不是删掉。
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(agent)
        .execute(&pool)
        .await
        .expect("archive agent");
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::OK, "delete failed: {body}");
    assert_eq!(body, json!({ "status": "ok" }));

    let runtime_id: Option<Uuid> = sqlx::query_scalar("SELECT runtime_id FROM agent WHERE id = $1")
        .bind(agent)
        .fetch_one(&pool)
        .await
        .expect("reload agent");
    assert_eq!(runtime_id, None, "agent survives, just unbound");
    let gone: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_runtime WHERE id = $1")
        .bind(rt)
        .fetch_one(&pool)
        .await
        .expect("count runtime");
    assert_eq!(gone, 0);

    let (status, body) = call(&app, "DELETE", &uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_message(&body), "runtime");

    let (status, _) = call(
        &app,
        "DELETE",
        "/api/runtimes/nope/",
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    cleanup(&pool, workspace_id, &[admin, member]).await;
    cleanup(&pool, other_workspace, &[outsider]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn unbind_and_delete_returns_counts_and_checks_the_confirmed_plan() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;

    let rt = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let agent_a = seed_agent(&pool, workspace_id, rt, "alpha", None).await;
    let agent_b = seed_agent(&pool, workspace_id, rt, "beta", Some("mika")).await;
    seed_task(&pool, workspace_id, admin, rt, agent_a, "queued").await;

    // 计划漂移：用户确认的是空集，实际有两个 → 409 + 最新快照。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/runtimes/{rt}/unbind-agents-and-delete"),
        workspace_id,
        admin,
        Some(json!({ "expected_active_agent_ids": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(flat_code(&body), "runtime_delete_plan_changed");
    assert_eq!(body["active_agents"].as_array().map(Vec::len), Some(2));

    // 非法 UUID 不会被静默忽略（否则会去比对另一组集合）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/runtimes/{rt}/unbind-agents-and-delete"),
        workspace_id,
        admin,
        Some(json!({ "expected_active_agent_ids": ["not-a-uuid"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_message(&body),
        "expected_active_agent_ids must be a list of valid UUIDs"
    );

    // 空 body 是 400（上游 `json.Decode` 对空体也是错），不是「空计划」。
    let (status, body) = call_raw(
        &app,
        empty_body_req(
            "POST",
            &format!("/api/runtimes/{rt}/unbind-agents-and-delete"),
            workspace_id,
            admin,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "invalid request body");

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/runtimes/{}/unbind-agents-and-delete", Uuid::new_v4()),
        workspace_id,
        admin,
        Some(json!({ "expected_active_agent_ids": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 确认后：agent 解绑保留、非终态任务取消、系统 agent 删掉。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/runtimes/{rt}/unbind-agents-and-delete"),
        workspace_id,
        admin,
        Some(json!({
            "expected_active_agent_ids": [agent_a.to_string(), agent_b.to_string()]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unbind failed: {body}");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["agents_unbound"], 2);
    assert_eq!(
        body["agents_archived"], 2,
        "legacy mirror of agents_unbound"
    );
    assert_eq!(body["tasks_cancelled"], 1);

    let alive: i64 = sqlx::query_scalar("SELECT count(*) FROM agent WHERE id = ANY($1)")
        .bind(vec![agent_a, agent_b])
        .fetch_one(&pool)
        .await
        .expect("count agents");
    assert_eq!(alive, 2, "user agents survive unbinding");
    let unbound: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent WHERE id = ANY($1) AND runtime_id IS NOT NULL",
    )
    .bind(vec![agent_a, agent_b])
    .fetch_one(&pool)
    .await
    .expect("count bound agents");
    assert_eq!(unbound, 0);

    // 旧客户端的 `archive-agents-and-delete` 走同一条 handler。
    let rt2 = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let agent_c = seed_agent(&pool, workspace_id, rt2, "gamma", None).await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/runtimes/{rt2}/archive-agents-and-delete"),
        workspace_id,
        admin,
        Some(json!({ "expected_active_agent_ids": [agent_c.to_string()] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "archive alias failed: {body}");
    assert_eq!(body["agents_unbound"], 1);

    cleanup(&pool, workspace_id, &[admin]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn profile_backed_runtimes_refuse_individual_deletion() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let profile = seed_profile(&pool, workspace_id, "in-house codex", "codex", "codex").await;

    let online = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed {
            daemon_id: Some("machine-online"),
            provider: "codex",
            profile_id: Some(profile),
            status: "online",
            ..Default::default()
        },
    )
    .await;
    seed_agent(&pool, workspace_id, online, "codex-agent", None).await;

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/runtimes/{online}/"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        flat_code(&body),
        "runtime_profile_instance_delete_unsupported"
    );
    assert_eq!(body["profile_id"], profile.to_string());
    assert_eq!(body["profile_name"], "in-house codex");
    assert_eq!(body["runtime_status"], "online");
    assert_eq!(body["active_agent_count"], 1);
    assert_eq!(body["auto_cleanup_after_days"], 7);
    assert!(
        body["error"].as_str().unwrap().contains("still online"),
        "online instance must warn the daemon would re-register: {body}"
    );

    // 离线且无人绑 → 只是「等着被 GC 回收」，不是建议去删 profile。
    let idle = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed {
            daemon_id: Some("machine-idle"),
            provider: "codex",
            profile_id: Some(profile),
            status: "offline",
            ..Default::default()
        },
    )
    .await;
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/runtimes/{idle}/"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["active_agent_count"], 0);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("removes it automatically after 7 days offline"),
        "idle instance should point at GC: {body}"
    );

    cleanup(&pool, workspace_id, &[admin]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn trailing_slash_and_bare_paths_hit_the_same_handlers() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let rt = seed_runtime(
        &pool,
        workspace_id,
        Some(admin),
        "private",
        RuntimeSeed::default(),
    )
    .await;

    // `matchit` 把 `/x` 与 `/x/` 当两条路由：只注册一种，另一种就是 404
    // （不是 307 重定向），而 ⑦ 的 parity 脚本会把两种写法折叠、看不见这个故障。
    for uri in ["/api/runtimes", "/api/runtimes/"] {
        assert_eq!(
            call_status(&app, "GET", uri, workspace_id, admin, None).await,
            StatusCode::OK,
            "GET {uri}"
        );
    }
    for uri in [
        format!("/api/runtimes/{rt}"),
        format!("/api/runtimes/{rt}/"),
    ] {
        let (status, body) = call(
            &app,
            "PATCH",
            &uri,
            workspace_id,
            admin,
            Some(json!({ "custom_name": "via alias" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "PATCH {uri}: {body}");
        assert_eq!(body["custom_name"], "via alias");
    }
    // 不带斜杠的别名也要能删（每种形态各用一条 runtime：删过一次就 404 了）。
    for uri in [
        format!("/api/runtimes/{rt}"),
        format!("/api/runtimes/{rt}/"),
    ] {
        let target = if uri.ends_with('/') {
            seed_runtime(
                &pool,
                workspace_id,
                Some(admin),
                "private",
                RuntimeSeed::default(),
            )
            .await
        } else {
            rt
        };
        let uri = uri.replace(&rt.to_string(), &target.to_string());
        let (status, body) = call(&app, "DELETE", &uri, workspace_id, admin, None).await;
        assert_eq!(status, StatusCode::OK, "DELETE {uri}: {body}");
    }

    cleanup(&pool, workspace_id, &[admin]).await;
}
