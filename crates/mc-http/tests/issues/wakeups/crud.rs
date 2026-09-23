//! ①–⑤：四种 kind 建/读、`mode` 正交、PUT upsert、disable→enable、instruction 编辑。

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace};

use super::support::{
    create_event_wakeup, list_wakeups, post_wakeup, seed_agent, seed_runtime, seed_wakeup_world,
};
/// 1) 四种 kind 各自建一条 + 列表回读（`next_fire_at` 与 `mode` 的形态一次看全）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn wakeup_kinds_create_and_readback() {
    let Some((app, pool, ws, user, issue_id, agent)) = seed_wakeup_world().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let agent = agent.to_string();

    let (status, event) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({
            "agent_id": agent,
            "instruction": "  event one  ",
            "kind": "event",
            "event_types": ["comment.created", "issue.updated"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{event}");
    assert_eq!(event["kind"], "event");
    assert_eq!(event["mode"], "once", "event 的 mode 默认 once");
    assert_eq!(event["enabled"], true);
    assert_eq!(event["revision"], 1);
    assert_eq!(event["instruction"], "event one", "instruction 两端 trim");
    assert_eq!(event["next_fire_at"], Value::Null, "event 无排程");
    assert_eq!(event["created_by"], user.to_string());
    assert_eq!(event["issue_id"], issue_id);
    assert_eq!(event["timezone"], "UTC");
    assert_eq!(event["event_types"].as_array().unwrap().len(), 2);

    let (status, at) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({"agent_id": agent, "instruction": "at one", "kind": "at", "after_seconds": 600}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{at}");
    assert_eq!(at["kind"], "at");
    assert_eq!(at["mode"], "once");
    assert!(at["next_fire_at"].is_string(), "after_seconds 折算成绝对时间");

    let (status, every) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({"agent_id": agent, "instruction": "every one", "kind": "every", "interval_seconds": 3600}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{every}");
    assert_eq!(every["kind"], "every");
    assert_eq!(every["mode"], "continuous", "every 的 mode 默认 continuous");
    assert_eq!(every["interval_seconds"], 3600);
    assert!(every["next_fire_at"].is_string());

    let (status, cron) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({"agent_id": agent, "instruction": "cron one", "kind": "cron",
               "cron_expression": "0 9 * * *", "timezone": "Asia/Shanghai"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{cron}");
    assert_eq!(cron["kind"], "cron");
    assert_eq!(cron["mode"], "continuous");
    assert_eq!(cron["cron_expression"], "0 9 * * *");
    assert_eq!(cron["timezone"], "Asia/Shanghai");
    assert!(cron["next_fire_at"].is_string(), "M5-6 自带 5 字段 cron 解析");
    assert!(cron["next_fire_at"].as_str().unwrap() > "2020-01-01");

    let rows = list_wakeups(&app, ws, user, &issue_id).await;
    assert_eq!(rows.len(), 4, "{rows:?}");
    let mut kinds = rows
        .iter()
        .map(|row| row["kind"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    kinds.sort();
    assert_eq!(kinds, vec!["at", "cron", "event", "every"]);
    let instructions = rows
        .iter()
        .map(|row| row["instruction"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    for expected in ["at one", "cron one", "event one", "every one"] {
        assert!(instructions.contains(&expected.to_string()), "{rows:?}");
    }
    // 列表视图是掩码后的展示形态（`agent_name` 冗余列），与会话行同源。
    assert!(rows.iter().all(|row| row["agent_name"].is_string()));

    cleanup(&pool, ws, user).await;
}

/// 2) `mode` 与 `kind` **正交**：event 也能 continuous；显式 mode 覆盖默认。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn wakeup_mode_is_orthogonal_to_kind() {
    let Some((app, pool, ws, user, issue_id, agent)) = seed_wakeup_world().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (status, body) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({
            "agent_id": agent.to_string(),
            "instruction": "watch forever",
            "kind": "event",
            "mode": "continuous",
            "event_types": ["comment.created"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["kind"], "event");
    assert_eq!(body["mode"], "continuous");

    // 非法 mode ⇒ 400（上游 `mode must be once or continuous`）。
    let (status, body) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({
            "agent_id": agent.to_string(),
            "instruction": "bad mode",
            "kind": "event",
            "mode": "sometimes",
            "event_types": ["comment.created"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "validation_error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("mode must be once or continuous"));

    // 非法 kind ⇒ 400（上游 `kind must be event, at, every or cron`）。
    let (status, body) = post_wakeup(
        &app,
        ws,
        user,
        &issue_id,
        json!({"agent_id": agent.to_string(), "instruction": "bad kind", "kind": "later"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("kind must be event, at, every or cron"));

    cleanup(&pool, ws, user).await;
}

/// 3) PUT upsert 复用 POST 的服务路径；`revision` 递增并作废旧 revision 的收据。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn wakeup_upsert_bumps_revision_and_drops_receipts() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let runtime = seed_runtime(&pool, ws).await;
    let agent = seed_agent(&pool, ws, runtime, user).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone())
        .with_state(state.clone())
        .with_state(());
    let issue = create_issue(&app, ws, user, json!({"title": "upsert"})).await;
    let issue_id = issue["id"].as_str().unwrap().to_string();

    let created = create_event_wakeup(&app, ws, user, &issue_id, agent, "first").await;
    let wakeup_id = created["id"].as_str().unwrap().to_string();
    let revision = created["revision"].as_i64().unwrap();
    assert_eq!(revision, 1);

    // 手工塞一条属于当前 revision 的待处理收据：upsert 必须把它作废（`processed_at` 落值）。
    sqlx::query(
        "INSERT INTO issue_wakeup_receipt(id, wakeup_id, revision, event_key, event_type, payload) \
         VALUES ($1, $2, $3, 'evt-1', 'comment.created', '{}'::jsonb)",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::parse_str(&wakeup_id).unwrap())
    .bind(revision)
    .execute(&pool)
    .await
    .expect("insert receipt");
    let pending = |wakeup_id: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM issue_wakeup_receipt WHERE wakeup_id = $1 AND processed_at IS NULL",
            )
            .bind(Uuid::parse_str(&wakeup_id).unwrap())
            .fetch_one(&pool)
            .await
            .expect("count pending receipts")
        }
    };
    assert_eq!(pending(wakeup_id.clone()).await, 1);

    // PUT 到同一个 id = upsert（上游 `CreateIssueWakeup` 复用同一 handler）。
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}"),
            ws,
            user,
            Some(json!({
                "agent_id": agent.to_string(),
                "instruction": "second",
                "kind": "event",
                "event_types": ["comment.created"],
            })),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let updated = body_json(res.into_body()).await;
    assert_eq!(updated["id"], wakeup_id, "upsert 更新同一行，不新建");
    assert_eq!(updated["revision"], revision + 1, "revision 必须递增");
    assert_eq!(updated["instruction"], "second");
    assert_eq!(updated["enabled"], true);

    let receipts = pending(wakeup_id.clone()).await;
    assert_eq!(receipts, 0, "旧 revision 的收据必须被作废（不能再被认领）");

    // 列表里仍只有一行（upsert 不是 create）。
    let rows = list_wakeups(&app, ws, user, &issue_id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["revision"], revision + 1);

    // PUT 到一个不属于本 issue 的 id ⇒ 404 `wakeup`。
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/wakeups/{}", Uuid::new_v4()),
            ws,
            user,
            Some(json!({
                "agent_id": agent.to_string(),
                "instruction": "ghost",
                "kind": "event",
                "event_types": ["comment.created"],
            })),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["error"]["code"], "not_found");
    assert!(body["error"]["message"].as_str().unwrap().contains("wakeup"));

    // 非法 wakeup id ⇒ 400 `invalid wakeup id`（上游 `parseUUIDOrBadRequest`）。
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/wakeups/not-a-uuid"),
            ws,
            user,
            Some(json!({"agent_id": agent.to_string(), "instruction": "x", "kind": "event",
                        "event_types": ["comment.created"]})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["error"]["code"], "validation_error");
    // 本仓 `ApiError` 的信封是「`<code 标签>: <message>`」，上游原文保持为后缀（记录文件里有口径表）。
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("invalid wakeup id"),
        "{body}"
    );

    cleanup(&pool, ws, user).await;
}

/// 4) `disable` 停用（含待处理收据作废）→ `enable` 带 revision 重新启用。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn wakeup_disable_then_enable() {
    let Some((app, pool, ws, user, issue_id, agent)) = seed_wakeup_world().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let created = create_event_wakeup(&app, ws, user, &issue_id, agent, "toggle").await;
    let wakeup_id = created["id"].as_str().unwrap().to_string();
    let revision = created["revision"].as_i64().unwrap();

    // disable：无 body，200，行里 enabled=false + disabled_at 落值。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/disable"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let disabled = body_json(res.into_body()).await;
    assert_eq!(disabled["enabled"], false);
    assert!(disabled["disabled_at"].is_string());
    assert_eq!(disabled["revision"], revision, "disable 不动 revision");

    // 停用后仍在列表里（默认 scope 不含 disabled ⇒ 这里用 issues 面列表，它不过滤 enabled）。
    let rows = list_wakeups(&app, ws, user, &issue_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["enabled"], false);

    // enable：必须带 revision；成功后 revision 递增、重新生效。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/enable"),
            ws,
            user,
            Some(json!({"revision": revision})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let enabled = body_json(res.into_body()).await;
    assert_eq!(enabled["enabled"], true);
    assert!(enabled["disabled_at"].is_null(), "重新启用要清掉 disabled_at");
    assert_eq!(enabled["revision"], revision + 1);
    assert_eq!(enabled["instruction"], "toggle", "enable 不回传配置，从库里恢复");

    // revision 缺失 ⇒ 400（上游 `revision is required`）。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/enable"),
            ws,
            user,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res.into_body()).await;
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("revision is required"));

    // 陈旧的 revision ⇒ 409 冲突（乐观并发）。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/enable"),
            ws,
            user,
            Some(json!({"revision": revision})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["error"]["code"], "conflict");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("wakeup changed; refresh and retry"),
        "{body}"
    );

    // 体上限 1024（上游 `MaxBytesReader`）——超限是 400 而不是 413。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/enable"),
            ws,
            user,
            Some(json!({"revision": revision + 1, "rearm": "x".repeat(2000)})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(res.into_body()).await["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("invalid enable body")
    );

    cleanup(&pool, ws, user).await;
}

/// 5) `PATCH .../instruction`：只改指令（204，revision 不变），陈旧回显 ⇒ 409。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn wakeup_instruction_edit() {
    let Some((app, pool, ws, user, issue_id, agent)) = seed_wakeup_world().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let created = create_event_wakeup(&app, ws, user, &issue_id, agent, "old text").await;
    let wakeup_id = created["id"].as_str().unwrap().to_string();
    let revision = created["revision"].as_i64().unwrap();

    let patch = |body: Value| {
        req(
            "PATCH",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/instruction"),
            ws,
            user,
            Some(body),
        )
    };

    // 陈旧 revision ⇒ 409（`revision` 与 `expected_instruction` 任一不符都冲突）。
    let res = app
        .clone()
        .oneshot(patch(json!({"instruction": "new text", "expected_instruction": "old text",
                              "revision": revision + 5})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["error"]["code"], "conflict");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("wakeup changed; refresh and retry"),
        "{body}"
    );

    // 正常路径：204 无 body。
    let res = app
        .clone()
        .oneshot(patch(json!({"instruction": "new text", "expected_instruction": "old text",
                              "revision": revision})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert!(body_json(res.into_body()).await.is_null());

    let rows = list_wakeups(&app, ws, user, &issue_id).await;
    assert_eq!(rows[0]["instruction"], "new text");
    assert_eq!(rows[0]["revision"], revision, "改指令不 bump revision");
    assert_eq!(rows[0]["enabled"], true, "改指令不影响启用状态");

    // 指令为空 ⇒ 400（上游 `instruction must be 1–12000 bytes and revision is required`）。
    let res = app
        .clone()
        .oneshot(patch(json!({"instruction": "   ", "expected_instruction": "new text",
                              "revision": revision})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res.into_body()).await;
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("instruction must be 1"));

    // 体上限 160000（上游 `MaxBytesReader`）⇒ 400 `invalid instruction body`。
    let res = app
        .clone()
        .oneshot(patch(json!({"instruction": "x".repeat(200_000), "expected_instruction": "",
                              "revision": revision})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(res.into_body()).await["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("invalid instruction body")
    );

    // 未知字段 ⇒ 400 `invalid instruction body`（上游 `DisallowUnknownFields`）。
    let res = app
        .clone()
        .oneshot(patch(json!({"instruction": "y", "expected_instruction": "new text",
                              "revision": revision, "bogus": 1})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(res.into_body()).await["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("invalid instruction body")
    );

    cleanup(&pool, ws, user).await;
}

