//! agent-builder 四条路由（`/api/agent-builder/sessions*`）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    assert_bad_request, call, cleanup, error_code, error_message, id_of, req_with,
    seed_chat_session, seed_foreign_user, seed_runtime, seed_user, send, setup, TaskSeed,
};

const CREATE_URI: &str = "/api/agent-builder/sessions/";

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn session_lifecycle_create_draft_switch_and_list() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();

    let (status, body) = call(
        &app,
        "POST",
        CREATE_URI,
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "runtime_id": fx.runtime_id.to_string(), "model": "claude-sonnet" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["runtime_id"], fx.runtime_id.to_string());
    let session = id_of(&json!({ "id": body["session_id"] }));
    let builder_agent = Uuid::parse_str(body["builder_agent_id"].as_str().expect("agent"))
        .expect("builder agent uuid");

    // 载体是隐藏 agent：`kind='system'` + `system_key` 前缀，且 owner 是调用者
    let carrier = sqlx::query_as::<_, (String, Option<Uuid>, Option<String>)>(
        "SELECT kind, owner_id, system_key FROM agent WHERE id = $1",
    )
    .bind(builder_agent)
    .fetch_one(&fx.pool)
    .await
    .expect("carrier agent");
    assert_eq!(carrier.0, "system");
    assert_eq!(carrier.1, Some(fx.user_id));
    assert!(carrier.2.expect("system_key").starts_with("agent_builder:"));

    // 还没存草稿、也没消息 ⇒ 列表里看不到（上游同一道门）
    let (status, body) = call(&app, "GET", CREATE_URI, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"], json!([]));
    // 注册了尾斜杠别名，两种形态都要能命中
    let (status, body) = call(
        &app,
        "GET",
        "/api/agent-builder/sessions",
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 存草稿 → 204
    let draft = json!({ "name": "draft-agent", "instructions": "be nice" });
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/agent-builder/sessions/{session}/draft"),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": draft })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(body.is_null(), "204 没有正文");

    let (status, body) = call(&app, "GET", CREATE_URI, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1, "{body}");
    assert_eq!(sessions[0]["session_id"], session.to_string());
    assert_eq!(sessions[0]["runtime_id"], fx.runtime_id.to_string());
    assert_eq!(sessions[0]["draft"], draft);
    assert!(
        sessions[0]["created_at"]
            .as_str()
            .is_some_and(|v| v.contains('T')),
        "{body}"
    );

    // 切 runtime：200 + 新 id
    let runtime_two = seed_runtime(
        &fx.pool,
        fx.workspace_id,
        Some(fx.user_id),
        "public",
        "online",
    )
    .await;
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/agent-builder/sessions/{session}/runtime"),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "runtime_id": runtime_two.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runtime_id"], runtime_two.to_string());
    // `chat_session.runtime_id` 故意留在旧值上（上游让新 runtime 开新会话）
    let stale: Uuid = sqlx::query_scalar("SELECT runtime_id FROM chat_session WHERE id = $1")
        .bind(session)
        .fetch_one(&fx.pool)
        .await
        .expect("session runtime");
    assert_eq!(stale, fx.runtime_id);
    let carrier_runtime: Uuid = sqlx::query_scalar("SELECT runtime_id FROM agent WHERE id = $1")
        .bind(builder_agent)
        .fetch_one(&fx.pool)
        .await
        .expect("carrier runtime");
    assert_eq!(carrier_runtime, runtime_two);
    // 切换时模型被清零（model id 是 per-runtime 的）
    let model: Option<String> = sqlx::query_scalar("SELECT model FROM agent WHERE id = $1")
        .bind(builder_agent)
        .fetch_one(&fx.pool)
        .await
        .expect("carrier model");
    assert_eq!(model, None);

    // 列表跟着载体走，草稿整体覆盖
    let (status, body) = call(&app, "GET", CREATE_URI, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"][0]["runtime_id"], runtime_two.to_string());
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/agent-builder/sessions/{session}/draft"),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": { "name": "second" } })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = call(&app, "GET", CREATE_URI, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"][0]["draft"], json!({ "name": "second" }));

    // 别人的会话不在我的列表里
    let other = seed_user(&fx.pool, fx.workspace_id, "member").await;
    let (status, body) = call(&app, "GET", CREATE_URI, fx.workspace_id, other, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"], json!([]));

    fx.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn runtime_gates_and_session_ownership() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let create = |body: serde_json::Value| {
        call(
            &app,
            "POST",
            CREATE_URI,
            fx.workspace_id,
            fx.user_id,
            Some(body),
        )
    };
    let draft_uri = |session: Uuid| format!("/api/agent-builder/sessions/{session}/draft");
    let runtime_uri = |session: Uuid| format!("/api/agent-builder/sessions/{session}/runtime");

    // runtime 门的第一道：缺失 / 空 / 解析不了 / 查不到
    let (status, body) = create(json!({})).await;
    assert_bad_request(status, &body, "runtime_id is required");
    let (status, body) = create(json!({ "runtime_id": "   " })).await;
    assert_bad_request(status, &body, "runtime_id is required");
    let (status, body) = create(json!({ "runtime_id": "not-a-uuid" })).await;
    assert_bad_request(status, &body, "invalid runtime_id");
    let (status, body) = create(json!({ "runtime_id": Uuid::new_v4().to_string() })).await;
    assert_bad_request(status, &body, "invalid runtime_id");
    // 别的 workspace 的 runtime 等价于不存在
    let (foreign_ws, foreign_user) = seed_foreign_user(&fx.pool).await;
    let foreign_runtime =
        seed_runtime(&fx.pool, foreign_ws, Some(fx.user_id), "public", "online").await;
    let (status, body) = create(json!({ "runtime_id": foreign_runtime.to_string() })).await;
    assert_bad_request(status, &body, "invalid runtime_id");
    // 请求体不是对象
    let (status, body) = call(
        &app,
        "POST",
        CREATE_URI,
        fx.workspace_id,
        fx.user_id,
        Some(json!([1, 2, 3])),
    )
    .await;
    assert_bad_request(status, &body, "invalid request body");

    // 第二道：私有 runtime 只有 owner 能用
    let other = seed_user(&fx.pool, fx.workspace_id, "member").await;
    let private_runtime =
        seed_runtime(&fx.pool, fx.workspace_id, Some(other), "private", "online").await;
    let (status, body) = create(json!({ "runtime_id": private_runtime.to_string() })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        error_message(&body),
        "this runtime is private; only its owner can use it"
    );
    // 没有 owner 的 runtime 谁都不能用
    let orphan_runtime = seed_runtime(&fx.pool, fx.workspace_id, None, "public", "online").await;
    let (status, body) = create(json!({ "runtime_id": orphan_runtime.to_string() })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // 第三道：必须 online（动词进文案）
    let offline = seed_runtime(
        &fx.pool,
        fx.workspace_id,
        Some(fx.user_id),
        "public",
        "offline",
    )
    .await;
    let (status, body) = create(json!({ "runtime_id": offline.to_string() })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "runtime must be online to start an agent builder session"
    );

    // 草稿门：会话不存在 / 不是 builder 载体 / 不是我的
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "chat session");
    let plain_session = seed_chat_session(&fx.pool, fx.workspace_id, fx.agent_id, fx.user_id).await;
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(plain_session),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "agent builder session");
    let (status, body) = call(
        &app,
        "PUT",
        "/api/agent-builder/sessions/nope/draft",
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": {} })),
    )
    .await;
    assert_bad_request(status, &body, "invalid chat session id");

    // 草稿：缺 key / 非对象 / 超限 / 归档
    let (status, body) = create(json!({ "runtime_id": fx.runtime_id.to_string() })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session = id_of(&json!({ "id": body["session_id"] }));
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(session),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "other": 1 })),
    )
    .await;
    assert_bad_request(status, &body, "draft is required");
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(session),
        fx.workspace_id,
        fx.user_id,
        Some(json!([1])),
    )
    .await;
    assert_bad_request(status, &body, "invalid request body");
    let huge = "x".repeat(300 * 1024);
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(session),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": { "blob": huge } })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(error_code(&body), "payload_too_large");
    sqlx::query("UPDATE chat_session SET status = 'archived' WHERE id = $1")
        .bind(session)
        .execute(&fx.pool)
        .await
        .expect("archive session");
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(session),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "draft": {} })),
    )
    .await;
    assert_bad_request(status, &body, "chat session is archived");

    // 切 runtime 的门
    let (status, body) = create(json!({ "runtime_id": fx.runtime_id.to_string() })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let live = id_of(&json!({ "id": body["session_id"] }));
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "runtime_id": fx.runtime_id.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "chat session");
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(plain_session),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "runtime_id": fx.runtime_id.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "agent builder session");
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(live),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_bad_request(status, &body, "runtime_id is required");
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(live),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "runtime_id": offline.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "runtime must be online to switch an agent builder session"
    );
    // 有在飞任务时不允许切换
    TaskSeed {
        issue_id: None,
        status: Some("queued".to_owned()),
        chat_session_id: Some(live),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(live),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "runtime_id": fx.runtime_id.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "stop the current reply before switching runtime"
    );

    // 另一个成员动不了我的会话（折叠成 404）
    let (status, body) = call(
        &app,
        "PUT",
        &draft_uri(live),
        fx.workspace_id,
        other,
        Some(json!({ "draft": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "chat session");
    // runtime 门先于会话归属：调用者不是那个私有 runtime 的 owner ⇒ 403；
    // 即便这个会话本来也不是他的，两处都 403 就证明不了顺序，所以特意用
    // 「owner 是 fx.user 的私有 runtime」来区分
    let owner_private = seed_runtime(
        &fx.pool,
        fx.workspace_id,
        Some(fx.user_id),
        "private",
        "online",
    )
    .await;
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(live),
        fx.workspace_id,
        other,
        Some(json!({ "runtime_id": owner_private.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 他用自己的私有 runtime 时门会放行，接着在会话归属上拿到 404
    let (status, body) = call(
        &app,
        "PATCH",
        &runtime_uri(live),
        fx.workspace_id,
        other,
        Some(json!({ "runtime_id": private_runtime.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "chat session");

    // 未认证（缺身份 header）/ 未知用户 / 别的 workspace
    let (status, _, body) = send(
        &app,
        req_with("GET", CREATE_URI, None, Some(fx.workspace_id), &[], None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, body) = call(
        &app,
        "GET",
        CREATE_URI,
        fx.workspace_id,
        Uuid::new_v4(),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "未知用户 = 非成员 ⇒ 404: {body}"
    );
    let (status, body) = call(&app, "GET", CREATE_URI, foreign_ws, fx.user_id, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "非成员 ⇒ 404 workspace: {body}"
    );

    cleanup(&fx.pool, foreign_ws, &[foreign_user]).await;
    fx.cleanup().await;
}
