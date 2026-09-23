//! daemon 执行环（自注册到任务完结）的端到端用例。
//!
//! 用例形状与 issue 的验收表一一对应：claim 幂等、prepare-lease / progress / complete /
//! fail / cancel-ack 各 ≥1、recover-orphans ≥1，外加「注册 + 心跳 + 消息回传」串成一条
//! 真实主链（硬项：daemon loop e2e）。

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{self, DAEMON_ID_HEADER, USER_ID_HEADER};

/// 注册一台 daemon（`POST /api/daemon/register`），返回 `runtimes[0].id`。
async fn register(
    app: &axum::Router,
    workspace_id: Uuid,
    user_id: Uuid,
    daemon_id: &str,
) -> String {
    let (status, body) = support::call(
        app,
        "POST",
        "/api/daemon/register",
        user_id,
        Some(daemon_id),
        Some(json!({
            "workspace_id": workspace_id,
            "daemon_id": daemon_id,
            "device_name": "itest-devbox",
            "cli_version": "1.0.0",
            "launched_by": "cli",
            "runtimes": [{
                "name": "Claude Code",
                "type": "claude",
                "version": "1.0.0",
                "status": "online",
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register: {body}");
    assert_eq!(body["runtimes"].as_array().map(Vec::len), Some(1), "{body}");
    body["runtimes"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("runtime id: {body}"))
        .to_owned()
}

/// 从 claim 响应里取出第一条任务（没有就 panic，附上整包便于定位）。
fn first_task(body: &Value) -> Value {
    body["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .cloned()
        .unwrap_or_else(|| panic!("no claimed task in {body}"))
}

/// 主链：注册 → 心跳 → claim → prepare-lease → start → progress → messages → usage →
/// complete，每步都断言服务端可观察的状态。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 主链按调用顺序平铺，拆函数会丢掉步进关系
async fn daemon_loop_end_to_end() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);

    // 1. 注册：runtime 由 daemon 自报，register 落行并回该行的投影。
    let runtime_id = register(&app, workspace_id, user_id, "m1").await;
    let runtime_uuid = Uuid::parse_str(&runtime_id).expect("runtime uuid");

    // 2. 心跳：HTTP 面回的是 `{status} ∪ pending_*` —— 没有 runtime_id、
    //    没有 server_capabilities（协议协商只走 WS，`docs/16` §10.1）。
    let (status, ack) = support::call(
        &app,
        "POST",
        "/api/daemon/heartbeat",
        user_id,
        Some("m1"),
        Some(json!({ "runtime_id": runtime_id, "supports_batch_import": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "heartbeat: {ack}");
    assert!(
        ack.get("runtime_id").is_none(),
        "http ack 不该带 runtime_id: {ack}"
    );
    assert!(
        ack.get("server_capabilities").is_none(),
        "http ack 不该带 server_capabilities: {ack}"
    );

    // 3. 排队一条任务（claim 只认 `queued` + 同 runtime 的 agent + online 的 runtime）。
    let agent_id = support::seed_agent(&pool, workspace_id, runtime_uuid).await;
    let task_id = support::seed_task(
        &pool,
        workspace_id,
        user_id,
        runtime_uuid,
        agent_id,
        "queued",
    )
    .await;
    let task = task_id.to_string();

    // 4. claim：任务转 dispatched + 发任务 token。
    let (status, claimed) = support::claim(&app, user_id, "m1", &[runtime_uuid], 4).await;
    assert_eq!(status, StatusCode::OK, "claim: {claimed}");
    let first = first_task(&claimed);
    assert_eq!(first["id"], json!(task));
    assert_eq!(first["status"], json!("dispatched"));
    assert_eq!(first["runtime_id"], json!(runtime_id));
    assert!(
        first["auth_token"].as_str().is_some_and(|t| !t.is_empty()),
        "claim 必须发任务 token: {first}"
    );

    // 5. claim 幂等：同一条任务不会被第二次领走。
    let (status, again) = support::claim(&app, user_id, "m1", &[runtime_uuid], 4).await;
    assert_eq!(status, StatusCode::OK, "re-claim: {again}");
    assert_eq!(again["tasks"].as_array().map(Vec::len), Some(0), "{again}");

    // 6. prepare-lease：dispatched 且未 start ⇒ 续租。
    let (status, leased) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/tasks/{task}/prepare-lease"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "prepare-lease: {leased}");
    assert_eq!(leased["status"], json!("dispatched"));

    // 7. start → running（无请求体，与上游一致）。
    let (status, started) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/start"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {started}");
    assert_eq!(started["status"], json!("running"));

    let (status, current) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/tasks/{task}/status"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["status"], json!("running"));

    // 8. progress：纯回执（上游没有可观察的落库副作用）。
    let (status, progress) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/progress"),
        user_id,
        Some("m1"),
        Some(json!({ "summary": "half way", "step": 1, "total": 2 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "progress: {progress}");
    assert_eq!(progress["status"], json!("ok"));

    // 9. messages：批量上报后能按 seq 读回（`created_at` 在批内是同一个时钟）。
    let (status, reported) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/messages"),
        user_id,
        Some("m1"),
        Some(json!({ "messages": [{
            "seq": 1,
            "type": "assistant",
            "tool": "Read",
            "call_id": "c1",
            "content": "hello",
            "input": { "path": "a.rs" },
            "output": "ok",
        }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "report messages: {reported}");
    assert_eq!(reported["status"], json!("ok"));

    let (status, listed) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/tasks/{task}/messages"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list messages: {listed}");
    let rows = listed.as_array().expect("messages 是裸数组");
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["content"], json!("hello"));
    assert_eq!(rows[0]["input"], json!({ "path": "a.rs" }));
    assert_eq!(rows[0]["seq"], json!(1));

    // 10. usage：1e-10 USD 的整数刻度。
    let (status, usage) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/usage"),
        user_id,
        Some("m1"),
        Some(json!({ "usage": [{
            "provider": "anthropic",
            "model": "claude-sonnet",
            "input_tokens": 10,
            "output_tokens": 20,
            "cost_usd_ticks": 1_500_000_000_i64,
        }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "usage: {usage}");
    assert_eq!(usage["status"], json!("ok"));

    // 11. complete：终态 + 结果投影。
    let (status, done) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/complete"),
        user_id,
        Some("m1"),
        Some(json!({
            "output": "all green",
            "session_id": "sess-1",
            "work_dir": "/tmp/itest-work",
            "branch_name": "agent/m1/task",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "complete: {done}");
    assert_eq!(done["status"], json!("completed"));
    assert_eq!(done["id"], json!(task));

    let (_, current) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/tasks/{task}/status"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(current["status"], json!("completed"));

    // 12. 完结后的 pending 列表为空数组（不是 404）。
    let (status, pending) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/runtimes/{runtime_id}/tasks/pending"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "pending: {pending}");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// claim 的三条前置校验：缺 `daemon_id` ⇒ 400；`max_tasks` 为负 ⇒ 400；
/// `max_tasks == 0` ⇒ 200 空列表（明确不领，且不查库）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn claim_validates_request_shape() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);

    let (status, body) = support::call(
        &app,
        "POST",
        "/api/daemon/tasks/claim",
        user_id,
        Some("m1"),
        Some(json!({ "runtime_ids": [], "max_tasks": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(support::error_message(&body), "daemon_id is required");

    let (status, body) = support::claim(&app, user_id, "m1", &[], -1).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        support::error_message(&body),
        "max_tasks must not be negative"
    );

    let (status, body) = support::claim(&app, user_id, "m1", &[], 0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "tasks": [] }));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// `daemon_id` 与连接身份不一致 ⇒ 403（一枚同 workspace 的 token 不能替别的机器领任务）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn claim_rejects_daemon_id_mismatch() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (runtime_id, _agent_id, task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;

    // 头里的机器名与体里的 `daemon_id` 必须一致（上游 `ctx daemon_id != req.daemon_id`
    // ⇒ 403）：头是凭据来源，体是客户端自己声明的，两者不同就是冒名。
    let (status, body) = support::call(
        &app,
        "POST",
        "/api/daemon/tasks/claim",
        user_id,
        Some("m1"),
        Some(json!({
            "daemon_id": "m2",
            "runtime_ids": [runtime_id.to_string()],
            "max_tasks": 4,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        support::error_message(&body),
        "daemon_id does not match token"
    );

    // 任务没被领走：仍然是 queued。
    let status: String = sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = $1")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .expect("task status");
    assert_eq!(status, "queued");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 认领到别的机器名下的 runtime ⇒ 跳过而不是 4xx，整批结果只剩空列表。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn claim_skips_runtimes_of_other_daemons() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (_task_runtime, _agent_id, task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "other-machine").await;
    let mine = support::seed_runtime(&pool, workspace_id, user_id, "m1").await;

    let (status, body) = support::claim(&app, user_id, "m1", &[mine], 4).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tasks"], json!([]));

    let task_status: String =
        sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .expect("task status");
    assert_eq!(task_status, "queued");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 并发 claim：两条同时到达的认领请求，同一条任务**只会被发出一次**。
///
/// 互斥靠 SQL 的 `FOR UPDATE SKIP LOCKED`（`mc-repos/src/daemon/tasks.rs:462`），不是应用层的
/// 锁——所以断言的是「两次应答的并集恰好一条」，而不是谁先谁后。两条请求都会 200：
/// 输的那条只是拿到空列表（与上游同款语义，不是 409）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn concurrent_claims_yield_the_task_exactly_once() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (runtime_id, _agent_id, task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;
    let claimed_runtimes = [runtime_id];

    let (a, b) = tokio::join!(
        support::claim(&app, user_id, "m1", &claimed_runtimes, 4),
        support::claim(&app, user_id, "m1", &claimed_runtimes, 4),
    );
    let (a_status, a_body) = a;
    let (b_status, b_body) = b;
    assert_eq!(a_status, StatusCode::OK, "claim A: {a_body}");
    assert_eq!(b_status, StatusCode::OK, "claim B: {b_body}");

    let mut ids: Vec<String> = [&a_body, &b_body]
        .into_iter()
        .filter_map(|body| body["tasks"].as_array())
        .flatten()
        .filter_map(|task| task["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![task_id.to_string()],
        "同一任务只能被发出一次：A={a_body} B={b_body}"
    );

    // 只有一枚任务 token（没领到的请求不会签发凭据）。
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM task_token WHERE task_id = $1")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .expect("task_token count");
    assert_eq!(tokens, 1, "两枚 token 意味着任务被发给了两台机器");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// fail：把在飞任务打成 failed 并落 `failure_reason`（终态后 status 端点跟着变）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn fail_task_marks_terminal_state() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (runtime_id, _agent_id, task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;
    let task = task_id.to_string();

    let (status, claimed) = support::claim(&app, user_id, "m1", &[runtime_id], 4).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let _ = first_task(&claimed);
    let _ = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/start"),
        user_id,
        Some("m1"),
        None,
    )
    .await;

    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{task}/fail"),
        user_id,
        Some("m1"),
        Some(json!({ "error": "boom", "failure_reason": "agent_crashed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fail: {body}");
    assert_eq!(body["status"], json!("failed"));

    let (_, current) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/tasks/{task}/status"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(current["status"], json!("failed"));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// cancel-ack：用户面取消后 daemon 回执；空体也接受（`decode_body(...).unwrap_or_default()`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn cancel_ack_accepts_empty_body() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (runtime_id, agent_id, _queued) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;
    let cancelled = support::seed_task(
        &pool,
        workspace_id,
        user_id,
        runtime_id,
        agent_id,
        "cancelled",
    )
    .await;

    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/tasks/{cancelled}/cancel-ack"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cancel-ack: {body}");
    assert_eq!(body["status"], json!("ok"));

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// recover-orphans：上一次进程留下的 dispatched/running 行被回收并计数。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn recover_orphans_reports_counts() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (runtime_id, _agent_id, _task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;

    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/runtimes/{runtime_id}/recover-orphans"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "recover-orphans: {body}");
    // `retried` 恒 0（auto-retry 属 RuntimeSweeper 面）；`orphaned` 是回收条数。
    assert_eq!(body["retried"], json!(0));
    assert!(
        body["orphaned"].as_u64().is_some(),
        "orphaned 应是整数: {body}"
    );

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 未知 runtime / 未知 task ⇒ 404 且用的是 daemon 面的上游短语（`scope.rs` 规则 4）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn unknown_runtime_and_task_are_404() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);

    let (status, body) = support::call(
        &app,
        "GET",
        "/api/daemon/tasks/00000000-0000-4000-8000-000000000000/status",
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(support::error_message(&body), "task not found");

    let (status, body) = support::call(
        &app,
        "GET",
        "/api/daemon/runtimes/00000000-0000-4000-8000-000000000000/tasks/pending",
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(support::error_message(&body), "runtime not found");

    // 无身份头 ⇒ 401（dev-mode 兜底的拒绝路径）。
    let status = support::call_anon(&app, "GET", "/api/daemon/workspaces").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 请求头名字写错 / 缺身份头时**不能**被当成有效身份（回归：头是客户端可写的字节，
/// 只有 `x-multica-user-id` 那一个才是 D-1 的约定）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn spoofed_identity_headers_do_not_authenticate() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);

    // 只有 `x-daemon-id`（没有用户头）⇒ 401，不是「daemon 身份」。
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/daemon/workspaces")
        .header(DAEMON_ID_HEADER, "m1")
        .body(axum::body::Body::empty())
        .unwrap();
    let status = app
        .clone()
        .oneshot(request)
        .await
        .expect("router call")
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 用户头里塞别的头名（`x-user-id`）同样不算身份。
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/daemon/workspaces")
        .header("x-user-id", user_id.to_string())
        .body(axum::body::Body::empty())
        .unwrap();
    let status = app
        .clone()
        .oneshot(request)
        .await
        .expect("router call")
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_ne!(USER_ID_HEADER, "x-user-id");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}
