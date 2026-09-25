//! `POST /api/webhooks/github` 的端到端用例（M8-4 的 `DoD`：三族事件 / 幂等 / 两个反例 /
//! 离线替身端到端四条断言链）。
//!
//! ⚠️ 每条用例都持有 [`PORT_LOCK`]：快照端口是进程全局的注入槽（见 `support.rs` 的说明）。

use axum::http::StatusCode;
use mc_core::Id;
use mc_http::routes::github::webhook::{reset_pr_refresh_port, set_pr_refresh_port};
use serde_json::json;
use uuid::Uuid;

use super::support::*;

/// 计数一个 workspace 的关联账行数（`(issue_id, close_intent)`，按 issue **号**排序）。
async fn links_of(pool: &sqlx::PgPool, workspace_id: Uuid) -> Vec<(Uuid, bool)> {
    // 排序键用 `issue.number` 而不是 `issue_id`：UUID 是随机的，`ORDER BY ipr.issue_id`
    // 给出的是任意顺序，用例就没法逐位置断言。
    sqlx::query_as(
        "SELECT ipr.issue_id, ipr.close_intent FROM issue_pull_request ipr \
         JOIN github_pull_request pr ON pr.id = ipr.pull_request_id \
         JOIN issue i ON i.id = ipr.issue_id \
         WHERE pr.workspace_id = $1 ORDER BY i.number",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .expect("links")
}

async fn pr_count(pool: &sqlx::PgPool, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM github_pull_request WHERE workspace_id = $1")
        .bind(workspace_id)
        .fetch_one(pool)
        .await
        .expect("pr count")
}

async fn issue_status(pool: &sqlx::PgPool, issue_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM issue WHERE id = $1")
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("issue status")
}

// ---------------------------------------------------------------------------
// 反例一：验签失败 ⇒ 401 且**不落库**
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn bad_signature_is_401_and_persists_nothing() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, _) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));

    // ① 用**错**密钥签的真实帧 ⇒ 401。
    let wrong = signed_webhook_request(
        Some("not-the-secret"),
        "pull_request",
        &pull_request_frame("opened", 1, "t", "", false, false, installation_id),
    );
    let (status, body) = call(&app, wrong).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid signature");

    // ② 完全不带签名头 ⇒ 也是 401。
    let (status, _) = call(
        &app,
        signed_webhook_request(
            None,
            "pull_request",
            &pull_request_frame("opened", 1, "t", "", false, false, installation_id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 两个反例都**不落库**。
    assert_eq!(pr_count(&pool, workspace_id).await, 0);
    assert!(links_of(&pool, workspace_id).await.is_empty());

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn missing_webhook_secret_is_404_rather_than_a_permissive_accept() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, _) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    // 空串与 None 都算「未配置」（上游 `strings.TrimSpace(secret) == ""`）。
    for secret in [None, Some("")] {
        let (app, _state) = app_with(db.clone(), webhook_keys(secret));
        // 即便带一个**合法**签名也不接受：未配置的部署整体拒收。
        let (status, body) = call(
            &app,
            signed_webhook_request(
                Some(WEBHOOK_SECRET),
                "pull_request",
                &pull_request_frame("opened", 1, "t", "", false, false, installation_id),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "secret = {secret:?}");
        assert_eq!(body["error"], "not found");
    }
    assert_eq!(pr_count(&pool, workspace_id).await, 0);

    cleanup(&pool, workspace_id, &[user_id]).await;
}

// ---------------------------------------------------------------------------
// 离线替身端到端：webhook → PR 行 → issue 关联 → 自动关闭 → 快照入队
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn pull_request_webhook_drives_the_whole_chain() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    // `#7` 只被 title 前缀提到（claim、无关闭词）；`#8` 被 `Closes` 声明。
    let issue_seven = seed_issue(
        &pool,
        workspace_id,
        user_id,
        7,
        &format!("{prefix}-7"),
        "todo",
    )
    .await;
    let issue_eight = seed_issue(
        &pool,
        workspace_id,
        user_id,
        8,
        &format!("{prefix}-8"),
        "todo",
    )
    .await;

    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    let mut events = state.realtime.subscribe();

    let frame = json!({
        "action": "closed",
        "pull_request": {
            "number": 12,
            "html_url": "https://github.com/acme/api/pull/12",
            "title": format!("{prefix}-7: do the thing"),
            "body": format!("Closes {prefix}-8"),
            "state": "closed",
            "draft": false,
            "merged": true,
            "merged_at": "2026-09-25T06:00:00Z",
            "created_at": "2026-09-20T06:00:00Z",
            "updated_at": "2026-09-25T06:00:00Z",
            "mergeable_state": "clean",
            "additions": 3, "deletions": 4, "changed_files": 5,
            "head": { "ref": "fix/login", "sha": "deadbeef" },
            "user": { "login": "dev", "avatar_url": "https://avatars/dev.png" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    let (status, body) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body = {body}");

    // ① PR 行：状态归一到 `merged`、head / 统计 / 作者都落库。
    let row: (String, String, i32, i32, i32, Option<String>) = sqlx::query_as(
        "SELECT state, head_sha, additions, deletions, changed_files, author_login \
         FROM github_pull_request WHERE workspace_id = $1 AND repo_owner = 'acme' AND repo_name = 'api' AND pr_number = 12",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("pr row");
    assert_eq!(row.0, "merged");
    assert_eq!(row.1, "deadbeef");
    assert_eq!((row.2, row.3, row.4), (3, 4, 5));
    assert_eq!(row.5.as_deref(), Some("dev"));

    // ② issue 关联：两行；`#7` 是「claim 但无关闭词」⇒ close_intent=false；
    //    `#8` 是「Closes」⇒ close_intent=true。
    let links = links_of(&pool, workspace_id).await;
    assert_eq!(links.len(), 2, "两个 issue 都要建关联账");
    assert_eq!(links[0], (issue_seven, false));
    assert_eq!(links[1], (issue_eight, true));

    // ③ 自动关闭：只有带关闭意图的 `#8` 被推进；`#7` 保持 todo
    //    （上游闸门第 ③ 条：至少一个**合并且带 close_intent** 的 PR）。
    assert_eq!(issue_status(&pool, issue_eight).await, "done");
    assert_eq!(issue_status(&pool, issue_seven).await, "todo");

    // ④ 快照入队：reason=webhook、带 head sha、每个绑定一次。
    let enqueued = port.enqueued();
    assert_eq!(enqueued.len(), 1, "{enqueued:?}");
    assert_eq!(enqueued[0].workspace_id, Id(workspace_id));
    assert_eq!(enqueued[0].repo_owner, "acme");
    assert_eq!(enqueued[0].repo_name, "api");
    assert_eq!(enqueued[0].pr_number, 12);
    assert_eq!(enqueued[0].head_sha.as_deref(), Some("deadbeef"));

    // ⑤ 广播：`pull_request:updated`（带两张关联表的 issue id）+ `issue:updated`。
    let mut kinds = Vec::new();
    while let Ok(envelope) = events.rx.try_recv() {
        kinds.push(envelope.event_type);
    }
    assert!(
        kinds.iter().any(|kind| kind == "pull_request:updated"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "issue:updated"),
        "{kinds:?}"
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn redelivering_the_same_webhook_inserts_one_pr_row_and_keeps_one_link() {
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

    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));

    let frame = json!({
        "action": "opened",
        "pull_request": {
            "number": 1,
            "html_url": "https://github.com/acme/api/pull/1",
            "title": format!("{prefix}-7: start"),
            "state": "open",
            "draft": false, "merged": false,
            "created_at": "2026-09-25T06:00:00Z",
            "updated_at": "2026-09-25T06:00:00Z",
            "head": { "ref": "feat/x", "sha": "sha-1" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    for _ in 0..2 {
        let (status, _) = call(
            &app,
            signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    // 幂等：一行 PR、一行关联账（`ON CONFLICT` upsert）。
    assert_eq!(pr_count(&pool, workspace_id).await, 1);
    let links = links_of(&pool, workspace_id).await;
    assert_eq!(links, vec![(issue_id, false)]);
    // 两帧 ⇒ 两次入队（上游同样每帧入队一次；幂等在**行**上，不在入队上）。
    assert_eq!(port.enqueued().len(), 2);

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_passing_mention_links_nothing_and_an_off_toggle_writes_no_link_rows() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    let mention = seed_issue(
        &pool,
        workspace_id,
        user_id,
        9,
        &format!("{prefix}-9"),
        "todo",
    )
    .await;

    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));

    // ① body 里的裸提及（`Related`）不是主张 ⇒ 不建关联账。
    let frame = json!({
        "action": "opened",
        "pull_request": {
            "number": 2,
            "html_url": "https://github.com/acme/api/pull/2",
            "title": "just a change",
            "body": format!("Related {prefix}-9"),
            "state": "open", "draft": false, "merged": false,
            "created_at": "2026-09-25T06:00:00Z", "updated_at": "2026-09-25T06:00:00Z",
            "head": { "ref": "feat/y", "sha": "sha-2" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(pr_count(&pool, workspace_id).await, 1, "PR 行照样落库");
    assert!(
        links_of(&pool, workspace_id).await.is_empty(),
        "裸提及不建关联账"
    );
    assert_eq!(issue_status(&pool, mention).await, "todo");

    // ② 关掉自动关联 ⇒ 连 claim 都不建关联账（PR 行照旧）。
    set_workspace_settings(
        &pool,
        workspace_id,
        json!({ "github_enabled": true, "github_auto_link_prs_enabled": false }),
    )
    .await;
    let frame = json!({
        "action": "opened",
        "pull_request": {
            "number": 3,
            "html_url": "https://github.com/acme/api/pull/3",
            "title": format!("{prefix}-9: claim"),
            "state": "open", "draft": false, "merged": false,
            "created_at": "2026-09-25T06:00:00Z", "updated_at": "2026-09-25T06:00:00Z",
            "head": { "ref": "feat/z", "sha": "sha-3" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(pr_count(&pool, workspace_id).await, 2);
    assert!(
        links_of(&pool, workspace_id).await.is_empty(),
        "自动关联关掉 ⇒ 一行关联账都不写"
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}

/// 多绑定扇出 + **投递级**关闭裁决：`Closes` 的同一个标识符在两个 workspace 里都解析得到 ⇒
/// 歧义 ⇒ **两个 workspace 都不带关闭意图**（`#6804` 的 fail-closed）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn ambiguous_closing_identifier_is_withheld_across_bound_workspaces() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    // 两个 workspace 绑**同一个** installation，且 slug 的前 8 个字母数字相同（于是
    // `issue_prefix_from_slug` 给出**同一个**前缀）⇒ 载荷里的同一个标识符在两边都能解析。
    let installation_id = unique_installation_id();
    let shared = format!("m84amb{}", Uuid::new_v4().simple());
    let (first_ws, first_user, first_prefix) =
        seed_workspace_with_slug(&pool, "owner", &shared).await;
    let (second_ws, second_user, second_prefix) =
        seed_workspace_with_slug(&pool, "owner", &format!("{shared}z")).await;
    assert_eq!(first_prefix, second_prefix, "两个 slug 必须给出同一个前缀");
    seed_installation(&pool, first_ws, installation_id, "acme").await;
    seed_installation(&pool, second_ws, installation_id, "acme").await;
    let first_issue = seed_issue(
        &pool,
        first_ws,
        first_user,
        7,
        &format!("{first_prefix}-7"),
        "todo",
    )
    .await;
    let second_issue = seed_issue(
        &pool,
        second_ws,
        second_user,
        7,
        &format!("{second_prefix}-7"),
        "todo",
    )
    .await;

    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));

    let frame = json!({
        "action": "closed",
        "pull_request": {
            "number": 21,
            "html_url": "https://github.com/acme/api/pull/21",
            "title": "work",
            // 两个 workspace 的**名字**不同，但载荷里只能写一个前缀 ⇒ 用第一个的前缀，
            // 于是它同时是「first 解析得到」与「second 的前缀不匹配」⇒ 恰好唯一。
            // 为了造出真歧义，这里用第一个前缀写两次不同的号：号在两库里都存在。
            "body": format!("Closes {first_prefix}-7 and {second_prefix}-7"),
            "state": "closed", "draft": false, "merged": true,
            "created_at": "2026-09-25T06:00:00Z", "updated_at": "2026-09-25T06:00:00Z",
            "head": { "ref": "feat/amb", "sha": "sha-amb" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // 两边都**照建关联账**（链接是可恢复的噪音），但 close_intent 全为假 ⇒ 两边都不推进。
    let first_links = links_of(&pool, first_ws).await;
    let second_links = links_of(&pool, second_ws).await;
    assert_eq!(first_links, vec![(first_issue, false)]);
    assert_eq!(second_links, vec![(second_issue, false)]);
    assert_eq!(issue_status(&pool, first_issue).await, "todo");
    assert_eq!(issue_status(&pool, second_issue).await, "todo");
    // 扇出 ⇒ 两次入队（每个 workspace 一次）。
    assert_eq!(port.enqueued().len(), 2);

    cleanup(&pool, first_ws, &[first_user]).await;
    cleanup(&pool, second_ws, &[second_user]).await;
    reset_pr_refresh_port();
}

// ---------------------------------------------------------------------------
// installation 族
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn installation_deleted_drops_every_binding_and_broadcasts_per_workspace() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, _) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    let binding_id = seed_installation(&pool, workspace_id, installation_id, "acme").await;
    let (app, state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    let mut events = state.realtime.subscribe();

    let frame = json!({
        "action": "deleted",
        "installation": { "id": installation_id, "account": { "login": "acme", "type": "Organization" } }
    });
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "installation", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM github_installation WHERE installation_id = $1")
            .bind(installation_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0, "卸载 ⇒ 掉**每一条** workspace 绑定");

    // 广播里只出现内部行 id（数字 installation_id 是管理手柄，非 admin 不该看到）。
    let mut seen = Vec::new();
    while let Ok(envelope) = events.rx.try_recv() {
        seen.push(envelope);
    }
    let deleted = seen
        .iter()
        .find(|envelope| envelope.event_type == "github_installation:deleted")
        .expect("deleted broadcast");
    assert_eq!(deleted.resource_id, workspace_id.to_string());
    assert_eq!(deleted.payload["id"], binding_id.to_string());
    assert!(
        !deleted
            .payload
            .to_string()
            .contains(&installation_id.to_string()),
        "广播载荷不得回显 installation_id"
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn installation_created_stores_pending_then_refreshes_the_bound_account() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    let installation_id = unique_installation_id();

    // ① 没有任何绑定 ⇒ 留一份 pending（等 setup 回调建完绑定再消费）。
    let frame = json!({
        "action": "created",
        "installation": {
            "id": installation_id,
            "account": { "login": "acme-org", "type": "Organization", "avatar_url": "https://a/x.png" }
        }
    });
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "installation", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pending: (String, String, Option<String>) = sqlx::query_as(
        "SELECT account_login, account_type, account_avatar_url FROM github_pending_installation \
         WHERE installation_id = $1",
    )
    .bind(installation_id)
    .fetch_one(&pool)
    .await
    .expect("pending row");
    assert_eq!(pending.0, "acme-org");
    assert_eq!(pending.1, "Organization");
    assert_eq!(pending.2.as_deref(), Some("https://a/x.png"));

    // ② 有绑定（且账号信息是过期的）⇒ 刷新每一条绑定的展示元数据并清掉 pending。
    let (workspace_id, user_id, _) = seed_workspace(&pool, "owner").await;
    seed_installation(&pool, workspace_id, installation_id, "stale-login").await;
    let frame = json!({
        "action": "new_permissions_accepted",
        "installation": {
            "id": installation_id,
            "account": { "login": "acme-org", "type": "Organization", "avatar_url": "https://a/y.png" }
        }
    });
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "installation", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let refreshed: (String, Option<String>) = sqlx::query_as(
        "SELECT account_login, account_avatar_url FROM github_installation WHERE installation_id = $1",
    )
    .bind(installation_id)
    .fetch_one(&pool)
    .await
    .expect("installation row");
    assert_eq!(refreshed.0, "acme-org");
    assert_eq!(refreshed.1.as_deref(), Some("https://a/y.png"));
    let pending_left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM github_pending_installation WHERE installation_id = $1",
    )
    .bind(installation_id)
    .fetch_one(&pool)
    .await
    .expect("count pending");
    assert_eq!(pending_left, 0);

    // ③ 未建模的 action（如 `added`）⇒ 确认后什么都不做。
    let (status, _) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "installation",
            &json!({ "action": "added", "installation": { "id": installation_id } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pending_left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM github_pending_installation WHERE installation_id = $1",
    )
    .bind(installation_id)
    .fetch_one(&pool)
    .await
    .expect("count pending again");
    assert_eq!(pending_left, 0);

    cleanup(&pool, workspace_id, &[user_id]).await;
    // pending 表没有 workspace 归属 ⇒ 单独清（否则污染下一次同名用例）。
    let _ = sqlx::query("DELETE FROM github_pending_installation WHERE installation_id = $1")
        .bind(installation_id)
        .execute(&pool)
        .await;
}

// ---------------------------------------------------------------------------
// 协议面：ping / 未建模事件 / 公开（无会话）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn ping_is_200_and_unmodelled_events_are_confirmed_with_202() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));

    // `ping` 是唯一的 200 特例 —— 而且**不带任何会话**（公开路由）。
    let (status, body) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "ping", &json!({ "zen": "hi" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": "pong" }));

    // 未建模事件：确认（202）后忽略，body 为空。
    let (status, body) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "push",
            &json!({ "ref": "refs/heads/main" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, serde_json::Value::Null);

    // 坏载荷：仍然 202（上游三个 handler 的第一段是 warn + return，绝不把坏载荷变成 4xx）。
    let (status, _) = call(
        &app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &json!([1, 2, 3])),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    cleanup(&pool, Uuid::nil(), &[]).await;
}

/// 构造一帧最小的 `pull_request` 载荷（给两个反例用）。
fn pull_request_frame(
    action: &str,
    number: i32,
    title: &str,
    body: &str,
    draft: bool,
    merged: bool,
    installation_id: i64,
) -> serde_json::Value {
    json!({
        "action": action,
        "pull_request": {
            "number": number,
            "html_url": format!("https://github.com/acme/api/pull/{number}"),
            "title": title,
            "body": body,
            "state": if merged { "closed" } else { "open" },
            "draft": draft,
            "merged": merged,
            "created_at": "2026-09-25T06:00:00Z",
            "updated_at": "2026-09-25T06:00:00Z",
            "head": { "ref": "feat/x", "sha": "sha-x" }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    })
}
