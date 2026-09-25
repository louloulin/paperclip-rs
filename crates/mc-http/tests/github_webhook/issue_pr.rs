//! `GET /api/issues/:id/pull-requests` 的端到端用例（M8-4 的 `DoD`：501 → 真实现且
//! **路由仍在**；越权收窄；页面访问触发刷新）。
//!
//! ⚠️ 每条用例都持有 [`PORT_LOCK`]（见 `support.rs`）。

use axum::http::StatusCode;
use mc_http::routes::github::webhook::{reset_pr_refresh_port, set_pr_refresh_port};
use serde_json::json;
use uuid::Uuid;

use super::support::*;

/// 用一帧真实 webhook 把 PR 行镜像出来（不经 repo 直插，链路与生产一致）。
async fn mirror_one_pr(
    app: &axum::Router,
    prefix: &str,
    number: i32,
    installation_id: i64,
) -> serde_json::Value {
    let frame = json!({
        "action": "opened",
        "pull_request": {
            "number": number,
            "html_url": format!("https://github.com/acme/api/pull/{number}"),
            "title": format!("{prefix}-7: do it"),
            "body": "",
            "state": "open", "draft": false, "merged": false,
            "created_at": "2026-09-25T06:00:00Z",
            "updated_at": "2026-09-25T06:00:00Z",
            "mergeable_state": "clean",
            "additions": 1, "deletions": 2, "changed_files": 3,
            "head": { "ref": "feat/read", "sha": "sha-read" },
            "user": { "login": "dev", "avatar_url": "https://avatars/dev.png" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    let (status, body) = call(
        app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    frame
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn read_face_returns_the_linked_card_and_triggers_a_page_view_refresh() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    let issue_id = seed_issue(
        &pool,
        workspace_id,
        user_id,
        7,
        &format!("{prefix}-7"),
        "todo",
    )
    .await;

    // 端口 `enabled() == true`（生产里 = 配了 App 私钥）但**还没有快照行** ⇒ `snapshot_available`
    // 仍为 `false`（四个条件缺一即假），页面访问照样按照有节制入队。
    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    mirror_one_pr(&app, &prefix, 4, installation_id).await;

    let uri = format!("/api/issues/{issue_id}/pull-requests");
    let (status, body) = call(
        &app,
        get_with_workspace(&uri, Some(user_id), Some(workspace_id)),
    )
    .await;
    // 「路由仍在」：不是 501（anchor 期的占位已被真实现替换）。
    assert_ne!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(status, StatusCode::OK, "{body}");

    let cards = body["pull_requests"].as_array().expect("array").clone();
    assert_eq!(cards.len(), 1, "{body}");
    let card = &cards[0];
    assert_eq!(card["provider"], "github");
    assert_eq!(card["number"], 4);
    assert_eq!(card["state"], "open");
    assert_eq!(card["repo_owner"], "acme");
    assert_eq!(card["repo_name"], "api");
    assert_eq!(card["branch"], "feat/read");
    assert_eq!(card["author_login"], "dev");
    // ⚠️ `mergeable_state` 是 `null`：`opened` 这类**会变状态**的事件必须抹掉旧裁决
    //（上游 `derivePRMergeableState` 的 clear 分支 —— GitHub 异步重算，载荷里的值可能是
    // 上一个 head 的结论）。写入路径由 `mirror.rs` 的单测钉住。
    assert!(card["mergeable_state"].is_null());
    assert_eq!(card["additions"], 1);
    assert_eq!(card["changed_files"], 3);
    // 端口 `enabled() == true` 但**还没有快照** ⇒ 没有快照 ⇒ snapshot_available=false 且 CI 区为空。
    assert_eq!(card["snapshot_available"], false);
    assert!(card["mergeable"].is_null());
    assert!(card["checks_rollup"].is_null());
    assert_eq!(card["checks_conclusion"], serde_json::Value::Null);
    assert_eq!(card["failed_check_names"], json!([]));
    assert_eq!(card["snapshot_stale"], false);
    // 页面访问触发了**一次**有节制的入队。
    let view = port.view_enqueued();
    assert_eq!(view.len(), 1, "{view:?}");
    assert_eq!(view[0].pr_number, 4);
    assert_eq!(view[0].head_sha.as_deref(), Some("sha-read"));

    // 快照落地之后（模拟 M8-5 的管道写过行）⇒ `snapshot_available=true` 且 GraphQL 字段
    // 小写化出现；`checks_pending` 与 `checks_running` 同值。
    sqlx::query(
        "UPDATE github_pull_request SET snapshot_head_sha = head_sha, snapshot_fetched_at = now(), \
             api_mergeable = 'MERGEABLE', api_merge_state_status = 'CLEAN', \
             checks_rollup_state = 'SUCCESS' \
         WHERE workspace_id = $1 AND pr_number = 4",
    )
    .bind(workspace_id)
    .execute(&pool)
    .await
    .expect("simulate snapshot");
    let (status, body) = call(
        &app,
        get_with_workspace(&uri, Some(user_id), Some(workspace_id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let card = &body["pull_requests"][0];
    assert_eq!(card["snapshot_available"], true);
    assert_eq!(card["mergeable"], "mergeable");
    assert_eq!(card["merge_state_status"], "clean");
    assert_eq!(card["checks_rollup"], "success");
    assert_eq!(card["checks_conclusion"], "passed");
    assert_eq!(card["checks_pending"], card["checks_running"]);

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn non_member_gets_404_and_a_foreign_issue_is_invisible() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let issue_id = seed_issue(
        &pool,
        workspace_id,
        user_id,
        7,
        &format!("{prefix}-7"),
        "todo",
    )
    .await;
    let outro = Uuid::new_v4();
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    let uri = format!("/api/issues/{issue_id}/pull-requests");

    // ① 非成员 ⇒ 404 `workspace`（上游 `RequireWorkspaceMemberFromURL`）。
    let (status, body) = call(
        &app,
        get_with_workspace(&uri, Some(outro), Some(workspace_id)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("workspace"),
        "错误体应是本仓嵌套 envelope 的 workspace 未找到：{body}"
    );

    // ② 缺 workspace 选择器 ⇒ 400（本仓的 workspace 解析契约：header 或 `?workspace_id=`）。
    let (status, _) = call(&app, get_with_workspace(&uri, Some(user_id), None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ③ 另一个 workspace 里的 issue id ⇒ 404（查询按 workspace 收窄）。
    let (other_ws, other_user, other_prefix) = seed_workspace(&pool, "owner").await;
    let other_issue = seed_issue(
        &pool,
        other_ws,
        other_user,
        1,
        &format!("{other_prefix}-1"),
        "todo",
    )
    .await;
    let (status, _) = call(
        &app,
        get_with_workspace(
            &format!("/api/issues/{other_issue}/pull-requests"),
            Some(user_id),
            Some(workspace_id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, workspace_id, &[user_id, outro]).await;
    cleanup(&pool, other_ws, &[other_user]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn vcs_pull_requests_merge_into_the_same_list_without_snapshot_available() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let issue_id = seed_issue(
        &pool,
        workspace_id,
        user_id,
        7,
        &format!("{prefix}-7"),
        "todo",
    )
    .await;

    // 直插一行 `vcs_pull_request` + 关联账（M8-2 的读面；本片只负责把它并进同一张列表）。
    let connection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO vcs_connection(workspace_id, provider, instance_url, account_login, \
             access_token_encrypted, webhook_secret_encrypted) \
         VALUES ($1, 'gitlab', 'https://gitlab.example', 'acme', $2, $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(vec![0u8; 8])
    .bind(vec![0u8; 8])
    .fetch_one(&pool)
    .await
    .expect("insert connection");
    let pr_id: Uuid = sqlx::query_scalar(
        "INSERT INTO vcs_pull_request(workspace_id, connection_id, provider, repo_owner, repo_name, \
             pr_number, title, state, html_url, head_sha, pr_created_at, pr_updated_at) \
         VALUES ($1, $2, 'gitlab', 'acme', 'api', 9, 'mr', 'open', 'https://gitlab.example/9', 'sha', \
             '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z') RETURNING id",
    )
    .bind(workspace_id)
    .bind(connection_id)
    .fetch_one(&pool)
    .await
    .expect("insert vcs pr");
    sqlx::query(
        "INSERT INTO issue_vcs_pull_request(issue_id, pull_request_id, close_intent) VALUES ($1, $2, false)",
    )
    .bind(issue_id)
    .bind(pr_id)
    .execute(&pool)
    .await
    .expect("link vcs pr");

    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    let (status, body) = call(
        &app,
        get_with_workspace(
            &format!("/api/issues/{issue_id}/pull-requests"),
            Some(user_id),
            Some(workspace_id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cards = body["pull_requests"].as_array().expect("array");
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["provider"], "gitlab");
    assert_eq!(cards[0]["number"], 9);
    assert!(
        cards[0].get("snapshot_available").is_none(),
        "非 GitHub provider ⇒ 该字段缺席（上游 omitempty）"
    );
    assert!(cards[0]["mergeable_state"].is_null());

    cleanup(&pool, workspace_id, &[user_id]).await;
}
