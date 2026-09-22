//! `POST /api/client-usage`、`GET /api/issues/:id/usage`、`GET /api/tasks/:taskId/messages`。
//!
//! 三条路由的**投影语义**是重点：client-usage 的探针列在「没有探针的上报」时不得被清空；
//! usage 要区分计费/未计费 token；messages 要把空字段整个省略（前端按 key 存在性渲染）。

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::support::{
    assert_bad_request, call, call_extra, cleanup, error_code, error_message, req_with, send,
    setup, TaskSeed, CLIENT_OS_HEADER, CLIENT_PLATFORM_HEADER, CLIENT_VERSION_HEADER,
};

const WEB: [(&str, &str); 2] = [
    (CLIENT_PLATFORM_HEADER, "web"),
    (CLIENT_VERSION_HEADER, "1.2.3"),
];

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn client_usage_upserts_activity_and_keeps_last_probe() {
    let Some(fx) = setup().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let app = fx.app();
    let install_id = Uuid::new_v4();
    let report = || json!({ "install_id": install_id.to_string() });

    // 1) web：带 OS，落一行
    let mut web = WEB.to_vec();
    web.push((CLIENT_OS_HEADER, "MacOS"));
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &web,
        Some(report()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // 2) web 再报一次（新版本、不带 OS）：仍是同一行（PK 含 client_type+install_id+date）
    let (status, _) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &[
            (CLIENT_PLATFORM_HEADER, "web"),
            (CLIENT_VERSION_HEADER, "2.0.0"),
        ],
        Some(report()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let row = sqlx::query(
        "SELECT client_version, os FROM client_usage_daily \
         WHERE user_id = $1 AND client_type = 'web'",
    )
    .bind(fx.user_id)
    .fetch_one(&fx.pool)
    .await
    .expect("web row");
    assert_eq!(row.get::<String, _>("client_version"), "2.0.0");
    assert_eq!(row.get::<String, _>("os"), "unknown");

    // 3) desktop：成功探针
    let desktop = [
        (CLIENT_PLATFORM_HEADER, "desktop"),
        (CLIENT_VERSION_HEADER, "1.0.0"),
        (CLIENT_OS_HEADER, "linux"),
    ];
    let probe = json!({
        "install_id": install_id.to_string(),
        "runtime": {
            "probe_result": "success",
            "runtime_count": 3,
            "online_count": 2,
            "offline_count": 1,
            "provider_summary": { "claude": 2, "codex": 1 }
        }
    });
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &desktop,
        Some(probe),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // 4) desktop 再报一次但不带探针：探针列必须保留（上游的 CASE WHEN hasRuntimeProbe）
    let (status, _) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &desktop,
        Some(report()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let row = sqlx::query(
        "SELECT probe_result, runtime_count, online_count, offline_count, provider_summary \
         FROM client_usage_daily WHERE user_id = $1 AND client_type = 'desktop'",
    )
    .bind(fx.user_id)
    .fetch_one(&fx.pool)
    .await
    .expect("desktop row");
    assert_eq!(
        row.get::<Option<String>, _>("probe_result").as_deref(),
        Some("success")
    );
    assert_eq!(row.get::<Option<i32>, _>("runtime_count"), Some(3));
    assert_eq!(row.get::<Option<i32>, _>("online_count"), Some(2));
    assert_eq!(row.get::<Option<i32>, _>("offline_count"), Some(1));
    assert_eq!(
        row.get::<Value, _>("provider_summary"),
        json!({ "claude": 2, "codex": 1 })
    );

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM client_usage_daily WHERE user_id = $1")
            .bind(fx.user_id)
            .fetch_one(&fx.pool)
            .await
            .expect("count");
    assert_eq!(count, 2, "web 与 desktop 各一行");
    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn client_usage_rejects_bad_platform_version_and_payload() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let install_id = Uuid::new_v4();
    let report = || json!({ "install_id": install_id.to_string() });

    // 缺 platform
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &[],
        Some(report()),
    )
    .await;
    assert_bad_request(status, &body, "client platform must be web or desktop");

    // 版本号超出 64 个可打印字符
    let long = "a".repeat(65);
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &[
            (CLIENT_PLATFORM_HEADER, "web"),
            (CLIENT_VERSION_HEADER, &long),
        ],
        Some(report()),
    )
    .await;
    assert_bad_request(status, &body, "invalid client version");

    // web 不许带 runtime 探针
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &WEB,
        Some(json!({ "install_id": install_id.to_string(), "runtime": { "probe_result": "success" } })),
    )
    .await;
    assert_bad_request(status, &body, "runtime data is only accepted from desktop");

    // 未知字段（上游 DisallowUnknownFields）
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &WEB,
        Some(json!({ "install_id": install_id.to_string(), "nope": 1 })),
    )
    .await;
    assert_bad_request(status, &body, "invalid request body");

    // install_id 不是 UUID
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &WEB,
        Some(json!({ "install_id": "not-a-uuid" })),
    )
    .await;
    assert_bad_request(status, &body, "invalid install_id");

    // 超过 16KB 上限
    let (status, body) = call_extra(
        &app,
        "POST",
        "/api/client-usage",
        fx.workspace_id,
        fx.user_id,
        &WEB,
        Some(json!({ "install_id": "x".repeat(17 * 1024) })),
    )
    .await;
    assert_bad_request(status, &body, "invalid request body");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM client_usage_daily WHERE user_id = $1")
            .bind(fx.user_id)
            .fetch_one(&fx.pool)
            .await
            .expect("count");
    assert_eq!(count, 0, "被拒的上报不得落库");
    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn client_usage_workspace_is_optional_but_member_checked() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let install_id = Uuid::new_v4();
    let report = || json!({ "install_id": install_id.to_string() });

    // 完全没有 workspace 上下文 → 204，workspace_id 记 NULL
    let (status, _, body) = send(
        &app,
        req_with(
            "POST",
            "/api/client-usage",
            Some(fx.user_id),
            None,
            &WEB,
            Some(report()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let workspace_id: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM client_usage_daily WHERE user_id = $1")
            .bind(fx.user_id)
            .fetch_one(&fx.pool)
            .await
            .expect("row");
    assert_eq!(workspace_id, None);

    // 非成员 workspace（通过 query 指定）→ 403
    let (foreign_ws, foreign_user) = crate::support::seed_foreign_user(&fx.pool).await;
    let uri = format!("/api/client-usage?workspace_id={foreign_ws}");
    let (status, _, body) = send(
        &app,
        req_with("POST", &uri, Some(fx.user_id), None, &WEB, Some(report())),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_message(&body), "workspace not found");

    // workspace_id 不是 UUID → 400
    let (status, _, body) = send(
        &app,
        req_with(
            "POST",
            "/api/client-usage?workspace_id=nope",
            Some(fx.user_id),
            None,
            &WEB,
            Some(report()),
        ),
    )
    .await;
    assert_bad_request(status, &body, "invalid workspace id");

    // 成员身份下显式指定 workspace → 记下来
    let (status, _, body) = send(
        &app,
        req_with(
            "POST",
            &format!("/api/client-usage?workspace_id={}", fx.workspace_id),
            Some(fx.user_id),
            None,
            &WEB,
            Some(json!({ "install_id": Uuid::new_v4().to_string() })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    cleanup(&fx.pool, foreign_ws, &[foreign_user]).await;
    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn issue_usage_sums_metered_and_unmetered_tokens() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();

    let done = TaskSeed::queued(fx.issue_id)
        .status("completed")
        .insert(&fx)
        .await;
    let _pending = TaskSeed::queued(fx.issue_id).insert(&fx).await;
    for (model, input, output, read, write, ticks) in [
        ("m1", 10_i64, 20_i64, 1_i64, 2_i64, Some(5_i64)),
        ("m2", 3, 4, 0, 0, None),
    ] {
        sqlx::query(
            "INSERT INTO task_usage (task_id, provider, model, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens, cost_usd_ticks) \
             VALUES ($1, 'anthropic', $2, $3, $4, $5, $6, $7)",
        )
        .bind(done)
        .bind(model)
        .bind(input)
        .bind(output)
        .bind(read)
        .bind(write)
        .bind(ticks)
        .execute(&fx.pool)
        .await
        .expect("insert task_usage");
    }

    let uri = format!("/api/issues/{}/usage", fx.issue_id);
    let (status, body) = call(&app, "GET", &uri, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_input_tokens"], 13);
    assert_eq!(body["total_output_tokens"], 24);
    assert_eq!(body["total_cache_read_tokens"], 1);
    assert_eq!(body["total_cache_write_tokens"], 2);
    assert_eq!(body["cost_usd_ticks"], 5);
    assert_eq!(body["uncosted_input_tokens"], 3);
    assert_eq!(body["uncosted_output_tokens"], 4);
    assert_eq!(body["uncosted_cache_read_tokens"], 0);
    assert_eq!(body["uncosted_cache_write_tokens"], 0);
    assert_eq!(body["task_count"], 1);
    assert_eq!(body["terminal_task_count"], 1);
    assert_eq!(body["metered_task_count"], 1);
    assert_eq!(body["unreported_task_count"], 0);

    // 也接受 issue 的 identifier，而不是 UUID
    let identifier: String = sqlx::query_scalar("SELECT identifier FROM issue WHERE id = $1")
        .bind(fx.issue_id)
        .fetch_one(&fx.pool)
        .await
        .expect("identifier");
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{identifier}/usage"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_input_tokens"], 13);

    // 未知 issue → 404
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{}/usage", Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // task_message 的省略矩阵逐字段断言
async fn task_messages_omit_empty_fields_and_filter_since() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let task = TaskSeed::queued(fx.issue_id).insert(&fx).await;

    sqlx::query(
        "INSERT INTO task_message (task_id, seq, type, content) VALUES ($1, 1, 'text', 'hello')",
    )
    .bind(task)
    .execute(&fx.pool)
    .await
    .expect("msg 1");
    sqlx::query(
        "INSERT INTO task_message (task_id, seq, type, tool, call_id, input, output, output_truncated) \
         VALUES ($1, 2, 'tool_use', 'bash', 'c1', '{\"command\":\"ls\"}'::jsonb, 'raw out', true)",
    )
    .bind(task)
    .execute(&fx.pool)
    .await
    .expect("msg 2");
    sqlx::query("INSERT INTO task_message (task_id, seq, type) VALUES ($1, 3, 'result')")
        .bind(task)
        .execute(&fx.pool)
        .await
        .expect("msg 3");

    let uri = format!("/api/tasks/{task}/messages");
    let (status, body) = call(&app, "GET", &uri, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["seq"], 1);
    assert_eq!(rows[0]["type"], "text");
    assert_eq!(rows[0]["content"], "hello");
    assert_eq!(rows[0]["task_id"], task.to_string());
    assert_eq!(rows[0]["issue_id"], fx.issue_id.to_string());
    assert!(rows[0].get("tool").is_none() && rows[0].get("input").is_none());
    assert!(rows[0].get("output").is_none() && rows[0].get("output_truncated").is_none());

    assert_eq!(rows[1]["tool"], "bash");
    assert_eq!(rows[1]["call_id"], "c1");
    assert_eq!(rows[1]["input"], json!({ "command": "ls" }));
    assert_eq!(rows[1]["output"], "raw out");
    assert_eq!(rows[1]["output_truncated"], true);

    // 全空行：只剩 id / task_id / issue_id / seq / type / created_at
    assert_eq!(rows[2]["type"], "result");
    for key in [
        "tool",
        "content",
        "input",
        "output",
        "output_truncated",
        "call_id",
    ] {
        assert!(
            rows[2].get(key).is_none(),
            "空字段 {key} 必须省略: {}",
            rows[2]
        );
    }

    // since 过滤（严格大于）
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?since=2"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 1);

    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?since=abc"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "invalid since parameter");

    // 未知 task / 未绑 issue 的任务
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/tasks/{}/messages", Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");

    let loose = TaskSeed {
        status: Some("queued".to_owned()),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/tasks/{loose}/messages"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!([]));

    fx.cleanup().await;
}
