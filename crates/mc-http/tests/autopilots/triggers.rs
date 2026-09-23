//! trigger 写面 e2e（M5-3 / LUM-1568）：**路由形态与鉴权分层**，并托管本 target 的共享工具。
//!
//! 五条写路由（#10–#14）的「骨架」断言都在这里，数据面在 `trigger_crud.rs`、凭据面在
//! `credentials.rs`（gate ⑩ 单文件 800 行硬上限 ⇒ 按关注点拆三个文件）：
//!
//! 1. **路由形态**（要真 router）：401 / 400 / 405、双形态 vs 单形态
//!    （多注册一个尾斜杠＝`EXTRA_ALIAS`，少注册＝`MISSING_ALIAS`）；
//! 2. **权限链**：非成员 → 404（掩盖存在性）、普通成员 → 403、
//!    「trigger 不属于本 autopilot」→ 404（不可与不存在区分）；
//! 3. **共享工具**：`WRITE_ROUTES` / `route_of` / `probe_status` / `upstream_message`
//!    （`pub(crate)`，供同 target 的另两个文件复用）。
//!
//! 断言里的文案用 [`upstream_message`]（剥掉 `mc-errors` 的内部前缀），另在个别用例里
//! 显式钉一次**带前缀**的原文，把本仓「404 = `not found: <resource>`、400 = `validation
//! error: …`」这条全仓偏差（docs/40 §5）也写进测试。
//!
//! 运行方式见 `main.rs`（需要真 PG）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{
    app_with_db, call, cleanup, connect, err_message, seed_autopilot, seed_schedule_trigger,
    seed_user, seed_webhook_trigger, seed_workspace, USER_ID_HEADER, WORKSPACE_HEADER,
};

/// 写面 5 条路由（#10–#14）的 `(方法, URI 模板)`。
pub(crate) const WRITE_ROUTES: [(&str, &str); 5] = [
    ("POST", "/api/autopilots/{ap}/triggers"),
    ("PATCH", "/api/autopilots/{ap}/triggers/{t}"),
    ("DELETE", "/api/autopilots/{ap}/triggers/{t}/"),
    (
        "POST",
        "/api/autopilots/{ap}/triggers/{t}/rotate-webhook-token",
    ),
    ("PUT", "/api/autopilots/{ap}/triggers/{t}/signing-secret"),
];

/// 把 URI 模板里的两个占位符替换成真 id。
pub(crate) fn route_of(template: &str, autopilot_id: Uuid, trigger_id: Uuid) -> String {
    template
        .replace("{ap}", &autopilot_id.to_string())
        .replace("{t}", &trigger_id.to_string())
}

/// 401 / 400 探针：`support::{call_no_user, call_no_workspace}` 是 **GET-only**
/// （M5-1 只给读面写过），而写面的 401/400 必须在**同方法**上验证 —— 用 GET 打一条
/// 只注册了 POST 的路由只会得到 405，验不到鉴权层。
pub(crate) async fn probe_status(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    workspace: Option<&str>,
) -> StatusCode {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header(USER_ID_HEADER, user.to_string());
    }
    if let Some(workspace) = workspace {
        builder = builder.header(WORKSPACE_HEADER, workspace);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("router call")
        .status()
}

/// 上游原文那一段（剥掉 `mc-errors` 的内部前缀）。
///
/// `autopilots/support.rs::err_message` 返回**线上正文**，而线上正文带 `thiserror` 的前缀
/// （`not found: ` / `validation error: ` / `forbidden: ` …），那是全仓统一的形状偏差
/// （docs/40 §5）。这里与 `agents/support.rs::error_message` 同口径剥掉它，断言才对得上上游文案。
pub(crate) fn upstream_message(body: &Value) -> &str {
    const PREFIXES: [&str; 7] = [
        "validation error: ",
        "not found: ",
        "conflict: ",
        "unprocessable entity: ",
        "forbidden: ",
        "unauthorized: ",
        "database error: ",
    ];
    let raw = err_message(body);
    for prefix in PREFIXES {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest;
        }
    }
    raw
}

/// 缺 `X-Multica-User-Id` → 401（五条写路由都要）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn missing_user_header_is_401_on_every_write_route() {
    let Some((pool, db)) = connect().await else {
        println!("skip missing_user_header_is_401_on_every_write_route: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;

    for (method, template) in WRITE_ROUTES {
        let uri = route_of(template, autopilot, trigger);
        assert_eq!(
            probe_status(&app, method, &uri, None, Some(&ws.to_string())).await,
            401,
            "{method} {uri}"
        );
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 缺工作区头 / 工作区头非 UUID → 400（五条写路由都要，且**不**是 500）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn bad_workspace_header_is_400_on_every_write_route() {
    let Some((pool, db)) = connect().await else {
        println!("skip bad_workspace_header_is_400_on_every_write_route: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;

    for (method, template) in WRITE_ROUTES {
        let uri = route_of(template, autopilot, trigger);
        assert_eq!(
            probe_status(&app, method, &uri, Some(owner), None).await,
            400,
            "缺头 {method} {uri}"
        );
        assert_eq!(
            probe_status(&app, method, &uri, Some(owner), Some("not-a-uuid")).await,
            400,
            "坏 uuid {method} {uri}"
        );
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 405：`/triggers` 上没有 GET、`/triggers/:id` 上没有 PUT —— 证明注册的方法集合是**精确**的
/// （多注册一个方法在这里就会变成 2xx/4xx 而不是 405）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn method_not_allowed_outside_the_route_table() {
    let Some((pool, db)) = connect().await else {
        println!("skip method_not_allowed_outside_the_route_table: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;

    let probes = [
        ("GET", format!("/api/autopilots/{autopilot}/triggers")),
        (
            "PUT",
            format!("/api/autopilots/{autopilot}/triggers/{trigger}"),
        ),
        (
            "DELETE",
            format!("/api/autopilots/{autopilot}/triggers/{trigger}/rotate-webhook-token"),
        ),
    ];
    for (method, uri) in probes {
        let (status, _) = call(&app, method, &uri, ws, owner, None).await;
        assert_eq!(status, 405, "{method} {uri}");
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 单形态的路由**不该**有尾斜杠别名（多了就是 `EXTRA_ALIAS`）；双形态的两条则两种都通。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn single_form_routes_reject_the_trailing_slash_alias() {
    let Some((pool, db)) = connect().await else {
        println!("skip single_form_routes_reject_the_trailing_slash_alias: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;

    // #10 / #13 / #14 是 plain 子路由 ⇒ `…/` 是 404（不是重定向、也不是 405）。
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/triggers/"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "#10 不该有尾斜杠别名");
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/triggers/{trigger}/rotate-webhook-token/"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "#13 不该有尾斜杠别名");
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/autopilots/{autopilot}/triggers/{trigger}/signing-secret/"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "#14 不该有尾斜杠别名");
    cleanup(&pool, ws, &[owner]).await;
}

/// 权限链：非成员 404（不泄露存在性）→ 普通成员 403（文案逐字）→ 创建者 200。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn membership_and_write_permission_split_404_from_403() {
    let Some((pool, db)) = connect().await else {
        println!("skip membership_and_write_permission_split_404_from_403: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let plain_member = seed_user(&pool, ws, "member").await;
    let outsider = super::support::seed_outsider(&pool).await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;
    let uri = format!("/api/autopilots/{autopilot}/triggers/{trigger}");

    // 非成员（不在 member 表里）：404，且与「autopilot 不存在」不可区分。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        outsider,
        Some(json!({"label":"x"})),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(err_message(&body), "not found: workspace");
    assert_eq!(upstream_message(&body), "workspace");

    // 成员但既非创建者、又非 admin、也没被授权：403（上游 `requireAutopilotWrite` 文案）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        plain_member,
        Some(json!({"label":"x"})),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        err_message(&body),
        "forbidden: insufficient permission for this autopilot"
    );

    // #10 的创建面同理（403 在写入之前）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/triggers"),
        ws,
        plain_member,
        Some(json!({"kind":"schedule","cron_expression":"0 9 * * *"})),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // 创建者自己改得动。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"label":"renamed"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["label"], "renamed");

    cleanup(&pool, ws, &[owner, plain_member, outsider]).await;
}

/// 跨 autopilot 的 trigger id 一律 404：`get_by_id` 不绑 autopilot，绑定比对是这个 handler 的责任。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn trigger_from_another_autopilot_is_not_found() {
    let Some((pool, db)) = connect().await else {
        println!("skip trigger_from_another_autopilot_is_not_found: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let mine = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let theirs = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let their_trigger = seed_webhook_trigger(&pool, theirs, "awt_foreign", None, None).await;

    let probes = [
        (
            "PATCH",
            format!("/api/autopilots/{mine}/triggers/{their_trigger}"),
            Some(json!({"label":"x"})),
        ),
        (
            "DELETE",
            format!("/api/autopilots/{mine}/triggers/{their_trigger}"),
            None,
        ),
        (
            "POST",
            format!("/api/autopilots/{mine}/triggers/{their_trigger}/rotate-webhook-token"),
            None,
        ),
    ];
    for (method, uri, body) in probes {
        let (status, resp) = call(&app, method, &uri, ws, owner, body).await;
        assert_eq!(status, 404, "{method} {uri} → {resp}");
        // 本仓 404 走 `not found: <resource>`（上游是 `<resource> not found`，docs/40 §5）。
        assert_eq!(err_message(&resp), "not found: trigger");
        assert_eq!(upstream_message(&resp), "trigger");
    }

    // 顺带钉住「trigger id 合法但 autopilot id 不存在」也是 404 autopilot。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/autopilots/{}/triggers/{their_trigger}", Uuid::nil()),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "autopilot");

    cleanup(&pool, ws, &[owner]).await;
}

/// 非 UUID 的路径参数 → 400，且 **autopilot id 的 400 早于 trigger id 的 400**（上游顺序）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn non_uuid_path_params_are_400_in_upstream_order() {
    let Some((pool, db)) = connect().await else {
        println!("skip non_uuid_path_params_are_400_in_upstream_order: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/autopilots/{autopilot}/triggers/nope"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "trigger id must be a valid uuid");

    // 两个都坏时先报 autopilot（上游 `parseUUIDOrBadRequest(autopilotID)` 在前）。
    let (status, body) = call(
        &app,
        "DELETE",
        "/api/autopilots/nope/triggers/also-nope",
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot id must be a valid uuid");

    cleanup(&pool, ws, &[owner]).await;
}
