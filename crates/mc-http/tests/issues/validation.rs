//! `/api/issues*` 端到端测试：validation 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{body_json, build_state_with_db, cleanup, connect, req, seed_workspace};

/// 7) LUM-1410：`POST /api/issues` 的 `(assignee_type, assignee_id)` **存在性**校验与
/// `attachment_ids` 形态校验（上游 `validateAssigneePair` / `parseUUIDSliceOrBadRequest`）。
///
/// 对应 golden fixture `contracts/golden/issues/021..025`：真库层期望 400，此前实测 201。
/// 同步覆盖 `PUT /api/issues/:id`（上游同一把校验，`issue.go:3814`）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn create_issue_validates_assignee_and_attachment_ids() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let issue_count = |pool: sqlx::PgPool, ws: Uuid| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM issue WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    // 本 workspace 里不存在的目标 id（fixture 021/022 用的就是全零 UUID）
    let ghost = Uuid::nil();

    let post = |body: serde_json::Value| {
        let app = app.clone();
        async move {
            app.oneshot(req("POST", "/api/issues", ws, user, Some(body)))
                .await
                .unwrap()
        }
    };

    // 1) 不存在的 member → 400（上游 `getMemberByUserAndWorkspace` 未命中）
    let res = post(json!({
        "title": "Ghost member assignee",
        "assignee_type": "member",
        "assignee_id": ghost.to_string(),
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "validation_error"
    );

    // 2) 不存在的 agent → 400（上游此前是 403 "agent not found"，现为 400）
    let res = post(json!({
        "title": "Ghost agent assignee",
        "assignee_type": "agent",
        "assignee_id": ghost.to_string(),
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 2b) 不存在的 squad → 400（本仓同类校验；无 leader 的 squad 也拒）
    let res = post(json!({
        "title": "Ghost squad assignee",
        "assignee_type": "squad",
        "assignee_id": ghost.to_string(),
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 3) 只给一半 → 400（fixture 023 / 024：这两个在修复前就是 400，作为回归锚点保留）
    for body in [
        json!({"title": "Lone assignee_type", "assignee_type": "member"}),
        json!({"title": "Lone assignee_id", "assignee_id": "not-a-uuid"}),
    ] {
        let res = post(body).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // 4) 两半都给但 id 形态非法 → 400（修复前这条会 201：只查了"成对"没查形态）
    let res = post(json!({
        "title": "Malformed assignee_id",
        "assignee_type": "member",
        "assignee_id": "not-a-uuid",
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 5) `attachment_ids` 含非 UUID → 400，且**写库之前**（fixture 025 的
    //    "BeforeWrite"：创建前后的 issue 计数必须相等）
    let before = issue_count(pool.clone(), ws).await;
    let res = post(json!({
        "title": "Malformed attachment issue",
        "attachment_ids": ["not-a-uuid"],
    }))
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "validation_error"
    );
    assert_eq!(
        issue_count(pool.clone(), ws).await,
        before,
        "400 必须发生在写库之前"
    );

    // 6) 正向对照：真实 member + 合法 attachment UUID → 201（附件只校验形态、不绑定）
    let res = post(json!({
        "title": "Valid member assignee",
        "assignee_type": "member",
        "assignee_id": user.to_string(),
        "attachment_ids": [Uuid::new_v4().to_string()],
    }))
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let created = body_json(res.into_body()).await;
    // 上游入参写作 `member`，本仓落库/回显统一 `user`（见 docs/11 §5）
    assert_eq!(created["assignee_type"], "user");
    assert_eq!(created["assignee_id"], user.to_string());
    let issue_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(issue_count(pool.clone(), ws).await, before + 1);

    // 6b) 形态非法的 `assignee_id` 在 `null` 清除路径上不参与校验（三态补丁）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"assignee_type": null, "assignee_id": null})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res.into_body()).await["assignee_id"].is_null());

    // 7) PUT 路径同把校验：不存在的 member → 400（上游 `UpdateIssue` L3814）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"assignee_type": "member", "assignee_id": ghost.to_string()})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 7b) PUT 指向真实的 member → 200（同一把校验不能误杀合法指派）
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"assignee_type": "member", "assignee_id": user.to_string()})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        body_json(res.into_body()).await["assignee_id"],
        user.to_string()
    );

    cleanup(&pool, ws, user).await;
}
