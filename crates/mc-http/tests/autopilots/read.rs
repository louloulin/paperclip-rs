//! 读面 e2e：`GET /api/autopilots[/]` 与 `GET /api/autopilots/:id[/]`（M5-1）。
//!
//! 断言打在**上游 JSON 契约**上：派生三列（`trigger_kinds` / `next_run_at` /
//! `last_run_status`）、订阅者的 `[]`、`can_write` / `can_manage_access` 的两态、
//! webhook 凭据的抹除，以及「解不开的 `event_filters` 是**丢掉**而不是 500」。

use serde_json::json;
use uuid::Uuid;

use super::support::{
    call, cleanup, err_message, first_id, seed_autopilot, seed_collaborator, seed_disabled_trigger,
    seed_run, seed_schedule_trigger, seed_subscriber, seed_user, seed_webhook_trigger,
    seed_workspace,
};

/// 列表：默认排除 `archived`，派生三列 + 订阅者 + `can_write` 全在一条响应里；
/// `can_manage_access` **不在列表上**（上游只在详情盖章）。
#[tokio::test]
#[ignore]
async fn list_excludes_archived_and_carries_derived_columns() {
    let Some((pool, db)) = super::support::connect().await else {
        println!(
            "skip list_excludes_archived_and_carries_derived_columns: no MULTICA_TEST_DATABASE_URL"
        );
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let viewer = seed_user(&pool, ws, "member").await;

    let active = seed_autopilot(&pool, ws, "active", "member", owner).await;
    seed_schedule_trigger(&pool, active, "0 9 * * 1-5", "1 hour").await;
    seed_disabled_trigger(&pool, active).await;
    seed_run(&pool, active, "failed", "2 hours").await;
    seed_subscriber(&pool, active, viewer).await;
    let archived = seed_autopilot(&pool, ws, "archived", "member", owner).await;

    // 带斜杠形态（上游 chi 的 `Route("/api/autopilots") + Get("/")`）。
    let (status, body) = call(&app, "GET", "/api/autopilots/", ws, owner, None).await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["total"], 1, "默认必须排除 archived: {body}");
    let row = &body["autopilots"][0];
    assert_eq!(row["id"], active.to_string());
    assert_eq!(row["workspace_id"], ws.to_string());
    assert_eq!(row["status"], "active");
    assert_eq!(row["assignee_type"], "agent");
    assert_eq!(row["execution_mode"], "run_only");
    assert_eq!(row["created_by_type"], "member");
    assert_eq!(row["created_by_id"], owner.to_string());
    // `description` / `project_id` / `pause_reason` 无 omitempty ⇒ 显式 null。
    assert!(row["description"].is_null());
    assert!(row["project_id"].is_null());
    assert!(row["pause_reason"].is_null());

    // 派生三列：停用的 `api` 触发器不算进 trigger_kinds。
    assert_eq!(row["trigger_kinds"], json!(["schedule"]));
    let next = row["next_run_at"].as_str().expect("next_run_at 是字符串");
    let parsed = chrono::DateTime::parse_from_rfc3339(next).expect("next_run_at 可解析");
    assert!(parsed > chrono::Utc::now(), "next_run_at 应在未来: {next}");
    assert_eq!(row["last_run_status"], "failed");
    // 订阅者是权威值：有就带出来（空数组也一样）。
    assert_eq!(row["subscribers"].as_array().map(Vec::len), Some(1));
    assert_eq!(row["subscribers"][0]["user_type"], "member");
    assert_eq!(row["subscribers"][0]["user_id"], viewer.to_string());
    assert_eq!(row["can_write"], json!(true));
    assert!(
        row.get("can_manage_access").is_none(),
        "列表不该有 can_manage_access: {row}"
    );

    // 无斜杠别名是同一 handler（不是 307）。
    let (status, body) = call(&app, "GET", "/api/autopilots", ws, owner, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);
    assert_eq!(first_id(&body), Some(active));

    // `?status=archived` 精确过滤；没有白名单校验（未知状态 → 空列表而不是 400）。
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots?status=archived",
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);
    assert_eq!(first_id(&body), Some(archived));
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots?status=nonsense",
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "未知 status 不是 400: {body}");
    assert_eq!(body["total"], 0);
    // `?status=`（空串）与缺省同义。
    let (status, body) = call(&app, "GET", "/api/autopilots?status=", ws, owner, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);

    cleanup(&pool, ws, &[owner, viewer]).await;
}

/// `can_write` 的三条腿：role（owner/admin）、创建者（**仅当 `created_by_type='member'`**）、
/// 协作者。列表上还要验证：协作者集合是「按人取一次」而不是按行查。
#[tokio::test]
#[ignore]
async fn list_can_write_covers_role_creator_and_collaborator() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip list_can_write_covers_role_creator_and_collaborator: no MULTICA_TEST_DATABASE_URL");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let admin = seed_user(&pool, ws, "admin").await;
    let plain = seed_user(&pool, ws, "member").await;

    // 三个自动机：owner 建的（角色腿命中 admin/owner）、agent 建的（创建者腿**不**命中）、
    // 别人建的（plain 只能靠协作者腿）。
    let by_owner = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let by_agent = seed_autopilot(&pool, ws, "active", "agent", plain).await;
    let other = seed_autopilot(&pool, ws, "active", "member", owner).await;
    seed_collaborator(&pool, other, plain, owner).await;

    let (status, body) = call(&app, "GET", "/api/autopilots/", ws, admin, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 3);
    for row in body["autopilots"].as_array().unwrap() {
        assert_eq!(row["can_write"], json!(true), "admin 对全部行可写: {row}");
    }

    let (status, body) = call(&app, "GET", "/api/autopilots/", ws, plain, None).await;
    assert_eq!(status, 200);
    let by_id = |id: Uuid| -> serde_json::Value {
        body["autopilots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id.to_string())
            .cloned()
            .expect("row present")
    };
    assert_eq!(by_id(by_owner)["can_write"], json!(false));
    assert_eq!(
        by_id(by_agent)["can_write"],
        json!(false),
        "`created_by_type='agent'` 时创建者腿不生效（上游 autopilotWriteByOwnership）"
    );
    assert_eq!(by_id(other)["can_write"], json!(true), "协作者可写");

    cleanup(&pool, ws, &[owner, admin, plain]).await;
}

/// 详情：`autopilot` + `triggers` + `collaborators` 一次取齐；写者拿得到 webhook 凭据。
#[tokio::test]
#[ignore]
async fn detail_returns_triggers_collaborators_and_credentials_for_writer() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip detail_returns_triggers_collaborators_and_credentials_for_writer: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let collaborator = seed_user(&pool, ws, "member").await;

    let ap = seed_autopilot(&pool, ws, "active", "member", owner).await;
    // token 是**全局唯一**列（`idx_autopilot_trigger_webhook_token`）⇒ 每次生成新的。
    let token = format!("tok_lum1564_w{}", Uuid::new_v4().simple());
    let trigger = seed_webhook_trigger(
        &pool,
        ap,
        &token,
        Some("abcd1234wxyz"),
        Some(r#"[{"event":"issues","actions":["opened"]}]"#),
    )
    .await;
    seed_subscriber(&pool, ap, collaborator).await;
    seed_collaborator(&pool, ap, collaborator, owner).await;

    let uri = format!("/api/autopilots/{ap}/");
    let (status, body) = call(&app, "GET", &uri, ws, owner, None).await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["autopilot"]["id"], ap.to_string());
    assert_eq!(body["autopilot"]["can_write"], json!(true));
    assert_eq!(body["autopilot"]["can_manage_access"], json!(true));
    assert_eq!(
        body["autopilot"]["subscribers"].as_array().map(Vec::len),
        Some(1)
    );

    let trig = &body["triggers"][0];
    assert_eq!(trig["id"], trigger.to_string());
    assert_eq!(trig["kind"], "webhook");
    assert_eq!(trig["enabled"], json!(true));
    assert_eq!(trig["webhook_token"], token.as_str());
    assert_eq!(
        trig["webhook_path"],
        format!("/api/webhooks/autopilots/{token}")
    );
    // `MULTICA_PUBLIC_URL` 未配置 ⇒ `webhook_url` 是显式 null（上游 `*string` 无 omitempty）。
    assert!(trig["webhook_url"].is_null(), "trigger={trig}");
    assert_eq!(trig["provider"], "github");
    assert_eq!(trig["has_signing_secret"], json!(true));
    assert_eq!(trig["signing_secret_hint"], "wxyz");
    assert_eq!(
        trig["event_filters"],
        json!([{ "event": "issues", "actions": ["opened"] }])
    );

    assert_eq!(
        body["collaborators"][0]["user_id"],
        collaborator.to_string()
    );
    assert_eq!(body["collaborators"][0]["granted_by"], owner.to_string());

    // 无斜杠别名同 handler。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{ap}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["autopilot"]["id"], ap.to_string());

    cleanup(&pool, ws, &[owner, collaborator]).await;
}

/// 非写者（普通成员、非创建者、非协作者）：凭据三件套必须被抹掉，但
/// `has_signing_secret` / `signing_secret_hint` **保留**（hint 不是凭据），
/// 且 `can_write=false` / `can_manage_access=false`（显式 false，不是省略）。
#[tokio::test]
#[ignore]
async fn detail_redacts_webhook_credentials_for_non_writer() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip detail_redacts_webhook_credentials_for_non_writer: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let plain = seed_user(&pool, ws, "member").await;

    let ap = seed_autopilot(&pool, ws, "paused", "member", owner).await;
    let signed = seed_webhook_trigger(
        &pool,
        ap,
        &format!("tok_lum1564_r{}", Uuid::new_v4().simple()),
        Some("abcd1234wxyz"),
        None,
    )
    .await;
    // 解不开的 `event_filters`（这里是 JSON 字符串而不是数组）⇒ 上游丢掉该字段，
    // 而不是把整个详情打成 500。
    let broken = seed_webhook_trigger(
        &pool,
        ap,
        &format!("tok_lum1564_b{}", Uuid::new_v4().simple()),
        None,
        Some(r#""not-an-array""#),
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{ap}/"),
        ws,
        plain,
        None,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["autopilot"]["can_write"], json!(false));
    assert_eq!(body["autopilot"]["can_manage_access"], json!(false));

    let triggers = body["triggers"].as_array().expect("triggers 是数组");
    assert_eq!(triggers.len(), 2);
    for trig in triggers {
        assert!(trig["webhook_token"].is_null(), "凭据必须抹除: {trig}");
        assert!(trig["webhook_path"].is_null(), "凭据必须抹除: {trig}");
        assert!(trig["webhook_url"].is_null(), "凭据必须抹除: {trig}");
    }
    let first = triggers
        .iter()
        .find(|t| t["id"] == signed.to_string())
        .expect("signed trigger present");
    assert_eq!(first["has_signing_secret"], json!(true), "hint 不是凭据");
    assert_eq!(first["signing_secret_hint"], "wxyz");
    // 坏 event_filters 被丢掉（`omitempty` 语义 = 字段缺席）。
    let broken_row = body["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == broken.to_string())
        .expect("broken trigger present");
    assert!(
        broken_row.get("event_filters").is_none(),
        "坏过滤器要丢掉: {broken_row}"
    );
    assert_eq!(broken_row["has_signing_secret"], json!(false));
    assert!(broken_row["signing_secret_hint"].is_null());

    // 协作者可以写，但**不能**改授权（`can_manage_access` 不含协作者腿）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{ap}/"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["autopilot"]["can_write"], json!(true));
    assert_eq!(body["autopilot"]["can_manage_access"], json!(true));

    cleanup(&pool, ws, &[owner, plain]).await;
}

/// 空工作区 / 没有订阅者的行：`subscribers` 是 `[]`（MUL-6680），`trigger_kinds` 等三个
/// 列表专属键**缺席**（`omitempty`）。
#[tokio::test]
#[ignore]
async fn list_omits_empty_derived_columns_but_keeps_subscribers_array() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip list_omits_empty_derived_columns_but_keeps_subscribers_array: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let ap = seed_autopilot(&pool, ws, "active", "member", owner).await;

    let (status, body) = call(&app, "GET", "/api/autopilots/", ws, owner, None).await;
    assert_eq!(status, 200);
    let row = &body["autopilots"][0];
    assert_eq!(row["id"], ap.to_string());
    assert_eq!(row["subscribers"], json!([]));
    assert!(
        row.get("trigger_kinds").is_none(),
        "无启用触发器 ⇒ 缺席: {row}"
    );
    assert!(
        row.get("next_run_at").is_none(),
        "无 schedule ⇒ 缺席: {row}"
    );
    assert!(
        row.get("last_run_status").is_none(),
        "从没跑过 ⇒ 缺席: {row}"
    );

    // 未知 id → 404（不是 500）；错误体仍是本仓嵌套形状。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{}/", Uuid::new_v4()),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert!(!err_message(&body).is_empty());

    cleanup(&pool, ws, &[owner]).await;
}
