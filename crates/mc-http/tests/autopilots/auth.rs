//! 鉴权与可见性：`/api/autopilots*` 的 401 / 400 / 404 三档（M5-1）。
//!
//! 上游口径（`requireWorkspaceMember`）是「非成员 → **404**，成员但无权 → 403」。
//! 本仓读面只有成员能过，因此对外可见的只有 404 这一档 —— 这条**取代**了 DoD 里
//! 「非成员 403」的写法，理由见 `docs/46-M5-1-READ-FACE.md` §5。
//!
//! 401 来自 `AuthUser` 提取器（M1 dev-mode：缺 `X-Multica-User-Id`）；
//! 400 来自 `resolve_workspace_id`（缺 / 非 UUID 的 `X-Workspace-ID`）与
//! 路径参数非 UUID 的 `parse_uuid`。

use super::support::{
    call, call_no_user, call_no_workspace, cleanup, err_message, seed_autopilot, seed_outsider,
    seed_workspace,
};

const READ_URIS: [&str; 4] = [
    "/api/autopilots/",
    "/api/autopilots/cron-preview",
    "/api/autopilots/usage",
    "/api/autopilots/00000000-0000-0000-0000-000000000000",
];

/// 缺用户头 → 401（四条路由都要）。
#[tokio::test]
#[ignore]
async fn missing_user_header_is_401_on_every_read_route() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip missing_user_header_is_401_on_every_read_route: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    for uri in READ_URIS {
        assert_eq!(call_no_user(&app, uri).await, 401, "uri={uri}");
    }
    let _ = (ws, owner);
    cleanup(&pool, ws, &[owner]).await;
}

/// 有用户头但缺 `X-Workspace-ID` → 400（四条路由都要）。
#[tokio::test]
#[ignore]
async fn missing_workspace_header_is_400_on_every_read_route() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip missing_workspace_header_is_400_on_every_read_route: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    for uri in READ_URIS {
        assert_eq!(call_no_workspace(&app, uri, owner).await, 400, "uri={uri}");
    }

    // 非 UUID 的工作区头也是 400，而不是 500。
    let status = call(&app, "GET", "/api/autopilots/", ws, owner, None)
        .await
        .0;
    assert_eq!(status, 200);
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/autopilots/")
        .header("x-multica-user-id", owner.to_string())
        .header("x-workspace-id", "not-a-uuid")
        .body(axum::body::Body::empty())
        .unwrap();
    let status = tower::ServiceExt::oneshot(app.clone(), request)
        .await
        .expect("router call")
        .status();
    assert_eq!(status, 400);

    cleanup(&pool, ws, &[owner]).await;
}

/// 非成员（跨工作区）→ **404**，不是 403：上游 `requireWorkspaceMember` 用 404 掩盖存在性。
#[tokio::test]
#[ignore]
async fn non_member_gets_404_not_403() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip non_member_gets_404_not_403: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let ap = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let outsider = seed_outsider(&pool).await;

    let (status, body) = call(&app, "GET", "/api/autopilots/", ws, outsider, None).await;
    assert_eq!(status, 404, "{}", err_message(&body));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{ap}/"),
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, 404, "{}", err_message(&body));
    let (status, _) = call(
        &app,
        "GET",
        "/api/autopilots/cron-preview?expr=*+*+*+*+*",
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = call(&app, "GET", "/api/autopilots/usage", ws, outsider, None).await;
    assert_eq!(status, 404);

    cleanup(&pool, ws, &[owner, outsider]).await;
}

/// 另一个工作区的成员看本工作区的 autopilot：同 404（`get_in_workspace` 把跨区当不存在）。
#[tokio::test]
#[ignore]
async fn cross_workspace_id_is_404() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip cross_workspace_id_is_404: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws_a, owner_a) = seed_workspace(&pool, "owner").await;
    let (ws_b, owner_b) = seed_workspace(&pool, "owner").await;
    let ap = seed_autopilot(&pool, ws_a, "active", "member", owner_a).await;

    // B 的成员用 B 的工作区头请求 A 的 autopilot id。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{ap}/"),
        ws_b,
        owner_b,
        None,
    )
    .await;
    assert_eq!(status, 404, "{}", err_message(&body));

    // B 的成员用 **A 的工作区头**：`require_member` 查的是 A 的成员表 ⇒ 404。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{ap}/"),
        ws_a,
        owner_b,
        None,
    )
    .await;
    assert_eq!(status, 404, "{}", err_message(&body));

    cleanup(&pool, ws_a, &[owner_a]).await;
    cleanup(&pool, ws_b, &[owner_b]).await;
}

/// 路径参数非 UUID → 400（`parse_uuid`），且**先于**任何库查询。
#[tokio::test]
#[ignore]
async fn non_uuid_path_id_is_400() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip non_uuid_path_id_is_400: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    for id in ["not-a-uuid", "123", "%20"] {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/api/autopilots/{id}/"),
            ws,
            owner,
            None,
        )
        .await;
        assert_eq!(status, 400, "id={id} body={body}");
        assert!(!err_message(&body).is_empty());
    }

    cleanup(&pool, ws, &[owner]).await;
}
