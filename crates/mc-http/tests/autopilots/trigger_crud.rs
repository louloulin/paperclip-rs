//! trigger 写面 e2e（M5-3 / LUM-1568）：**数据面** —— #10 创建、#11 更新、#12 删除。
//!
//! 与 `triggers.rs`（路由形态 / 鉴权分层）共用 `super::triggers` 里的工具与种子。
//! 三块契约在这里被钉住：
//! - **校验顺序**：同一份坏输入只能给一个文案（上游是一串提前返回的 `if`）；
//! - **`next_run_at` 的直赋值保护**：SQL 里这一列是 `= $n` 而不是 `COALESCE`，
//!   所以 PATCH 只改 label 时 handler 必须**播种旧值**，否则会被抹成 NULL；
//! - **`event_filters` 三态**：缺省/null 保留、`[]` 清空、值替换。

use serde_json::{json, Value};
use uuid::Uuid;

use super::support::{
    app_with_db, call, cleanup, connect, err_message, seed_autopilot, seed_schedule_trigger,
    seed_webhook_trigger, seed_workspace,
};
use super::triggers::upstream_message;
/// `POST …/triggers` 的校验顺序就是契约：同一份坏输入只能给一个文案。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_validation_order_matches_upstream() {
    let Some((pool, db)) = connect().await else {
        println!("skip create_validation_order_matches_upstream: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let uri = format!("/api/autopilots/{autopilot}/triggers");

    // (请求体, 期望的 400 文案)
    let cases: [(Value, &str); 10] = [
        (json!({}), "kind is required"),
        (json!({"kind":"api"}), "kind must be schedule or webhook"),
        (
            json!({"kind":"schedule"}),
            "cron_expression is required for schedule triggers",
        ),
        (
            json!({"kind":"webhook","timezone":"Asia/Shanghai"}),
            "timezone is not valid for webhook triggers",
        ),
        (
            json!({"kind":"schedule","cron_expression":"0 9 * * *","event_filters":[{"event":"push"}]}),
            "event_filters is only valid for webhook triggers",
        ),
        (
            json!({"kind":"schedule","cron_expression":"0 9 * * *","provider":"github"}),
            "provider is only valid for webhook triggers",
        ),
        (
            json!({"kind":"webhook","provider":"gitlab"}),
            "provider must be generic or github",
        ),
        (
            json!({"kind":"schedule","cron_expression":"nonsense"}),
            "expected exactly 5 fields",
        ),
        (
            json!({"kind":"schedule","cron_expression":"0 9 * * *","timezone":"Nowhere/Nope"}),
            "invalid timezone \"Nowhere/Nope\"",
        ),
        (
            json!({"kind":"webhook","event_filters":[{"event":""}]}),
            "event_filters[0].event must not be empty",
        ),
    ];
    for (body, expected) in cases {
        let (status, resp) = call(&app, "POST", &uri, ws, owner, Some(body.clone())).await;
        assert_eq!(status, 400, "{body} → {resp}");
        assert!(
            upstream_message(&resp).starts_with(expected),
            "{body} → {}（期望以 {expected:?} 开头）",
            upstream_message(&resp)
        );
    }

    // 文案本身是 `validation error: …`（本仓统一前缀，docs/40 §5）。
    let (status, resp) = call(&app, "POST", &uri, ws, owner, Some(json!({"kind":"api"}))).await;
    assert_eq!(status, 400, "{resp}");
    assert_eq!(
        err_message(&resp),
        "validation error: kind must be schedule or webhook"
    );

    // 坏 body / 空 body：Go 的 `json.Decode` 语义 ⇒ 400 `invalid request body`。
    let (status, resp) = call(&app, "POST", &uri, ws, owner, None).await;
    assert_eq!(status, 400, "{resp}");
    assert_eq!(upstream_message(&resp), "invalid request body");
    let (status, resp) = call(&app, "POST", &uri, ws, owner, Some(json!([1, 2]))).await;
    assert_eq!(status, 400, "{resp}");
    assert_eq!(upstream_message(&resp), "invalid request body");
    // 字面量 `null` **不是**解码错误（Go 的零值语义）⇒ 落到 `kind is required`。
    let (status, resp) = call(&app, "POST", &uri, ws, owner, Some(Value::Null)).await;
    assert_eq!(status, 400, "{resp}");
    assert_eq!(upstream_message(&resp), "kind is required");

    cleanup(&pool, ws, &[owner]).await;
}

/// schedule 创建：201 + 派生 `next_run_at`；`timezone` 缺省写 `''`（响应回 `""`）而不是 NULL。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_schedule_trigger_returns_201_and_a_computed_next_run() {
    let Some((pool, db)) = connect().await else {
        println!("skip create_schedule_trigger_returns_201_and_a_computed_next_run: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let uri = format!("/api/autopilots/{autopilot}/triggers");

    let (status, body) = call(
        &app,
        "POST",
        &uri,
        ws,
        owner,
        Some(json!({
            "kind": "schedule",
            "cron_expression": "0 9 * * 1-5",
            "timezone": "Asia/Shanghai",
            "label": "morning"
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["autopilot_id"], autopilot.to_string());
    assert_eq!(body["kind"], "schedule");
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(body["cron_expression"], "0 9 * * 1-5");
    assert_eq!(body["timezone"], "Asia/Shanghai");
    assert_eq!(body["label"], "morning");
    let next = body["next_run_at"].as_str().expect("next_run_at 必须有值");
    let parsed = chrono::DateTime::parse_from_rfc3339(next).expect("next_run_at 可解析");
    assert!(parsed > chrono::Utc::now(), "next_run_at 应在未来: {next}");
    // 非 webhook 的凭据字段一律为空（同生同死）。
    assert!(body["webhook_token"].is_null(), "{body}");
    assert!(body["webhook_path"].is_null(), "{body}");
    assert!(body["provider"].is_null(), "{body}");
    assert_eq!(body["has_signing_secret"], json!(false));
    assert!(body["signing_secret_hint"].is_null(), "{body}");
    assert!(body.get("event_filters").is_none(), "{body}");

    // 时区缺省：`ptrToText(nil)` = `""` ⇒ 落库是空串（不是 NULL），响应回 `""`。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        ws,
        owner,
        Some(json!({"kind":"schedule","cron_expression":"0 9 * * *"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["timezone"], json!(""));
    let stored: Option<String> =
        sqlx::query_scalar("SELECT timezone FROM autopilot_trigger WHERE id = $1")
            .bind(Uuid::parse_str(body["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(stored.as_deref(), Some(""), "缺省时区落空串而不是 NULL");

    // 时区给空串是**合法**输入（`time.LoadLocation("")` = UTC），不是 400。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        ws,
        owner,
        Some(json!({"kind":"schedule","cron_expression":"0 9 * * *","timezone":""})),
    )
    .await;
    assert_eq!(status, 201, "{body}");

    cleanup(&pool, ws, &[owner]).await;
}

/// webhook 创建：铸 token + `webhook_path`（与 M5-5 的 ingress 入口同形）+ provider 白名单。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_webhook_trigger_mints_a_scoped_token() {
    let Some((pool, db)) = connect().await else {
        println!("skip create_webhook_trigger_mints_a_scoped_token: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let uri = format!("/api/autopilots/{autopilot}/triggers");

    let (status, body) = call(
        &app,
        "POST",
        &uri,
        ws,
        owner,
        Some(json!({
            "kind": "webhook",
            "provider": "github",
            "event_filters": [{"event": "push", "actions": ["opened"]}]
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["kind"], "webhook");
    assert_eq!(body["provider"], "github");
    assert_eq!(body["timezone"], Value::Null, "webhook 不落时区: {body}");
    assert!(body["cron_expression"].is_null(), "{body}");
    assert!(body["next_run_at"].is_null(), "{body}");
    assert_eq!(
        body["event_filters"],
        json!([{"event":"push","actions":["opened"]}])
    );

    let token = body["webhook_token"]
        .as_str()
        .expect("webhook_token 必须有值");
    assert!(token.starts_with("awt_"), "{token}");
    assert_eq!(
        token.len(),
        47,
        "awt_ + 32 字节的 base64url（无填充）: {token}"
    );
    let expected_path = format!("/api/webhooks/autopilots/{token}");
    assert_eq!(body["webhook_path"], expected_path);
    // `MULTICA_PUBLIC_URL` 未配置时省略；配了就一定是 base + path。
    if let Some(url) = body["webhook_url"].as_str() {
        assert!(url.ends_with(&expected_path), "{url}");
    }

    // 库里真的落了同一个 token + provider + event_filters。
    let trigger_id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    let (stored_token, stored_provider, stored_filters): (String, String, Value) = sqlx::query_as(
        "SELECT webhook_token, provider, event_filters FROM autopilot_trigger WHERE id = $1",
    )
    .bind(trigger_id)
    .fetch_one(&pool)
    .await
    .expect("trigger row");
    assert_eq!(stored_token, token);
    assert_eq!(stored_provider, "github");
    assert_eq!(
        stored_filters,
        json!([{"event":"push","actions":["opened"]}])
    );

    // 显式 `provider: "generic"` 也合法；缺省同样是 `generic`。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        ws,
        owner,
        Some(json!({"kind":"webhook","provider":"generic"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["provider"], "generic");
    assert!(
        body.get("event_filters").is_none(),
        "空 event_filters 省略（接受全部事件）: {body}"
    );

    cleanup(&pool, ws, &[owner]).await;
}

/// PATCH 的三态：缺省/null 保留、`[]` 清空、值替换；未触及的列**逐字保留**。
///
/// 这里只覆盖 schedule 面（`next_run_at` 是 SQL 直赋值列，是这批路由里最容易写错的一列）；
/// webhook 的 `event_filters` 三态另开一个用例。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn update_keeps_untouched_columns_and_honours_the_tri_state() {
    let Some((pool, db)) = connect().await else {
        println!("skip update_keeps_untouched_columns_and_honours_the_tri_state: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    // 直接种一行（`next_run_at` 是展示列，测试要的是「被 PATCH 保留」而不是「重算对」）。
    let schedule = seed_schedule_trigger(&pool, autopilot, "0 9 * * 1-5", "1 hour").await;
    let schedule_uri = format!("/api/autopilots/{autopilot}/triggers/{schedule}");

    // 只改 label：`next_run_at` 是 SQL 直赋值列 ⇒ handler 必须先播种旧值。
    let (status, created) = call(
        &app,
        "PATCH",
        &schedule_uri,
        ws,
        owner,
        Some(json!({"label":"x"})),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["label"], "x");
    assert_eq!(created["enabled"], json!(true), "未触及的 enabled 必须保留");
    assert_eq!(created["cron_expression"], "0 9 * * 1-5");
    let kept_next = created["next_run_at"].clone();
    assert!(
        !kept_next.is_null(),
        "未改 cron 时 next_run_at 不能被抹成 NULL"
    );

    // `{}` 与字面量 `null`（Go 的零值语义）都是「什么都不改」。
    for body in [json!({}), Value::Null] {
        let (status, unchanged) = call(&app, "PATCH", &schedule_uri, ws, owner, Some(body)).await;
        assert_eq!(status, 200, "{unchanged}");
        assert_eq!(unchanged["label"], "x");
        assert_eq!(unchanged["next_run_at"], kept_next);
    }

    // 关掉：只有 enabled 变。
    let (status, off) = call(
        &app,
        "PATCH",
        &schedule_uri,
        ws,
        owner,
        Some(json!({"enabled":false})),
    )
    .await;
    assert_eq!(status, 200, "{off}");
    assert_eq!(off["enabled"], json!(false));
    assert_eq!(off["label"], "x");
    assert_eq!(off["next_run_at"], kept_next);

    // 改 cron ⇒ `next_run_at` 重算（两次算出的时刻不可能相同：03:00 与 09:00 互斥）。
    let (status, moved) = call(
        &app,
        "PATCH",
        &schedule_uri,
        ws,
        owner,
        Some(json!({"cron_expression":"0 3 * * *"})),
    )
    .await;
    assert_eq!(status, 200, "{moved}");
    assert_ne!(moved["next_run_at"], kept_next, "{moved}");
    assert_eq!(moved["cron_expression"], "0 3 * * *");

    cleanup(&pool, ws, &[owner]).await;
}

/// webhook 的 `event_filters` 三态（缺省保留 → `[]` 清空 → 值替换），
/// 以及「非 schedule 触发器不接受 schedule 专有字段」。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn update_event_filters_tri_state_and_cross_kind_rejection() {
    let Some((pool, db)) = connect().await else {
        println!("skip update_event_filters_tri_state_and_cross_kind_rejection: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let hook = seed_webhook_trigger(
        &pool,
        autopilot,
        "awt_seeded",
        None,
        Some(r#"[{"event":"push"}]"#),
    )
    .await;
    let hook_uri = format!("/api/autopilots/{autopilot}/triggers/{hook}");

    // 缺省保留：只改 label，`event_filters` 逐字不动。
    let (status, kept) = call(
        &app,
        "PATCH",
        &hook_uri,
        ws,
        owner,
        Some(json!({"label":"h"})),
    )
    .await;
    assert_eq!(status, 200, "{kept}");
    assert_eq!(kept["event_filters"], json!([{"event":"push"}]));

    // `[]` 清空：库里落 `[]`（不是 NULL），响应省略字段（空 = 接受全部事件）。

    let (status, cleared) = call(
        &app,
        "PATCH",
        &hook_uri,
        ws,
        owner,
        Some(json!({"event_filters":[]})),
    )
    .await;
    assert_eq!(status, 200, "{cleared}");
    assert!(cleared.get("event_filters").is_none(), "{cleared}");
    let stored: Option<Value> =
        sqlx::query_scalar("SELECT event_filters FROM autopilot_trigger WHERE id = $1")
            .bind(hook)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(stored, Some(json!([])), "清空落 `[]` 而不是 NULL");

    // 替换。
    let (status, replaced) = call(
        &app,
        "PATCH",
        &hook_uri,
        ws,
        owner,
        Some(json!({"event_filters":[{"event":"issues"}]})),
    )
    .await;
    assert_eq!(status, 200, "{replaced}");
    assert_eq!(replaced["event_filters"], json!([{"event":"issues"}]));

    // 非 schedule 触发器不接受 schedule 专有字段。
    let (status, body) = call(
        &app,
        "PATCH",
        &hook_uri,
        ws,
        owner,
        Some(json!({"cron_expression":"0 3 * * *"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "cron_expression is only valid for schedule triggers"
    );
    let (status, body) = call(
        &app,
        "PATCH",
        &hook_uri,
        ws,
        owner,
        Some(json!({"timezone":"UTC"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "timezone is only valid for schedule triggers"
    );

    cleanup(&pool, ws, &[owner]).await;
}

/// DELETE：两种形态都 204，且真的把行删掉（第二次同 id 就是 404）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn delete_removes_the_trigger_on_both_slash_forms() {
    let Some((pool, db)) = connect().await else {
        println!("skip delete_removes_the_trigger_on_both_slash_forms: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let slash = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;
    let plain = seed_webhook_trigger(&pool, autopilot, "awt_delete", None, None).await;

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/autopilots/{autopilot}/triggers/{slash}/"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(body, Value::Null, "204 没有响应体");

    // 无斜杠形态是**同一 handler**（不是 307 重定向）。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/autopilots/{autopilot}/triggers/{plain}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM autopilot_trigger WHERE autopilot_id = $1")
            .bind(autopilot)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0);

    // 再删一次：404 `not found: trigger`。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/autopilots/{autopilot}/triggers/{slash}/"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "trigger");

    cleanup(&pool, ws, &[owner]).await;
}
