//! `/api/labels*` + `/api/properties*` + `/api/issues/:id/labels*` +
//! `/api/issues/:id/properties/:propertyId` 端到端测试（M2-E / LUM-1370）。
//!
//! 需要真实 PG：`issue_label` / `issue_to_label` / `issue_property` / `issue`（含
//! `properties` JSONB）都必须已迁移。本文件的测试均为 `#[ignore]`，通过
//! `MULTICA_TEST_DATABASE_URL` 触发；没有该 env 时静默 skip。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test label_property --features test-util -- --ignored
//! ```
//!
//! 种子是 **owner**（property 定义面要求 owner/admin）+ 一个普通 **member**（用于 403 / 404
//! 负例）+ 一个 outsider（非成员 → 404）。

#![cfg(feature = "test-util")]

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";
const WORKSPACE_HEADER: &str = "x-workspace-id";

fn build_state_with_db(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let state = AppState::new(
        db,
        RuntimeHandles { actors, adapters },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        realtime,
        ws,
    );
    Arc::new(state)
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// workspace + owner + 普通 member + outsider（非成员）。
async fn seed(pool: &sqlx::PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m2e-ws', $1) RETURNING id",
    )
    .bind(format!("itest-m2e-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let owner: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m2e-owner', $1) RETURNING id"#,
    )
    .bind(format!("m2e-owner-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert owner");

    let member: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m2e-member', $1) RETURNING id"#,
    )
    .bind(format!("m2e-member-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert member");

    let outsider: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m2e-outsider', $1) RETURNING id"#,
    )
    .bind(format!("m2e-outsider-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert outsider");

    for (user, role) in [(owner, "owner"), (member, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace_id)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member row");
    }

    (workspace_id, owner, member, outsider)
}

async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, users: [Uuid; 3]) {
    // `issue_property` 没有 workspace FK（上游 193 特意去掉）⇒ 必须显式删。
    let _ = sqlx::query("DELETE FROM issue_property WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for user in users {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

fn req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "application/json");
    match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    ws: Uuid,
    user: Uuid,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(req(method, uri, ws, user, body))
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

async fn create_issue(app: &Router, ws: Uuid, user: Uuid) -> String {
    let (status, body) = send(
        app,
        "POST",
        "/api/issues",
        ws,
        user,
        Some(json!({"title": "m2e target"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create issue: {body}");
    body["id"].as_str().expect("issue id").to_string()
}

/// 1) 标签目录生命周期 + issue 挂/摘 + 幂等 + 跨 workspace 收窄。
#[allow(clippy::too_many_lines)] // 一条端到端链路（建/列/改/挂/摘/删）平铺，拆开就看不出顺序依赖
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn label_catalog_lifecycle_and_issue_assignment() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, owner, member, outsider) = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 建标签（普通 member 即可，上游这条没有 admin 门）。
    let (status, label) = send(
        &app,
        "POST",
        "/api/labels",
        ws,
        member,
        Some(json!({"name": "  Bug  ", "color": "3B82F6", "description": "  triage  "})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create label: {label}");
    assert_eq!(label["name"], "Bug", "name 被 trim");
    assert_eq!(label["color"], "#3b82f6", "颜色规范化成小写带 #");
    assert_eq!(label["description"], "triage", "description 被 trim");
    assert_eq!(label["resource_type"], "issue", "默认 resource_type");
    assert_eq!(label["usage_count"], 0);
    let label_id = label["id"].as_str().expect("label id").to_string();

    // 重名（大小写不敏感）→ 409。
    let (status, err) = send(
        &app,
        "POST",
        "/api/labels",
        ws,
        owner,
        Some(json!({"name": "bug", "color": "#3b82f6"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate name: {err}");
    assert_eq!(err["error"]["code"], "conflict");

    // 非法颜色 → 400；非法 resource_type → 400；空名 → 400。
    for body in [
        json!({"name": "x", "color": "not-a-color"}),
        json!({"name": "x", "color": "#3b82f6", "resource_type": "banana"}),
        json!({"name": "   ", "color": "#3b82f6"}),
    ] {
        let (status, err) = send(&app, "POST", "/api/labels", ws, owner, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "invalid body: {err}");
        assert_eq!(err["error"]["code"], "validation_error");
    }

    // 非成员 → 404（不泄露存在性）。
    let (status, _) = send(&app, "GET", "/api/labels", ws, outsider, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 列表：`{labels, total}`；未知 resource_type → 400。
    let (status, listed) = send(&app, "GET", "/api/labels", ws, member, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["labels"][0]["id"], label_id.as_str());
    let (status, err) = send(
        &app,
        "GET",
        &format!("/api/labels?resource_type=banana&workspace_id={ws}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    // 单体读/改：PUT 三态（只改名字）。
    let (status, fetched) = send(
        &app,
        "GET",
        &format!("/api/labels/{label_id}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["color"], "#3b82f6");
    let (status, updated) = send(
        &app,
        "PUT",
        &format!("/api/labels/{label_id}"),
        ws,
        member,
        Some(json!({"name": "regression", "color": "#FF0000"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update: {updated}");
    assert_eq!(updated["name"], "regression");
    assert_eq!(updated["color"], "#ff0000");
    assert_eq!(updated["description"], "triage", "未提供的字段不动");

    // 未知 label id / 非 UUID id → 404 / 400。
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/labels/{}", Uuid::new_v4()),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, err) = send(&app, "GET", "/api/labels/not-a-uuid", ws, member, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    // --- issue 侧 ---
    let issue_id = create_issue(&app, ws, member).await;
    let (status, empty) = send(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/labels"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["labels"].as_array().map(Vec::len), Some(0));
    assert!(
        empty["issue_revision"].is_i64(),
        "list 恒带 issue_revision: {empty}"
    );

    // 挂：变更 ⇒ 带 issue_revision。
    let (status, attached) = send(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/labels"),
        ws,
        member,
        Some(json!({"label_id": label_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "attach: {attached}");
    assert_eq!(attached["labels"].as_array().map(Vec::len), Some(1));
    assert_eq!(attached["labels"][0]["name"], "regression");
    assert_eq!(
        attached["labels"][0]["usage_count"], 0,
        "issue 侧用 labelToResponse：不带 usage_count"
    );
    assert!(
        attached["issue_revision"].is_i64(),
        "首次挂载必须带 issue_revision: {attached}"
    );

    // 幂等：重复挂 ⇒ 不再变更 ⇒ 不带 issue_revision。
    let (status, again) = send(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/labels"),
        ws,
        member,
        Some(json!({"label_id": label_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["labels"].as_array().map(Vec::len), Some(1));
    assert!(
        again.get("issue_revision").is_none(),
        "重复挂载不算变更: {again}"
    );

    // 空 label_id → 400。
    let (status, err) = send(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/labels"),
        ws,
        member,
        Some(json!({"label_id": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    // 跨 workspace：另一个 workspace 的标签挂不上（404 `label`）。
    let (other_ws, other_owner, _, _) = seed(&pool).await;
    let (_, foreign) = send(
        &app,
        "POST",
        "/api/labels",
        other_ws,
        other_owner,
        Some(json!({"name": "foreign", "color": "#111111"})),
    )
    .await;
    let foreign_id = foreign["id"].as_str().expect("foreign id").to_string();
    let (status, err) = send(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/labels"),
        ws,
        member,
        Some(json!({"label_id": foreign_id})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-workspace: {err}");

    // 摘：变更 ⇒ 带 revision；再摘 ⇒ 幂等。
    let (status, detached) = send(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/labels/{label_id}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detach: {detached}");
    assert_eq!(detached["labels"].as_array().map(Vec::len), Some(0));
    assert!(detached["issue_revision"].is_i64());
    let (status, twice) = send(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/labels/{label_id}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(twice.get("issue_revision").is_none());

    // 尾斜杠双形态：目录面两种形态都要能命中（上游 `r.Route` + `Get("/")`）。
    let (status, _) = send(&app, "GET", "/api/labels/", ws, member, None).await;
    assert_eq!(status, StatusCode::OK, "trailing-slash 列表形态");
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/labels/{label_id}/"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trailing-slash 单体形态");

    // 删：清关联后 204，再删 → 404。
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/labels/{label_id}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/labels/{label_id}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, other_ws, [other_owner, Uuid::nil(), Uuid::nil()]).await;
    cleanup(&pool, ws, [owner, member, outsider]).await;
}

/// 2) property 定义面（admin 门 + 上限 + config 校验）+ 值面桥接。
#[allow(clippy::too_many_lines)]
// 定义面与值面是一次事务语义的两个投影，拆成两例会丢掉「同一 workspace」前提
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn property_definition_and_value_bridging() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, owner, member, outsider) = seed(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 普通 member 建定义 → 403（上游 `requirePropertyAdmin`）。
    let (status, err) = send(
        &app,
        "POST",
        "/api/properties",
        ws,
        member,
        Some(json!({"name": "severity", "type": "text"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
    assert_eq!(err["error"]["code"], "forbidden");

    // 非法类型 / 保留名 → 400。
    for body in [
        json!({"name": "x", "type": "banana"}),
        json!({"name": "title", "type": "text"}),
        json!({"name": "x", "type": "select"}),
    ] {
        let (status, err) = send(&app, "POST", "/api/properties", ws, owner, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "invalid def: {err}");
    }

    // select + 选项（id 缺省由服务端生成 UUID）。
    let option_id = Uuid::new_v4().to_string();
    let (status, def) = send(
        &app,
        "POST",
        "/api/properties",
        ws,
        owner,
        Some(json!({
            "name": "Severity",
            "type": "select",
            "description": "how bad",
            "icon": "flag",
            "config": {"options": [{"id": option_id, "name": "High", "color": "#ef4444"}]},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create def: {def}");
    assert_eq!(def["type"], "select");
    assert_eq!(def["icon"], "flag");
    assert_eq!(def["archived"], false);
    assert!(def["archived_at"].is_null(), "未归档是 null 而不是缺字段");
    assert_eq!(def["usage_count"], 0);
    assert_eq!(def["config"]["options"][0]["id"], option_id.as_str());
    assert_eq!(def["config"]["options"][0]["color"], "#ef4444");
    let select_id = def["id"].as_str().expect("def id").to_string();

    // text 定义（值面 happy path 用它）。
    let (status, text_def) = send(
        &app,
        "POST",
        "/api/properties/",
        ws,
        owner,
        Some(json!({"name": "note", "type": "text"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "trailing-slash create: {text_def}"
    );
    assert_eq!(
        text_def["config"],
        json!({}),
        "非 select 类型 config 是空对象"
    );
    let text_id = text_def["id"].as_str().expect("text id").to_string();

    // 列表 / 单体（无 admin 门）。
    let (status, listed) = send(&app, "GET", "/api/properties", ws, member, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 2);
    assert_eq!(
        listed["properties"][0]["name"], "Severity",
        "按 position 排序"
    );
    let (status, err) = send(&app, "GET", "/api/properties", ws, outsider, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
    let (status, one) = send(
        &app,
        "GET",
        &format!("/api/properties/{text_id}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["id"], text_id.as_str());
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/properties/{}", Uuid::new_v4()),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // --- 值面 ---
    let issue_id = create_issue(&app, ws, member).await;
    let value_uri = |pid: &str| format!("/api/issues/{issue_id}/properties/{pid}");

    // 定义不存在 → 404（在值校验之前）。
    let (status, err) = send(
        &app,
        "PUT",
        &value_uri(&Uuid::new_v4().to_string()),
        ws,
        member,
        Some(json!({"value": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{err}");

    // 缺 `value` → 400 `value is required`；显式 null → 400 另一条消息。
    let (status, err) = send(
        &app,
        "PUT",
        &value_uri(&text_id),
        ws,
        member,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 本仓 400 统一带 `validation error: ` 前缀（`Error::Validation` 的 Display），
    // 上游的裸文案是前缀之后的部分（登记为偏差）。
    assert_eq!(
        err["error"]["message"],
        "validation error: value is required"
    );
    let (status, err) = send(
        &app,
        "PUT",
        &value_uri(&text_id),
        ws,
        member,
        Some(json!({"value": null})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        err["error"]["message"],
        "validation error: value cannot be null (use DELETE to unset a property)"
    );

    // 类型不匹配（select 定义收到数字）→ 400，消息里列出合法 option id。
    let (status, err) = send(
        &app,
        "PUT",
        &value_uri(&select_id),
        ws,
        member,
        Some(json!({"value": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains(&option_id),
        "错误消息应枚举合法选项: {err}"
    );

    // happy path：写 text 值 → JSONB 落地（key = 定义 UUID 文本）。
    let (status, written) = send(
        &app,
        "PUT",
        &value_uri(&text_id),
        ws,
        member,
        Some(json!({"value": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set value: {written}");
    assert_eq!(written["properties"][text_id.as_str()], "hello");
    assert!(written["issue_revision"].is_i64());

    // select 值（选项 id）→ OK，且 usage_count 反映引用数。
    let (status, written) = send(
        &app,
        "PUT",
        &value_uri(&select_id),
        ws,
        member,
        Some(json!({"value": option_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set select: {written}");
    let (_, listed) = send(&app, "GET", "/api/properties", ws, member, None).await;
    assert_eq!(listed["properties"][0]["usage_count"], 1);

    // 删值：恒带 issue_revision。
    let (status, removed) = send(&app, "DELETE", &value_uri(&text_id), ws, member, None).await;
    assert_eq!(status, StatusCode::OK, "unset: {removed}");
    assert!(removed["issue_revision"].is_i64());
    assert!(
        removed["properties"].get(text_id.as_str()).is_none(),
        "删掉的 key 不应残留: {removed}"
    );

    // PATCH：改名 + 归档；随后归档定义不再收新值（400），但删值仍允许（200）。
    let (status, patched) = send(
        &app,
        "PATCH",
        &format!("/api/properties/{select_id}"),
        ws,
        owner,
        Some(json!({"name": "Severity v2", "archived": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(patched["archived"], true);
    assert!(patched["archived_at"].is_string(), "归档后带时间戳");

    let (status, err) = send(
        &app,
        "PUT",
        &value_uri(&select_id),
        ws,
        member,
        Some(json!({"value": option_id})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "archived def: {err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("archived"),
        "{err}"
    );
    let (status, _) = send(&app, "DELETE", &value_uri(&select_id), ws, member, None).await;
    assert_eq!(status, StatusCode::OK, "归档定义仍可删值");

    // 默认列表不含归档；`include_archived=true` 才含（字面量比较）。
    let (_, active) = send(&app, "GET", "/api/properties", ws, member, None).await;
    assert_eq!(active["total"], 1);
    let (status, all) = send(
        &app,
        "GET",
        &format!("/api/properties/?include_archived=true&workspace_id={ws}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["total"], 2);
    let (_, upper) = send(
        &app,
        "GET",
        &format!("/api/properties?include_archived=TRUE&workspace_id={ws}"),
        ws,
        member,
        None,
    )
    .await;
    assert_eq!(upper["total"], 1, "只有字面量 true 才算真");

    // 非 admin 改定义 → 403。
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/api/properties/{select_id}"),
        ws,
        member,
        Some(json!({"name": "nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    cleanup(&pool, ws, [owner, member, outsider]).await;
}
