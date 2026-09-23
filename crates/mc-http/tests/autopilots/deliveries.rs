//! 投递读面 + replay 的 e2e（M5-4 / LUM-1569）：#18 `GET …/deliveries`、
//! #19 `GET …/deliveries/:deliveryId`、#20 `POST …/deliveries/:deliveryId/replay`。
//!
//! 三条**都**只测到「路由 + 库」这一层：`webhook_delivery` 行是**上游形状**，直接 SQL 铺，
//! 不经 M5-5 的入站面（那一片还没合）。
//!
//! 关注点：
//!
//! 1. **投影差别**：列表（`slim`）三个字段**整个缺席**（不是 `null`）——
//!    `selected_headers` / `raw_body` / `response_body`；详情才带；
//! 2. **鉴权**：#18/#19 成员即可；#20 与写面同闸（非成员 404 / 成员 403）；
//! 3. **跨 autopilot 的 deliveryId 一律 404**（ID 猜中也不能读到、更不能 replay）；
//! 4. **replay 的判负阶梯逐字对照上游**：签名失败 → 无 raw body → autopilot 未启用 →
//!    trigger 没了（404）/ 停用（400）→ body 解不开（400）→ `Idempotency-Key` 过长（400）；
//!    成功与幂等命中都是 **202**，且幂等命中**不新建第二行**。
//!
//! replay **不唤醒 worker**（本切片登记的 `known_gap`）= 新行落 `queued` 后没人取走，
//! 所以这里只断言「行落库 + 字段对」，不断言它被处理。

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::support::{
    app_with_db, call, cleanup, connect, err_message, seed_autopilot, seed_outsider, seed_user,
    seed_webhook_trigger, seed_workspace,
};
use super::triggers::upstream_message;

/// #18–#20 的 `(方法, URI 模板)`。
const DELIVERY_ROUTES: [(&str, &str); 3] = [
    ("GET", "/api/autopilots/{ap}/deliveries"),
    ("GET", "/api/autopilots/{ap}/deliveries/{d}"),
    ("POST", "/api/autopilots/{ap}/deliveries/{d}/replay"),
];

fn route_of(template: &str, autopilot_id: Uuid, delivery_id: Uuid) -> String {
    template
        .replace("{ap}", &autopilot_id.to_string())
        .replace("{d}", &delivery_id.to_string())
}

/// 一条投递的铺陈参数（默认值见 [`DeliverySpec::new`]）。
///
/// `pub(crate)`：`deliveries_replay.rs` 复用同一份夹具（同 target 内的兄弟文件）。
pub(crate) struct DeliverySpec<'a> {
    pub(crate) status: &'a str,
    pub(crate) signature_status: &'a str,
    pub(crate) raw_body: Option<&'a [u8]>,
    pub(crate) response_body: Option<&'a str>,
    pub(crate) selected_headers: Value,
}

impl<'a> DeliverySpec<'a> {
    pub(crate) fn new() -> DeliverySpec<'a> {
        Self {
            status: "dispatched",
            signature_status: "valid",
            raw_body: Some(br#"{"action":"opened"}"#),
            response_body: None,
            selected_headers: json!({"content-type": "application/json"}),
        }
    }
}

/// 每个用例一个**唯一** webhook token。
///
/// `idx_autopilot_trigger_webhook_token` 是**全局**唯一索引（`WHERE kind = 'webhook' AND
/// webhook_token IS NOT NULL`，不是按工作区收窄），而门 ⑥ 的 e2e 是**并发**跑的
/// （`cargo test … -- --ignored`，没有 `--test-threads=1`）⇒ 同一个 binary 里两个用例用同一个
/// 固定 token 会互撞（实测 `23505 … Key (webhook_token)=(awt_e2e) already exists`）。
/// `pub(crate)`：`deliveries_replay.rs` 同样要用。
pub(crate) fn unique_webhook_token(stem: &str) -> String {
    format!("{stem}_{}", Uuid::new_v4().simple())
}

/// 铺一条 `webhook_delivery` 行（`created_at` 用 SQL 相对时间，排序用例要用）。
///
/// `pub(crate)`：同 [`DeliverySpec`]（`deliveries_replay.rs` 复用）。
pub(crate) async fn seed_delivery(
    pool: &PgPool,
    workspace_id: Uuid,
    autopilot_id: Uuid,
    trigger_id: Uuid,
    spec: &DeliverySpec<'_>,
    created_ago: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO webhook_delivery \
            (workspace_id, autopilot_id, trigger_id, provider, event, signature_status, status, \
             selected_headers, content_type, raw_body, response_status, response_body, \
             created_at, received_at, last_attempt_at, available_at) \
         VALUES ($1, $2, $3, 'github', 'push', $4, $5, $6::jsonb, 'application/json', $7, 200, $8, \
             now() - $9::interval, now() - $9::interval, now() - $9::interval, now() - $9::interval) \
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(autopilot_id)
    .bind(trigger_id)
    .bind(spec.signature_status)
    .bind(spec.status)
    .bind(spec.selected_headers.to_string())
    .bind(spec.raw_body)
    .bind(spec.response_body)
    .bind(created_ago)
    .fetch_one(pool)
    .await
    .expect("insert webhook_delivery")
}

/// 缺 `X-Multica-User-Id` → 401（三条路由都要，同方法上探）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn missing_user_header_is_401_on_every_delivery_route() {
    let Some((pool, db)) = connect().await else {
        println!("skip missing_user_header_is_401_on_every_delivery_route: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(
        &pool,
        autopilot,
        &unique_webhook_token("awt_e2e"),
        None,
        None,
    )
    .await;
    let delivery = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;

    for (method, template) in DELIVERY_ROUTES {
        let uri = route_of(template, autopilot, delivery);
        assert_eq!(
            super::triggers::probe_status(&app, method, &uri, None, Some(&ws.to_string())).await,
            401,
            "{method} {uri}"
        );
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 缺工作区头 / 坏工作区头 → 400。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn bad_workspace_header_is_400_on_every_delivery_route() {
    let Some((pool, db)) = connect().await else {
        println!("skip bad_workspace_header_is_400_on_every_delivery_route: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(
        &pool,
        autopilot,
        &unique_webhook_token("awt_e2e"),
        None,
        None,
    )
    .await;
    let delivery = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;

    for (method, template) in DELIVERY_ROUTES {
        let uri = route_of(template, autopilot, delivery);
        assert_eq!(
            super::triggers::probe_status(&app, method, &uri, Some(owner), None).await,
            400,
            "缺头 {method} {uri}"
        );
        assert_eq!(
            super::triggers::probe_status(&app, method, &uri, Some(owner), Some("nope")).await,
            400,
            "坏 uuid {method} {uri}"
        );
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 405 + 尾斜杠 404：方法集合与路径形态都是精确的。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn method_set_and_path_form_are_exact() {
    let Some((pool, db)) = connect().await else {
        println!("skip method_set_and_path_form_are_exact: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(
        &pool,
        autopilot,
        &unique_webhook_token("awt_e2e"),
        None,
        None,
    )
    .await;
    let delivery = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;

    // 405：`/deliveries` 上没有 POST、详情上没有 DELETE、replay 上没有 GET。
    for (method, uri) in [
        ("POST", format!("/api/autopilots/{autopilot}/deliveries")),
        (
            "DELETE",
            format!("/api/autopilots/{autopilot}/deliveries/{delivery}"),
        ),
        (
            "GET",
            format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay"),
        ),
    ] {
        let (status, _) = call(&app, method, &uri, ws, owner, None).await;
        assert_eq!(status, 405, "{method} {uri}");
    }

    // 单形态：尾斜杠一律 404。
    for (method, uri) in [
        ("GET", format!("/api/autopilots/{autopilot}/deliveries/")),
        (
            "GET",
            format!("/api/autopilots/{autopilot}/deliveries/{delivery}/"),
        ),
        (
            "POST",
            format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay/"),
        ),
    ] {
        let (status, body) = call(&app, method, &uri, ws, owner, None).await;
        assert_eq!(status, 404, "{method} {uri} → {body}");
    }

    cleanup(&pool, ws, &[owner]).await;
}

/// #18/#19 成员即可读；#20 要写权（非成员 404 / 普通成员 403 / 创建者 202）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn reads_are_member_wide_and_replay_needs_write_permission() {
    let Some((pool, db)) = connect().await else {
        println!("skip reads_are_member_wide_and_replay_needs_write_permission: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let plain_member = seed_user(&pool, ws, "member").await;
    let outsider = seed_outsider(&pool).await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(
        &pool,
        autopilot,
        &unique_webhook_token("awt_e2e"),
        None,
        None,
    )
    .await;
    let delivery = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;

    // ① 读面：普通成员 200。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries"),
        ws,
        plain_member,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 1);
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries/{delivery}"),
        ws,
        plain_member,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // ② replay 非成员 → 404（与读面同一条 `require_member` 腿）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay"),
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "workspace");

    // ③ replay 普通成员 → 403（写权闸）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay"),
        ws,
        plain_member,
        None,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        err_message(&body),
        "forbidden: insufficient permission for this autopilot"
    );

    // ④ 创建者 → 202（成功路径在本文件另一条用例里细测）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 202, "{body}");

    cleanup(&pool, ws, &[owner, plain_member, outsider]).await;
}

/// 列表：`created_at DESC`、`total` = 本页条数、slim 三字段**整个缺席**、空列表是 `[]`。
#[allow(clippy::too_many_lines)] // 101 行：一条链路要依次验排序、分页上限、缺席字段与空列表
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn list_is_slim_ordered_and_omits_the_detail_fields() {
    let Some((pool, db)) = connect().await else {
        println!("skip list_is_slim_ordered_and_omits_the_detail_fields: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let empty = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(
        &pool,
        autopilot,
        &unique_webhook_token("awt_e2e"),
        None,
        None,
    )
    .await;

    let oldest = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "3 hours",
    )
    .await;
    let middle = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            status: "failed",
            signature_status: "missing",
            response_body: Some("boom"),
            ..DeliverySpec::new()
        },
        "2 hours",
    )
    .await;
    let newest = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            status: "rejected",
            signature_status: "invalid",
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 3, "{body}");
    let rows = body["deliveries"].as_array().expect("deliveries array");
    let ids = rows
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![newest.to_string(), middle.to_string(), oldest.to_string()],
        "created_at DESC"
    );

    // slim：三个详情专属字段**整个缺席**（`omitempty`），其余字段即使在库里是 NULL 也照发 `null`。
    for row in rows {
        for absent in ["selected_headers", "raw_body", "response_body"] {
            assert!(row.get(absent).is_none(), "slim 不该带 `{absent}`: {row}");
        }
        for present in ["dedupe_key", "error", "reason_code", "response_status"] {
            assert!(
                row.get(present).is_some(),
                "非 omitempty 字段必须显式 null: `{present}` in {row}"
            );
        }
        assert_eq!(row["provider"], "github");
        assert_eq!(row["event"], "push");
        assert_eq!(row["attempt_count"], 1);
        assert_eq!(row["dispatch_attempts"], 0);
        assert_eq!(row["content_type"], "application/json");
    }

    // 空列表：`[]` + `total: 0`。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{empty}/deliveries"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"deliveries": [], "total": 0}));

    cleanup(&pool, ws, &[owner]).await;
}

/// 详情：三个专属字段回来（`raw_body` 是 UTF-8 化原文、`selected_headers` 是 JSON、`response_body`
/// 原样）；`limit`/`offset` 与 runs 面同一口径（`limit=1` 只回最新一条）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn get_returns_the_detail_projection_and_honours_paging() {
    let Some((pool, db)) = connect().await else {
        println!("skip get_returns_the_detail_projection_and_honours_paging: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(
        &pool,
        autopilot,
        &unique_webhook_token("awt_e2e"),
        None,
        None,
    )
    .await;
    let older = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "2 hours",
    )
    .await;
    let newer = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            raw_body: Some(r#"{"action":"closed","n":1}"#.as_bytes()),
            response_body: Some("accepted"),
            selected_headers: json!({"x-github-event": "push", "n": 1}),
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries/{newer}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["raw_body"], r#"{"action":"closed","n":1}"#);
    assert_eq!(body["response_body"], "accepted");
    assert_eq!(
        body["selected_headers"],
        json!({"x-github-event": "push", "n": 1})
    );
    assert_eq!(body["id"], newer.to_string());

    // `limit=1` → 只有最新那条（分页与 runs 面共用 `parse_limit_offset`）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries?limit=1"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 1);
    assert_eq!(body["deliveries"][0]["id"], newer.to_string());
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries?limit=1&offset=1"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["deliveries"][0]["id"], older.to_string());

    cleanup(&pool, ws, &[owner]).await;
}

/// 跨 autopilot 的 deliveryId：读与 replay 都是 404，且与「不存在」逐字同文案。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn delivery_from_another_autopilot_is_not_found() {
    let Some((pool, db)) = connect().await else {
        println!("skip delivery_from_another_autopilot_is_not_found: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let mine = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let theirs = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let their_trigger = seed_webhook_trigger(
        &pool,
        theirs,
        &unique_webhook_token("awt_foreign"),
        None,
        None,
    )
    .await;
    let foreign = seed_delivery(
        &pool,
        ws,
        theirs,
        their_trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{mine}/deliveries/{foreign}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "delivery");

    let (status, replay) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{mine}/deliveries/{foreign}/replay"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{replay}");
    assert_eq!(err_message(&body), err_message(&replay));

    // 路径参数不是 uuid → 400（delivery id 的 400 在 `load_delivery_for_autopilot` 里最先发生）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{mine}/deliveries/nope"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "delivery id must be a valid uuid");

    // autopilot id 坏在前（两个都坏时报 autopilot）。
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/nope/deliveries/also-nope",
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot id must be a valid uuid");

    cleanup(&pool, ws, &[owner]).await;
}
