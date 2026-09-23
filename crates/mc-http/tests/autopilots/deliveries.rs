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

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::support::{
    app_with_db, call, cleanup, connect, err_message, seed_autopilot, seed_outsider,
    seed_schedule_trigger, seed_user, seed_webhook_trigger, seed_workspace,
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
struct DeliverySpec<'a> {
    status: &'a str,
    signature_status: &'a str,
    raw_body: Option<&'a [u8]>,
    response_body: Option<&'a str>,
    selected_headers: Value,
}

impl<'a> DeliverySpec<'a> {
    fn new() -> DeliverySpec<'a> {
        Self {
            status: "dispatched",
            signature_status: "valid",
            raw_body: Some(br#"{"action":"opened"}"#),
            response_body: None,
            selected_headers: json!({"content-type": "application/json"}),
        }
    }
}

/// 铺一条 `webhook_delivery` 行（`created_at` 用 SQL 相对时间，排序用例要用）。
async fn seed_delivery(
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
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_e2e", None, None).await;
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
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_e2e", None, None).await;
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
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_e2e", None, None).await;
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
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_e2e", None, None).await;
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
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_e2e", None, None).await;

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
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_e2e", None, None).await;
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
    let their_trigger = seed_webhook_trigger(&pool, theirs, "awt_foreign", None, None).await;
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

/// replay 的**判负阶梯**：五道闸按上游顺序，各自一句文案。
#[allow(clippy::too_many_lines)] // 127 行：五道闸各自要造自己的前置状态（raw_body / 状态 / trigger）
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn replay_gate_ladder_matches_upstream_order() {
    let Some((pool, db)) = connect().await else {
        println!("skip replay_gate_ladder_matches_upstream_order: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_ladder", None, None).await;
    // 停用的 schedule 触发器专供「trigger is disabled」那一档（webhook 触发器默认启用）。
    let disabled = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;
    sqlx::query("UPDATE autopilot_trigger SET enabled = false WHERE id = $1")
        .bind(disabled)
        .execute(&pool)
        .await
        .expect("disable trigger");
    let replay_uri =
        |delivery: Uuid| format!("/api/autopilots/{autopilot}/deliveries/{delivery}/replay");

    // ① 签名失败（`status='rejected'`）→ 400。
    let rejected = seed_delivery(
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
    let (status, body) = call(&app, "POST", &replay_uri(rejected), ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "cannot replay a delivery that failed signature verification"
    );

    // ② 没有 raw body → 400（顺序在「autopilot 是否 active」之前）。
    let bodyless = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            raw_body: None,
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;
    let (status, body) = call(&app, "POST", &replay_uri(bodyless), ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "original delivery has no raw body to replay"
    );

    // ③ autopilot 不是 active → 400。
    let paused = seed_autopilot(&pool, ws, "paused", "member", owner).await;
    let paused_trigger = seed_webhook_trigger(&pool, paused, "awt_paused", None, None).await;
    let paused_delivery = seed_delivery(
        &pool,
        ws,
        paused,
        paused_trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{paused}/deliveries/{paused_delivery}/replay"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot is not active");

    // ④ trigger 停用 → 400。
    let disabled_delivery = seed_delivery(
        &pool,
        ws,
        autopilot,
        disabled,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &replay_uri(disabled_delivery),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "trigger is disabled");

    // ⑤ `raw_body` 不再是合法 JSON → 400（文案带 `stored body no longer parses: `）。
    let broken = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            raw_body: Some(b"{not json"),
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;
    let (status, body) = call(&app, "POST", &replay_uri(broken), ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert!(
        upstream_message(&body).starts_with("stored body no longer parses: "),
        "{body}"
    );

    // ⑥ `Idempotency-Key` 过长 → 400（在插入之前）。
    let long_key = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec::new(),
        "1 hour",
    )
    .await;
    let (status, body) =
        keyed_replay(&app, &replay_uri(long_key), ws, owner, &"k".repeat(256)).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "Idempotency-Key is too long");

    cleanup(&pool, ws, &[owner]).await;
}

/// replay 成功：202 + 新行（`queued` / `replayed_from_delivery_id` / 幂等键），
/// 同键重放**不新建第二行**、两次都 202 且 id 相同。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn replay_is_202_and_idempotent_per_key() {
    let Some((pool, db)) = connect().await else {
        println!("skip replay_is_202_and_idempotent_per_key: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let trigger = seed_webhook_trigger(&pool, autopilot, "awt_replay", None, None).await;
    let original = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            status: "failed",
            signature_status: "valid",
            raw_body: Some(br#"{"action":"opened","payload":{"n":1}}"#),
            response_body: Some("upstream 500"),
            selected_headers: json!({"content-type": "application/json"}),
        },
        "1 hour",
    )
    .await;
    let uri = format!("/api/autopilots/{autopilot}/deliveries/{original}/replay");

    let (status, body) = keyed_replay(&app, &uri, ws, owner, "key-1").await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["status"], "queued");
    assert_eq!(body["signature_status"], "not_required");
    assert_eq!(body["replayed_from_delivery_id"], original.to_string());
    assert_eq!(body["replay_idempotency_key"], "key-1");
    assert_eq!(body["raw_body"], r#"{"action":"opened","payload":{"n":1}}"#);
    assert_eq!(body["selected_headers"]["content-type"], "application/json");
    // replay 行不带去重键（上游刻意让重放绕开 provider 去重）。
    assert!(body["dedupe_key"].is_null(), "{body}");
    assert_eq!(body["provider"], "github");
    assert_eq!(body["event"], "push");
    assert!(body["autopilot_run_id"].is_null(), "{body}");
    let replay_id = body["id"].as_str().unwrap().to_string();

    // 同键：202 + 同一行（幂等命中走 `find_replay`，不插第二行）。
    let (status, body) = keyed_replay(&app, &uri, ws, owner, "key-1").await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["id"], replay_id);
    // 换键：新行。
    let (status, body) = keyed_replay(&app, &uri, ws, owner, "key-2").await;
    assert_eq!(status, 202, "{body}");
    assert_ne!(body["id"], replay_id);

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_delivery WHERE replayed_from_delivery_id = $1",
    )
    .bind(original)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 2, "同一个原投递 + 两个键 = 两行 replay");

    // 原投递没被动过（replay 不改原行）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/deliveries/{original}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "failed");
    assert_eq!(body["response_body"], "upstream 500");

    cleanup(&pool, ws, &[owner]).await;
}

/// 带 `Idempotency-Key` 的 replay 请求。
async fn keyed_replay(
    app: &axum::Router,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    key: &str,
) -> (StatusCode, Value) {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(super::support::USER_ID_HEADER, user_id.to_string())
        .header(super::support::WORKSPACE_HEADER, workspace_id.to_string())
        .header("Idempotency-Key", key)
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (status, super::support::body_json(res.into_body()).await)
}
